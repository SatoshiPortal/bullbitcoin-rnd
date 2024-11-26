//!
//! ### Boltz v2 API
//! ## Estimate fees
//!
//! ### Example
//! ```
//! let client = BoltzApiClient::new(BOLTZ_MAINNET_URL);
//! let pairs = client.get_pairs()?;
//! let btc_pair = pairs.get_btc_pair();
//! let output_amount = 75_000;
//! let base_fees = btc_pair.fees.reverse_base(output_amount)?;
//! let claim_fee = btc_pair.fees.reverse_claim_estimate();
//! println!("CALCULATED FEES: {}", base_fees);
//! println!("ONCHAIN LOCKUP: {}", output_amount - base_fees);
//! println!(
//!     "ONCHAIN RECIEVABLE: {}",
//!     output_amount - base_fees - claim_fee
//! );

use bitcoin::{
    hashes::sha256, hex::DisplayHex, taproot::TapLeaf, PublicKey, ScriptBuf, Transaction,
};
use bitcoin::{key, Amount};
use lightning_invoice::Bolt11Invoice;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::str::FromStr;
use std::sync::Arc;
use std::{collections::HashMap, fmt::format, net::TcpStream};
use tungstenite::{connect, http::response, stream::MaybeTlsStream, WebSocket};
use ureq::json;
use ureq::{AgentBuilder, TlsConnector};

use crate::{error::Error, network::Chain, util::secrets::Preimage};
use crate::{BtcSwapScript, LBtcSwapScript};

pub const BOLTZ_TESTNET_URL_V2: &str = "https://api.testnet.boltz.exchange/v2";
pub const BOLTZ_MAINNET_URL_V2: &str = "https://api.boltz.exchange/v2";
pub const BOLTZ_REGTEST: &str = "http://127.0.0.1:9001/v2";

use url::Url;

use elements::secp256k1_zkp::{
    MusigAggNonce, MusigKeyAggCache, MusigPartialSignature, MusigPubNonce, MusigSession,
    MusigSessionId,
};

#[derive(Serialize, Deserialize, Debug)]
pub struct HeightResponse {
    #[serde(rename = "BTC")]
    pub btc: u32,
    #[serde(rename = "L-BTC")]
    pub lbtc: u32,
}

fn check_limits_within(maximal: u64, minimal: u64, output_amount: u64) -> Result<(), Error> {
    if output_amount < minimal as u64 {
        return Err(Error::Protocol(format!(
            "Output amount is below minimum {}",
            minimal
        )));
    }
    if output_amount > maximal as u64 {
        return Err(Error::Protocol(format!(
            "Output amount is above maximum {}",
            maximal
        )));
    }
    Ok(())
}

/// Various limits of swap parameters
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PairLimits {
    /// Maximum swap amount
    pub maximal: u64,
    /// Minimum swap amount
    pub minimal: u64,
    /// Maximum amount allowed for zero-conf
    pub maximal_zero_conf: u64,
}

impl PairLimits {
    /// Check whether the output amount intended is within the Limits
    pub fn within(&self, output_amount: u64) -> Result<(), Error> {
        check_limits_within(self.maximal, self.minimal, output_amount)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ReverseLimits {
    /// Maximum swap amount
    pub maximal: u64,
    /// Minimum swap amount
    pub minimal: u64,
}

impl ReverseLimits {
    /// Check whether the output amount intended is within the Limits
    pub fn within(&self, output_amount: u64) -> Result<(), Error> {
        check_limits_within(self.maximal, self.minimal, output_amount)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PairMinerFees {
    pub lockup: u64,
    pub claim: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ChainMinerFees {
    pub server: u64,
    pub user: PairMinerFees,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ChainFees {
    pub percentage: f64,
    pub miner_fees: ChainMinerFees,
}

impl ChainFees {
    pub fn total(&self, amount_sat: u64) -> u64 {
        self.boltz(amount_sat) + self.claim_estimate() + self.lockup() + self.server()
    }

    pub fn boltz(&self, amount_sat: u64) -> u64 {
        ((self.percentage / 100.0) * amount_sat as f64).ceil() as u64
    }

    pub fn claim_estimate(&self) -> u64 {
        self.miner_fees.user.claim
    }

    pub fn lockup(&self) -> u64 {
        self.miner_fees.user.lockup
    }

    pub fn server(&self) -> u64 {
        self.miner_fees.server
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ReverseFees {
    pub percentage: f64,
    pub miner_fees: PairMinerFees,
}

impl ReverseFees {
    pub fn total(&self, invoice_amount_sat: u64) -> u64 {
        self.boltz(invoice_amount_sat) + self.claim_estimate() + self.lockup()
    }

    pub fn boltz(&self, invoice_amount_sat: u64) -> u64 {
        ((self.percentage / 100.0) * invoice_amount_sat as f64).ceil() as u64
    }

    pub fn claim_estimate(&self) -> u64 {
        self.miner_fees.claim
    }

    pub fn lockup(&self) -> u64 {
        self.miner_fees.lockup
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SubmarineFees {
    /// The percentage of the "send amount" that is charged by Boltz as "Boltz Fee".
    pub percentage: f64,
    /// The network fees charged for locking up and claiming funds onchain. These values are absolute, denominated in 10 ** -8 of the quote asset.
    pub miner_fees: u64,
}

impl SubmarineFees {
    pub fn total(&self, invoice_amount_sat: u64) -> u64 {
        self.boltz(invoice_amount_sat) + self.network()
    }

    pub fn boltz(&self, invoice_amount_sat: u64) -> u64 {
        ((self.percentage / 100.0) * invoice_amount_sat as f64).ceil() as u64
    }

    pub fn network(&self) -> u64 {
        self.miner_fees
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ChainPair {
    /// Pair hash, representing an id for an asset-pair swap
    pub hash: String,
    /// The exchange rate of the pair
    pub rate: f64,
    /// The swap limits
    pub limits: PairLimits,
    /// Total fees required for the swap
    pub fees: ChainFees,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ReversePair {
    /// Pair hash, representing an id for an asset-pair swap
    pub hash: String,
    /// The exchange rate of the pair
    pub rate: f64,
    /// The swap limits
    pub limits: ReverseLimits,
    /// Total fees required for the swap
    pub fees: ReverseFees,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SubmarinePair {
    /// Pair hash, representing an id for an asset-pair swap
    pub hash: String,
    /// The exchange rate of the pair
    pub rate: f64,
    /// The swap limits
    pub limits: PairLimits,
    /// Total fees required for the swap
    pub fees: SubmarineFees,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetSubmarinePairsResponse {
    #[serde(rename = "BTC")]
    pub btc: HashMap<String, SubmarinePair>,
    #[serde(rename = "L-BTC")]
    pub lbtc: HashMap<String, SubmarinePair>,
}

impl GetSubmarinePairsResponse {
    /// Get the BtcBtc Pair data from the response.
    /// Returns None if not found.
    pub fn get_btc_to_btc_pair(&self) -> Option<SubmarinePair> {
        self.btc.get("BTC").cloned()
    }

    /// Get the BtcLBtc Pair data from the response.
    /// Returns None if not found.
    pub fn get_btc_to_lbtc_pair(&self) -> Option<SubmarinePair> {
        self.btc.get("L-BTC").cloned()
    }

    /// Get the LBtcBtc Pair data from the response.
    /// Returns None if not found.
    pub fn get_lbtc_to_btc_pair(&self) -> Option<SubmarinePair> {
        self.lbtc.get("BTC").cloned()
    }

    /// Get the LBtcLBtc Pair data from the response.
    /// Returns None if not found.
    pub fn get_lbtc_to_lbtc_pair(&self) -> Option<SubmarinePair> {
        self.lbtc.get("L-BTC").cloned()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetReversePairsResponse {
    #[serde(rename = "BTC")]
    pub btc: HashMap<String, ReversePair>,
}

impl GetReversePairsResponse {
    /// Get the BtcBtc Pair data from the response.
    /// Returns None if not found.
    pub fn get_btc_to_btc_pair(&self) -> Option<ReversePair> {
        self.btc.get("BTC").cloned()
    }

    /// Get the BtcLBtc Pair data from the response.
    /// Returns None if not found.
    pub fn get_btc_to_lbtc_pair(&self) -> Option<ReversePair> {
        self.btc.get("L-BTC").cloned()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetChainPairsResponse {
    #[serde(rename = "BTC")]
    pub btc: HashMap<String, ChainPair>,
    #[serde(rename = "L-BTC")]
    pub lbtc: HashMap<String, ChainPair>,
}

impl GetChainPairsResponse {
    /// Get the BtcLBtc Pair data from the response.
    /// Returns None if not found.
    pub fn get_btc_to_lbtc_pair(&self) -> Option<ChainPair> {
        self.btc.get("L-BTC").cloned()
    }

    /// Get the LBtcBtc Pair data from the response.
    /// Returns None if not found.
    pub fn get_lbtc_to_btc_pair(&self) -> Option<ChainPair> {
        self.lbtc.get("BTC").cloned()
    }
}

/// Reference Documnetation: <https://api.boltz.exchange/swagger>
#[derive(Debug, Clone)]
pub struct BoltzApiClientV2 {
    base_url: String,
}

impl BoltzApiClientV2 {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
        }
    }

    /// Returns the web socket connection to the boltz server
    pub fn connect_ws(&self) -> Result<WebSocket<MaybeTlsStream<TcpStream>>, Error> {
        let ws_string = self.base_url.clone().replace("http", "ws") + "/ws";
        let (socket, response) = connect(Url::parse(&ws_string)?)?;
        log::debug!("websocket response: {:?}", response);
        Ok(socket)
    }

    /// Make a get request. returns the Response
    fn get(&self, end_point: &str) -> Result<String, Error> {
        let url = format!("{}/{}", self.base_url, end_point);
        Ok(ureq::get(&url).call()?.into_string()?)
    }

    /// Make a Post request. Returns the Response
    fn post(&self, end_point: &str, data: impl Serialize) -> Result<String, Error> {
        let url = format!("{}/{}", self.base_url, end_point);
        // Ok(ureq::post(&url).send_json(data)?.into_string()?)

        let response = match native_tls::TlsConnector::new() {
            // If native_tls is available, use that for TLS
            // It has better handling of close_notify, which avoids some POST call failures
            // See https://github.com/SatoshiPortal/boltz-rust/issues/39
            Ok(tls_connector) => {
                let response = match AgentBuilder::new()
                    .tls_connector(Arc::new(tls_connector))
                    .build()
                    .request("POST", &url)
                    .send_json(data)
                {
                    Ok(r) => {
                        log::debug!("POST response: {:#?}", r);
                        r.into_string()?
                    }
                    Err(ureq_err) => {
                        log::error!("POST error: {:#?}", ureq_err);
                        let err = match ureq_err {
                            ureq::Error::Status(_code, err_resp) => {
                                let e_val: Value = serde_json::from_str(&err_resp.into_string()?)?;
                                let e_str = e_val.get("error").unwrap_or(&Value::Null).to_string();
                                Error::HTTP(e_str)
                            }
                            ureq::Error::Transport(_) => ureq_err.into(),
                        };
                        return Err(err.into());
                    }
                };
                response
            }
            // If native_tls is not available, fallback to the default (rustls)
            Err(_) => ureq::post(&url).send_json(data)?.into_string()?,
        };
        Ok(response)
    }

    pub fn get_fee_estimation(&self) -> Result<GetFeeEstimationResponse, Error> {
        Ok(serde_json::from_str(&self.get("chain/fees")?)?)
    }

    pub fn get_height(&self) -> Result<HeightResponse, Error> {
        Ok(serde_json::from_str(&self.get("chain/heights")?)?)
    }

    pub fn get_submarine_pairs(&self) -> Result<GetSubmarinePairsResponse, Error> {
        Ok(serde_json::from_str(&self.get("swap/submarine")?)?)
    }

    pub fn get_reverse_pairs(&self) -> Result<GetReversePairsResponse, Error> {
        Ok(serde_json::from_str(&self.get("swap/reverse")?)?)
    }

    pub fn get_chain_pairs(&self) -> Result<GetChainPairsResponse, Error> {
        Ok(serde_json::from_str(&self.get("swap/chain")?)?)
    }

    pub fn post_swap_req(
        &self,
        swap_request: &CreateSubmarineRequest,
    ) -> Result<CreateSubmarineResponse, Error> {
        let data = serde_json::to_value(swap_request)?;
        Ok(serde_json::from_str(&self.post("swap/submarine", data)?)?)
    }

    pub fn post_reverse_req(
        &self,
        req: CreateReverseRequest,
    ) -> Result<CreateReverseResponse, Error> {
        Ok(serde_json::from_str(&self.post("swap/reverse", req)?)?)
    }

    pub fn post_chain_req(&self, req: CreateChainRequest) -> Result<CreateChainResponse, Error> {
        Ok(serde_json::from_str(&self.post("swap/chain", req)?)?)
    }

    pub fn get_submarine_claim_tx_details(
        &self,
        id: &String,
    ) -> Result<SubmarineClaimTxResponse, Error> {
        let endpoint = format!("swap/submarine/{}/claim", id);
        Ok(serde_json::from_str(&self.get(&endpoint)?)?)
    }

    pub fn get_chain_claim_tx_details(&self, id: &String) -> Result<ChainClaimTxResponse, Error> {
        let endpoint = format!("swap/chain/{}/claim", id);
        Ok(serde_json::from_str(&self.get(&endpoint)?)?)
    }

    pub fn post_submarine_claim_tx_details(
        &self,
        id: &String,
        pub_nonce: MusigPubNonce,
        partial_sig: MusigPartialSignature,
    ) -> Result<Value, Error> {
        let data = json!(
            {
                "pubNonce": pub_nonce.serialize().to_lower_hex_string(),
                "partialSignature": partial_sig.serialize().to_lower_hex_string()
            }
        );
        let endpoint = format!("swap/submarine/{}/claim", id);
        Ok(serde_json::from_str(&self.post(&endpoint, data)?)?)
    }

    pub fn post_chain_claim_tx_details(
        &self,
        id: &String,
        preimage: &Preimage,
        pub_nonce: MusigPubNonce,
        partial_sig: MusigPartialSignature,
        to_sign: ToSign,
    ) -> Result<PartialSig, Error> {
        let data = json!(
            {
                "preimage": preimage.bytes.expect("expected").to_lower_hex_string(),
                "signature": PartialSig {
                    pub_nonce: pub_nonce.serialize().to_lower_hex_string(),
                    partial_signature: partial_sig.serialize().to_lower_hex_string(),
                },
                "toSign": to_sign,
            }
        );
        let endpoint = format!("swap/chain/{}/claim", id);
        Ok(serde_json::from_str(&self.post(&endpoint, data)?)?)
    }

    pub fn get_reverse_tx(&self, id: &str) -> Result<ReverseSwapTxResp, Error> {
        Ok(serde_json::from_str(
            &self.get(&format!("swap/reverse/{}/transaction", id))?,
        )?)
    }

    pub fn get_submarine_tx(&self, id: &str) -> Result<SubmarineSwapTxResp, Error> {
        Ok(serde_json::from_str(
            &self.get(&format!("swap/submarine/{}/transaction", id))?,
        )?)
    }

    pub fn get_chain_txs(&self, id: &str) -> Result<ChainSwapTxResp, Error> {
        Ok(serde_json::from_str(
            &self.get(&format!("swap/chain/{}/transactions", id))?,
        )?)
    }

    pub fn get_reverse_partial_sig(
        &self,
        id: &String,
        preimage: &Preimage,
        pub_nonce: &MusigPubNonce,
        claim_tx_hex: &String,
    ) -> Result<PartialSig, Error> {
        let data = json!(
            {
                "preimage": preimage.bytes.expect("expected").to_lower_hex_string(),
                "pubNonce": pub_nonce.serialize().to_lower_hex_string(),
                "transaction": claim_tx_hex,
                "index": 0
            }
        );

        let endpoint = format!("swap/reverse/{}/claim", id);
        Ok(serde_json::from_str(&self.post(&endpoint, data)?)?)
    }

    pub fn get_submarine_partial_sig(
        &self,
        id: &String,
        input_index: usize,
        pub_nonce: &MusigPubNonce,
        refund_tx_hex: &String,
    ) -> Result<PartialSig, Error> {
        let data = json!(
            {
                "pubNonce": pub_nonce.serialize().to_lower_hex_string(),
                "transaction": refund_tx_hex,
                "index": input_index
            }
        );

        let endpoint = format!("swap/submarine/{}/refund", id);
        Ok(serde_json::from_str(&self.post(&endpoint, data)?)?)
    }

    pub fn get_chain_partial_sig(
        &self,
        id: &String,
        input_index: usize,
        pub_nonce: &MusigPubNonce,
        refund_tx_hex: &String,
    ) -> Result<PartialSig, Error> {
        let data = json!(
            {
                "pubNonce": pub_nonce.serialize().to_lower_hex_string(),
                "transaction": refund_tx_hex,
                "index": input_index
            }
        );

        let endpoint = format!("swap/chain/{}/refund", id);
        Ok(serde_json::from_str(&self.post(&endpoint, data)?)?)
    }

    pub fn get_mrh_bip21(&self, invoice: &str) -> Result<MrhResponse, Error> {
        let request = format!("swap/reverse/{}/bip21", invoice);
        Ok(serde_json::from_str(&self.get(&request)?)?)
    }

    pub fn broadcast_tx(&self, chain: Chain, tx_hex: &String) -> Result<Value, Error> {
        let data = json!(
            {
                "hex": tx_hex
            }
        );

        let chain = match chain {
            Chain::Bitcoin | Chain::BitcoinRegtest | Chain::BitcoinTestnet => "BTC",
            Chain::Liquid | Chain::LiquidTestnet | Chain::LiquidRegtest => "L-BTC",
        };

        let end_point = format!("chain/{}/transaction", chain);
        Ok(serde_json::from_str(&self.post(&end_point, data)?)?)
    }

    /// Looks up the quote for a Zero-Amount Receive Chain Swap.
    ///
    /// If the user locked up a valid amount, it will return the server lockup amount. In all other
    /// cases, it will return an error.
    pub fn get_quote(&self, swap_id: &str) -> Result<GetQuoteResponse, Error> {
        let end_point = format!("swap/chain/{swap_id}/quote");
        Ok(serde_json::from_str(&self.get(&end_point)?)?)
    }

    /// Accepts a specific quote for a Zero-Amount or over- or underpaid Chain Swap.
    pub fn accept_quote(&self, swap_id: &str, amount_sat: u64) -> Result<(), Error> {
        let data = json!(
            {
                "amount": amount_sat
            }
        );

        let end_point = format!("swap/chain/{swap_id}/quote");
        self.post(&end_point, data)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainClaimTxResponse {
    pub pub_nonce: String,
    pub public_key: PublicKey,
    pub transaction_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmarineClaimTxResponse {
    pub preimage: String,
    pub pub_nonce: String,
    pub public_key: PublicKey,
    pub transaction_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MrhResponse {
    pub bip21: String,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Webhook<T> {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash_swap_id: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Vec<T>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSubmarineRequest {
    pub from: String,
    pub to: String,
    pub invoice: String,
    pub refund_public_key: PublicKey,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pair_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referral_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook: Option<Webhook<SubSwapStates>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSubmarineResponse {
    pub accept_zero_conf: bool,
    pub address: String,
    pub bip21: String,
    pub claim_public_key: PublicKey,
    pub expected_amount: u64,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referral_id: Option<String>,
    pub swap_tree: SwapTree,
    pub timeout_block_height: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blinding_key: Option<String>,
}
impl CreateSubmarineResponse {
    /// Ensure submarine swap redeem script uses the preimage hash used in the invoice
    pub fn validate(
        &self,
        invoice: &str,
        our_pubkey: &PublicKey,
        chain: Chain,
    ) -> Result<(), Error> {
        let preimage = Preimage::from_invoice_str(&invoice).unwrap();

        match chain {
            Chain::Bitcoin | Chain::BitcoinTestnet | Chain::BitcoinRegtest => {
                let boltz_sub_script = BtcSwapScript::submarine_from_swap_resp(&self, *our_pubkey)?;
                boltz_sub_script.validate_address(chain, self.address.clone())
            }
            Chain::Liquid | Chain::LiquidTestnet | Chain::LiquidRegtest => {
                let blinding_key = self.blinding_key.as_ref().unwrap();
                let boltz_sub_script =
                    LBtcSwapScript::submarine_from_swap_resp(&self, *our_pubkey)?;
                if &boltz_sub_script.hashlock != &preimage.hash160 {
                    return Err(Error::Protocol(format!(
                        "Hash160 mismatch: {},{}",
                        boltz_sub_script.hashlock,
                        preimage.hash160.to_string()
                    )));
                }

                boltz_sub_script.validate_address(chain, self.address.clone())
            }
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwapTree {
    pub claim_leaf: Leaf,
    pub refund_leaf: Leaf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Leaf {
    pub output: String,
    pub version: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    op: String,
    channel: String,
    args: Vec<String>,
}

impl Subscription {
    pub fn new(id: &String) -> Self {
        Self {
            op: "subscribe".to_string(),
            channel: "swap.update".to_string(),
            args: vec![id.clone()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateReverseRequest {
    pub invoice_amount: u64,
    pub from: String,
    pub to: String,
    pub preimage_hash: sha256::Hash,
    pub claim_public_key: PublicKey,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address_signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referral_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook: Option<Webhook<RevSwapStates>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateReverseResponse {
    pub id: String,
    pub invoice: String,
    pub swap_tree: SwapTree,
    pub lockup_address: String,
    pub refund_public_key: PublicKey,
    pub timeout_block_height: u32,
    pub onchain_amount: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blinding_key: Option<String>,
}
impl CreateReverseResponse {
    /// Validate reverse swap response
    /// Ensure reverse swap invoice uses the provided preimage
    /// Ensure reverse swap redeem script matches locally constructured SwapScript
    pub fn validate(
        &self,
        preimage: &Preimage,
        our_pubkey: &PublicKey,
        chain: Chain,
    ) -> Result<(), Error> {
        let invoice = Bolt11Invoice::from_str(&self.invoice)?;
        if &invoice.payment_hash().to_string() == &preimage.sha256.to_string() {
            ()
        } else {
            return Err(Error::Protocol(format!(
                "Preimage missmatch : {},{}",
                &invoice.payment_hash().to_string(),
                preimage.sha256.to_string()
            )));
        }

        match chain {
            Chain::Bitcoin | Chain::BitcoinTestnet | Chain::BitcoinRegtest => {
                let boltz_rev_script = BtcSwapScript::reverse_from_swap_resp(&self, *our_pubkey)?;
                boltz_rev_script.validate_address(chain, self.lockup_address.clone())
            }
            Chain::Liquid | Chain::LiquidTestnet | Chain::LiquidRegtest => {
                let blinding_key = self.blinding_key.as_ref().unwrap();
                let boltz_rev_script = LBtcSwapScript::reverse_from_swap_resp(&self, *our_pubkey)?;
                boltz_rev_script.validate_address(chain, self.lockup_address.clone())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Side {
    Lockup,
    Claim,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainSwapDetails {
    pub swap_tree: SwapTree,
    pub lockup_address: String,
    pub server_public_key: PublicKey,
    pub timeout_block_height: u32,
    pub amount: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blinding_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refund_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bip21: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateChainRequest {
    pub from: String,
    pub to: String,
    pub preimage_hash: sha256::Hash,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_public_key: Option<PublicKey>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refund_public_key: Option<PublicKey>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_lock_amount: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_lock_amount: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pair_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referral_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook: Option<Webhook<ChainSwapStates>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateChainResponse {
    pub id: String,
    pub claim_details: ChainSwapDetails,
    pub lockup_details: ChainSwapDetails,
}
impl CreateChainResponse {
    /// Validate chain swap response
    pub fn validate(
        &self,
        claim_pubkey: &PublicKey,
        refund_pubkey: &PublicKey,
        from_chain: Chain,
        to_chain: Chain,
    ) -> Result<(), Error> {
        self.validate_side(
            Side::Lockup,
            from_chain,
            &self.lockup_details,
            refund_pubkey,
        )?;
        self.validate_side(Side::Claim, to_chain, &self.claim_details, claim_pubkey)
    }

    fn validate_side(
        &self,
        side: Side,
        chain: Chain,
        details: &ChainSwapDetails,
        our_pubkey: &PublicKey,
    ) -> Result<(), Error> {
        match chain {
            Chain::Bitcoin | Chain::BitcoinTestnet | Chain::BitcoinRegtest => {
                let boltz_chain_script =
                    BtcSwapScript::chain_from_swap_resp(side, details.clone(), *our_pubkey)?;
                boltz_chain_script.validate_address(chain, details.lockup_address.clone())
            }
            Chain::Liquid | Chain::LiquidTestnet | Chain::LiquidRegtest => {
                let blinding_key = details.blinding_key.as_ref().unwrap();
                let boltz_chain_script =
                    LBtcSwapScript::chain_from_swap_resp(side, details.clone(), *our_pubkey)?;
                boltz_chain_script.validate_address(chain, details.lockup_address.clone())
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainSwapTx {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hex: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainSwapTxTimeout {
    pub block_height: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainSwapTxLock {
    pub transaction: ChainSwapTx,
    pub timeout: ChainSwapTxTimeout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainSwapTxResp {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_lock: Option<ChainSwapTxLock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_lock: Option<ChainSwapTxLock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReverseSwapTxResp {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hex: Option<String>,
    pub timeout_block_height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmarineSwapTxResp {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_block_height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_eta: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialSig {
    pub pub_nonce: String,
    pub partial_signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToSign {
    pub pub_nonce: String,
    pub transaction: String,
    pub index: u32,
}

#[derive(Debug, Clone)]
pub struct Cooperative<'a> {
    pub boltz_api: &'a BoltzApiClientV2,
    pub swap_id: String,
    /// The pub_nonce is needed to post the claim tx details of the Chain swap
    pub pub_nonce: Option<MusigPubNonce>,
    /// The partial_sig is needed to post the claim tx details of the Chain swap
    pub partial_sig: Option<MusigPartialSignature>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapUpdateTxDetails {
    pub id: String,
    pub hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Update {
    pub id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transaction: Option<SwapUpdateTxDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zero_conf_rejected: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RespError {
    pub id: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SwapUpdate {
    Subscription {
        event: String,
        channel: String,
        args: Vec<String>,
    },
    Update {
        event: String,
        channel: String,
        args: Vec<Update>,
    },
    Error {
        event: String,
        channel: String,
        args: Vec<RespError>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum SwapTxKind {
    Claim,
    Refund,
}

/// States for a submarine swap.
///
/// See <https://docs.boltz.exchange/v/api/lifecycle#normal-submarine-swaps>
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SubSwapStates {
    /// Initial state of the swap; optionally the initial state can also be `invoice.set` in case
    /// the invoice was already specified in the request that created the swap.
    #[serde(rename = "swap.created")]
    Created,
    /// The lockup transaction was found in the mempool, meaning the user sent funds to the
    /// lockup address.
    #[serde(rename = "transaction.mempool")]
    TransactionMempool,
    /// The lockup transaction was included in a block.
    #[serde(rename = "transaction.confirmed")]
    TransactionConfirmed,
    /// The swap has an invoice that should be paid.
    /// Can be the initial state when the invoice was specified in the request that created the swap
    #[serde(rename = "invoice.set")]
    InvoiceSet,
    /// Boltz successfully paid the invoice.
    #[serde(rename = "invoice.paid")]
    InvoicePaid,
    /// Boltz started paying the invoice.
    #[serde(rename = "invoice.pending")]
    InvoicePending,
    /// Boltz failed to pay the invoice. In this case the user needs to broadcast a refund
    /// transaction to reclaim the locked up onchain coins.
    #[serde(rename = "invoice.failedToPay")]
    InvoiceFailedToPay,
    /// Indicates that after the invoice was successfully paid, the onchain were successfully
    /// claimed by Boltz. This is the final status of a successful Normal Submarine Swap.
    #[serde(rename = "transaction.claimed")]
    TransactionClaimed,
    /// Indicates that Boltz is ready for the creation of a cooperative signature for a key path
    /// spend. Taproot Swaps are not claimed immediately by Boltz after the invoice has been paid,
    /// but instead Boltz waits for the API client to post a signature for a key path spend. If the
    /// API client does not cooperate in a key path spend, Boltz will eventually claim via the script path.
    #[serde(rename = "transaction.claim.pending")]
    TransactionClaimPending,
    /// Indicates the lockup failed, which is usually because the user sent too little.
    #[serde(rename = "transaction.lockupFailed")]
    TransactionLockupFailed,
    /// Indicates the user didn't send onchain (lockup) and the swap expired (approximately 24h).
    /// This means that it was cancelled and chain L-BTC shouldn't be sent anymore.
    #[serde(rename = "swap.expired")]
    SwapExpired,
}

impl ToString for SubSwapStates {
    fn to_string(&self) -> String {
        match self {
            SubSwapStates::Created => "swap.created".to_string(),
            SubSwapStates::TransactionMempool => "transaction.mempool".to_string(),
            SubSwapStates::TransactionConfirmed => "transaction.confirmed".to_string(),
            SubSwapStates::InvoiceSet => "invoice.set".to_string(),
            SubSwapStates::InvoicePaid => "invoice.paid".to_string(),
            SubSwapStates::InvoicePending => "invoice.pending".to_string(),
            SubSwapStates::InvoiceFailedToPay => "invoice.failedToPay".to_string(),
            SubSwapStates::TransactionClaimed => "transaction.claimed".to_string(),
            SubSwapStates::TransactionClaimPending => "transaction.claim.pending".to_string(),
            SubSwapStates::TransactionLockupFailed => "transaction.lockupFailed".to_string(),
            SubSwapStates::SwapExpired => "swap.expired".to_string(),
        }
    }
}

impl FromStr for SubSwapStates {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "swap.created" => Ok(SubSwapStates::Created),
            "transaction.mempool" => Ok(SubSwapStates::TransactionMempool),
            "transaction.confirmed" => Ok(SubSwapStates::TransactionConfirmed),
            "invoice.set" => Ok(SubSwapStates::InvoiceSet),
            "invoice.paid" => Ok(SubSwapStates::InvoicePaid),
            "invoice.pending" => Ok(SubSwapStates::InvoicePending),
            "invoice.failedToPay" => Ok(SubSwapStates::InvoiceFailedToPay),
            "transaction.claimed" => Ok(SubSwapStates::TransactionClaimed),
            "transaction.claim.pending" => Ok(SubSwapStates::TransactionClaimPending),
            "transaction.lockupFailed" => Ok(SubSwapStates::TransactionLockupFailed),
            "swap.expired" => Ok(SubSwapStates::SwapExpired),
            _ => Err(()),
        }
    }
}

/// States for a reverse swap.
///
/// See <https://docs.boltz.exchange/v/api/lifecycle#reverse-submarine-swaps>
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RevSwapStates {
    /// Initial state of a newly created Reverse Submarine Swap.
    #[serde(rename = "swap.created")]
    Created,
    /// Optional and currently not enabled on Boltz. If Boltz requires prepaying miner fees via a
    /// separate Lightning invoice, this state is set when the miner fee invoice was successfully paid.
    #[serde(rename = "minerfee.paid")]
    MinerFeePaid,
    /// Boltz's lockup transaction is found in the mempool which will only happen after the user
    /// paid the Lightning hold invoice.
    #[serde(rename = "transaction.mempool")]
    TransactionMempool,
    /// The lockup transaction was included in a block. This state is skipped, if the client
    /// optionally accepts the transaction without confirmation. Boltz broadcasts chain transactions
    /// non-RBF only.
    #[serde(rename = "transaction.confirmed")]
    TransactionConfirmed,
    /// The transaction claiming onchain was broadcast by the user's client and Boltz used the
    /// preimage of this transaction to settle the Lightning invoice. This is the final status of a
    /// successful Reverse Submarine Swap.
    #[serde(rename = "invoice.settled")]
    InvoiceSettled,
    /// Set when the invoice of Boltz expired and pending HTLCs are cancelled. Boltz invoices
    /// currently expire after 50% of the swap timeout window.
    #[serde(rename = "invoice.expired")]
    InvoiceExpired,
    /// This is the final status of a swap, if the swap expires without the lightning invoice being paid.
    #[serde(rename = "swap.expired")]
    SwapExpired,
    /// Set in the unlikely event that Boltz is unable to send the agreed amount of onchain coins
    /// after the user set up the payment to the provided Lightning invoice. If this happens, the
    /// pending Lightning HTLC will also be cancelled. The Lightning bitcoin automatically bounce
    /// back to the user, no further action or refund is required and the user didn't pay any fees.
    #[serde(rename = "transaction.failed")]
    TransactionFailed,
    /// This is the final status of a swap, if the user successfully set up the Lightning payment
    /// and Boltz successfully locked up coins onchain, but the Boltz API Client did not claim
    /// the locked oncahin coins before swap expiry. In this case, Boltz will also automatically refund
    /// its own locked onchain coins and the Lightning payment is cancelled.
    #[serde(rename = "transaction.refunded")]
    TransactionRefunded,
}

impl ToString for RevSwapStates {
    fn to_string(&self) -> String {
        match self {
            RevSwapStates::Created => "swap.created".to_string(),
            RevSwapStates::MinerFeePaid => "minerfee.paid".to_string(),
            RevSwapStates::TransactionMempool => "transaction.mempool".to_string(),
            RevSwapStates::TransactionConfirmed => "transaction.confirmed".to_string(),
            RevSwapStates::InvoiceSettled => "invoice.settled".to_string(),
            RevSwapStates::InvoiceExpired => "invoice.expired".to_string(),
            RevSwapStates::SwapExpired => "swap.expired".to_string(),
            RevSwapStates::TransactionFailed => "transaction.failed".to_string(),
            RevSwapStates::TransactionRefunded => "transaction.refunded".to_string(),
        }
    }
}

impl FromStr for RevSwapStates {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "swap.created" => Ok(RevSwapStates::Created),
            "minerfee.paid" => Ok(RevSwapStates::MinerFeePaid),
            "transaction.mempool" => Ok(RevSwapStates::TransactionMempool),
            "transaction.confirmed" => Ok(RevSwapStates::TransactionConfirmed),
            "invoice.settled" => Ok(RevSwapStates::InvoiceSettled),
            "invoice.expired" => Ok(RevSwapStates::InvoiceExpired),
            "swap.expired" => Ok(RevSwapStates::SwapExpired),
            "transaction.failed" => Ok(RevSwapStates::TransactionFailed),
            "transaction.refunded" => Ok(RevSwapStates::TransactionRefunded),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChainSwapStates {
    /// The initial state of the chain swap.
    #[serde(rename = "swap.created")]
    Created,
    /// The server has rejected a 0-conf transaction for this swap.
    #[serde(rename = "transaction.zeroconf.rejected")]
    TransactionZeroConfRejected,
    /// The lockup transaction of the client was found in the mempool.
    #[serde(rename = "transaction.mempool")]
    TransactionMempool,
    /// The lockup transaction of the client was confirmed in a block. When the server accepts 0-conf,
    /// for the lockup transaction, this state is skipped.
    #[serde(rename = "transaction.confirmed")]
    TransactionConfirmed,
    /// The lockup transaction of the server has been broadcast.
    #[serde(rename = "transaction.server.mempool")]
    TransactionServerMempool,
    /// The lockup transaction of the server has been included in a block.
    #[serde(rename = "transaction.server.confirmed")]
    TransactionServerConfirmed,
    /// The server claimed the coins that the client locked.
    #[serde(rename = "transaction.claimed")]
    TransactionClaimed,
    /// Indicates the lockup failed, which is usually because the user sent too little.
    #[serde(rename = "transaction.lockupFailed")]
    TransactionLockupFailed,
    /// This is the final status of a swap, if the swap expires without a chain bitcoin transaction.
    #[serde(rename = "swap.expired")]
    SwapExpired,
    /// Set in the unlikely event that Boltz is unable to lock the agreed amount of chain bitcoin.
    /// The user needs to submit a refund transaction to reclaim the chain bitcoin if bitcoin were
    /// already sent.
    #[serde(rename = "transaction.failed")]
    TransactionFailed,
    /// If the user and Boltz both successfully locked up bitcoin on the chain, but the user did not
    /// claim the locked chain bitcoin until swap expiry, Boltz will automatically refund its own locked
    /// chain bitcoin.
    #[serde(rename = "transaction.refunded")]
    TransactionRefunded,
}

impl ToString for ChainSwapStates {
    fn to_string(&self) -> String {
        match self {
            ChainSwapStates::Created => "swap.created".to_string(),
            ChainSwapStates::TransactionZeroConfRejected => {
                "transaction.zeroconf.rejected".to_string()
            }
            ChainSwapStates::TransactionMempool => "transaction.mempool".to_string(),
            ChainSwapStates::TransactionConfirmed => "transaction.confirmed".to_string(),
            ChainSwapStates::TransactionServerMempool => "transaction.server.mempool".to_string(),
            ChainSwapStates::TransactionServerConfirmed => {
                "transaction.server.confirmed".to_string()
            }
            ChainSwapStates::TransactionClaimed => "transaction.claimed".to_string(),
            ChainSwapStates::TransactionLockupFailed => "transaction.lockupFailed".to_string(),
            ChainSwapStates::SwapExpired => "swap.expired".to_string(),
            ChainSwapStates::TransactionFailed => "transaction.failed".to_string(),
            ChainSwapStates::TransactionRefunded => "transaction.refunded".to_string(),
        }
    }
}

impl FromStr for ChainSwapStates {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "swap.created" => Ok(ChainSwapStates::Created),
            "transaction.zeroconf.rejected" => Ok(ChainSwapStates::TransactionZeroConfRejected),
            "transaction.mempool" => Ok(ChainSwapStates::TransactionMempool),
            "transaction.confirmed" => Ok(ChainSwapStates::TransactionConfirmed),
            "transaction.server.mempool" => Ok(ChainSwapStates::TransactionServerMempool),
            "transaction.server.confirmed" => Ok(ChainSwapStates::TransactionServerConfirmed),
            "transaction.claimed" => Ok(ChainSwapStates::TransactionClaimed),
            "transaction.lockupFailed" => Ok(ChainSwapStates::TransactionLockupFailed),
            "swap.expired" => Ok(ChainSwapStates::SwapExpired),
            "transaction.failed" => Ok(ChainSwapStates::TransactionFailed),
            "transaction.refunded" => Ok(ChainSwapStates::TransactionRefunded),
            _ => Err(()),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum SwapType {
    Submarine,
    ReverseSubmarine,
    Chain,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum OrderSide {
    Buy,
    Sell,
}

impl ToString for OrderSide {
    fn to_string(&self) -> String {
        match self {
            OrderSide::Buy => "buy".to_string(),
            OrderSide::Sell => "sell".to_string(),
        }
    }
}

impl FromStr for OrderSide {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "buy" => Ok(OrderSide::Buy),
            "sell" => Ok(OrderSide::Sell),
            _ => Err(()),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct GetFeeEstimationResponse {
    #[serde(rename = "BTC")]
    pub btc: f64,
    #[serde(rename = "L-BTC")]
    pub lbtc: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetQuoteResponse {
    /// Server lockup amount, in sat
    pub amount: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_fee_estimation() {
        let client = BoltzApiClientV2::new(BOLTZ_MAINNET_URL_V2);
        let result = client.get_fee_estimation();
        assert!(result.is_ok(), "Failed to get fee estimation");
    }

    #[test]
    fn test_get_height() {
        let client = BoltzApiClientV2::new(BOLTZ_MAINNET_URL_V2);
        let result = client.get_height();
        assert!(result.is_ok(), "Failed to get height");
    }

    #[test]
    fn test_get_submarine_pairs() {
        let client = BoltzApiClientV2::new(BOLTZ_MAINNET_URL_V2);
        let result = client.get_submarine_pairs();
        assert!(result.is_ok(), "Failed to get submarine pairs");
    }

    #[test]
    fn test_get_reverse_pairs() {
        let client = BoltzApiClientV2::new(BOLTZ_MAINNET_URL_V2);
        let result = client.get_reverse_pairs();
        assert!(result.is_ok(), "Failed to get reverse pairs");
    }

    #[test]
    fn test_get_chain_pairs() {
        let client = BoltzApiClientV2::new(BOLTZ_MAINNET_URL_V2);
        let result = client.get_chain_pairs();
        assert!(result.is_ok(), "Failed to get chain pairs");
    }

    #[test]
    #[ignore]
    fn test_get_submarine_claim_tx_details() {
        let client = BoltzApiClientV2::new(BOLTZ_MAINNET_URL_V2);
        let id = "G6c6GJJY8eXz".to_string();
        let result = client.get_submarine_claim_tx_details(&id);
        assert!(
            result.is_ok(),
            "Failed to get submarine claim transaction details"
        );
    }

    #[test]
    #[ignore]
    fn test_get_chain_claim_tx_details() {
        let client = BoltzApiClientV2::new(BOLTZ_MAINNET_URL_V2);
        let id = "3BIJf8UqGaSC".to_string();
        let result = client.get_chain_claim_tx_details(&id);
        assert!(
            result.is_ok(),
            "Failed to get chain claim transaction details"
        );
    }

    #[test]
    #[ignore]
    fn test_get_reverse_tx() {
        let client = BoltzApiClientV2::new(BOLTZ_MAINNET_URL_V2);
        let id = "G6c6GJJY8eXz";
        let result = client.get_reverse_tx(id);
        assert!(result.is_ok(), "Failed to get reverse transaction");
    }

    #[test]
    #[ignore]
    fn test_get_submarine_tx() {
        let client = BoltzApiClientV2::new(BOLTZ_MAINNET_URL_V2);
        let id = "G6c6GJJY8eXz";
        let result = client.get_submarine_tx(id);
        assert!(result.is_ok(), "Failed to get submarine transaction");
    }

    #[test]
    fn test_get_chain_txs() {
        let client = BoltzApiClientV2::new(BOLTZ_MAINNET_URL_V2);
        let id = "G6c6GJJY8eXz";
        let result = client.get_chain_txs(id);
        assert!(result.is_ok(), "Failed to get chain transactions");
    }
}
