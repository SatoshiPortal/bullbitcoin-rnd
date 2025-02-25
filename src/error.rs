/// The Global Error enum. Encodes all possible internal library errors
#[derive(Debug)]
pub enum Error {
    #[cfg(feature = "electrum")]
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    Electrum(electrum_client::Error),
    #[cfg(feature = "esplora")]
    Esplora(String),
    Hex(String),
    Protocol(String),
    Key(bitcoin::key::ParsePublicKeyError),
    Address(String),
    Sighash(bitcoin::sighash::TaprootError),
    ElSighash(elements::sighash::Error),
    Secp(bitcoin::secp256k1::Error),
    HTTP(String),
    JSON(serde_json::Error),
    IO(std::io::Error),
    Bolt11(lightning_invoice::ParseOrSemanticError),
    LiquidEncode(elements::encode::Error),
    BitcoinEncode(bitcoin::consensus::encode::Error),
    Blind(String),
    ConfidentialTx(elements::ConfidentialTxOutError),
    BIP32(bitcoin::bip32::Error),
    BIP39(bip39::Error),
    Hash(bitcoin::hashes::FromSliceError),
    Locktime(String),
    Url(url::ParseError),
    WebSocket(tokio_tungstenite_wasm::Error),
    Taproot(String),
    Musig2(String),
    Generic(String),
}

#[cfg(feature = "electrum")]
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
impl From<electrum_client::Error> for Error {
    fn from(value: electrum_client::Error) -> Self {
        Self::Electrum(value)
    }
}

impl From<bitcoin::hex::HexToBytesError> for Error {
    fn from(value: bitcoin::hex::HexToBytesError) -> Self {
        Self::Hex(value.to_string())
    }
}

impl From<bitcoin::key::ParsePublicKeyError> for Error {
    fn from(value: bitcoin::key::ParsePublicKeyError) -> Self {
        Self::Key(value)
    }
}

impl From<bitcoin::hex::HexToArrayError> for Error {
    fn from(value: bitcoin::hex::HexToArrayError) -> Self {
        Self::Hex(value.to_string())
    }
}

impl From<hex::FromHexError> for Error {
    fn from(value: hex::FromHexError) -> Self {
        Self::Hex(value.to_string())
    }
}

impl From<bitcoin::address::ParseError> for Error {
    fn from(value: bitcoin::address::ParseError) -> Self {
        Self::Address(value.to_string())
    }
}

impl From<elements::address::AddressError> for Error {
    fn from(value: elements::address::AddressError) -> Self {
        Self::Address(value.to_string())
    }
}

impl From<elements::sighash::Error> for Error {
    fn from(value: elements::sighash::Error) -> Self {
        Self::ElSighash(value)
    }
}

impl From<bitcoin::sighash::TaprootError> for Error {
    fn from(value: bitcoin::sighash::TaprootError) -> Self {
        Self::Sighash(value)
    }
}

impl From<bitcoin::secp256k1::Error> for Error {
    fn from(value: bitcoin::secp256k1::Error) -> Self {
        Self::Secp(value)
    }
}

impl From<reqwest::Error> for Error {
    fn from(value: reqwest::Error) -> Self {
        Self::HTTP(value.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::JSON(value)
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::IO(value)
    }
}

impl From<lightning_invoice::ParseOrSemanticError> for Error {
    fn from(value: lightning_invoice::ParseOrSemanticError) -> Self {
        Self::Bolt11(value)
    }
}

impl From<elements::hex::Error> for Error {
    fn from(value: elements::hex::Error) -> Self {
        Self::Hex(value.to_string())
    }
}

impl From<elements::encode::Error> for Error {
    fn from(value: elements::encode::Error) -> Self {
        Self::LiquidEncode(value)
    }
}

impl From<elements::BlindError> for Error {
    fn from(value: elements::BlindError) -> Self {
        Self::Blind(value.to_string())
    }
}

impl From<elements::UnblindError> for Error {
    fn from(value: elements::UnblindError) -> Self {
        Self::Blind(value.to_string())
    }
}

impl From<elements::ConfidentialTxOutError> for Error {
    fn from(value: elements::ConfidentialTxOutError) -> Self {
        Self::ConfidentialTx(value)
    }
}

impl From<bitcoin::bip32::Error> for Error {
    fn from(value: bitcoin::bip32::Error) -> Self {
        Self::BIP32(value)
    }
}

impl From<bitcoin::hashes::FromSliceError> for Error {
    fn from(value: bitcoin::hashes::FromSliceError) -> Self {
        Self::Hash(value)
    }
}

impl From<bip39::Error> for Error {
    fn from(value: bip39::Error) -> Self {
        Self::BIP39(value)
    }
}

impl From<bitcoin::absolute::ConversionError> for Error {
    fn from(value: bitcoin::absolute::ConversionError) -> Self {
        Self::Locktime(value.to_string())
    }
}

impl From<elements::locktime::Error> for Error {
    fn from(value: elements::locktime::Error) -> Self {
        Self::Locktime(value.to_string())
    }
}

impl From<url::ParseError> for Error {
    fn from(value: url::ParseError) -> Self {
        Self::Url(value)
    }
}

impl From<tokio_tungstenite_wasm::Error> for Error {
    fn from(value: tokio_tungstenite_wasm::Error) -> Self {
        Self::WebSocket(value)
    }
}

impl From<bitcoin::taproot::TaprootError> for Error {
    fn from(value: bitcoin::taproot::TaprootError) -> Self {
        Self::Taproot(value.to_string())
    }
}

impl From<elements::taproot::TaprootError> for Error {
    fn from(value: elements::taproot::TaprootError) -> Self {
        Self::Taproot(value.to_string())
    }
}

impl From<elements::taproot::TaprootBuilderError> for Error {
    fn from(value: elements::taproot::TaprootBuilderError) -> Self {
        Self::Taproot(value.to_string())
    }
}

impl From<bitcoin::taproot::TaprootBuilderError> for Error {
    fn from(value: bitcoin::taproot::TaprootBuilderError) -> Self {
        Self::Taproot(value.to_string())
    }
}

impl From<elements::secp256k1_zkp::MusigTweakErr> for Error {
    fn from(value: elements::secp256k1_zkp::MusigTweakErr) -> Self {
        Self::Musig2(value.to_string())
    }
}

impl From<elements::secp256k1_zkp::MusigNonceGenError> for Error {
    fn from(value: elements::secp256k1_zkp::MusigNonceGenError) -> Self {
        Self::Musig2(value.to_string())
    }
}

impl From<elements::secp256k1_zkp::ParseError> for Error {
    fn from(value: elements::secp256k1_zkp::ParseError) -> Self {
        Self::Musig2(value.to_string())
    }
}

impl From<elements::secp256k1_zkp::MusigSignError> for Error {
    fn from(value: elements::secp256k1_zkp::MusigSignError) -> Self {
        Self::Musig2(value.to_string())
    }
}

impl From<bitcoin::consensus::encode::Error> for Error {
    fn from(value: bitcoin::consensus::encode::Error) -> Self {
        Self::BitcoinEncode(value)
    }
}

impl Error {
    // Returns the name of the enum variant as a string
    pub fn name(&self) -> String {
        match self {
            #[cfg(feature = "electrum")]
            #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
            Error::Electrum(_) => "Electrum",
            #[cfg(feature = "esplora")]
            Error::Esplora(_) => "Esplora",
            Error::Hex(_) => "Hex",
            Error::Protocol(_) => "Protocol",
            Error::Key(_) => "Key",
            Error::Address(_) => "Address",
            Error::Sighash(_) => "Sighash",
            Error::ElSighash(_) => "Elements-Sighash",
            Error::Secp(_) => "Secp",
            Error::HTTP(_) => "HTTP",
            Error::JSON(_) => "JSON",
            Error::IO(_) => "IO",
            Error::Bolt11(_) => "Bolt11",
            Error::LiquidEncode(_) => "LiquidEncode",
            Error::BitcoinEncode(_) => "BitcoinEncode",
            Error::Blind(_) => "Blind",
            Error::ConfidentialTx(_) => "ConfidentialTx",
            Error::BIP32(_) => "BIP32",
            Error::BIP39(_) => "BIP39",
            Error::Hash(_) => "Hash",
            Error::Locktime(_) => "Locktime",
            Error::Url(_) => "Url",
            Error::WebSocket(_) => "WebSocket",
            Error::Taproot(_) => "Taproot",
            Error::Musig2(_) => "Musig2",
            Error::Generic(_) => "Generic",
        }
        .to_string()
    }

    // Returns the error message as a string
    pub fn message(&self) -> String {
        match self {
            #[cfg(feature = "electrum")]
            #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
            Error::Electrum(e) => e.to_string(),
            #[cfg(feature = "esplora")]
            Error::Esplora(e) => e.clone(),
            Error::Hex(e) => e.clone(),
            Error::Protocol(e) => e.clone(),
            Error::Key(e) => e.to_string(),
            Error::Address(e) => e.clone(),
            Error::Sighash(e) => e.to_string(),
            Error::ElSighash(e) => e.to_string(),
            Error::Secp(e) => e.to_string(),
            Error::HTTP(e) => e.to_string(),
            Error::JSON(e) => e.to_string(),
            Error::IO(e) => e.to_string(),
            Error::Bolt11(e) => e.to_string(),
            Error::LiquidEncode(e) => e.to_string(),
            Error::BitcoinEncode(e) => e.to_string(),
            Error::Blind(e) => e.clone(),
            Error::ConfidentialTx(e) => e.to_string(),
            Error::BIP32(e) => e.to_string(),
            Error::BIP39(e) => e.to_string(),
            Error::Hash(e) => e.to_string(),
            Error::Locktime(e) => e.clone(),
            Error::Url(e) => e.to_string(),
            Error::WebSocket(e) => e.to_string(),
            Error::Taproot(e) => e.clone(),
            Error::Musig2(e) => e.clone(),
            Error::Generic(e) => e.clone(),
        }
    }
}
