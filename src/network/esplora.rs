use crate::error::Error;
use crate::network::{
    BitcoinClient, BitcoinNetworkConfig, Chain, LiquidClient, LiquidNetworkConfig,
};
use bitcoin::ScriptBuf;
use elements::hex::ToHex;
use elements::pset::serialize::Serialize;
use reqwest::Response;
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt::format;
use std::str::FromStr;
use std::time::Duration;

pub const DEFAULT_TESTNET_NODE: &str = "https://blockstream.info/testnet/api";
pub const DEFAULT_MAINNET_NODE: &str = "https://blockstream.info/api";
pub const DEFAULT_LIQUID_TESTNET_NODE: &str = "https://blockstream.info/liquidtestnet/api";
pub const DEFAULT_LIQUID_MAINNET_NODE: &str = "https://blockstream.info/liquid/api";

pub const DEFAULT_ELECTRUM_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Clone)]
pub struct EsploraConfig {
    network: Chain,
    url: String,
    timeout: u64,
}

impl EsploraConfig {
    pub fn new(network: Chain, url: &str, timeout: u64) -> Self {
        Self {
            network,
            url: url.to_string(),
            timeout,
        }
    }
    pub fn default(chain: Chain, regtest_url: Option<String>) -> Result<Self, Error> {
        if (chain == Chain::LiquidRegtest || chain == Chain::BitcoinRegtest)
            && regtest_url.is_none()
        {
            return Err(Error::Esplora(
                "Regtest requires using a custom url".to_string(),
            ));
        }
        match chain {
            Chain::Bitcoin => Ok(Self::new(
                Chain::Bitcoin,
                DEFAULT_MAINNET_NODE,
                DEFAULT_ELECTRUM_TIMEOUT_SECS,
            )),
            Chain::BitcoinTestnet => Ok(Self::new(
                Chain::BitcoinTestnet,
                DEFAULT_TESTNET_NODE,
                DEFAULT_ELECTRUM_TIMEOUT_SECS,
            )),
            Chain::BitcoinRegtest => Ok(Self::new(
                Chain::BitcoinTestnet,
                &regtest_url.unwrap(),
                DEFAULT_ELECTRUM_TIMEOUT_SECS,
            )),
            Chain::Liquid => Ok(Self::new(
                Chain::Liquid,
                DEFAULT_LIQUID_MAINNET_NODE,
                DEFAULT_ELECTRUM_TIMEOUT_SECS,
            )),
            Chain::LiquidTestnet => Ok(Self::new(
                Chain::LiquidTestnet,
                DEFAULT_LIQUID_TESTNET_NODE,
                DEFAULT_ELECTRUM_TIMEOUT_SECS,
            )),
            Chain::LiquidRegtest => Ok(Self::new(
                Chain::BitcoinTestnet,
                &regtest_url.unwrap(),
                DEFAULT_ELECTRUM_TIMEOUT_SECS,
            )),
        }
    }

    pub fn default_bitcoin() -> Self {
        Self::new(
            Chain::BitcoinTestnet,
            DEFAULT_TESTNET_NODE,
            DEFAULT_ELECTRUM_TIMEOUT_SECS,
        )
    }

    pub fn default_liquid() -> Self {
        Self::new(
            Chain::LiquidTestnet,
            DEFAULT_LIQUID_TESTNET_NODE,
            DEFAULT_ELECTRUM_TIMEOUT_SECS,
        )
    }
}

impl BitcoinNetworkConfig<EsploraBitcoinClient> for EsploraConfig {
    fn build_bitcoin_client(&self) -> Result<EsploraBitcoinClient, Error> {
        Ok(EsploraBitcoinClient::new(&self.url, self.timeout))
    }

    fn network(&self) -> Chain {
        self.network
    }
}

impl LiquidNetworkConfig<EsploraLiquidClient> for EsploraConfig {
    fn build_liquid_client(&self) -> Result<EsploraLiquidClient, Error> {
        Ok(EsploraLiquidClient::new(&self.url, self.timeout))
    }

    fn network(&self) -> Chain {
        self.network
    }
}

pub struct EsploraBitcoinClient {
    client: reqwest::Client,
    base_url: String,
    timeout: Duration,
}

impl EsploraBitcoinClient {
    pub fn new(url: &str, timeout: u64) -> Self {
        let client = reqwest::Client::new();

        Self {
            client,
            base_url: url.to_string(),
            timeout: Duration::from_secs(timeout),
        }
    }

    fn fetch_utxos_core(
        txs: &[Transaction],
        address: &str,
    ) -> Result<Vec<(bitcoin::OutPoint, bitcoin::TxOut)>, Error> {
        let mut result = Vec::new();

        for tx in txs {
            for (vout, output) in tx.vout.iter().enumerate() {
                // Check if this output belongs to our address
                if output.scriptpubkey_address != address {
                    continue;
                }

                // Check if this output is spent by any confirmed transaction
                let is_spent = txs.iter().any(|spending_tx| {
                    let spends_our_output = spending_tx
                        .vin
                        .iter()
                        .any(|input| input.txid == tx.txid && input.vout == vout as u32);

                    spends_our_output && spending_tx.status.confirmed
                });

                if is_spent {
                    continue;
                }

                let txid = match bitcoin::Txid::from_str(&tx.txid) {
                    Ok(txid) => txid,
                    Err(e) => {
                        return Err(Error::Esplora(format!(
                            "Failed to parse txid {}: {e}",
                            tx.txid
                        )))
                    }
                };
                let script_pubkey = match ScriptBuf::from_hex(&output.scriptpubkey) {
                    Ok(script) => script,
                    Err(e) => {
                        return Err(Error::Esplora(format!(
                            "Failed to parse script pubkey {}: {e}",
                            output.scriptpubkey
                        )))
                    }
                };
                let out_point = bitcoin::OutPoint::new(txid, vout as u32);
                let tx_out = bitcoin::TxOut {
                    value: bitcoin::Amount::from_sat(output.value),
                    script_pubkey,
                };

                result.push((out_point, tx_out));
            }
        }

        Ok(result)
    }
}

#[macros::async_trait]
impl BitcoinClient for EsploraBitcoinClient {
    async fn get_address_balance(&self, address: &bitcoin::Address) -> Result<(u64, i64), Error> {
        let url = format!("{}/address/{}", self.base_url, address);
        let response = get_with_retry(&self.client, &url, self.timeout).await?;
        let address_info: AddressInfo = serde_json::from_str(&response.text().await?)?;

        let confirmed_balance = address_info
            .chain_stats
            .funded_txo_sum
            .checked_sub(address_info.chain_stats.spent_txo_sum)
            .ok_or(Error::Generic(format!(
                "Confirmed spent {} > Confirmed funded {}",
                address_info.chain_stats.spent_txo_sum, address_info.chain_stats.funded_txo_sum
            )))?;
        let unconfirmed_balance = address_info.mempool_stats.funded_txo_sum as i64
            - address_info.mempool_stats.spent_txo_sum as i64;

        Ok((confirmed_balance, unconfirmed_balance))
    }

    async fn get_address_utxos(
        &self,
        address: &bitcoin::Address,
    ) -> Result<Vec<(bitcoin::OutPoint, bitcoin::TxOut)>, Error> {
        let url = format!("{}/address/{}/txs", self.base_url, address);
        let response = get_with_retry(&self.client, &url, self.timeout).await?;

        let txs: Vec<Transaction> = serde_json::from_str(&response.text().await?)?;

        Self::fetch_utxos_core(&txs, &address.to_string())
    }

    async fn broadcast_tx(&self, signed_tx: &bitcoin::Transaction) -> Result<bitcoin::Txid, Error> {
        let tx_hex = signed_tx.serialize().to_hex();
        let response = self
            .client
            .post(format!("{}/tx", self.base_url))
            .timeout(self.timeout)
            .body(tx_hex)
            .send()
            .await
            .map_err(|e| Error::Esplora(e.to_string()))?;
        let txid = bitcoin::Txid::from_str(&response.text().await?)?;
        Ok(txid)
    }
}

pub struct EsploraLiquidClient {
    client: reqwest::Client,
    base_url: String,
    timeout: Duration,
}

impl EsploraLiquidClient {
    pub fn new(url: &str, timeout: u64) -> Self {
        let client = reqwest::Client::new();

        Self {
            client,
            base_url: url.to_string(),
            timeout: Duration::from_secs(timeout),
        }
    }
}

#[macros::async_trait]
impl LiquidClient for EsploraLiquidClient {
    async fn get_address_utxo(
        &self,
        address: &elements::Address,
    ) -> Result<(elements::OutPoint, elements::TxOut), Error> {
        // List address txs (GET /address/:address/txs)
        let utxos_url = format!("{}/address/{}/utxo", self.base_url, address);
        let utxos_response = get_with_retry(&self.client, &utxos_url, self.timeout).await?;
        let utxos: Vec<Utxo> = serde_json::from_str(&utxos_response.text().await?)?;

        let txid = &utxos
            .last()
            .ok_or(Error::Protocol("No Transaction History".to_string()))?
            .txid;

        let raw_tx_url = format!("{}/tx/{}/raw", self.base_url, txid);
        let raw_tx_response = get_with_retry(&self.client, &raw_tx_url, self.timeout).await?;
        let raw_tx = raw_tx_response.bytes().await?;
        let tx: elements::Transaction = elements::encode::deserialize(&raw_tx)?;
        for (vout, output) in tx.clone().output.into_iter().enumerate() {
            if output.script_pubkey == address.script_pubkey() {
                let outpoint_0 = elements::OutPoint::new(tx.txid(), vout as u32);

                return Ok((outpoint_0, output));
            }
        }
        Err(Error::Protocol(
            "Electrum could not find a Liquid UTXO for script".to_string(),
        ))
    }

    async fn get_genesis_hash(&self) -> Result<elements::BlockHash, Error> {
        let url = format!("{}/block-height/0", self.base_url);
        let response = get_with_retry(&self.client, &url, self.timeout).await?;
        let text = response.text().await?;
        Ok(elements::BlockHash::from_str(&text)?)
    }

    async fn broadcast_tx(&self, signed_tx: &elements::Transaction) -> Result<String, Error> {
        let url = format!("{}/tx", self.base_url);
        let tx_hex = signed_tx.serialize().to_hex();
        let response = self
            .client
            .post(url)
            .timeout(self.timeout)
            .body(tx_hex)
            .send()
            .await
            .map_err(|e| Error::Esplora(e.to_string()))?;
        Ok(response.text().await?)
    }
}

async fn get_with_retry(
    client: &reqwest::Client,
    url: &str,
    timeout: Duration,
) -> Result<Response, Error> {
    let mut attempt = 0;
    loop {
        let response = client
            .get(url)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| Error::Esplora(e.to_string()))?;

        let level = if response.status() == 200 {
            log::Level::Trace
        } else {
            log::Level::Info
        };
        log::log!(
            level,
            "{} status_code:{} - body bytes:{:?}",
            &url,
            response.status(),
            response.content_length(),
        );

        // 429 Too many requests
        // 503 Service Temporarily Unavailable
        if response.status() == 429 || response.status() == 503 {
            if attempt > 6 {
                log::warn!("{url} tried 6 times, failing");
                return Err(Error::Esplora("Too many retries".to_string()));
            }
            let secs = 1 << attempt;

            log::debug!("{url} waiting {secs}");

            async_sleep(secs * 1000).await;
            attempt += 1;
        } else {
            return Ok(response);
        }
    }
}

// based on https://users.rust-lang.org/t/rust-wasm-async-sleeping-for-100-milli-seconds-goes-up-to-1-minute/81177
// TODO remove/handle/justify unwraps
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
pub async fn async_sleep(millis: i32) {
    let mut cb = |resolve: js_sys::Function, _reject: js_sys::Function| {
        web_sys::window()
            .unwrap()
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis)
            .unwrap();
    };
    let p = js_sys::Promise::new(&mut cb);
    wasm_bindgen_futures::JsFuture::from(p).await.unwrap();
}
#[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
pub async fn async_sleep(millis: i32) {
    tokio::time::sleep(tokio::time::Duration::from_millis(millis as u64)).await;
}

#[derive(Debug, Deserialize)]
struct AddressInfo {
    address: String,
    chain_stats: Stats,
    mempool_stats: Stats,
}

#[derive(Debug, Deserialize)]
struct Stats {
    funded_txo_count: u64,
    funded_txo_sum: u64,
    spent_txo_count: u64,
    spent_txo_sum: u64,
    tx_count: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Transaction {
    pub txid: String,
    pub vin: Vec<Input>,
    pub vout: Vec<Output>,
    pub fee: u64,
    pub status: Status,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Input {
    pub txid: String,
    pub vout: u32,
    pub prevout: Prevout,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Prevout {
    pub scriptpubkey: String,
    pub value: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Output {
    pub scriptpubkey: String,
    pub scriptpubkey_address: String,
    pub value: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Status {
    pub confirmed: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Utxo {
    pub txid: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::esplora::EsploraConfig;
    use crate::network::{
        BitcoinClient, BitcoinNetworkConfig, Chain, LiquidClient, LiquidNetworkConfig,
    };
    use elements::hex::ToHex;
    use std::str::FromStr;

    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    #[macros::async_test_all]
    async fn test_esplora_default_clients() {
        let network_config = EsploraConfig::default(Chain::Bitcoin, None).unwrap();
        let electrum_client = network_config.build_bitcoin_client().unwrap();
        assert!(electrum_client
            .get_address_balance(
                &bitcoin::Address::from_str("bc1qlaghkgntxw84d8jfv45deup7v32dfmncs7t3ct")
                    .unwrap()
                    .assume_checked()
            )
            .await
            .is_ok());

        let network_config = EsploraConfig::default(Chain::Liquid, None).unwrap();
        let electrum_client = network_config.build_liquid_client().unwrap();
        assert_eq!(
            electrum_client.get_genesis_hash().await.unwrap().to_hex(),
            "1466275836220db2944ca059a3a10ef6fd2ea684b0688d2c379296888a206003"
        );
    }

    #[macros::test_all]
    fn test_utxo_fetching() {
        let our_script_hex = "aaaa";
        let other_script_hex = "bbbb";
        let our_address = "test_address";
        let other_address = "other_address";

        let txid_1 = "1111111111111111111111111111111111111111111111111111111111111111";
        let txid_2 = "2222222222222222222222222222222222222222222222222222222222222222";
        let txid_3 = "3333333333333333333333333333333333333333333333333333333333333333";
        let txid_4 = "4444444444444444444444444444444444444444444444444444444444444444";
        let txid_confirmed_spend =
            "5555555555555555555555555555555555555555555555555555555555555555";
        let txid_unconfirmed_spend =
            "6666666666666666666666666666666666666666666666666666666666666666";

        // Pending tx with unspent output
        let tx1 = Transaction {
            txid: txid_1.to_string(),
            vin: vec![Input {
                txid: "1".to_string(),
                vout: 0,
                prevout: Prevout {
                    scriptpubkey: other_script_hex.to_string(),
                    value: 2000,
                },
            }],
            vout: vec![Output {
                scriptpubkey: our_script_hex.to_string(),
                scriptpubkey_address: our_address.to_string(),
                value: 1000,
            }],
            fee: 100,
            status: Status { confirmed: false },
        };

        // Confirmed tx with unspent output
        let tx2 = Transaction {
            txid: txid_2.to_string(),
            vin: vec![Input {
                txid: "2".to_string(),
                vout: 0,
                prevout: Prevout {
                    scriptpubkey: other_script_hex.to_string(),
                    value: 3000,
                },
            }],
            vout: vec![Output {
                scriptpubkey: our_script_hex.to_string(),
                scriptpubkey_address: our_address.to_string(),
                value: 2000,
            }],
            fee: 100,
            status: Status { confirmed: true },
        };

        // Confirmed tx with unconfirmed spend
        let tx3 = Transaction {
            txid: txid_3.to_string(),
            vin: vec![Input {
                txid: "3".to_string(),
                vout: 0,
                prevout: Prevout {
                    scriptpubkey: other_script_hex.to_string(),
                    value: 6000,
                },
            }],
            vout: vec![Output {
                scriptpubkey: our_script_hex.to_string(),
                scriptpubkey_address: our_address.to_string(),
                value: 5000,
            }],
            fee: 100,
            status: Status { confirmed: true },
        };

        // Confirmed tx with confirmed spend
        let tx4 = Transaction {
            txid: txid_4.to_string(),
            vin: vec![Input {
                txid: "4".to_string(),
                vout: 0,
                prevout: Prevout {
                    scriptpubkey: other_script_hex.to_string(),
                    value: 5000,
                },
            }],
            vout: vec![Output {
                scriptpubkey: our_script_hex.to_string(),
                scriptpubkey_address: our_address.to_string(),
                value: 4500,
            }],
            fee: 100,
            status: Status { confirmed: true },
        };

        // Confirmed spending tx for tx4's output
        let spending_tx = Transaction {
            txid: txid_confirmed_spend.to_string(),
            vin: vec![Input {
                txid: txid_4.to_string(),
                vout: 0,
                prevout: Prevout {
                    scriptpubkey: our_script_hex.to_string(),
                    value: 4500,
                },
            }],
            vout: vec![Output {
                scriptpubkey: other_script_hex.to_string(),
                scriptpubkey_address: other_address.to_string(),
                value: 4000,
            }],
            fee: 100,
            status: Status { confirmed: true },
        };

        // Pending spending tx for tx3's output
        let pending_spending_tx = Transaction {
            txid: txid_unconfirmed_spend.to_string(),
            vin: vec![Input {
                txid: txid_3.to_string(),
                vout: 0,
                prevout: Prevout {
                    scriptpubkey: our_script_hex.to_string(),
                    value: 5000,
                },
            }],
            vout: vec![Output {
                scriptpubkey: other_script_hex.to_string(),
                scriptpubkey_address: other_address.to_string(),
                value: 4950,
            }],
            fee: 50,
            status: Status { confirmed: false },
        };

        // Call the updated method
        let utxo_pairs = EsploraBitcoinClient::fetch_utxos_core(
            &[
                tx1.clone(),
                tx2.clone(),
                tx3.clone(),
                tx4.clone(),
                spending_tx.clone(),
                pending_spending_tx.clone(),
            ],
            our_address,
        )
        .unwrap();

        assert_eq!(utxo_pairs.len(), 3);

        // Pending tx with unspent output
        assert!(utxo_pairs
            .iter()
            .any(|(outpoint, _)| outpoint.txid.to_string() == tx1.txid));

        // Confirmed tx with unspent output
        assert!(utxo_pairs
            .iter()
            .any(|(outpoint, _)| outpoint.txid.to_string() == tx2.txid));

        // Confirmed tx with unconfirmed spend
        assert!(utxo_pairs
            .iter()
            .any(|(outpoint, _)| outpoint.txid.to_string() == tx3.txid));

        // Make sure tx4 is NOT in the result (because it's spent by a confirmed tx)
        assert!(!utxo_pairs
            .iter()
            .any(|(outpoint, _)| outpoint.txid.to_string() == tx4.txid));
    }
}
