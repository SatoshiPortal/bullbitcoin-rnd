use electrum_client::ElectrumApi;
use std::{hash, str::FromStr};

use bitcoin::{
    hashes::{hash160, Hash},
    hex::DisplayHex,
    key::rand::{rngs::OsRng, thread_rng, RngCore},
    script::Script as BitcoinScript,
    secp256k1::Keypair,
    Amount, Witness, XOnlyPublicKey,
};
use elements::{
    confidential::{self, Asset, AssetBlindingFactor, Value, ValueBlindingFactor},
    hex::{FromHex, ToHex},
    secp256k1_zkp::{
        self, MusigAggNonce, MusigKeyAggCache, MusigPartialSignature, MusigPubNonce, MusigSession,
        MusigSessionId, Secp256k1, SecretKey,
    },
    sighash::{Prevouts, SighashCache},
    taproot::{LeafVersion, TapLeafHash, TaprootBuilder, TaprootSpendInfo},
    Address, AssetIssuance, BlockHash, LockTime, OutPoint, SchnorrSig, SchnorrSighashType, Script,
    Sequence, Transaction, TxIn, TxInWitness, TxOut, TxOutSecrets, TxOutWitness,
};

use elements::encode::serialize;
use elements::secp256k1_zkp::Message;

use crate::{
    network::{electrum::ElectrumConfig, Chain},
    util::{liquid_genesis_hash, secrets::Preimage},
};

use crate::error::Error;

use super::boltz::{
    BoltzApiClientV2, ChainClaimTxResponse, ChainSwapDetails, Cooperative, CreateReverseResponse,
    CreateSubmarineResponse, Side, SubmarineClaimTxResponse, SwapTxKind, SwapType, ToSign,
};
use crate::fees::{create_tx_with_fee, Fee};
use elements::bitcoin::PublicKey;
use elements::secp256k1_zkp::Keypair as ZKKeyPair;
use elements::{
    address::Address as EAddress,
    opcodes::all::*,
    script::{Builder as EBuilder, Instruction, Script as EScript},
    AddressParams,
};

/// Liquid v2 swap script helper.
#[derive(Debug, Clone, PartialEq)]
pub struct LBtcSwapScript {
    pub swap_type: SwapType,
    pub side: Option<Side>,
    pub funding_addrs: Option<Address>,
    pub hashlock: hash160::Hash,
    pub receiver_pubkey: PublicKey,
    pub locktime: LockTime,
    pub sender_pubkey: PublicKey,
    pub blinding_key: ZKKeyPair,
}

impl LBtcSwapScript {
    /// Create the struct for a submarine swap from boltz create response.
    pub fn submarine_from_swap_resp(
        create_swap_response: &CreateSubmarineResponse,
        our_pubkey: PublicKey,
    ) -> Result<Self, Error> {
        let claim_script = Script::from_hex(&create_swap_response.swap_tree.claim_leaf.output)?;
        let refund_script = Script::from_hex(&create_swap_response.swap_tree.refund_leaf.output)?;

        let claim_instructions = claim_script.instructions();
        let refund_instructions = refund_script.instructions();

        let mut last_op = OP_0NOTEQUAL;
        let mut hashlock = None;
        let mut locktime = None;

        for instruction in claim_instructions {
            match instruction {
                Ok(Instruction::PushBytes(bytes)) => {
                    if bytes.len() == 20 {
                        hashlock = Some(hash160::Hash::from_slice(bytes)?);
                    } else {
                        continue;
                    }
                }
                _ => continue,
            }
        }

        for instruction in refund_instructions {
            match instruction {
                Ok(Instruction::Op(opcode)) => last_op = opcode,
                Ok(Instruction::PushBytes(bytes)) => {
                    if last_op == OP_CHECKSIGVERIFY {
                        locktime =
                            Some(LockTime::from_consensus(bytes_to_u32_little_endian(bytes)));
                    } else {
                        continue;
                    }
                }
                _ => continue,
            }
        }

        let hashlock =
            hashlock.ok_or_else(|| Error::Protocol("No hashlock provided".to_string()))?;

        let locktime =
            locktime.ok_or_else(|| Error::Protocol("No timelock provided".to_string()))?;

        let funding_addrs = Address::from_str(&create_swap_response.address)?;

        let blinding_str = create_swap_response
            .blinding_key
            .as_ref()
            .expect("No blinding key provided in CreateSwapResp");
        let blinding_key = ZKKeyPair::from_seckey_str(&Secp256k1::new(), blinding_str)?;

        Ok(Self {
            swap_type: SwapType::Submarine,
            side: None,
            funding_addrs: Some(funding_addrs),
            hashlock,
            receiver_pubkey: create_swap_response.claim_public_key,
            locktime,
            sender_pubkey: our_pubkey,
            blinding_key,
        })
    }

    /// Create the struct for a reverse swap from boltz create response.
    pub fn reverse_from_swap_resp(
        reverse_response: &CreateReverseResponse,
        our_pubkey: PublicKey,
    ) -> Result<Self, Error> {
        let claim_script = Script::from_hex(&reverse_response.swap_tree.claim_leaf.output)?;
        let refund_script = Script::from_hex(&reverse_response.swap_tree.refund_leaf.output)?;

        let claim_instructions = claim_script.instructions();
        let refund_instructions = refund_script.instructions();

        let mut last_op = OP_0NOTEQUAL;
        let mut hashlock = None;
        let mut locktime = None;

        for instruction in claim_instructions {
            match instruction {
                Ok(Instruction::PushBytes(bytes)) => {
                    if bytes.len() == 20 {
                        hashlock = Some(hash160::Hash::from_slice(bytes)?);
                    } else {
                        continue;
                    }
                }
                _ => continue,
            }
        }

        for instruction in refund_instructions {
            match instruction {
                Ok(Instruction::Op(opcode)) => last_op = opcode,
                Ok(Instruction::PushBytes(bytes)) => {
                    if last_op == OP_CHECKSIGVERIFY {
                        locktime =
                            Some(LockTime::from_consensus(bytes_to_u32_little_endian(bytes)));
                    } else {
                        continue;
                    }
                }
                _ => continue,
            }
        }

        let hashlock =
            hashlock.ok_or_else(|| Error::Protocol("No hashlock provided".to_string()))?;

        let locktime =
            locktime.ok_or_else(|| Error::Protocol("No timelock provided".to_string()))?;

        let funding_addrs = Address::from_str(&reverse_response.lockup_address)?;

        let blinding_str = reverse_response
            .blinding_key
            .as_ref()
            .expect("No blinding key provided in CreateSwapResp");
        let blinding_key = ZKKeyPair::from_seckey_str(&Secp256k1::new(), blinding_str)?;

        Ok(Self {
            swap_type: SwapType::ReverseSubmarine,
            side: None,
            funding_addrs: Some(funding_addrs),
            hashlock,
            receiver_pubkey: our_pubkey,
            locktime,
            sender_pubkey: reverse_response.refund_public_key,
            blinding_key,
        })
    }

    /// Create the struct for a chain swap from boltz create response.
    pub fn chain_from_swap_resp(
        side: Side,
        chain_swap_details: ChainSwapDetails,
        our_pubkey: PublicKey,
    ) -> Result<Self, Error> {
        let claim_script = Script::from_hex(&chain_swap_details.swap_tree.claim_leaf.output)?;
        let refund_script = Script::from_hex(&chain_swap_details.swap_tree.refund_leaf.output)?;

        let claim_instructions = claim_script.instructions();
        let refund_instructions = refund_script.instructions();

        let mut last_op = OP_0NOTEQUAL;
        let mut hashlock = None;
        let mut locktime = None;

        for instruction in claim_instructions {
            match instruction {
                Ok(Instruction::PushBytes(bytes)) => {
                    if bytes.len() == 20 {
                        hashlock = Some(hash160::Hash::from_slice(bytes)?);
                    } else {
                        continue;
                    }
                }
                _ => continue,
            }
        }

        for instruction in refund_instructions {
            match instruction {
                Ok(Instruction::Op(opcode)) => last_op = opcode,
                Ok(Instruction::PushBytes(bytes)) => {
                    if last_op == OP_CHECKSIGVERIFY {
                        locktime =
                            Some(LockTime::from_consensus(bytes_to_u32_little_endian(bytes)));
                    } else {
                        continue;
                    }
                }
                _ => continue,
            }
        }

        let hashlock =
            hashlock.ok_or_else(|| Error::Protocol("No hashlock provided".to_string()))?;

        let locktime =
            locktime.ok_or_else(|| Error::Protocol("No timelock provided".to_string()))?;

        let funding_addrs = Address::from_str(&chain_swap_details.lockup_address)?;

        let (sender_pubkey, receiver_pubkey) = match side {
            Side::Lockup => (our_pubkey, chain_swap_details.server_public_key),
            Side::Claim => (chain_swap_details.server_public_key, our_pubkey),
        };

        let blinding_str = chain_swap_details
            .blinding_key
            .as_ref()
            .expect("No blinding key provided in ChainSwapDetails");
        let blinding_key = ZKKeyPair::from_seckey_str(&Secp256k1::new(), blinding_str)?;

        Ok(Self {
            swap_type: SwapType::Chain,
            side: Some(side),
            funding_addrs: Some(funding_addrs),
            hashlock,
            receiver_pubkey,
            locktime,
            sender_pubkey,
            blinding_key,
        })
    }

    fn claim_script(&self) -> Script {
        match self.swap_type {
            SwapType::Submarine => EBuilder::new()
                .push_opcode(OP_HASH160)
                .push_slice(self.hashlock.as_byte_array())
                .push_opcode(OP_EQUALVERIFY)
                .push_slice(&self.receiver_pubkey.inner.x_only_public_key().0.serialize())
                .push_opcode(OP_CHECKSIG)
                .into_script(),

            SwapType::ReverseSubmarine | SwapType::Chain => EBuilder::new()
                .push_opcode(OP_SIZE)
                .push_int(32)
                .push_opcode(OP_EQUALVERIFY)
                .push_opcode(OP_HASH160)
                .push_slice(self.hashlock.as_byte_array())
                .push_opcode(OP_EQUALVERIFY)
                .push_slice(&self.receiver_pubkey.inner.x_only_public_key().0.serialize())
                .push_opcode(OP_CHECKSIG)
                .into_script(),
        }
    }

    fn refund_script(&self) -> Script {
        // Refund scripts are same for all swap types
        EBuilder::new()
            .push_slice(&self.sender_pubkey.inner.x_only_public_key().0.serialize())
            .push_opcode(OP_CHECKSIGVERIFY)
            .push_int(self.locktime.to_consensus_u32().into())
            .push_opcode(OP_CLTV)
            .into_script()
    }

    pub fn musig_keyagg_cache(&self) -> MusigKeyAggCache {
        match (self.swap_type, self.side.clone()) {
            (SwapType::ReverseSubmarine, _) | (SwapType::Chain, Some(Side::Claim)) => {
                let pubkeys = [self.sender_pubkey.inner, self.receiver_pubkey.inner];
                MusigKeyAggCache::new(&Secp256k1::new(), &pubkeys)
            }

            (SwapType::Submarine, _) | (SwapType::Chain, _) => {
                let pubkeys = [self.receiver_pubkey.inner, self.sender_pubkey.inner];
                MusigKeyAggCache::new(&Secp256k1::new(), &pubkeys)
            }
        }
    }

    /// Internally used to convert struct into a bitcoin::Script type
    fn taproot_spendinfo(&self) -> Result<TaprootSpendInfo, Error> {
        let secp = Secp256k1::new();

        // Setup Key Aggregation cache
        let mut key_agg_cache = self.musig_keyagg_cache();

        // Construct the Taproot
        let internal_key = key_agg_cache.agg_pk();

        let taproot_builder = TaprootBuilder::new();

        let taproot_builder =
            taproot_builder.add_leaf_with_ver(1, self.claim_script(), LeafVersion::default())?;
        let taproot_builder =
            taproot_builder.add_leaf_with_ver(1, self.refund_script(), LeafVersion::default())?;

        let taproot_spend_info = taproot_builder.finalize(&secp, internal_key)?;

        // Verify taproot construction
        if let Some(funding_addrs) = &self.funding_addrs {
            let claim_key = taproot_spend_info.output_key();

            let lockup_spk = funding_addrs.script_pubkey();

            let pubkey_instruction = lockup_spk
                .instructions()
                .last()
                .expect("should contain value")
                .expect("should not fail");

            let lockup_xonly_pubkey_bytes = pubkey_instruction
                .push_bytes()
                .expect("pubkey bytes expected");

            let lockup_xonly_pubkey = XOnlyPublicKey::from_slice(lockup_xonly_pubkey_bytes)?;

            if lockup_xonly_pubkey != claim_key.into_inner() {
                return Err(Error::Protocol(format!(
                    "Taproot construction Failed. Lockup Pubkey: {}, Claim Pubkey {:?}",
                    lockup_xonly_pubkey, claim_key
                )));
            }

            log::info!("Taproot creation and verification success!");
        }

        Ok(taproot_spend_info)
    }

    /// Get taproot address for the swap script.
    /// Always returns a confidential address
    pub fn to_address(&self, network: Chain) -> Result<EAddress, Error> {
        let taproot_spend_info = self.taproot_spendinfo()?;
        let address_params = match network {
            Chain::Liquid => &AddressParams::LIQUID,
            Chain::LiquidTestnet => &AddressParams::LIQUID_TESTNET,
            Chain::LiquidRegtest => &AddressParams::ELEMENTS,
            _ => {
                return Err(Error::Address(
                    "Cannot derive Liquid address for Bitcoin network".to_string(),
                ))
            }
        };

        Ok(EAddress::p2tr(
            &Secp256k1::new(),
            taproot_spend_info.internal_key(),
            taproot_spend_info.merkle_root(),
            Some(self.blinding_key.public_key()),
            address_params,
        ))
    }

    pub fn validate_address(&self, chain: Chain, address: String) -> Result<(), Error> {
        let to_address = self.to_address(chain)?;
        if to_address.to_string() == address {
            Ok(())
        } else {
            Err(Error::Protocol("Script/LockupAddress Mismatch".to_string()))
        }
    }

    /// Fetch utxo for script from Electrum
    pub fn fetch_utxo(&self, network_config: &ElectrumConfig) -> Result<(OutPoint, TxOut), Error> {
        let electrum_client = network_config.clone().build_client()?;
        let address = self.to_address(network_config.network())?;
        let history = electrum_client.script_get_history(BitcoinScript::from_bytes(
            self.to_address(network_config.network())?
                .to_unconfidential()
                .script_pubkey()
                .as_bytes(),
        ))?;
        if history.is_empty() {
            return Err(Error::Protocol("No Transaction History".to_string()));
        }
        let bitcoin_txid = history.last().expect("txid expected").tx_hash;
        let raw_tx = electrum_client.transaction_get_raw(&bitcoin_txid)?;
        let tx: Transaction = elements::encode::deserialize(&raw_tx)?;
        for (vout, output) in tx.clone().output.into_iter().enumerate() {
            if output.script_pubkey == address.script_pubkey() {
                let outpoint_0 = OutPoint::new(tx.txid(), vout as u32);

                return Ok((outpoint_0, output));
            }
        }
        Err(Error::Protocol(
            "Electrum could not find a Liquid UTXO for script".to_string(),
        ))
    }

    /// Fetch utxo for script from BoltzApi
    pub fn fetch_lockup_utxo_boltz(
        &self,
        network_config: &ElectrumConfig,
        boltz_url: &str,
        swap_id: &str,
        tx_kind: SwapTxKind,
    ) -> Result<(OutPoint, TxOut), Error> {
        let boltz_client = BoltzApiClientV2::new(boltz_url);
        let hex = match self.swap_type {
            SwapType::Chain => match tx_kind {
                SwapTxKind::Claim => {
                    boltz_client
                        .get_chain_txs(swap_id)?
                        .server_lock
                        .ok_or(Error::Protocol(
                            "No server_lock transaction for Chain Swap available".to_string(),
                        ))?
                        .transaction
                        .hex
                }
                SwapTxKind::Refund => {
                    boltz_client
                        .get_chain_txs(swap_id)?
                        .user_lock
                        .ok_or(Error::Protocol(
                            "No user_lock transaction for Chain Swap available".to_string(),
                        ))?
                        .transaction
                        .hex
                }
            },
            SwapType::ReverseSubmarine => boltz_client.get_reverse_tx(swap_id)?.hex,
            SwapType::Submarine => boltz_client.get_submarine_tx(swap_id)?.hex,
        };
        if (hex.is_none()) {
            return Err(Error::Hex(
                "No transaction hex found in boltz response".to_string(),
            ));
        }
        let address = self.to_address(network_config.network())?;
        let tx: Transaction = elements::encode::deserialize(&hex::decode(hex.unwrap())?)?;
        for (vout, output) in tx.clone().output.into_iter().enumerate() {
            if output.script_pubkey == address.script_pubkey() {
                let outpoint_0 = OutPoint::new(tx.txid(), vout as u32);

                return Ok((outpoint_0, output));
            }
        }
        Err(Error::Protocol(
            "Boltz could not find a Liquid UTXO for script".to_string(),
        ))
    }

    // Get the chain genesis hash. Requires for sighash calculation
    pub fn genesis_hash(
        &self,
        electrum_config: &ElectrumConfig,
    ) -> Result<elements::BlockHash, Error> {
        let electrum = electrum_config.build_client()?;
        Ok(elements::BlockHash::from_raw_hash(
            electrum.block_header(0)?.block_hash().into(),
        ))
    }
}

fn bytes_to_u32_little_endian(bytes: &[u8]) -> u32 {
    let mut result = 0u32;
    for (i, &byte) in bytes.iter().enumerate() {
        result |= (byte as u32) << (8 * i);
    }
    result
}

/// Liquid swap transaction helper.
#[derive(Debug, Clone)]
pub struct LBtcSwapTx {
    pub kind: SwapTxKind,
    pub swap_script: LBtcSwapScript,
    pub output_address: Address,
    pub funding_outpoint: OutPoint,
    pub funding_utxo: TxOut, // there should only ever be one outpoint in a swap
    pub genesis_hash: BlockHash, // Required to calculate sighash
}

impl LBtcSwapTx {
    /// Craft a new ClaimTx. Only works for Reverse and Chain Swaps.
    pub fn new_claim(
        swap_script: LBtcSwapScript,
        output_address: String,
        network_config: &ElectrumConfig,
        boltz_url: String,
        swap_id: String,
    ) -> Result<LBtcSwapTx, Error> {
        if swap_script.swap_type == SwapType::Submarine {
            return Err(Error::Protocol(
                "Claim transactions cannot be constructed for Submarine swaps.".to_string(),
            ));
        }

        let (funding_outpoint, funding_utxo) = match swap_script.fetch_utxo(network_config) {
            Ok(r) => r,
            Err(_) => swap_script.fetch_lockup_utxo_boltz(
                network_config,
                &boltz_url,
                &swap_id,
                SwapTxKind::Claim,
            )?,
        };

        let electrum = network_config.build_client()?;
        let genesis_hash = liquid_genesis_hash(network_config)?;

        Ok(LBtcSwapTx {
            kind: SwapTxKind::Claim,
            swap_script,
            output_address: Address::from_str(&output_address)?,
            funding_outpoint,
            funding_utxo,
            genesis_hash,
        })
    }

    /// Construct a RefundTX corresponding to the swap_script. Only works for Submarine and Chain Swaps.
    pub fn new_refund(
        swap_script: LBtcSwapScript,
        output_address: &str,
        network_config: &ElectrumConfig,
        boltz_url: String,
        swap_id: String,
    ) -> Result<LBtcSwapTx, Error> {
        if swap_script.swap_type == SwapType::ReverseSubmarine {
            return Err(Error::Protocol(
                "Refund Txs cannot be constructed for Reverse Submarine Swaps.".to_string(),
            ));
        }

        let address = Address::from_str(output_address)?;
        let (funding_outpoint, funding_utxo) = match swap_script.fetch_utxo(network_config) {
            Ok(r) => r,
            Err(_) => swap_script.fetch_lockup_utxo_boltz(
                network_config,
                &boltz_url,
                &swap_id,
                SwapTxKind::Refund,
            )?,
        };

        let electrum = network_config.build_client()?;
        let genesis_hash = liquid_genesis_hash(network_config)?;

        Ok(LBtcSwapTx {
            kind: SwapTxKind::Refund,
            swap_script,
            output_address: address,
            funding_outpoint,
            funding_utxo,
            genesis_hash,
        })
    }

    /// Compute the Musig partial signature.
    /// This is used to cooperatively close a Submarine or Chain Swap.
    pub fn partial_sign(
        &self,
        keys: &Keypair,
        pub_nonce: &str,
        transaction_hash: &str,
    ) -> Result<(MusigPartialSignature, MusigPubNonce), Error> {
        // Step 1: Start with a Musig KeyAgg Cache
        let secp = Secp256k1::new();

        let pubkeys = [
            self.swap_script.receiver_pubkey.inner,
            self.swap_script.sender_pubkey.inner,
        ];

        let mut key_agg_cache = MusigKeyAggCache::new(&secp, &pubkeys);

        let tweak = SecretKey::from_slice(
            self.swap_script
                .taproot_spendinfo()?
                .tap_tweak()
                .as_byte_array(),
        )?;

        let _ = key_agg_cache.pubkey_xonly_tweak_add(&secp, tweak)?;

        let session_id = MusigSessionId::new(&mut thread_rng());

        let msg = Message::from_digest_slice(&Vec::from_hex(transaction_hash)?)?;

        // Step 4: Start the Musig2 Signing session
        let mut extra_rand = [0u8; 32];
        OsRng.fill_bytes(&mut extra_rand);

        let (gen_sec_nonce, gen_pub_nonce) =
            key_agg_cache.nonce_gen(&secp, session_id, keys.public_key(), msg, Some(extra_rand))?;

        let boltz_nonce = MusigPubNonce::from_slice(&Vec::from_hex(pub_nonce)?)?;

        let agg_nonce = MusigAggNonce::new(&secp, &[boltz_nonce, gen_pub_nonce]);

        let musig_session = MusigSession::new(&secp, &key_agg_cache, agg_nonce, msg);

        let partial_sig = musig_session.partial_sign(&secp, gen_sec_nonce, keys, &key_agg_cache)?;

        Ok((partial_sig, gen_pub_nonce))
    }

    /// Sign a claim transaction.
    /// Panics if called on a Submarine Swap or Refund Tx.
    /// If the claim is cooperative, provide the other party's partial sigs.
    /// If this is None, transaction will be claimed via taproot script path.
    pub fn sign_claim(
        &self,
        keys: &Keypair,
        preimage: &Preimage,
        fee: Fee,
        is_cooperative: Option<Cooperative>,
        is_discount_ct: bool,
    ) -> Result<Transaction, Error> {
        if self.swap_script.swap_type == SwapType::Submarine {
            return Err(Error::Protocol(
                "Claim Tx signing is not applicable for Submarine Swaps".to_string(),
            ));
        }

        if self.kind == SwapTxKind::Refund {
            return Err(Error::Protocol(
                "Cannot sign claim with refund-type LBtcSwapTx".to_string(),
            ));
        }

        let mut claim_tx = create_tx_with_fee(
            fee,
            |fee| self.create_claim(keys, preimage, fee, is_cooperative.is_some()),
            |tx| tx_size(&tx, is_discount_ct),
        )?;

        // If its a cooperative claim, compute the Musig2 Aggregate Signature and use Keypath spending
        if let Some(Cooperative {
            boltz_api,
            swap_id,
            pub_nonce,
            partial_sig,
        }) = is_cooperative
        {
            let claim_tx_taproot_hash = SighashCache::new(&claim_tx)
                .taproot_key_spend_signature_hash(
                    0,
                    &Prevouts::All(&[&self.funding_utxo]),
                    SchnorrSighashType::Default,
                    self.genesis_hash,
                )?;

            let msg = Message::from_digest_slice(claim_tx_taproot_hash.as_byte_array())?;

            let mut key_agg_cache = self.swap_script.musig_keyagg_cache();

            let tweak = SecretKey::from_slice(
                self.swap_script
                    .taproot_spendinfo()?
                    .tap_tweak()
                    .as_byte_array(),
            )?;

            let secp = Secp256k1::new();
            let _ = key_agg_cache.pubkey_xonly_tweak_add(&secp, tweak)?;

            let session_id = MusigSessionId::new(&mut thread_rng());

            let mut extra_rand = [0u8; 32];
            OsRng.fill_bytes(&mut extra_rand);

            let (claim_sec_nonce, claim_pub_nonce) = key_agg_cache.nonce_gen(
                &secp,
                session_id,
                keys.public_key(),
                msg,
                Some(extra_rand),
            )?;

            // Step 7: Get boltz's partial sig
            let claim_tx_hex = serialize(&claim_tx).to_lower_hex_string();
            let partial_sig_resp = match self.swap_script.swap_type {
                SwapType::Chain => match (pub_nonce, partial_sig) {
                    (Some(pub_nonce), Some(partial_sig)) => boltz_api.post_chain_claim_tx_details(
                        &swap_id,
                        preimage,
                        pub_nonce,
                        partial_sig,
                        ToSign {
                            pub_nonce: claim_pub_nonce.serialize().to_lower_hex_string(),
                            transaction: claim_tx_hex,
                            index: 0,
                        },
                    ),
                    _ => Err(Error::Protocol(
                        "Chain swap claim needs a partial_sig".to_string(),
                    )),
                },
                SwapType::ReverseSubmarine => boltz_api.get_reverse_partial_sig(
                    &swap_id,
                    preimage,
                    &claim_pub_nonce,
                    &claim_tx_hex,
                ),
                _ => Err(Error::Protocol(format!(
                    "Cannot get partial sig for {:?} Swap",
                    self.swap_script.swap_type
                ))),
            }?;

            let boltz_public_nonce =
                MusigPubNonce::from_slice(&Vec::from_hex(&partial_sig_resp.pub_nonce)?)?;

            let boltz_partial_sig = MusigPartialSignature::from_slice(&Vec::from_hex(
                &partial_sig_resp.partial_signature,
            )?)?;

            let agg_nonce = MusigAggNonce::new(&secp, &[boltz_public_nonce, claim_pub_nonce]);

            let musig_session = MusigSession::new(&secp, &key_agg_cache, agg_nonce, msg);

            // Verify the sigs.
            let boltz_partial_sig_verify = musig_session.partial_verify(
                &secp,
                &key_agg_cache,
                boltz_partial_sig,
                boltz_public_nonce,
                self.swap_script.sender_pubkey.inner, //boltz key
            );

            if (!boltz_partial_sig_verify) {
                return Err(Error::Taproot(
                    "Unable to verify Partial Signature".to_string(),
                ));
            }

            let our_partial_sig =
                musig_session.partial_sign(&secp, claim_sec_nonce, keys, &key_agg_cache)?;

            let schnorr_sig = musig_session.partial_sig_agg(&[boltz_partial_sig, our_partial_sig]);

            let final_schnorr_sig = SchnorrSig {
                sig: schnorr_sig,
                hash_ty: SchnorrSighashType::Default,
            };

            let output_key = self.swap_script.taproot_spendinfo()?.output_key();

            secp.verify_schnorr(&final_schnorr_sig.sig, &msg, &output_key.into_inner())?;

            let mut script_witness = Witness::new();
            script_witness.push(final_schnorr_sig.to_vec());

            let witness = TxInWitness {
                amount_rangeproof: None,
                inflation_keys_rangeproof: None,
                script_witness: script_witness.to_vec(),
                pegin_witness: vec![],
            };

            claim_tx.input[0].witness = witness;
        }

        Ok(claim_tx)
    }

    fn create_claim(
        &self,
        keys: &Keypair,
        preimage: &Preimage,
        absolute_fees: u64,
        is_cooperative: bool,
    ) -> Result<Transaction, Error> {
        let preimage_bytes = preimage
            .bytes
            .ok_or(Error::Protocol("No preimage provided".to_string()))?;

        let claim_txin = TxIn {
            sequence: Sequence::MAX,
            previous_output: self.funding_outpoint,
            script_sig: Script::new(),
            witness: TxInWitness::default(),
            is_pegin: false,
            asset_issuance: AssetIssuance::default(),
        };

        let secp = Secp256k1::new();

        let unblined_utxo = self
            .funding_utxo
            .unblind(&secp, self.swap_script.blinding_key.secret_key())?;
        let asset_id = unblined_utxo.asset;
        let out_abf = AssetBlindingFactor::new(&mut thread_rng());
        let exp_asset = Asset::Explicit(asset_id);

        let (blinded_asset, asset_surjection_proof) =
            exp_asset.blind(&mut thread_rng(), &secp, out_abf, &[unblined_utxo])?;

        let output_value = Amount::from_sat(unblined_utxo.value) - Amount::from_sat(absolute_fees);

        let final_vbf = ValueBlindingFactor::last(
            &secp,
            output_value.to_sat(),
            out_abf,
            &[(
                unblined_utxo.value,
                unblined_utxo.asset_bf,
                unblined_utxo.value_bf,
            )],
            &[(
                absolute_fees,
                AssetBlindingFactor::zero(),
                ValueBlindingFactor::zero(),
            )],
        );
        let explicit_value = elements::confidential::Value::Explicit(output_value.to_sat());
        let msg = elements::RangeProofMessage {
            asset: asset_id,
            bf: out_abf,
        };
        let ephemeral_sk = SecretKey::new(&mut thread_rng());

        // assuming we always use a blinded address that has an extractable blinding pub
        let blinding_key = self
            .output_address
            .blinding_pubkey
            .ok_or(Error::Protocol("No blinding key in tx.".to_string()))?;
        let (blinded_value, nonce, rangeproof) = explicit_value.blind(
            &secp,
            final_vbf,
            blinding_key,
            ephemeral_sk,
            &self.output_address.script_pubkey(),
            &msg,
        )?;

        let tx_out_witness = TxOutWitness {
            surjection_proof: Some(Box::new(asset_surjection_proof)), // from asset blinding
            rangeproof: Some(Box::new(rangeproof)),                   // from value blinding
        };
        let payment_output: TxOut = TxOut {
            script_pubkey: self.output_address.script_pubkey(),
            value: blinded_value,
            asset: blinded_asset,
            nonce,
            witness: tx_out_witness,
        };
        let fee_output: TxOut = TxOut::new_fee(absolute_fees, asset_id);

        let mut claim_tx = Transaction {
            version: 2,
            lock_time: LockTime::ZERO,
            input: vec![claim_txin],
            output: vec![payment_output, fee_output],
        };

        if is_cooperative {
            claim_tx.input[0].witness = Self::stubbed_cooperative_witness();
        } else {
            // If Non-Cooperative claim use the Script Path spending
            claim_tx.input[0].sequence = Sequence::ZERO;
            let claim_script = self.swap_script.claim_script();
            let leaf_hash = TapLeafHash::from_script(&claim_script, LeafVersion::default());

            let sighash = SighashCache::new(&claim_tx).taproot_script_spend_signature_hash(
                0,
                &Prevouts::All(&[&self.funding_utxo]),
                leaf_hash,
                SchnorrSighashType::Default,
                self.genesis_hash,
            )?;

            let msg = Message::from_digest_slice(sighash.as_byte_array())?;

            let sig = secp.sign_schnorr(&msg, keys);

            let final_sig = SchnorrSig {
                sig,
                hash_ty: SchnorrSighashType::Default,
            };

            let control_block = match self
                .swap_script
                .taproot_spendinfo()?
                .control_block(&(claim_script.clone(), LeafVersion::default()))
            {
                Some(r) => r,
                None => return Err(Error::Taproot("Could not create control block".to_string())),
            };

            let mut script_witness = Witness::new();
            script_witness.push(final_sig.to_vec());
            script_witness.push(preimage.bytes.unwrap()); // checked for none
            script_witness.push(claim_script.as_bytes());
            script_witness.push(control_block.serialize());

            let witness = TxInWitness {
                amount_rangeproof: None,
                inflation_keys_rangeproof: None,
                script_witness: script_witness.to_vec(),
                pegin_witness: vec![],
            };

            claim_tx.input[0].witness = witness;
        }

        Ok(claim_tx)
    }

    /// Sign a refund transaction.
    /// Panics if called on a Reverse Swap or Claim Tx.
    pub fn sign_refund(
        &self,
        keys: &Keypair,
        fee: Fee,
        is_cooperative: Option<Cooperative>,
        is_discount_ct: bool,
    ) -> Result<Transaction, Error> {
        if self.swap_script.swap_type == SwapType::ReverseSubmarine {
            return Err(Error::Protocol(
                "Refund Tx signing is not applicable for Reverse Submarine Swaps".to_string(),
            ));
        }

        if self.kind == SwapTxKind::Claim {
            return Err(Error::Protocol(
                "Cannot sign refund with a claim-type LBtcSwapTx".to_string(),
            ));
        }

        let mut refund_tx = create_tx_with_fee(
            fee,
            |fee| self.create_refund(keys, fee, is_cooperative.is_some()),
            |tx| tx_size(&tx, is_discount_ct),
        )?;

        if let Some(Cooperative {
            boltz_api, swap_id, ..
        }) = is_cooperative
        {
            let secp = Secp256k1::new();

            refund_tx.lock_time = LockTime::ZERO;

            let claim_tx_taproot_hash = SighashCache::new(&refund_tx)
                .taproot_key_spend_signature_hash(
                    0,
                    &Prevouts::All(&[&self.funding_utxo]),
                    SchnorrSighashType::Default,
                    self.genesis_hash,
                )?;

            let msg = Message::from_digest_slice(claim_tx_taproot_hash.as_byte_array())?;

            let mut key_agg_cache = self.swap_script.musig_keyagg_cache();

            let tweak = SecretKey::from_slice(
                self.swap_script
                    .taproot_spendinfo()?
                    .tap_tweak()
                    .as_byte_array(),
            )?;

            let _ = key_agg_cache.pubkey_xonly_tweak_add(&secp, tweak)?;

            let session_id = MusigSessionId::new(&mut thread_rng());

            let mut extra_rand = [0u8; 32];
            OsRng.fill_bytes(&mut extra_rand);

            let (sec_nonce, pub_nonce) = key_agg_cache.nonce_gen(
                &secp,
                session_id,
                keys.public_key(),
                msg,
                Some(extra_rand),
            )?;

            // Step 7: Get boltz's partial sig
            let refund_tx_hex = serialize(&refund_tx).to_lower_hex_string();
            let partial_sig_resp = match self.swap_script.swap_type {
                SwapType::Chain => {
                    boltz_api.get_chain_partial_sig(&swap_id, 0, &pub_nonce, &refund_tx_hex)
                }
                SwapType::Submarine => {
                    boltz_api.get_submarine_partial_sig(&swap_id, 0, &pub_nonce, &refund_tx_hex)
                }
                _ => Err(Error::Protocol(format!(
                    "Cannot get partial sig for {:?} Swap",
                    self.swap_script.swap_type
                ))),
            }?;

            let boltz_public_nonce =
                MusigPubNonce::from_slice(&Vec::from_hex(&partial_sig_resp.pub_nonce)?)?;

            let boltz_partial_sig = MusigPartialSignature::from_slice(&Vec::from_hex(
                &partial_sig_resp.partial_signature,
            )?)?;

            let agg_nonce = MusigAggNonce::new(&secp, &[boltz_public_nonce, pub_nonce]);

            let musig_session = MusigSession::new(&secp, &key_agg_cache, agg_nonce, msg);

            // Verify the sigs.
            let boltz_partial_sig_verify = musig_session.partial_verify(
                &secp,
                &key_agg_cache,
                boltz_partial_sig,
                boltz_public_nonce,
                self.swap_script.receiver_pubkey.inner, //boltz key
            );

            if (!boltz_partial_sig_verify) {
                return Err(Error::Taproot(
                    "Unable to verify Partial Signature".to_string(),
                ));
            }

            let our_partial_sig =
                musig_session.partial_sign(&secp, sec_nonce, keys, &key_agg_cache)?;

            let schnorr_sig = musig_session.partial_sig_agg(&[boltz_partial_sig, our_partial_sig]);

            let final_schnorr_sig = SchnorrSig {
                sig: schnorr_sig,
                hash_ty: SchnorrSighashType::Default,
            };

            let output_key = self.swap_script.taproot_spendinfo()?.output_key();

            secp.verify_schnorr(&final_schnorr_sig.sig, &msg, &output_key.into_inner())?;

            let mut script_witness = Witness::new();
            script_witness.push(final_schnorr_sig.to_vec());

            let witness = TxInWitness {
                amount_rangeproof: None,
                inflation_keys_rangeproof: None,
                script_witness: script_witness.to_vec(),
                pegin_witness: vec![],
            };

            refund_tx.input[0].witness = witness;
        }

        Ok(refund_tx)
    }

    fn create_refund(
        &self,
        keys: &Keypair,
        absolute_fees: u64,
        is_cooperative: bool,
    ) -> Result<Transaction, Error> {
        // Create unsigned refund transaction
        let refund_txin = TxIn {
            sequence: Sequence::MAX,
            previous_output: self.funding_outpoint,
            script_sig: Script::new(),
            witness: TxInWitness::default(),
            is_pegin: false,
            asset_issuance: AssetIssuance::default(),
        };

        let secp = Secp256k1::new();

        let unblined_utxo = self
            .funding_utxo
            .unblind(&secp, self.swap_script.blinding_key.secret_key())?;
        let asset_id = unblined_utxo.asset;
        let out_abf = AssetBlindingFactor::new(&mut thread_rng());
        let exp_asset = Asset::Explicit(asset_id);

        let (blinded_asset, asset_surjection_proof) =
            exp_asset.blind(&mut thread_rng(), &secp, out_abf, &[unblined_utxo])?;

        let output_value = Amount::from_sat(unblined_utxo.value) - Amount::from_sat(absolute_fees);

        let final_vbf = ValueBlindingFactor::last(
            &secp,
            output_value.to_sat(),
            out_abf,
            &[(
                unblined_utxo.value,
                unblined_utxo.asset_bf,
                unblined_utxo.value_bf,
            )],
            &[(
                absolute_fees,
                AssetBlindingFactor::zero(),
                ValueBlindingFactor::zero(),
            )],
        );
        let explicit_value = elements::confidential::Value::Explicit(output_value.to_sat());
        let msg = elements::RangeProofMessage {
            asset: asset_id,
            bf: out_abf,
        };
        let ephemeral_sk = SecretKey::new(&mut thread_rng());

        // assuming we always use a blinded address that has an extractable blinding pub
        let blinding_key = self
            .output_address
            .blinding_pubkey
            .ok_or(Error::Protocol("No blinding key in tx.".to_string()))?;
        let (blinded_value, nonce, rangeproof) = explicit_value.blind(
            &secp,
            final_vbf,
            blinding_key,
            ephemeral_sk,
            &self.output_address.script_pubkey(),
            &msg,
        )?;

        let tx_out_witness = TxOutWitness {
            surjection_proof: Some(Box::new(asset_surjection_proof)), // from asset blinding
            rangeproof: Some(Box::new(rangeproof)),                   // from value blinding
        };
        let payment_output: TxOut = TxOut {
            script_pubkey: self.output_address.script_pubkey(),
            value: blinded_value,
            asset: blinded_asset,
            nonce,
            witness: tx_out_witness,
        };
        let fee_output: TxOut = TxOut::new_fee(absolute_fees, asset_id);

        let refund_script = self.swap_script.refund_script();

        let lock_time = match refund_script
            .instructions()
            .filter_map(|i| {
                let ins = i.unwrap();
                if let Instruction::PushBytes(bytes) = ins {
                    if bytes.len() < 5_usize {
                        Some(LockTime::from_consensus(bytes_to_u32_little_endian(bytes)))
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .next()
        {
            Some(r) => r,
            None => {
                return Err(Error::Protocol(
                    "Error getting timelock from refund script".to_string(),
                ))
            }
        };

        let mut refund_tx = Transaction {
            version: 2,
            lock_time,
            input: vec![refund_txin],
            output: vec![fee_output, payment_output],
        };

        if is_cooperative {
            refund_tx.input[0].witness = Self::stubbed_cooperative_witness();
        } else {
            refund_tx.input[0].sequence = Sequence::ZERO;

            let leaf_hash = TapLeafHash::from_script(&refund_script, LeafVersion::default());

            let sighash = SighashCache::new(&refund_tx).taproot_script_spend_signature_hash(
                0,
                &Prevouts::All(&[&self.funding_utxo]),
                leaf_hash,
                SchnorrSighashType::Default,
                self.genesis_hash,
            )?;

            let msg = Message::from_digest_slice(sighash.as_byte_array())?;

            let sig = secp.sign_schnorr(&msg, keys);

            let final_sig = SchnorrSig {
                sig,
                hash_ty: SchnorrSighashType::Default,
            };

            let control_block = match self
                .swap_script
                .taproot_spendinfo()?
                .control_block(&(refund_script.clone(), LeafVersion::default()))
            {
                Some(r) => r,
                None => return Err(Error::Taproot("Could not create control block".to_string())),
            };

            let mut script_witness = Witness::new();
            script_witness.push(final_sig.to_vec());
            script_witness.push(refund_script.as_bytes());
            script_witness.push(control_block.serialize());

            let witness = TxInWitness {
                amount_rangeproof: None,
                inflation_keys_rangeproof: None,
                script_witness: script_witness.to_vec(),
                pegin_witness: vec![],
            };

            refund_tx.input[0].witness = witness;
        }

        Ok(refund_tx)
    }

    fn stubbed_cooperative_witness() -> TxInWitness {
        let mut witness = Witness::new();
        // Stub because we don't want to create cooperative signatures here
        // but still be able to have an accurate size estimation
        witness.push([0; 64]);

        TxInWitness {
            amount_rangeproof: None,
            inflation_keys_rangeproof: None,
            script_witness: witness.to_vec(),
            pegin_witness: vec![],
        }
    }

    /// Calculate the size of a transaction.
    /// Use this before calling drain to help calculate the absolute fees.
    /// Multiply the size by the fee_rate to get the absolute fees.
    pub fn size(
        &self,
        keys: &Keypair,
        is_cooperative: bool,
        is_discount_ct: bool,
    ) -> Result<usize, Error> {
        let dummy_abs_fee = 1;
        let tx = match self.kind {
            SwapTxKind::Claim => {
                let preimage = Preimage::from_vec([0; 32].to_vec())?;
                self.create_claim(keys, &preimage, dummy_abs_fee, is_cooperative)?
            }
            SwapTxKind::Refund => self.create_refund(keys, dummy_abs_fee, is_cooperative)?,
        };
        Ok(tx_size(&tx, is_discount_ct))
    }

    /// Broadcast transaction to the network
    pub fn broadcast(
        &self,
        signed_tx: &Transaction,
        network_config: &ElectrumConfig,
        is_lowball: Option<(&BoltzApiClientV2, Chain)>,
    ) -> Result<String, Error> {
        if let Some((boltz_api, chain)) = is_lowball {
            log::info!("Attempting lowball broadcast");
            let tx_hex = serialize(signed_tx).to_lower_hex_string();
            let response = boltz_api.broadcast_tx(chain, &tx_hex)?;

            match response.as_object() {
                None => Err(Error::Protocol("Invalid broadcast reply".to_string())),
                Some(response_map) => match response_map.get("id") {
                    None => Err(Error::Protocol(
                        "No txid found in broadcast reply".to_string(),
                    )),
                    Some(txid_val) => match txid_val.as_str() {
                        None => Err(Error::Protocol("Returned txid is not a string".to_string())),
                        Some(txid_str) => {
                            let txid = txid_str.to_string();
                            log::info!("Broadcasted transaction via Boltz: {txid}");
                            Ok(txid)
                        }
                    },
                },
            }
        } else {
            let electrum_client = network_config.build_client()?;
            let serialized = serialize(signed_tx);
            Ok(electrum_client
                .transaction_broadcast_raw(&serialized)?
                .to_string())
        }
    }
}

fn tx_size(tx: &Transaction, is_discount_ct: bool) -> usize {
    match is_discount_ct {
        true => tx.discount_vsize(),
        false => tx.vsize(),
    }
}

fn hex_to_bytes(hex_str: &str) -> Result<Vec<u8>, Error> {
    if hex_str.len() % 2 != 0 {
        return Err(Error::Hex(
            "Hex string must have an even length".to_string(),
        ));
    }
    let mut bytes = Vec::new();
    for i in (0..hex_str.len()).step_by(2) {
        let hex_pair = &hex_str[i..i + 2];
        match u8::from_str_radix(hex_pair, 16) {
            Ok(byte) => bytes.push(byte),
            Err(_) => {
                return Err(Error::Hex(format!(
                    "Invalid hexadecimal pair: {}",
                    hex_pair
                )))
            }
        }
    }

    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tx_size() {
        // From https://github.com/ElementsProject/ELIPs/blob/main/elip-0200.mediawiki#test-vectors
        let tx: Transaction = elements::encode::deserialize(&hex::decode("0200000001017b85545c658d507ff56f315c77f910dd19cc9ceb7d5e1e4d3a3f8be4a91fe7440000000000fdffffff020bb6478c61c8f5f024ded219c967314685257f0ded894eaf626a00843a6ab80412091ee78237e38fb36c8be564ecd76e65f743065522f38f838367680ed7287b459103aabd97d4c8f3eac9555edfd2a709370b802335da478b6578501f72a4d100482716001455f4f701eec6059f956a40335e317a96a5e87ab5016d521c38ec1ea15734ae22b7c46064412829c0d0579f0a713d1c04ede979026f01000000000000000e00000000000000000347304402205d62bc013832eb6a631fe0285c49b7e27846e03189a245bec8f86346382282a702206c6e839b4b1d79d74662e432b724671402a6cfa2287911677c7061a3a32abe34012042c6504afda18a302bbf935f1dc646f71872a9a2fb5ed9e0cffb64588fd0d0a865a9141243397ee5e188bdcd17c9529c1382c7f8bc0fe987632102a3cd0d865794542994737e776dc3827a046c02ea2693f1d1f64315b3557bbb8b670395f72bb17521034a2e0343a515cf7d4a583d05bec3ee9fc16758cae791c10064fa92d65672d1fe68ac004301000177ce2a14a4f9e556fc846219827e1bc584caf9ef35e761dbf1f961a89b8285bde8fbe242c6984dd28719a792cd2e63535287db9a3b1fc4e4c5ae28cc5e8973d0fd4e10603300000000000000014cf45a01f0036bec883cdd4d5d8de1d7b3f2ec125733ce2e123ef3ff0085c50fd1b8cd3101c24fd8fff0bab803cda813aad9645ca6714ce768da75da09b58851585551c425e729d6faf4186a6659ea107f4ef35cc458dae565f1337af46cde218563eb3a756dc5d532717cc775fc0d04fbf4492070eb3cd9943a12fd07939d69a71090871e1ddf8fe716e2bc3f3364783cdb1d6a704325ca6c4334171563ae7bfcc9766ab848a65f47973753b2758b4404f17e54527080cfb980d1227f70cc0e77212d06aea909c7f2ac38f4a75c387464f8b70e33061f017a6fbbccf0673d08aebae2a1ce6cf9dd8c98791b1f4d653788b2ed6dd65cf9795eac568744e386d68c89d973ca079298f8d292b6bee71fad94a0f83aaf070ccfeb6c6de20baf8c6f1083dcdd539fae6ed74832100ea7c07296c0af2201523c3abf8b784ca8a235556d5bae668f17d9a353fd49dbae623ca44830a8fc4963419e49a9dc99bf87ea0414be3b43a6eab8ce54695d66887b261c08252a501d0c78d30be1ae3fc10f557f4d228ef38da496b22c5fa79d92e2c190b9d31f286dc0e3c8489fcb8e0603f8b93a6eb1ec726a7e0015e70407da186d85b290b054747276a8928443e1108cb67738d156787d20553c39fa0449f95addbf42170fdab8107d1f93fcd841964b6e6c4c140d0c4ed1463835e603f5012a4aafd5b038ceb9b4a5b7e2688cfd8c4f2bfafaf0bb5bb1aa7a7f13bd47ff3da57c4c88b741fd9ff97abc23d4047f690d59c4c67494f47125fe0f626ad409a92d72907ad0b1762b5271f474fa552d9139fcb1103db24f7a29726a5e41a6dbc43590c14a62eb1b2aa0f160134c42c6c87c696e7c42546bb72f9f531729555d01c529570553aeec70709c3a4f9aacf810d5018f776af48b93eff8e120242105c06a32e64bfc825fde488c99d5845adba2cf349717f64e488852cca73cc5813b7872f7e89d24b4bfafdf75faa368375d5bfdd8b8a7ad641703cbff131616c77e79d8f78c5fe63810781db44fb1fa5cc9387cf0de6807d1a3d5e3d8f9ec7418bbb1d4e10b1fcdb300abd8625b4e24842f1f4c4e567fe9f8c6e9d314757d4568889bccc740fb36f0270804cc11c0044093ab9586ed034cd1eb70bacdedd573750794f0286dfb91c91308e507147ea8e8534c655b931f4e68543e93c57cf2f2159e021739943e40c0dbc8a68193218d40d71e0956b00b4a01fa9c06e67ea55e0213fab48a8dfcf3a047e8c438e7c94fc195026cec82ad532e2aa5970a9fe6c03d9088d0ab45e0b9c7bf9597bd2db93ef7d7f139c291f59e03cda1a5f9a793eb7ec6d50fa9482b712500b5e5a780319769836f7053e3c5a3276a7d65467578a7fbf9079fb5c6bb1b0558acbf3cd896644d42a7b0fd87b12b571b3d8122b1c254750bf9b097d0ec5ed31f9af7db9571f706f5909f0ef2fdcdb255a0795f5c28b70fd1d25b74eb2524ae8f47756875ff439a2b2769adc844312c4ac7bde16b561e62ee3069d25718bf6c2e11ffbb83c863a51c52ff4ead581dd6b1ff0913905163683b97ecbad003a1c71469050eed5ad79e9bb44179b90b8e6b0e6a61a0ed4e919cb96c2615b61cf93905adc3e6e2a127bd661f05e928a45bc1c0599c41450dea0182043b977fcfcf3620f765d3aab13cbe684028dc78a4bd02324427379735934ab4cb821623f49e3af05391c1b7acfe8be33c9201efeded50838ff216d6744d61e8d1d600260c8f7275a46764ac9392132f0b3661e5e92e9daa87b9329d9c89353f40a130bcf8611cce25335f9f1c1208ae1bdc47d96c3f83170a7d27367a043debdfd0e43776d330d1f7a806b32c4363d1dca14715dae4f4d1c99a92673954094e61387080353974097adfde15de4009caa28d42703fdb56fcdac47bd9c5e3bad2fbf90b4a3fab4d89a9933e445ba85f759cc149101f5045a6f3a6d741424318249d96277cea3dc0c4814763d727c72a1867618ac05e5ff103b985cc6f78829bae92794680a51c4b7f7f8b88e39ddd4471890914594f3f03ae668d501732ea77b3eb1fb38b5ad9efdac8775e0995c60a3949e84d2298ea3463aaa16d5ff633da654463e90004915ccc19663c87e006fcd05e904b85b71428d79913e3afdecb7ad51a66f7dcb738d028b62b307025d524320dbe064330da5cbd70467635cf492197c7be3513363b4000bf176827011b2894d33dc9d806b2526a6e91cc1cf0582c5330484b8d48be4855c1859a5b20cab6d08d95b42b57fc709dcb637ba9c6e70b72c473af88ebe8723fe94a0d5ee5d483f19c3b2aade19bafed774b786c0d24383fe0f71c085655f4bd78cb36da83b5429576576c0718b4549efe5b8f602c543c3a8e3d86f19b70d6be1fb39b7cbbac6fcf6d80d69c00ed44dbed1b8555593bd6dcf9ddd519f9325f6faa146d4b631cc6ee418ef9d07a0036fb26a792e7733ec0b58d9f0ebba9ea9493fa026bab62f70381e534c8c3b349be651e9fd5d472b3cbf8f7e912b7030a1992df35e17f4c5aa54f1632464a7c3b0dd133da8d436205bf45d8ded924e35b366803ee52a3d1c85d9f4f976785270dafb63d2cd5052328ed2e5381e9a6e9d8409675c2a9a43c74b07e8a3df8043b2b6d42832cabfcd495b8b30727346990fbc79e436d7ba4d7035603ab98532c5497ef493511e498b1b9c5ff413e919ab6f3cd6acc472f6a39ad0a8c9677ac9a5380a6bebbaaf13a114d097efbf140acad7edecc758bb070fa0b88bb0646d3bed911414a3f10b12bf8372d66f4525f9a8a66d7bf2b5d364119a687e5f416511c27659cf70969863ed7f80e80a4f2e55bf25721e1ab415305b66bfc25b9630a265b553d3e806807f23ec1e2a5f657dbd73a4a36e95e6616faa6aefc5143ca29b0e4bc9eb1042d99c74115d96a2eec5e7fb8c3f598d4df8fa8953e96689651a705dd3f385cd27e0173baca570ce53001cdb002e4476e6af47b9a891f84f7c1c472cce3cd4a70a40c298819f6d75e6adac193798c740c9f5f57fee4df5d140cce8ee4152c17784899003dc000cd2e7c7f23e74da085b254e0843d97d147e44ab3ba12e308925fc6ab0460c7ceb107b0900cef5ff939bc3fe5640f0bb11597c561be275fc8b5b85f5e38a3c12ea26b5b7b32e407685db70d16a3ce51043d4009a647fd3656a54adcbd4d1baa6d89881973fe32faf071123de1712e85db628bdd987566b362845d0c5f818547ec2d1f7c668cae44f0bec74c6663134dd0273c3363f31901903e4e976a447af96f6f521059fb6b892a0599cf7aae457df3aed72f1f55e145332c91430a2f8184bb917d317f8d9c4b6769b9a3a0ac5baea88b39b8f7662ecc16585e7166f61a948f48e6d30c2cfd82820cccdf5e722db2156bd848ea4d13c92544d1d9064414a305215a8271631ffebf08cdf0bcbbbd939f78eafec0d7238bdb90f211d6c44589187d1a501eef7d0b6118e028afcf76ffda95a43e2211206d9d50d34c3e33a6c991952ccd73e722802a14227692f037bba585e73cb9a6cd7556f9ec2158f197a51e3884afb8e59eaa8e7ac3568d88b27b2a5ab8cd72648193ff6068e4d481c58c117e2adda564d5a49f6b992ff6f938acb283e7baf704c71861d60b263f6c6684d7544878b7aca942af8b3a70ae0def309b68fac2aed2b11ba753d7b47f7369805e5b3b9b41d22196e2cc098ece59bdf5231b03fba8adae08fee227a582490b0db34c115620c72afb6fcb507397d1333ea19e7969b729bc2733e6546d2d9f3edb08f9c74201f9ed4e3fcb446cc3fd688b1345e97b32492c9173fa71df2772bd825506ddd6447e9f9e8ece0ffb860e1c755bcf2400deef094219795d4ee84acc34dedc9a3b3adf7fc81733bc511b8edcb54769400940b53471d8e82cb82d9967a97297bdd87f165968ea046291234da176efd20889aa4c07179df83cb500b40bdb96b0c27f2bfa57353268b776740432d29f1761fee77755c7b219def785a42b683e1f70240ec45cdf660e894d4fb541d0511547c9a2c503cf605d72ea7f2abaee4e8adc222a82f4b86c34ad8b25e2932df02f0090d2dbf8817c44659b1245d5579277ad406c538914f90dbaefdd110c5ca0d63a24706cd51096ec19f819c446c9fcb55b777ae633f0257dc4d1b293e6ef68ea7867d852058212a0a9ace9442422a638f73dfb14cc4354b6481ee6591037e7287e962037d963b38a7e4ec12b30e0f6e0ee4d8c30d288e99e22e43b4c795c51d66cc4225c5cab3685b1b3a6fd3a82dfc355634b347cc4f4e55413728fb67fb9f34d3f7e4ecce3254ea843ab361b0f652faa9e54470e3e414c1bb2593e36d88109c36dfab505a16c19152fe021de608c6b3d924c981231ea9cf1cf8c93e53f0df78033e81fdb578a45b7dc4f3f0f68feedc78ec7c347f91a0464bccd58aa2fc11016e88cbaddfb22112edad752792af12fa550be3e6f15d69a6a9d547ab5381b93c58c12753b8085d9e17ed1f2519cc5cb756e3777ea9f8e49a6141460f8f6ced8d12d13d950691479e1207ed35ab71554122beb215a0fb6b34b90784f4be6bd6fbf93daf9d3bc4640bc52a662e750ce361c12c1bfa2ca4e2c784cbf70c406587b2ebd69faa7a891aca63d600247ad7dde426c1ef4e3b22a072ff8eb69c1b1cb30c605112786546c48cf1c4821b5bc0d0bd44ba83b05656b6e19a3d1a76931d983dd39efcc64298e892858e847e99519c1fa25b1998839788c5852b94202d803639d69058604374f76769670a60269dbc0688cea2d9d8672212b93ca501fbf6f7dfefad058e4bd0e0da1cff41b2f408c980f29a49b03efa9e3edef091d7df7529b6b5e8f7d43d103681cd7c38d02a431b15d539e9a3cf44dc71621664e756ad6404ba185b5e20c82760c488fde4253fb52ab850484a082e7ca275f475012be9c8d16d6b4a2c9d863440d5e113d18bbf42f128462764a99ca90af4fde890aee138fe4cbb45658eacd9d38c8a1fb4499c043cc25af87e6a650f38149ab018cc49f50bbd085e2a0ba3eeecde5764f7997748a660593191977792d7176e4c2ff0113d67b9abe8fbc10f364c6fa68e52a455aa56ff15099c6efb6b5812972380d5b8e256b0feb1190835b7d076744c1b5b738c710a07a32676a15d96583e89e39eb4ff08cf02c6e2ad540c2b66299afe01bf2e50c81465a04d229a07c58ffd25a6cd9288110045526b376548d373273e6227d117d491020fd68e366ed697a0d30a5bdff25fa9a5800aa534a3669215dfa8f30960f142a8ae7ffcb654ca60aa7dc8a586670f9db37d05644ff5f934785c5433e605f3fbd0340e168511e209a0aedd8b18f3b948eb58051136d155f53b0e2e027361330e005f83f3a72dcc5d9161dd4b1e6abd16635dc0887dcc833a1fb59c10e0b8bea2536e7acd58d5e11179d13a24dc4292624c527266351b9a48893b956ffe545c8d2c1563805addef2a82134c9c686449d83471f22c1e14601895e854a5f854230e4fb4ed4f9a7ee22e83234be6c5bb19d200c16543468f186ae11cba84ae1aeda5136f7f5b380d02ddb9cbe2c5f5bb39138fa29b2ceb549d2e337eba10171fc237473351cf8e5989c193ef0100c75778ad0c05b64b614067c9a70680c818a566c4ba5e2991eedfe165199a55b0bef1333988f2add167e268db389c2d25bd85eedff9e6851e3df84c9e41128b5a76869c086fcf9275b1d51af02e4a92b66850785319dbf004a29594e32d12ca42da69fac69f886f963409ce1d4514d1ab9e915e071887e7f316b15014d083769afea374e0771f74f632db5ed7d7352546ed686e3ee161cd263dafc2acab74a67a5721f923f9b07c647c2a04f7d1c2f831d4319a60b16ed4c995e35ccbc291ff647a382976ba5a957547b0000").unwrap()).unwrap();

        assert_eq!(tx_size(&tx, false), 1333);
        assert_eq!(tx_size(&tx, true), 216);
    }
}
