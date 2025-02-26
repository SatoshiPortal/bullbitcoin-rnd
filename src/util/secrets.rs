use std::fmt::Display;
use std::fmt::Formatter;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use bip39::Mnemonic;
use bitcoin::bip32::{DerivationPath, Fingerprint, Xpriv};
use bitcoin::hex::{DisplayHex, FromHex};
use bitcoin::key::rand::{rngs::OsRng, RngCore};
use bitcoin::secp256k1::hashes::{hash160, ripemd160, sha256, Hash};
use bitcoin::secp256k1::{Keypair, Secp256k1};
use elements::secp256k1_zkp::{Keypair as ZKKeyPair, Secp256k1 as ZKSecp256k1};
use lightning_invoice::Bolt11Invoice;
use serde::{Deserialize, Serialize};
use serde_json;

use crate::error::Error;
use crate::network::Chain;

const SUBMARINE_SWAP_ACCOUNT: u32 = 21;
const REVERSE_SWAP_ACCOUNT: u32 = 42;
const CHAIN_SWAP_ACCOUNT: u32 = 84;

const BITCOIN_NETWORK_PATH: u32 = 0;
const LIQUID_NETWORK_PATH: u32 = 1776;
const TESTNET_NETWORK_PATH: u32 = 1;

/// Derived Keypair for use in a script.
/// Can be used directly with Bitcoin structures
/// Can be converted .into() LiquidSwapKey
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SwapKey {
    pub fingerprint: Fingerprint,
    pub path: DerivationPath,
    pub keypair: Keypair,
}
impl SwapKey {
    /// Derives keys for a submarine swap at standardized path
    /// m/49'/<0;1777;1>/21'/0/*
    pub fn from_submarine_account(
        mnemonic: &str,
        passphrase: &str,
        network: Chain,
        index: u64,
    ) -> Result<SwapKey, Error> {
        let secp = Secp256k1::new();
        let mnemonic_struct = Mnemonic::from_str(mnemonic)?;
        let seed = mnemonic_struct.to_seed(passphrase);
        let root = Xpriv::new_master(bitcoin::Network::Testnet, &seed)?;
        let fingerprint = root.fingerprint(&secp);
        let purpose = DerivationPurpose::Compatible;
        let network_path = match network {
            Chain::Bitcoin => BITCOIN_NETWORK_PATH,
            Chain::Liquid => LIQUID_NETWORK_PATH,
            _ => TESTNET_NETWORK_PATH,
        };
        let derivation_path = format!(
            "m/{}h/{}h/{}h/0/{}",
            purpose, network_path, SUBMARINE_SWAP_ACCOUNT, index
        );
        let path = DerivationPath::from_str(&derivation_path)?;
        let child_xprv = root.derive_priv(&secp, &path)?;

        let key_pair = Keypair::from_secret_key(&secp, &child_xprv.private_key);

        Ok(SwapKey {
            path,
            fingerprint,
            keypair: key_pair,
        })
    }
    /// Derives keys for a reverse swap at standardized path
    /// m/49'/<0;1777;1>/42'/0/*
    pub fn from_reverse_account(
        mnemonic: &str,
        passphrase: &str,
        network: Chain,
        index: u64,
    ) -> Result<SwapKey, Error> {
        let secp = Secp256k1::new();
        let mnemonic_struct = Mnemonic::from_str(mnemonic)?;

        let seed = mnemonic_struct.to_seed(passphrase);
        let root = Xpriv::new_master(bitcoin::Network::Testnet, &seed)?;
        let fingerprint = root.fingerprint(&secp);
        let purpose = DerivationPurpose::Native;
        let network_path = match network {
            Chain::Bitcoin => BITCOIN_NETWORK_PATH,
            Chain::Liquid => LIQUID_NETWORK_PATH,
            _ => TESTNET_NETWORK_PATH,
        };
        // m/84h/1h/42h/<0;1>/*  - child key for segwit wallet - xprv
        let derivation_path = format!(
            "m/{}h/{}h/{}h/0/{}",
            purpose, network_path, REVERSE_SWAP_ACCOUNT, index
        );
        let path = DerivationPath::from_str(&derivation_path)?;
        let child_xprv = root.derive_priv(&secp, &path)?;

        let key_pair = Keypair::from_secret_key(&secp, &child_xprv.private_key);

        Ok(SwapKey {
            path,
            fingerprint,
            keypair: key_pair,
        })
    }
    /// Derives keys for a chain swap at standardized path
    pub fn from_chain_account(
        mnemonic: &str,
        passphrase: &str,
        network: Chain,
        index: u64,
    ) -> Result<SwapKey, Error> {
        let secp = Secp256k1::new();
        let mnemonic_struct = Mnemonic::from_str(mnemonic)?;

        let seed = mnemonic_struct.to_seed(passphrase);
        let root = Xpriv::new_master(bitcoin::Network::Testnet, &seed)?;
        let fingerprint = root.fingerprint(&secp);
        let purpose = DerivationPurpose::Taproot;
        let network_path = match network {
            Chain::Bitcoin => BITCOIN_NETWORK_PATH,
            Chain::Liquid => LIQUID_NETWORK_PATH,
            _ => TESTNET_NETWORK_PATH,
        };
        // m/84h/1h/42h/<0;1>/*  - child key for segwit wallet - xprv
        let derivation_path = format!(
            "m/{}h/{}h/{}h/0/{}",
            purpose, network_path, CHAIN_SWAP_ACCOUNT, index
        );
        let path = DerivationPath::from_str(&derivation_path)?;
        let child_xprv = root.derive_priv(&secp, &path)?;

        let key_pair = Keypair::from_secret_key(&secp, &child_xprv.private_key);

        Ok(SwapKey {
            path,
            fingerprint,
            keypair: key_pair,
        })
    }
}
#[derive(Clone)]

/// For Liquid keys, first create a SwapKey and then call .into() to get the equivalent ZKKeypair
/// let sk = SwapKey::from_reverse_account(&mnemonic.to_string(), "", Chain::LiquidTestnet, 1)?
/// let lsk: LiquidSwapKey = swap_key.try_into()?;
/// let zkkp = lsk.keypair;
#[derive(Serialize, Deserialize, Debug)]
pub struct LiquidSwapKey {
    pub fingerprint: Fingerprint,
    pub path: DerivationPath,
    pub keypair: ZKKeyPair,
}
impl TryFrom<SwapKey> for LiquidSwapKey {
    type Error = Error;
    fn try_from(swapkey: SwapKey) -> Result<Self, Self::Error> {
        let secp = ZKSecp256k1::new();
        let liquid_keypair =
            ZKKeyPair::from_seckey_str(&secp, &swapkey.keypair.display_secret().to_string())?;

        Ok(LiquidSwapKey {
            fingerprint: swapkey.fingerprint,
            path: swapkey.path,
            keypair: liquid_keypair,
        })
    }
}
enum DerivationPurpose {
    Compatible,
    Native,
    Taproot,
}
impl Display for DerivationPurpose {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            DerivationPurpose::Compatible => write!(f, "49"),
            DerivationPurpose::Native => write!(f, "84"),
            DerivationPurpose::Taproot => write!(f, "86"),
        }
    }
}

/// Internally used rng to generate secure 32 byte preimages
fn rng_32b() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes
}

/// Helper to work with Preimage & Hashes required for swap scripts.
#[derive(Debug, Clone, PartialEq)]
pub struct Preimage {
    pub bytes: Option<[u8; 32]>,
    pub sha256: sha256::Hash,
    pub hash160: hash160::Hash,
}

impl FromStr for Preimage {
    type Err = Error;

    /// Creates a struct from a preimage string.
    fn from_str(preimage: &str) -> Result<Self, Self::Err> {
        Self::from_vec(Vec::from_hex(preimage)?)
    }
}

impl Default for Preimage {
    fn default() -> Self {
        Preimage::new()
    }
}

impl Preimage {
    /// Creates a new random preimage
    pub fn new() -> Preimage {
        let preimage = rng_32b();
        let sha256 = sha256::Hash::hash(&preimage);
        let hash160 = hash160::Hash::hash(&preimage);

        Preimage {
            sha256,
            hash160,
            bytes: Some(preimage),
        }
    }

    /// Creates a struct from a preimage vector.
    pub fn from_vec(preimage: Vec<u8>) -> Result<Preimage, Error> {
        // Ensure the decoded bytes are exactly 32 bytes long
        let preimage: [u8; 32] = preimage
            .try_into()
            .map_err(|_| Error::Protocol("Decoded Preimage input is not 32 bytes".to_string()))?;
        let sha256 = sha256::Hash::hash(&preimage);
        let hash160 = hash160::Hash::hash(&preimage);
        Ok(Preimage {
            sha256,
            hash160,
            bytes: Some(preimage),
        })
    }

    /// Creates a Preimage struct without a value and only a hash
    /// Used only in submarine swaps where we do not know the preimage, only the hash
    pub fn from_sha256_str(preimage_sha256: &str) -> Result<Preimage, Error> {
        Self::from_sha256_vec(Vec::from_hex(preimage_sha256)?)
    }

    /// Creates a Preimage struct without a value and only a hash
    /// Used only in submarine swaps where we do not know the preimage, only the hash
    pub fn from_sha256_vec(preimage_sha256: Vec<u8>) -> Result<Preimage, Error> {
        let sha256 = sha256::Hash::from_slice(preimage_sha256.as_slice())?;
        let hash160 = hash160::Hash::from_slice(
            ripemd160::Hash::hash(sha256.as_byte_array()).as_byte_array(),
        )?;
        // will never fail as long as sha256 is a valid sha256::Hash
        Ok(Preimage {
            sha256,
            hash160,
            bytes: None,
        })
    }

    /// Extracts the preimage sha256 hash from a lightning invoice
    /// Creates a Preimage struct without a value and only a hash
    pub fn from_invoice_str(invoice_str: &str) -> Result<Preimage, Error> {
        let invoice = Bolt11Invoice::from_str(invoice_str)?;
        Preimage::from_sha256_str(&invoice.payment_hash().to_string())
    }

    /// Converts the preimage value bytes to String
    pub fn to_string(&self) -> Option<String> {
        self.bytes.map(|res| res.to_lower_hex_string())
    }
}

/// Boltz standard JSON refund swap file. Can be used to create a file that can be uploaded to boltz.exchange
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RefundSwapFile {
    pub id: String,
    pub currency: String,
    pub redeem_script: String,
    pub private_key: String,
    pub timeout_block_height: u32,
}
impl RefundSwapFile {
    pub fn file_name(&self) -> String {
        format!("boltz-{}.json", self.id)
    }
    pub fn write_to_file<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        let mut full_path = PathBuf::from(path.as_ref());
        full_path.push(self.file_name());
        let mut file = File::create(&full_path)?;
        let json = serde_json::to_string_pretty(self)?;
        writeln!(file, "{}", json)?;
        Ok(())
    }
    pub fn read_from_file<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let mut file = File::open(path)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        Ok(serde_json::from_str(&contents)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use elements::pset::serialize::Serialize;

    #[macros::test_all]
    fn test_derivation() {
        let mnemonic: &str = "bacon bacon bacon bacon bacon bacon bacon bacon bacon bacon bacon bacon bacon bacon bacon bacon bacon bacon bacon bacon bacon bacon bacon bacon";
        let index = 0_u64; // 0
        let sk = SwapKey::from_submarine_account(mnemonic, "", Chain::Bitcoin, index).unwrap();
        let lsk: LiquidSwapKey = match LiquidSwapKey::try_from(sk.clone()) {
            Ok(t) => t,
            Err(e) => {
                // Conversion failed, handle the error
                return println!("Error converting to LiquidSwapKey: {:?}", e);
            }
        };
        assert_eq!(sk.fingerprint, lsk.fingerprint);
        // println!("{:?}", derived.unwrap().Keypair.display_secret());
        assert_eq!(&sk.fingerprint.to_string().clone(), "9a6a2580");
        assert_eq!(
            &sk.keypair.display_secret().to_string(),
            "d8d26ab9ba4e2c44f1a1fb9e10dc9d78707aaaaf38b5d42cf5c8bf00306acd85"
        );
    }

    #[macros::test_all]
    fn test_preimage_from_str() {
        let preimage = Preimage::new();
        assert_eq!(
            Preimage::from_str(&hex::encode(preimage.bytes.unwrap()).to_string()).unwrap(),
            preimage
        );
    }

    #[macros::test_all]
    fn test_preimage_from_vec() {
        let preimage = Preimage::new();
        assert_eq!(
            Preimage::from_vec(Vec::from(preimage.bytes.unwrap())).unwrap(),
            preimage
        );
    }

    #[macros::test_all]
    fn test_preimage_from_vec_invalid_length() {
        let mut bytes = [0u8; 33];
        OsRng.fill_bytes(&mut bytes);
        assert_eq!(
            Preimage::from_vec(Vec::from(bytes))
                .err()
                .unwrap()
                .message(),
            "Decoded Preimage input is not 32 bytes".to_string()
        );
    }

    #[macros::test_all]
    fn test_preimage_from_sha256_str() {
        let preimage = Preimage::new();
        let compare = Preimage::from_sha256_str(preimage.sha256.to_string().as_str()).unwrap();

        assert_eq!(compare.bytes, None);
        assert_eq!(compare.sha256, preimage.sha256);
        assert_eq!(compare.hash160, preimage.hash160);
    }

    #[macros::test_all]
    fn test_preimage_from_sha256_vec() {
        let preimage = Preimage::new();
        let compare = Preimage::from_sha256_vec(preimage.sha256.serialize()).unwrap();

        assert_eq!(compare.bytes, None);
        assert_eq!(compare.sha256, preimage.sha256);
        assert_eq!(compare.hash160, preimage.hash160);
    }

    // #[macros::test_all]
    // #[ignore]
    // fn test_recover() {
    //     let recovery = BtcSubmarineRecovery {
    //         id: "y8uGeA".to_string(),
    //         refund_key: "5416f1e024c191605502017d066786e294f841e711d3d437d13e9d27e40e066e".to_string(),
    //         redeem_script: "a914046fabc17989627f6ca9c1846af8e470263e712d87632102c929edb654bc1da91001ec27d74d42b5d6a8cf8aef2fab7c55f2eb728eed0d1f6703634d27b1752102c530b4583640ab3df5c75c5ce381c4b747af6bdd6c618db7e5248cb0adcf3a1868ac".to_string(),
    //     };
    //     //let file: RefundSwapFile = recovery.try_into();

    //     let file: RefundSwapFile = match BtcSubmarineRecovery::try_into(recovery) {
    //         Ok(file) => file,
    //         Err(err) => {
    //             // Handle the error
    //             return println!("Error converting: {:?}", err);
    //         }
    //     };

    //     let base_path = "/tmp/boltz-rust";
    //     file.write_to_file(base_path).unwrap();
    //     let file_path = base_path.to_owned() + "/" + &file.file_name();
    //     let file_struct = RefundSwapFile::read_from_file(file_path);
    //     println!("Refund File: {:?}", file_struct);
    // }
}
