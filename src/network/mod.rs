use crate::error::Error;

#[cfg(feature = "electrum")]
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub mod electrum;

#[cfg(feature = "esplora")]
pub mod esplora;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chain {
    Bitcoin,
    BitcoinTestnet,
    BitcoinRegtest,
    Liquid,
    LiquidTestnet,
    LiquidRegtest,
}

pub trait BitcoinNetworkConfig<BC: BitcoinClient> {
    fn build_bitcoin_client(&self) -> Result<BC, Error>;

    fn network(&self) -> Chain;
}

pub trait LiquidNetworkConfig<LC: LiquidClient> {
    fn build_liquid_client(&self) -> Result<LC, Error>;

    fn network(&self) -> Chain;
}

#[macros::async_trait]
pub trait BitcoinClient {
    async fn get_address_balance(&self, address: &bitcoin::Address) -> Result<(u64, i64), Error>;

    async fn get_address_utxos(
        &self,
        address: &bitcoin::Address,
    ) -> Result<Vec<(bitcoin::OutPoint, bitcoin::TxOut)>, Error>;

    async fn broadcast_tx(&self, signed_tx: &bitcoin::Transaction) -> Result<bitcoin::Txid, Error>;
}

#[macros::async_trait]
pub trait LiquidClient {
    async fn get_address_utxo(
        &self,
        address: &elements::Address,
    ) -> Result<(elements::OutPoint, elements::TxOut), Error>;

    async fn get_genesis_hash(&self) -> Result<elements::BlockHash, Error>;

    async fn broadcast_tx(&self, signed_tx: &elements::Transaction) -> Result<String, Error>;
}
