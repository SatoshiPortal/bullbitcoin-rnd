use std::time::Duration;

use bitcoin::{key::rand::thread_rng, PublicKey};
use boltz_client::boltz::{
    BoltzApiClientV2, ChainSwapDetails, Cooperative, CreateChainRequest, Side, Subscription,
    SwapUpdate, BOLTZ_TESTNET_URL_V2,
};
use boltz_client::fees::Fee;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
#[cfg(feature = "electrum")]
use boltz_client::network::electrum::ElectrumConfig;
use boltz_client::network::esplora::EsploraConfig;
use boltz_client::network::{
    BitcoinClient, BitcoinNetworkConfig, LiquidClient, LiquidNetworkConfig,
};
use boltz_client::{
    network::Chain,
    util::{secrets::Preimage, setup_logger},
    BtcSwapScript, BtcSwapTx, Keypair, LBtcSwapScript, LBtcSwapTx, Secp256k1,
};
use elements::Address as EAddress;
use futures_util::{SinkExt, StreamExt};
use std::str::FromStr;
use tokio_tungstenite_wasm::Message;

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

#[macros::async_test]
#[cfg(feature = "electrum")]
#[ignore]
async fn bitcoin_liquid_v2_chain_electrum() {
    let bitcoin_network_config = ElectrumConfig::default_bitcoin();
    let liquid_network_config = ElectrumConfig::default_liquid();
    bitcoin_liquid_v2_chain(bitcoin_network_config, liquid_network_config).await
}

#[macros::async_test_all]
#[ignore]
async fn bitcoin_liquid_v2_chain_esplora() {
    let bitcoin_network_config = EsploraConfig::default_bitcoin();
    let liquid_network_config = EsploraConfig::default_liquid();
    bitcoin_liquid_v2_chain(bitcoin_network_config, liquid_network_config).await
}

async fn bitcoin_liquid_v2_chain<
    BC: BitcoinClient,
    BN: BitcoinNetworkConfig<BC>,
    LC: LiquidClient,
    LN: LiquidNetworkConfig<LC>,
>(
    bitcoin_network_config: BN,
    liquid_network_config: LN,
) {
    setup_logger();
    let network = Chain::BitcoinTestnet;
    let secp = Secp256k1::new();
    let preimage = Preimage::new();
    log::info!("{:#?}", preimage);
    let our_claim_keys = Keypair::new(&secp, &mut thread_rng());
    let claim_public_key = PublicKey {
        compressed: true,
        inner: our_claim_keys.public_key(),
    };

    let our_refund_keys = Keypair::new(&secp, &mut thread_rng());
    log::info!("Refund: {:#?}", our_refund_keys.display_secret());

    let refund_public_key = PublicKey {
        inner: our_refund_keys.public_key(),
        compressed: true,
    };

    let create_chain_req = CreateChainRequest {
        from: "BTC".to_string(),
        to: "L-BTC".to_string(),
        preimage_hash: preimage.sha256,
        claim_public_key: Some(claim_public_key),
        refund_public_key: Some(refund_public_key),
        referral_id: None,
        user_lock_amount: Some(1000000),
        server_lock_amount: None,
        pair_hash: None, // Add address signature here.
        webhook: None,
    };

    let boltz_api_v2 = BoltzApiClientV2::new(BOLTZ_TESTNET_URL_V2);

    let create_chain_response = boltz_api_v2.post_chain_req(create_chain_req).await.unwrap();
    let swap_id = create_chain_response.clone().id;
    let lockup_details: ChainSwapDetails = create_chain_response.clone().lockup_details;

    let lockup_script = BtcSwapScript::chain_from_swap_resp(
        Side::Lockup,
        lockup_details.clone(),
        refund_public_key,
    )
    .unwrap();
    log::debug!("Lockup Script: {:#?}", lockup_script);
    log::debug!(
        "Lockup Sender Pubkey: {:#?}",
        lockup_script.sender_pubkey.to_string()
    );
    log::debug!(
        "Lockup Receiver Pubkey: {:#?}",
        lockup_script.receiver_pubkey.to_string()
    );

    let lockup_address = lockup_script.clone().to_address(network).unwrap();
    assert_eq!(
        lockup_address.clone().to_string(),
        lockup_details.clone().lockup_address.to_string()
    );
    let refund_address = "tb1qra2cdypld3hyq3f84630cvj9d0lmzv66vn4k28".to_string();

    let claim_details: ChainSwapDetails = create_chain_response.claim_details;

    let claim_script =
        LBtcSwapScript::chain_from_swap_resp(Side::Claim, claim_details.clone(), claim_public_key)
            .unwrap();

    let claim_address = "tlq1qq0y3xudhc909fur3ktaws0yrhjv3ld9c2fk5hqzjfmgqurl0cy4z8yc8d9h54lj7ddwatzegwamyqhp4vttxj26wml4s9vecx".to_string();
    let lq_address = EAddress::from_str(&claim_address).unwrap();
    log::debug!("{:#?}", lq_address);
    // let claim_address = claim_script.to_address(network).unwrap();
    // assert_eq!(claim_address.to_string(), claim_details.claim_address.unwrap());
    let liquid_genesis_hash = liquid_network_config
        .build_liquid_client()
        .unwrap()
        .get_genesis_hash()
        .await
        .unwrap();
    log::debug!("{:#?}", liquid_genesis_hash);
    let (mut sender, mut receiver) = boltz_api_v2.connect_ws().await.unwrap().split();

    sender
        .send(Message::text(
            serde_json::to_string(&Subscription::new(&swap_id)).unwrap(),
        ))
        .await
        .unwrap();
    loop {
        let swap_id = swap_id.clone();

        let response =
            serde_json::from_str(&receiver.next().await.unwrap().unwrap().into_text().unwrap());

        if response.is_err() {
            if response.expect_err("Error in websocket respo").is_eof() {
                continue;
            }
        } else {
            match response.unwrap() {
                SwapUpdate::Subscription {
                    event,
                    channel,
                    args,
                } => {
                    assert!(event == "subscribe");
                    assert!(channel == "swap.update");
                    assert!(args.first().expect("expected") == &swap_id);
                    log::info!(
                        "Successfully subscribed for Swap updates. Swap ID : {}",
                        swap_id
                    );
                }

                SwapUpdate::Update {
                    event,
                    channel,
                    args,
                } => {
                    assert!(event == "update");
                    assert!(channel == "swap.update");
                    let update = args.first().expect("expected");
                    assert!(update.id == swap_id);
                    log::info!("Got Update from server: {}", update.status);

                    if update.status == "swap.created" {
                        log::info!(
                            "Send {} sats to BTC address {}",
                            create_chain_response.lockup_details.clone().amount,
                            create_chain_response.lockup_details.clone().lockup_address
                        );
                        log::info!(
                            "TO TRIGGER REFUND: Send 50,000 sats to BTC address {}",
                            create_chain_response.lockup_details.clone().lockup_address
                        );
                    }

                    if update.status == "transaction.server.confirmed" {
                        log::info!("Server lockup tx is confirmed!");

                        std::thread::sleep(Duration::from_secs(10));
                        log::info!("Claiming!");

                        let claim_tx = LBtcSwapTx::new_claim(
                            claim_script.clone(),
                            claim_address.clone(),
                            &liquid_network_config,
                            BOLTZ_TESTNET_URL_V2.to_string(),
                            swap_id.clone(),
                        )
                        .await
                        .unwrap();
                        let refund_tx = BtcSwapTx::new_refund(
                            lockup_script.clone(),
                            &refund_address,
                            &bitcoin_network_config,
                            BOLTZ_TESTNET_URL_V2.to_owned(),
                            swap_id.clone(),
                        )
                        .await
                        .unwrap();
                        let claim_tx_response = boltz_api_v2
                            .get_chain_claim_tx_details(&swap_id)
                            .await
                            .unwrap();
                        let (partial_sig, pub_nonce) = refund_tx
                            .partial_sign(
                                &our_refund_keys,
                                &claim_tx_response.pub_nonce,
                                &claim_tx_response.transaction_hash,
                            )
                            .unwrap();
                        let tx = claim_tx
                            .sign_claim(
                                &our_claim_keys,
                                &preimage,
                                Fee::Absolute(1000),
                                Some(Cooperative {
                                    boltz_api: &boltz_api_v2,
                                    swap_id: swap_id.clone(),
                                    pub_nonce: Some(pub_nonce),
                                    partial_sig: Some(partial_sig),
                                }),
                                false,
                            )
                            .await
                            .unwrap();

                        claim_tx
                            .broadcast(&tx, &liquid_network_config, None)
                            .await
                            .unwrap();

                        log::info!("Succesfully broadcasted claim tx!");
                    }

                    if update.status == "transaction.claimed" {
                        log::info!("Successfully completed chain swap");
                        break;
                    }

                    // This means the funding transaction was rejected by Boltz for whatever reason, and we need to get
                    // fund back via refund.
                    if update.status == "transaction.lockupFailed" {
                        std::thread::sleep(Duration::from_secs(10));
                        log::info!("REFUNDING!");
                        refund_bitcoin_liquid_v2_chain(
                            lockup_script.clone(),
                            refund_address.clone(),
                            swap_id.clone(),
                            our_refund_keys,
                            boltz_api_v2.clone(),
                            100,
                            &bitcoin_network_config,
                        )
                        .await;
                        log::info!("REFUNDING with higher fee");
                        refund_bitcoin_liquid_v2_chain(
                            lockup_script.clone(),
                            refund_address.clone(),
                            swap_id.clone(),
                            our_refund_keys,
                            boltz_api_v2.clone(),
                            1000,
                            &bitcoin_network_config,
                        )
                        .await;
                    }
                }

                SwapUpdate::Error {
                    event,
                    channel,
                    args,
                } => {
                    assert!(event == "update");
                    assert!(channel == "swap.update");
                    let error = args.first().expect("expected");
                    log::error!(
                        "Got Boltz response error : {} for swap: {}",
                        error.error,
                        error.id
                    );
                }
            }
        }
    }
}

async fn refund_bitcoin_liquid_v2_chain<BC: BitcoinClient, BN: BitcoinNetworkConfig<BC>>(
    lockup_script: BtcSwapScript,
    refund_address: String,
    swap_id: String,
    our_refund_keys: Keypair,
    boltz_api_v2: BoltzApiClientV2,
    absolute_fees: u64,
    bitcoin_network_config: &BN,
) {
    let refund_tx = BtcSwapTx::new_refund(
        lockup_script.clone(),
        &refund_address,
        bitcoin_network_config,
        BOLTZ_TESTNET_URL_V2.to_owned(),
        swap_id.clone(),
    )
    .await
    .unwrap();
    let tx = refund_tx
        .sign_refund(
            &our_refund_keys,
            Fee::Absolute(absolute_fees),
            Some(Cooperative {
                boltz_api: &boltz_api_v2,
                swap_id: swap_id.clone(),
                pub_nonce: None,
                partial_sig: None,
            }),
        )
        .await
        .unwrap();

    refund_tx
        .broadcast(&tx, bitcoin_network_config)
        .await
        .unwrap();

    log::info!("Successfully broadcasted refund tx!");
    log::debug!("Refund Tx {:?}", tx);
}

#[macros::async_test]
#[cfg(feature = "electrum")]
#[ignore]
async fn liquid_bitcoin_v2_chain_electrum() {
    let bitcoin_network_config = ElectrumConfig::default_bitcoin();
    let liquid_network_config = ElectrumConfig::default_liquid();
    liquid_bitcoin_v2_chain(bitcoin_network_config, liquid_network_config).await
}

#[macros::async_test_all]
#[ignore]
async fn liquid_bitcoin_v2_chain_esplora() {
    let bitcoin_network_config = EsploraConfig::default_bitcoin();
    let liquid_network_config = EsploraConfig::default_liquid();
    liquid_bitcoin_v2_chain(bitcoin_network_config, liquid_network_config).await
}

async fn liquid_bitcoin_v2_chain<
    BC: BitcoinClient,
    BN: BitcoinNetworkConfig<BC>,
    LC: LiquidClient,
    LN: LiquidNetworkConfig<LC>,
>(
    bitcoin_network_config: BN,
    liquid_network_config: LN,
) {
    setup_logger();
    let network = Chain::LiquidTestnet;
    let secp = Secp256k1::new();
    let preimage = Preimage::new();
    log::info!("{:#?}", preimage);
    let our_claim_keys = Keypair::new(&secp, &mut thread_rng());
    let claim_public_key = PublicKey {
        compressed: true,
        inner: our_claim_keys.public_key(),
    };

    let our_refund_keys = Keypair::new(&secp, &mut thread_rng());
    log::info!("Refund: {:#?}", our_refund_keys.display_secret());

    let refund_public_key = PublicKey {
        inner: our_refund_keys.public_key(),
        compressed: true,
    };

    let create_chain_req = CreateChainRequest {
        from: "L-BTC".to_string(),
        to: "BTC".to_string(),
        preimage_hash: preimage.sha256,
        claim_public_key: Some(claim_public_key),
        refund_public_key: Some(refund_public_key),
        referral_id: None,
        user_lock_amount: Some(1000000),
        server_lock_amount: None,
        pair_hash: None, // Add address signature here.
        webhook: None,
    };

    let boltz_api_v2 = BoltzApiClientV2::new(BOLTZ_TESTNET_URL_V2);

    let create_chain_response = boltz_api_v2.post_chain_req(create_chain_req).await.unwrap();
    let swap_id = create_chain_response.clone().id;
    let lockup_details: ChainSwapDetails = create_chain_response.clone().lockup_details;

    let lockup_script = LBtcSwapScript::chain_from_swap_resp(
        Side::Lockup,
        lockup_details.clone(),
        refund_public_key,
    )
    .unwrap();
    log::debug!("Lockup Script: {:#?}", lockup_script);
    log::debug!(
        "Lockup Sender Pubkey: {:#?}",
        lockup_script.sender_pubkey.to_string()
    );
    log::debug!(
        "Lockup Receiver Pubkey: {:#?}",
        lockup_script.receiver_pubkey.to_string()
    );
    log::debug!(
        "Lockup Blinding Key: {:#?}",
        lockup_script.blinding_key.display_secret()
    );

    let lockup_address = lockup_script.clone().to_address(network).unwrap();
    assert_eq!(
        lockup_address.clone().to_string(),
        lockup_details.clone().lockup_address.to_string()
    );
    let refund_address = "tlq1qq0y3xudhc909fur3ktaws0yrhjv3ld9c2fk5hqzjfmgqurl0cy4z8yc8d9h54lj7ddwatzegwamyqhp4vttxj26wml4s9vecx".to_string();

    let claim_details: ChainSwapDetails = create_chain_response.claim_details;

    let claim_script =
        BtcSwapScript::chain_from_swap_resp(Side::Claim, claim_details.clone(), claim_public_key)
            .unwrap();

    let claim_address = "tb1qra2cdypld3hyq3f84630cvj9d0lmzv66vn4k28".to_string();

    let (mut sender, mut receiver) = boltz_api_v2.connect_ws().await.unwrap().split();

    sender
        .send(Message::text(
            serde_json::to_string(&Subscription::new(&swap_id)).unwrap(),
        ))
        .await
        .unwrap();
    loop {
        let response =
            serde_json::from_str(&receiver.next().await.unwrap().unwrap().into_text().unwrap());

        if response.is_err() {
            if response.expect_err("Error in websocket respo").is_eof() {
                continue;
            }
        } else {
            match response.unwrap() {
                SwapUpdate::Subscription {
                    event,
                    channel,
                    args,
                } => {
                    assert!(event == "subscribe");
                    assert!(channel == "swap.update");
                    assert!(args.first().expect("expected") == &swap_id);
                    log::info!(
                        "Successfully subscribed for Swap updates. Swap ID : {}",
                        swap_id
                    );
                }

                SwapUpdate::Update {
                    event,
                    channel,
                    args,
                } => {
                    assert!(event == "update");
                    assert!(channel == "swap.update");
                    let update = args.first().expect("expected");
                    assert!(update.id == swap_id);
                    log::info!("Got Update from server: {}", update.status);

                    if update.status == "swap.created" {
                        log::info!(
                            "Send {} sats to L-BTC address {}",
                            create_chain_response.lockup_details.clone().amount,
                            create_chain_response.lockup_details.clone().lockup_address
                        );
                        log::info!(
                            "TO TRIGGER REFUND: Send 10,000 sats to L-BTC address {}",
                            create_chain_response.lockup_details.clone().lockup_address
                        );
                    }

                    if update.status == "transaction.server.confirmed" {
                        log::info!("Server lockup tx is confirmed!");

                        std::thread::sleep(Duration::from_secs(10));
                        log::info!("Claiming!");

                        let claim_tx = BtcSwapTx::new_claim(
                            claim_script.clone(),
                            claim_address.clone(),
                            &bitcoin_network_config,
                            BOLTZ_TESTNET_URL_V2.to_owned(),
                            swap_id.clone(),
                        )
                        .await
                        .unwrap();
                        let refund_tx = LBtcSwapTx::new_refund(
                            lockup_script.clone(),
                            &refund_address,
                            &liquid_network_config,
                            BOLTZ_TESTNET_URL_V2.to_string(),
                            swap_id.clone(),
                        )
                        .await
                        .unwrap();
                        let claim_tx_response = boltz_api_v2
                            .get_chain_claim_tx_details(&swap_id)
                            .await
                            .unwrap();
                        let (partial_sig, pub_nonce) = refund_tx
                            .partial_sign(
                                &our_refund_keys,
                                &claim_tx_response.pub_nonce,
                                &claim_tx_response.transaction_hash,
                            )
                            .unwrap();
                        let tx = claim_tx
                            .sign_claim(
                                &our_claim_keys,
                                &preimage,
                                Fee::Absolute(1000),
                                Some(Cooperative {
                                    boltz_api: &boltz_api_v2,
                                    swap_id: swap_id.clone(),
                                    pub_nonce: Some(pub_nonce),
                                    partial_sig: Some(partial_sig),
                                }),
                            )
                            .await
                            .unwrap();

                        claim_tx
                            .broadcast(&tx, &bitcoin_network_config)
                            .await
                            .unwrap();

                        log::info!("Succesfully broadcasted claim tx!");
                    }

                    if update.status == "transaction.claimed" {
                        log::info!("Successfully completed chain swap");
                        break;
                    }

                    // This means the funding transaction was rejected by Boltz for whatever reason, and we need to get
                    // fund back via refund.
                    if update.status == "transaction.lockupFailed" {
                        log::info!("REFUNDING!");
                        let refund_tx = LBtcSwapTx::new_refund(
                            lockup_script.clone(),
                            &refund_address,
                            &liquid_network_config,
                            BOLTZ_TESTNET_URL_V2.to_string(),
                            swap_id.clone(),
                        )
                        .await
                        .unwrap();
                        let tx = refund_tx
                            .sign_refund(
                                &our_refund_keys,
                                Fee::Absolute(1000),
                                Some(Cooperative {
                                    boltz_api: &boltz_api_v2,
                                    swap_id: swap_id.clone(),
                                    pub_nonce: None,
                                    partial_sig: None,
                                }),
                                false,
                            )
                            .await
                            .unwrap();

                        refund_tx
                            .broadcast(&tx, &liquid_network_config, None)
                            .await
                            .unwrap();

                        log::info!("Succesfully broadcasted claim tx!");
                        log::debug!("Claim Tx {:?}", tx);
                    }
                }

                SwapUpdate::Error {
                    event,
                    channel,
                    args,
                } => {
                    assert!(event == "update");
                    assert!(channel == "swap.update");
                    let error = args.first().expect("expected");
                    log::error!(
                        "Got Boltz response error : {} for swap: {}",
                        error.error,
                        error.id
                    );
                }
            }
        }
    }
}
