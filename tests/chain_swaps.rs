use std::time::Duration;

use bitcoin::{key::rand::thread_rng, PublicKey};
use boltz_client::boltz::{
    BoltzApiClientV2, ChainSwapDetails, Cooperative, CreateChainRequest, Side, Subscription,
    SwapUpdate, BOLTZ_TESTNET_URL_V2,
};
use boltz_client::fees::Fee;
use boltz_client::{
    network::{electrum::ElectrumConfig, Chain},
    util::{liquid_genesis_hash, secrets::Preimage, setup_logger},
    BtcSwapScript, BtcSwapTx, Keypair, LBtcSwapScript, LBtcSwapTx, Secp256k1,
};
use elements::Address as EAddress;
use std::str::FromStr;

#[test]
#[ignore]
fn bitcoin_liquid_v2_chain() {
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

    let create_chain_response = boltz_api_v2.post_chain_req(create_chain_req).unwrap();
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
    let liquid_genesis_hash = liquid_genesis_hash(&ElectrumConfig::default_liquid()).unwrap();
    log::debug!("{:#?}", liquid_genesis_hash);
    let mut socket = boltz_api_v2.connect_ws().unwrap();

    socket
        .send(tungstenite::Message::Text(
            serde_json::to_string(&Subscription::new(&swap_id)).unwrap(),
        ))
        .unwrap();
    loop {
        let swap_id = swap_id.clone();

        let response = serde_json::from_str(&socket.read().unwrap().to_string());

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
                            &ElectrumConfig::default_liquid(),
                            BOLTZ_TESTNET_URL_V2.to_string(),
                            swap_id.clone(),
                        )
                        .unwrap();
                        let refund_tx = BtcSwapTx::new_refund(
                            lockup_script.clone(),
                            &refund_address,
                            &ElectrumConfig::default_bitcoin(),
                            BOLTZ_TESTNET_URL_V2.to_owned(),
                            swap_id.clone(),
                        )
                        .unwrap();
                        let claim_tx_response =
                            boltz_api_v2.get_chain_claim_tx_details(&swap_id).unwrap();
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
                            .unwrap();

                        claim_tx
                            .broadcast(&tx, &ElectrumConfig::default_liquid(), None)
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
                        );
                        log::info!("REFUNDING with higher fee");
                        refund_bitcoin_liquid_v2_chain(
                            lockup_script.clone(),
                            refund_address.clone(),
                            swap_id.clone(),
                            our_refund_keys,
                            boltz_api_v2.clone(),
                            1000,
                        );
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

fn refund_bitcoin_liquid_v2_chain(
    lockup_script: BtcSwapScript,
    refund_address: String,
    swap_id: String,
    our_refund_keys: Keypair,
    boltz_api_v2: BoltzApiClientV2,
    absolute_fees: u64,
) {
    let refund_tx = BtcSwapTx::new_refund(
        lockup_script.clone(),
        &refund_address,
        &ElectrumConfig::default_bitcoin(),
        BOLTZ_TESTNET_URL_V2.to_owned(),
        swap_id.clone(),
    )
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
        .unwrap();

    refund_tx
        .broadcast(&tx, &ElectrumConfig::default_bitcoin())
        .unwrap();

    log::info!("Succesfully broadcasted claim tx!");
    log::debug!("Claim Tx {:?}", tx);
}

#[test]
#[ignore]
fn liquid_bitcoin_v2_chain() {
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

    let create_chain_response = boltz_api_v2.post_chain_req(create_chain_req).unwrap();
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

    let mut socket = boltz_api_v2.connect_ws().unwrap();

    socket
        .send(tungstenite::Message::Text(
            serde_json::to_string(&Subscription::new(&swap_id)).unwrap(),
        ))
        .unwrap();
    loop {
        let response = serde_json::from_str(&socket.read().unwrap().to_string());

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
                            &ElectrumConfig::default_bitcoin(),
                            BOLTZ_TESTNET_URL_V2.to_owned(),
                            swap_id.clone(),
                        )
                        .unwrap();
                        let refund_tx = LBtcSwapTx::new_refund(
                            lockup_script.clone(),
                            &refund_address,
                            &ElectrumConfig::default_liquid(),
                            BOLTZ_TESTNET_URL_V2.to_string(),
                            swap_id.clone(),
                        )
                        .unwrap();
                        let claim_tx_response =
                            boltz_api_v2.get_chain_claim_tx_details(&swap_id).unwrap();
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
                            .unwrap();

                        claim_tx
                            .broadcast(&tx, &ElectrumConfig::default_bitcoin())
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
                            &ElectrumConfig::default_liquid(),
                            BOLTZ_TESTNET_URL_V2.to_string(),
                            swap_id.clone(),
                        )
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
                            .unwrap();

                        refund_tx
                            .broadcast(&tx, &ElectrumConfig::default_liquid(), None)
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
