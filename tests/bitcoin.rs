#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
#[cfg(feature = "electrum")]
use boltz_client::network::electrum::ElectrumConfig;
use boltz_client::network::esplora::EsploraConfig;
use std::{str::FromStr, time::Duration};

use boltz_client::{
    network::Chain,
    swaps::{
        boltz::{
            BoltzApiClientV2, Cooperative, CreateReverseRequest, CreateSubmarineRequest,
            Subscription, SwapUpdate, BOLTZ_TESTNET_URL_V2,
        },
        magic_routing::{check_for_mrh, sign_address},
    },
    util::{secrets::Preimage, setup_logger},
    Bolt11Invoice, BtcSwapScript, BtcSwapTx, Secp256k1,
};

use bitcoin::{
    hashes::{sha256, Hash},
    hex::FromHex,
    key::rand::thread_rng,
    secp256k1::Keypair,
    PublicKey,
};
use boltz_client::fees::Fee;
use boltz_client::network::{BitcoinClient, BitcoinNetworkConfig};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite_wasm::Message;

pub mod test_utils;

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

#[macros::async_test]
#[cfg(feature = "electrum")]
#[ignore = "Requires testnet invoice and refund address"]
async fn bitcoin_v2_submarine_electrum() {
    let bitcoin_network_config = ElectrumConfig::default_bitcoin();
    bitcoin_v2_submarine(bitcoin_network_config).await
}

#[macros::async_test_all]
#[ignore = "Requires testnet invoice and refund address"]

async fn bitcoin_v2_submarine_esplora() {
    let bitcoin_network_config = EsploraConfig::default_bitcoin();
    bitcoin_v2_submarine(bitcoin_network_config).await
}

async fn bitcoin_v2_submarine<BC: BitcoinClient, BN: BitcoinNetworkConfig<BC>>(
    bitcoin_network_config: BN,
) {
    setup_logger();

    let secp = bitcoin::secp256k1::Secp256k1::new();
    let our_keys = Keypair::new(&secp, &mut thread_rng());

    let refund_public_key = PublicKey {
        inner: our_keys.public_key(),
        compressed: true,
    };

    // Set a new invoice string and refund address for each test.
    let invoice = "lntb5125180n1pnwmtnvpp5rt2ptzc4329nr8f9qnaeg8nszs2w40rr922wnqe6uw6rt6sfz0escqpjsp5lcvu233r6wmd6tvqvpnwpe97tjwzh0kygtjz9htw2y7j6h5grgkq9q7sqqqqqqqqqqqqqqqqqqqsqqqqqysgqdqqmqz9gxqyjw5qrzjqwfn3p9278ttzzpe0e00uhyxhned3j5d9acqak5emwfpflp8z2cnflctr6qq3f9n3gqqqqlgqqqqqeqqjqmrtu79yvjazp5tcn6nscf27arhevexq64yd0jjmkc8hxlqkh5ywzwk209xvmf484uutvjqv5rtgq0aulm9e4al72wwljm97a3vdcgxcq4vcmxq".to_string();
    let refund_address = "tb1qq20a7gqewc0un9mxxlqyqwn7ut7zjrj9y3d0mu".to_string();

    let boltz_api_v2 = BoltzApiClientV2::new(BOLTZ_TESTNET_URL_V2);

    // If there is MRH send directly to that address
    //    let (bip21_addrs, amount) =
    //         check_for_mrh(&boltz_api_v2, &invoice, Chain::BitcoinTestnet).unwrap();
    //         log::info!("Found MRH in invoice");
    //         log::info!("Send {} to {}", amount, bip21_addrs);
    //         return;

    // Initiate the swap with Boltz
    let create_swap_req = CreateSubmarineRequest {
        from: "BTC".to_string(),
        to: "BTC".to_string(),
        invoice: invoice.to_string(),
        refund_public_key,
        pair_hash: None,
        referral_id: None,
        webhook: None,
    };

    let create_swap_response = boltz_api_v2.post_swap_req(&create_swap_req).await.unwrap();

    log::info!("Got Swap Response from Boltz server");

    log::debug!("Swap Response: {:?}", create_swap_response);

    let swap_script =
        BtcSwapScript::submarine_from_swap_resp(&create_swap_response, refund_public_key).unwrap();
    let swap_id = create_swap_response.id.clone();
    log::debug!("Created Swap Script. : {:?}", swap_script);

    // Subscribe to websocket updates
    let (mut sender, mut receiver) = boltz_api_v2.connect_ws().await.unwrap().split();

    sender
        .send(Message::text(
            serde_json::to_string(&Subscription::new(&swap_id)).unwrap(),
        ))
        .await
        .unwrap();

    // Event handlers for various swap status.
    loop {
        let swap_id = &swap_id.clone();

        let response =
            serde_json::from_str(&receiver.next().await.unwrap().unwrap().into_text().unwrap());

        if response.is_err() {
            if response.expect_err("expected").is_eof() {
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
                    assert!(args.first().expect("expected") == swap_id);
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
                    assert!(update.id == *swap_id);
                    log::info!("Got Update from server: {}", update.status);

                    // Invoice is Set. Waiting for us to send onchain tx.
                    if update.status == "invoice.set" {
                        log::info!(
                            "Send {} sats to BTC address {}",
                            create_swap_response.expected_amount,
                            create_swap_response.address
                        );

                        // Test Cooperative Refund.
                        // Send 1 sat less to than expected amount to Boltz, and let Boltz fail the swap.
                    }

                    // Boltz has paid the invoice, and waiting for our partial sig.
                    if update.status == "transaction.claim.pending" {
                        // Create the refund transaction at this stage
                        // This will fail if the funding transaction isn't confirmed yet. Which should not happen.
                        let swap_tx = BtcSwapTx::new_refund(
                            swap_script.clone(),
                            &refund_address,
                            &bitcoin_network_config,
                            BOLTZ_TESTNET_URL_V2.to_owned(),
                            swap_id.to_owned(),
                        )
                        .await
                        .expect("Funding UTXO not found");

                        let claim_tx_response = boltz_api_v2
                            .get_submarine_claim_tx_details(swap_id)
                            .await
                            .unwrap();

                        log::debug!("Received claim tx details : {:?}", claim_tx_response);

                        // Check that boltz have the correct preimage.
                        // At this stage the client should verify that LN invoice has been paid.
                        let preimage = Vec::from_hex(&claim_tx_response.preimage).unwrap();
                        let preimage_hash = sha256::Hash::hash(&preimage);
                        let invoice = Bolt11Invoice::from_str(&create_swap_req.invoice).unwrap();
                        let invoice_payment_hash = invoice.payment_hash();
                        assert!(invoice_payment_hash.to_string() == preimage_hash.to_string());
                        log::info!("Correct Hash preimage received from Boltz.");

                        // Compute and send Musig2 partial sig
                        let (partial_sig, pub_nonce) = swap_tx
                            .partial_sign(
                                &our_keys,
                                &claim_tx_response.pub_nonce,
                                &claim_tx_response.transaction_hash,
                            )
                            .unwrap();
                        boltz_api_v2
                            .post_submarine_claim_tx_details(swap_id, pub_nonce, partial_sig)
                            .await
                            .unwrap();
                        log::info!("Successfully Sent partial signature");
                    }

                    if update.status == "transaction.claimed" {
                        log::info!("Successfully completed submarine swap");
                        break;
                    }

                    // This means the funding transaction was rejected by Boltz for whatever reason, and we need to get
                    // the funds back via refund.
                    if update.status == "transaction.lockupFailed"
                        || update.status == "invoice.failedToPay"
                    {
                        let swap_tx = BtcSwapTx::new_refund(
                            swap_script.clone(),
                            &refund_address,
                            &bitcoin_network_config,
                            BOLTZ_TESTNET_URL_V2.to_owned(),
                            swap_id.to_owned(),
                        )
                        .await
                        .expect("Funding UTXO not found");

                        match swap_tx
                            .sign_refund(
                                &our_keys,
                                Fee::Absolute(1000),
                                Some(Cooperative {
                                    boltz_api: &boltz_api_v2,
                                    swap_id: swap_id.clone(),
                                    pub_nonce: None,
                                    partial_sig: None,
                                }),
                            )
                            .await
                        {
                            Ok(tx) => {
                                let txid = swap_tx
                                    .broadcast(&tx, &bitcoin_network_config)
                                    .await
                                    .unwrap();
                                log::info!("Cooperative Refund Successfully broadcasted: {}", txid);
                            }
                            Err(e) => {
                                log::info!("Cooperative refund failed. {:?}", e);
                                log::info!("Attempting Non-cooperative refund.");

                                let tx = swap_tx
                                    .sign_refund(&our_keys, Fee::Absolute(1000), None)
                                    .await
                                    .unwrap();
                                let txid = swap_tx
                                    .broadcast(&tx, &bitcoin_network_config)
                                    .await
                                    .unwrap();
                                log::info!(
                                    "Non-cooperative Refund Successfully broadcasted: {}",
                                    txid
                                );
                            }
                        }
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

#[macros::async_test]
#[cfg(feature = "electrum")]
#[ignore = "Requires testnet invoice and refund address"]
async fn bitcoin_v2_reverse_electrum() {
    let bitcoin_network_config = ElectrumConfig::default_bitcoin();
    bitcoin_v2_reverse(bitcoin_network_config).await
}

#[macros::async_test_all]
#[ignore = "Requires testnet invoice and refund address"]
async fn bitcoin_v2_reverse_esplora() {
    let bitcoin_network_config = EsploraConfig::default_bitcoin();
    bitcoin_v2_reverse(bitcoin_network_config).await
}

async fn bitcoin_v2_reverse<BC: BitcoinClient, BN: BitcoinNetworkConfig<BC>>(
    bitcoin_network_config: BN,
) {
    setup_logger();

    let secp = Secp256k1::new();
    let preimage = Preimage::new();
    let our_keys = Keypair::new(&secp, &mut thread_rng());
    let invoice_amount = 100000;
    let claim_public_key = PublicKey {
        compressed: true,
        inner: our_keys.public_key(),
    };

    // Give a valid claim address or else funds will be lost.
    let claim_address = "tb1qq20a7gqewc0un9mxxlqyqwn7ut7zjrj9y3d0mu".to_string();

    let addrs_sig = sign_address(&claim_address, &our_keys).unwrap();
    let create_reverse_req = CreateReverseRequest {
        invoice_amount,
        from: "BTC".to_string(),
        to: "BTC".to_string(),
        preimage_hash: preimage.sha256,
        description: None,
        description_hash: None,
        address_signature: Some(addrs_sig.to_string()),
        address: Some(claim_address.clone()),
        claim_public_key,
        referral_id: None, // Add address signature here.
        webhook: None,
    };

    let boltz_api_v2 = BoltzApiClientV2::new(BOLTZ_TESTNET_URL_V2);

    let reverse_resp = boltz_api_v2
        .post_reverse_req(create_reverse_req)
        .await
        .unwrap();

    let _ = check_for_mrh(&boltz_api_v2, &reverse_resp.invoice, Chain::BitcoinTestnet)
        .await
        .unwrap()
        .unwrap();

    log::debug!("Got Reverse swap response: {:?}", reverse_resp);

    let swap_script =
        BtcSwapScript::reverse_from_swap_resp(&reverse_resp, claim_public_key).unwrap();
    let swap_id = reverse_resp.id.clone();
    // Subscribe to wss status updates
    let (mut sender, mut receiver) = boltz_api_v2.connect_ws().await.unwrap().split();

    sender
        .send(Message::text(
            serde_json::to_string(&Subscription::new(&swap_id)).unwrap(),
        ))
        .await
        .unwrap();

    // Event handlers for various swap status.
    loop {
        let swap_id = reverse_resp.id.clone();
        let response =
            serde_json::from_str(&receiver.next().await.unwrap().unwrap().into_text().unwrap());
        if response.is_err() {
            if response.expect_err("expected").is_eof() {
                continue;
            }
        } else {
            match response.as_ref().unwrap() {
                SwapUpdate::Subscription {
                    event,
                    channel,
                    args,
                } => {
                    assert!(event == "subscribe");
                    assert!(channel == "swap.update");
                    assert!(args.first().expect("expected") == &swap_id);
                    log::info!("Subscription successful for swap : {}", &swap_id);
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
                        log::info!("Waiting for Invoice to be paid: {}", &reverse_resp.invoice);
                        continue;
                    }

                    if update.status == "transaction.mempool" {
                        log::info!("Boltz broadcasted funding tx");

                        std::thread::sleep(Duration::from_secs(15));

                        let claim_tx = BtcSwapTx::new_claim(
                            swap_script.clone(),
                            claim_address.clone(),
                            &bitcoin_network_config,
                            BOLTZ_TESTNET_URL_V2.to_owned(),
                            swap_id.clone(),
                        )
                        .await
                        .expect("Funding tx expected");

                        let tx = claim_tx
                            .sign_claim(
                                &our_keys,
                                &preimage,
                                Fee::Absolute(1000),
                                Some(Cooperative {
                                    boltz_api: &boltz_api_v2,
                                    swap_id: swap_id.clone(),
                                    pub_nonce: None,
                                    partial_sig: None,
                                }),
                            )
                            .await
                            .unwrap();

                        claim_tx
                            .broadcast(&tx, &bitcoin_network_config)
                            .await
                            .unwrap();

                        log::info!("Successfully broadcasted claim tx!");
                        log::debug!("Claim Tx {:?}", tx);
                    }

                    if update.status == "invoice.settled" {
                        log::info!("Reverse Swap Successful!");
                        break;
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
                    println!("Got error : {} for swap: {}", error.error, error.id);
                }
            }
        }
    }
}

#[macros::async_test]
#[cfg(feature = "electrum")]
#[ignore = "Requires testnet invoice and refund address"]
async fn bitcoin_v2_reverse_script_path_electrum() {
    let bitcoin_network_config = ElectrumConfig::default_bitcoin();
    bitcoin_v2_reverse_script_path(bitcoin_network_config).await
}

#[macros::async_test_all]
#[ignore = "Requires testnet invoice and refund address"]
async fn bitcoin_v2_reverse_script_path_esplora() {
    let bitcoin_network_config = EsploraConfig::default_bitcoin();
    bitcoin_v2_reverse_script_path(bitcoin_network_config).await
}

async fn bitcoin_v2_reverse_script_path<BC: BitcoinClient, BN: BitcoinNetworkConfig<BC>>(
    bitcoin_network_config: BN,
) {
    setup_logger();

    let secp = Secp256k1::new();
    let preimage = Preimage::new();
    let our_keys = Keypair::new(&secp, &mut thread_rng());
    let invoice_amount = 100000;
    let claim_public_key = PublicKey {
        compressed: true,
        inner: our_keys.public_key(),
    };

    // Give a valid claim address or else funds will be lost.
    let claim_address = "tb1qq20a7gqewc0un9mxxlqyqwn7ut7zjrj9y3d0mu".to_string();

    let addrs_sig = sign_address(&claim_address, &our_keys).unwrap();
    let create_reverse_req = CreateReverseRequest {
        invoice_amount,
        from: "BTC".to_string(),
        to: "BTC".to_string(),
        preimage_hash: preimage.sha256,
        description: None,
        description_hash: None,
        address_signature: Some(addrs_sig.to_string()),
        address: Some(claim_address.clone()),
        claim_public_key,
        referral_id: None, // Add address signature here.
        webhook: None,
    };

    let boltz_api_v2 = BoltzApiClientV2::new(BOLTZ_TESTNET_URL_V2);

    let reverse_resp = boltz_api_v2
        .post_reverse_req(create_reverse_req)
        .await
        .unwrap();
    let swap_id = reverse_resp.id.clone();
    let _ = check_for_mrh(&boltz_api_v2, &reverse_resp.invoice, Chain::BitcoinTestnet)
        .await
        .unwrap()
        .unwrap();

    log::debug!("Got Reverse swap response: {:?}", reverse_resp);

    let swap_script =
        BtcSwapScript::reverse_from_swap_resp(&reverse_resp, claim_public_key).unwrap();

    // Subscribe to wss status updates
    let (mut sender, mut receiver) = boltz_api_v2.connect_ws().await.unwrap().split();

    sender
        .send(Message::text(
            serde_json::to_string(&Subscription::new(&swap_id)).unwrap(),
        ))
        .await
        .unwrap();

    // Event handlers for various swap status.
    loop {
        let swap_id = reverse_resp.id.clone();

        let response =
            serde_json::from_str(&receiver.next().await.unwrap().unwrap().into_text().unwrap());

        if response.is_err() {
            if response.expect_err("expected").is_eof() {
                continue;
            }
        } else {
            match response.as_ref().unwrap() {
                SwapUpdate::Subscription {
                    event,
                    channel,
                    args,
                } => {
                    assert!(event == "subscribe");
                    assert!(channel == "swap.update");
                    assert!(args.first().expect("expected") == &swap_id);
                    log::info!("Subscription successful for swap : {}", &swap_id);
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
                        log::info!("Waiting for Invoice to be paid: {}", &reverse_resp.invoice);
                        continue;
                    }

                    if update.status == "transaction.mempool" {
                        log::info!("Boltz broadcasted funding tx");

                        std::thread::sleep(Duration::from_secs(15));

                        let claim_tx = BtcSwapTx::new_claim(
                            swap_script.clone(),
                            claim_address.clone(),
                            &bitcoin_network_config,
                            BOLTZ_TESTNET_URL_V2.to_owned(),
                            swap_id,
                        )
                        .await
                        .expect("Funding tx expected");

                        let tx = claim_tx
                            .sign_claim(&our_keys, &preimage, Fee::Absolute(1000), None)
                            .await
                            .unwrap();

                        claim_tx
                            .broadcast(&tx, &bitcoin_network_config)
                            .await
                            .unwrap();

                        log::info!("Successfully broadcasted claim tx!");
                        log::debug!("Claim Tx {:?}", tx);
                    }

                    if update.status == "invoice.settled" {
                        log::info!("Reverse Swap Successful!");
                        break;
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
                    println!("Got error : {} for swap: {}", error.error, error.id);
                }
            }
        }
    }
}
