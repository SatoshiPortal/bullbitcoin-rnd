#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
#[cfg(feature = "electrum")]
use boltz_client::network::electrum::ElectrumConfig;
#[cfg(feature = "esplora")]
use boltz_client::network::esplora::EsploraConfig;
use std::{str::FromStr, time::Duration};

use boltz_client::{
    network::Chain,
    swaps::{
        boltz::{
            BoltzApiClientV2, Cooperative, CreateReverseRequest, CreateSubmarineRequest,
            Subscription, SwapType, SwapUpdate, BOLTZ_MAINNET_URL_V2, BOLTZ_TESTNET_URL_V2,
        },
        magic_routing::{check_for_mrh, sign_address},
    },
    util::{secrets::Preimage, setup_logger},
    Bolt11Invoice, Hash as BCHash, LBtcSwapScript, LBtcSwapTx, Secp256k1, Serialize,
};

use bitcoin::{
    hashes::{sha256, Hash},
    hex::{DisplayHex, FromHex},
    key::rand::thread_rng,
    secp256k1::Keypair,
    PublicKey,
};
use boltz_client::fees::Fee;
use boltz_client::network::{LiquidClient, LiquidNetworkConfig};
use elements::encode::serialize;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite_wasm::Message;

pub mod test_utils;

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

#[macros::async_test]
#[cfg(feature = "electrum")]
#[ignore = "Requires testnet invoice and refund address"]
async fn liquid_v2_submarine_electrum() {
    let liquid_network_config = ElectrumConfig::default_liquid();
    liquid_v2_submarine(liquid_network_config).await
}

#[macros::async_test_all]
#[cfg(feature = "esplora")]
#[ignore = "Requires testnet invoice and refund address"]
async fn liquid_v2_submarine_esplora() {
    let liquid_network_config = EsploraConfig::default_liquid();
    liquid_v2_submarine(liquid_network_config).await
}

async fn liquid_v2_submarine<LC: LiquidClient, LN: LiquidNetworkConfig<LC>>(
    liquid_network_config: LN,
) {
    setup_logger();

    let secp = Secp256k1::new();
    let our_keys = Keypair::new(&secp, &mut thread_rng());
    let refund_public_key = PublicKey {
        inner: our_keys.public_key(),
        compressed: true,
    };

    // Set a new invoice string and refund address for each test.
    let invoice = "lnbc12320n1pn9q9hlpp5v30gg9ylrpkgyn6pctthd22xvu0lsuctw9nee7t3emljvh5ty2nscqpjsp54p8gweqlqdnstedcmzy8ktgup4auaq6wy6lcryu0085kvr6a77gs9q7sqqqqqqqqqqqqqqqqqqqsqqqqqysgqdqqmqz9gxqyjw5qrzjqwryaup9lh50kkranzgcdnn2fgvx390wgj5jd07rwr3vxeje0glcllard4vsfze0gsqqqqlgqqqqqeqqjq3c4qzawwh62kzj3cdykcaszjd9l4wfcwlxhq4afwhvsjllu27pen26rsxaa0gfx602nl7feh87c4s39n5p47lfsu2k38vgfjc8nvhrspg50t63".to_string();
    let refund_address = "tlq1qq05cvratqvsfmvdkag7rat2sav97f2aq3x9mzqhcgavygjnnetgy57zhurzvwwklexrxtdecw0ejfgwcjsxvqmrptdf25vrj8".to_string();
    let boltz_url = BOLTZ_TESTNET_URL_V2;
    let chain = Chain::Liquid;
    let boltz_api_v2 = BoltzApiClientV2::new(boltz_url);

    // If there is MRH send directly to that address
    // if let Some((bip21_addrs, amount)) =
    //     check_for_mrh(&boltz_api_v2, &invoice, Chain::BitcoinTestnet).unwrap()
    // {
    //     log::info!("Found MRH in invoice");
    //     log::info!("Send {} to {}", amount, bip21_addrs);
    //     return;
    // }
    // Initiate the swap with Boltz
    let create_swap_req = CreateSubmarineRequest {
        from: "L-BTC".to_string(),
        to: "BTC".to_string(),
        invoice: invoice.to_string(),
        refund_public_key,
        pair_hash: None,
        referral_id: None,
        webhook: None,
    };

    let create_swap_response = boltz_api_v2.post_swap_req(&create_swap_req).await.unwrap();
    log::info!("Got Swap Response from Boltz server");

    create_swap_response
        .validate(&invoice, &refund_public_key, chain)
        .unwrap();
    log::info!("VALIDATED RESPONSE!");

    log::debug!("Swap Response: {:?}", create_swap_response);

    let swap_script =
        LBtcSwapScript::submarine_from_swap_resp(&create_swap_response, refund_public_key).unwrap();
    swap_script.to_address(chain).unwrap();

    log::debug!("Created Swap Script. : {:?}", swap_script);

    // Subscribe to websocket updates
    let (mut sender, mut receiver) = boltz_api_v2.connect_ws().await.unwrap().split();

    sender
        .send(Message::text(
            serde_json::to_string(&Subscription::new(&create_swap_response.id)).unwrap(),
        ))
        .await
        .unwrap();

    // Event handlers for various swap status.
    loop {
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
                    assert!(args.first().expect("expected") == &create_swap_response.clone().id);
                    log::info!(
                        "Successfully subscribed for Swap updates. Swap ID : {}",
                        create_swap_response.clone().id
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
                    assert!(update.id == create_swap_response.clone().id);
                    log::info!("Got Update from server: {}", update.status);

                    // Invoice is Set. Waiting for us to send onchain tx.
                    if update.status == "invoice.set" {
                        log::info!(
                            "Send {} sats to BTC address {}",
                            create_swap_response.expected_amount,
                            create_swap_response.address
                        );
                    }

                    // Boltz has paid the invoice, and waiting for our partial sig.
                    if update.status == "transaction.claim.pending" {
                        // Create the refund transaction at this stage
                        let swap_tx = LBtcSwapTx::new_refund(
                            swap_script.clone(),
                            &refund_address,
                            &liquid_network_config,
                            boltz_url.to_string(),
                            create_swap_response.clone().id,
                        )
                        .await
                        .unwrap();
                        // why? ^^^s

                        let claim_tx_response = boltz_api_v2
                            .get_submarine_claim_tx_details(&create_swap_response.clone().id)
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
                            .post_submarine_claim_tx_details(
                                &create_swap_response.clone().id,
                                pub_nonce,
                                partial_sig,
                            )
                            .await
                            .unwrap();
                        log::info!("Successfully Sent partial signature");
                    }

                    // This means the funding transaction was rejected by Boltz for whatever reason, and we need to get
                    // fund back via refund.
                    if update.status == "transaction.lockupFailed"
                        || update.status == "invoice.failedToPay"
                    {
                        let swap_tx = LBtcSwapTx::new_refund(
                            swap_script.clone(),
                            &refund_address,
                            &liquid_network_config,
                            boltz_url.to_string(),
                            create_swap_response.clone().id,
                        )
                        .await
                        .unwrap();

                        match swap_tx
                            .sign_refund(
                                &our_keys,
                                Fee::Absolute(1000),
                                None,
                                // Some(Cooperative {
                                //     boltz_api: &boltz_api_v2,
                                //     swap_id: create_swap_response.id.clone(),
                                //     pub_nonce: None,
                                //     partial_sig: None,
                                // }),
                                false,
                            )
                            .await
                        {
                            Ok(tx) => {
                                println!("{}", tx.serialize().to_lower_hex_string());
                                let txid = swap_tx
                                    .broadcast(&tx, &liquid_network_config, None)
                                    .await
                                    .unwrap();
                                log::info!("Cooperative Refund Successfully broadcasted: {}", txid);
                            }
                            Err(e) => {
                                log::info!("Cooperative refund failed. {:?}", e);
                                log::info!("Attempting Non-cooperative refund.");

                                let tx = swap_tx
                                    .sign_refund(&our_keys, Fee::Absolute(1000), None, false)
                                    .await
                                    .unwrap();
                                let txid = swap_tx
                                    .broadcast(&tx, &liquid_network_config, None)
                                    .await
                                    .unwrap();
                                log::info!(
                                    "Non-cooperative Refund Successfully broadcasted: {}",
                                    txid
                                );
                            }
                        }
                    }

                    if update.status == "transaction.claimed" {
                        log::info!("Successfully completed submarine swap");
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
async fn liquid_v2_reverse_electrum() {
    let liquid_network_config = ElectrumConfig::default_liquid();
    liquid_v2_reverse(liquid_network_config).await
}

#[macros::async_test_all]
#[cfg(feature = "esplora")]
#[ignore = "Requires testnet invoice and refund address"]
async fn liquid_v2_reverse_esplora() {
    let liquid_network_config = EsploraConfig::default_liquid();
    liquid_v2_reverse(liquid_network_config).await
}

async fn liquid_v2_reverse<LC: LiquidClient, LN: LiquidNetworkConfig<LC>>(
    liquid_network_config: LN,
) {
    setup_logger();

    let secp = Secp256k1::new();
    let preimage = Preimage::new();
    let our_keys = Keypair::new(&secp, &mut thread_rng());
    let invoice_amount = 1111;
    let claim_public_key = PublicKey {
        compressed: true,
        inner: our_keys.public_key(),
    };

    // Give a valid claim address or else funds will be lost.
    let claim_address = "tlq1qq05cvratqvsfmvdkag7rat2sav97f2aq3x9mzqhcgavygjnnetgy57zhurzvwwklexrxtdecw0ejfgwcjsxvqmrptdf25vrj8".to_string();
    let boltz_url = BOLTZ_MAINNET_URL_V2;
    let chain = Chain::Liquid;
    let boltz_api_v2 = BoltzApiClientV2::new(boltz_url);

    let addrs_sig = sign_address(&claim_address, &our_keys).unwrap();

    let create_reverse_req = CreateReverseRequest {
        invoice_amount,
        from: "BTC".to_string(),
        to: "L-BTC".to_string(),
        preimage_hash: preimage.sha256,
        description: None,
        description_hash: None,
        address_signature: Some(addrs_sig.to_string()),
        address: Some(claim_address.clone()),
        claim_public_key,
        referral_id: None,
        webhook: None,
    };

    let reverse_resp = boltz_api_v2
        .post_reverse_req(create_reverse_req)
        .await
        .unwrap();
    reverse_resp
        .validate(&preimage, &claim_public_key, chain)
        .unwrap();
    log::info!("VALIDATED RESPONSE!");

    let swap_id = reverse_resp.clone().id;

    let _ = check_for_mrh(&boltz_api_v2, &reverse_resp.invoice, Chain::BitcoinTestnet)
        .await
        .unwrap()
        .unwrap();

    log::debug!("Got Reverse swap response: {:?}", reverse_resp);

    let swap_script =
        LBtcSwapScript::reverse_from_swap_resp(&reverse_resp, claim_public_key).unwrap();
    swap_script.to_address(Chain::LiquidTestnet).unwrap();

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

                        std::thread::sleep(Duration::from_secs(5));

                        let claim_tx = LBtcSwapTx::new_claim(
                            swap_script.clone(),
                            claim_address.clone(),
                            &liquid_network_config,
                            BOLTZ_TESTNET_URL_V2.to_string(),
                            swap_id.clone(),
                        )
                        .await
                        .unwrap();

                        let tx = claim_tx
                            .sign_claim(
                                &our_keys,
                                &preimage,
                                Fee::Absolute(1000),
                                None,
                                // Some(Cooperative {
                                //     boltz_api: &boltz_api_v2,
                                //     swap_id: swap_id.clone(),
                                //     pub_nonce: None,
                                //     partial_sig: None,
                                // }),
                                false,
                            )
                            .await
                            .unwrap();

                        claim_tx
                            .broadcast(&tx, &liquid_network_config, None)
                            .await
                            .unwrap();

                        // To test Lowball broadcast uncomment below line
                        // claim_tx
                        //     .broadcast(
                        //         &tx,
                        //         &ElectrumConfig::default_liquid(),
                        //         Some((&boltz_api_v2, boltz_client::network::Chain::LiquidTestnet)),
                        //     )
                        //     .unwrap();

                        log::info!("Succesfully broadcasted claim tx!");
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
async fn liquid_v2_reverse_script_path_electrum() {
    let liquid_network_config = ElectrumConfig::default_liquid();
    liquid_v2_reverse_script_path(liquid_network_config).await
}

#[macros::async_test_all]
#[cfg(feature = "esplora")]
#[ignore = "Requires testnet invoice and refund address"]
async fn liquid_v2_reverse_script_path_esplora() {
    let liquid_network_config = EsploraConfig::default_liquid();
    liquid_v2_reverse_script_path(liquid_network_config).await
}

async fn liquid_v2_reverse_script_path<LC: LiquidClient, LN: LiquidNetworkConfig<LC>>(
    liquid_network_config: LN,
) {
    setup_logger();

    let secp = Secp256k1::new();
    let preimage = Preimage::new();
    let our_keys = Keypair::new(&secp, &mut thread_rng());
    let invoice_amount = 11110;
    let claim_public_key = PublicKey {
        compressed: true,
        inner: our_keys.public_key(),
    };

    // Give a valid claim address or else funds will be lost.
    let claim_address = "tlq1qq05cvratqvsfmvdkag7rat2sav97f2aq3x9mzqhcgavygjnnetgy57zhurzvwwklexrxtdecw0ejfgwcjsxvqmrptdf25vrj8".to_string();
    let boltz_url = BOLTZ_TESTNET_URL_V2;
    let chain = Chain::LiquidTestnet;
    let boltz_api_v2 = BoltzApiClientV2::new(boltz_url);

    let addrs_sig = sign_address(&claim_address, &our_keys).unwrap();

    let create_reverse_req = CreateReverseRequest {
        invoice_amount,
        from: "BTC".to_string(),
        to: "L-BTC".to_string(),
        preimage_hash: preimage.sha256,
        description: None,
        description_hash: None,
        address_signature: Some(addrs_sig.to_string()),
        address: Some(claim_address.clone()),
        claim_public_key,
        referral_id: None,
        webhook: None,
    };

    let reverse_resp = boltz_api_v2
        .post_reverse_req(create_reverse_req)
        .await
        .unwrap();
    reverse_resp
        .validate(&preimage, &claim_public_key, chain)
        .unwrap();
    log::info!("VALIDATED RESPONSE!");

    let swap_id = reverse_resp.clone().id;

    let _ = check_for_mrh(&boltz_api_v2, &reverse_resp.invoice, Chain::BitcoinTestnet)
        .await
        .unwrap()
        .unwrap();

    log::debug!("Got Reverse swap response: {:?}", reverse_resp);

    let swap_script =
        LBtcSwapScript::reverse_from_swap_resp(&reverse_resp, claim_public_key).unwrap();
    swap_script.to_address(Chain::LiquidTestnet).unwrap();

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

                        std::thread::sleep(Duration::from_secs(5));

                        let claim_tx = LBtcSwapTx::new_claim(
                            swap_script.clone(),
                            claim_address.clone(),
                            &liquid_network_config,
                            BOLTZ_TESTNET_URL_V2.to_string(),
                            swap_id.clone(),
                        )
                        .await
                        .unwrap();

                        let tx = claim_tx
                            .sign_claim(&our_keys, &preimage, Fee::Absolute(1000), None, false)
                            .await
                            .unwrap();

                        claim_tx
                            .broadcast(&tx, &liquid_network_config, None)
                            .await
                            .unwrap();

                        // To test Lowball broadcast uncomment below line
                        // claim_tx
                        //     .broadcast(
                        //         &tx,
                        //         &ElectrumConfig::default_liquid(),
                        //         Some((&boltz_api_v2, boltz_client::network::Chain::LiquidTestnet)),
                        //     )
                        //     .unwrap();

                        log::info!("Succesfully broadcasted claim tx!");
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
async fn test_recover_liquidv2_refund_electrum() {
    let liquid_network_config = ElectrumConfig::default_liquid();
    test_recover_liquidv2_refund(liquid_network_config).await
}

#[macros::async_test_all]
#[cfg(feature = "esplora")]
#[ignore = "Requires testnet invoice and refund address"]
async fn test_recover_liquidv2_refund_esplora() {
    let liquid_network_config = EsploraConfig::default_liquid();
    test_recover_liquidv2_refund(liquid_network_config).await
}

// _$SwapTxSensitiveImpl (SwapTxSensitive(id: xJ9E5spWSbmw, secretKey: 5c2c8120ff354ed8b6440121c621b0395d7cccc839a6200cf0b8208e19483ed1, publicKey: 0273daf3d9728b7bd7a716fdf4b05b4b12a0febd570f8b708bfcffc1026e2c0f47, preimage: , sha256: 451f1df5a5ccc1e487cf691c21c1b90737557a9df3171a69aaa6dc1578bc6be4, hash160: 726bbbe8393827392d21445922ee87b254c07285, redeemScript: redeemScript, boltzPubkey: 0329724923c9a845eb044fa4ff323f850af6b995185b2cc18335d896011f894acd, isSubmarine: true, scriptAddress: null, locktime: 2868372, blindingKey: 1b581bed3300b146c61bdb3e5b58413f85299b71ab36401a8e02ec38d57925aa))

async fn test_recover_liquidv2_refund<LC: LiquidClient, LN: LiquidNetworkConfig<LC>>(
    liquid_network_config: LN,
) {
    setup_logger();

    let id = "xJ9E5spWSbmw".to_string();
    let secp = Secp256k1::new();
    const RETURN_ADDRESS: &str = "lq1qqf0la7qlx5un0ssn6h2s3s4m3wlgyqlkprf3lzd8utjfvd95dc5ljhsk2684ahr842dse89whesfcgtm4vkazdjzc7e42lg0c";
    let _out_amount = 50_000;
    let keypair = Keypair::from_seckey_str(
        &secp,
        "5c2c8120ff354ed8b6440121c621b0395d7cccc839a6200cf0b8208e19483ed1",
    )
    .unwrap();

    let our_pubkey = "0273daf3d9728b7bd7a716fdf4b05b4b12a0febd570f8b708bfcffc1026e2c0f47";
    assert!(keypair.public_key().to_string() == our_pubkey);
    let locktime = 2868372;
    let preimage = Preimage::from_sha256_str(
        "451f1df5a5ccc1e487cf691c21c1b90737557a9df3171a69aaa6dc1578bc6be4",
    )
    .unwrap();
    let boltz_pubkey =
        "0329724923c9a845eb044fa4ff323f850af6b995185b2cc18335d896011f894acd".to_string();

    let script_address = "lq1pqvngcdxu9c2dprw8m2kye22m78358pug3chgk29a2wk9kh4kn0axxvjwfznzrxvvvwf59xm35kxdzv44ctjdsjzpxg0804l9vknzd2x0d8eh36d54zce".to_string();
    let blinding_key =
        "1b581bed3300b146c61bdb3e5b58413f85299b71ab36401a8e02ec38d57925aa".to_string();
    let absolute_fees = 1_200;
    let network_config = liquid_network_config;
    let swap_script: LBtcSwapScript = create_swap_script_v2(
        script_address,
        preimage.hash160.to_string(),
        boltz_pubkey,
        keypair.public_key().to_string(),
        locktime,
        blinding_key,
    );

    let rev_swap_tx = LBtcSwapTx::new_refund(
        swap_script,
        RETURN_ADDRESS,
        &network_config,
        BOLTZ_MAINNET_URL_V2.to_string(),
        id.clone(),
    )
    .await
    .unwrap();
    let client = BoltzApiClientV2::new(BOLTZ_MAINNET_URL_V2);
    let coop = Some(Cooperative {
        boltz_api: &client,
        swap_id: id,
        pub_nonce: None,
        partial_sig: None,
    });
    let signed_tx = rev_swap_tx
        .sign_refund(&keypair, Fee::Absolute(absolute_fees), coop, false)
        .await
        .unwrap();
    let tx_hex = serialize(&signed_tx).to_lower_hex_string();
    log::info!("TX_HEX: {}", tx_hex);

    let txid = rev_swap_tx
        .broadcast(&signed_tx, &network_config, None)
        .await
        .unwrap();
    println!("{}", txid);
}

fn create_swap_script_v2(
    address: String,
    hashlock: String,
    receiver_pub: String,
    sender_pub: String,
    locktime: u32,
    blinding_key: String,
) -> LBtcSwapScript {
    let address = elements::Address::from_str(&address).unwrap();
    let hashlock = BCHash::from_str(&hashlock).unwrap();
    let receiver_pubkey = PublicKey::from_str(&receiver_pub).unwrap();
    let sender_pubkey = PublicKey::from_str(&sender_pub).unwrap();
    let locktime = boltz_client::ElementsLockTime::from_height(locktime).unwrap();
    let blinding_key = Keypair::from_str(&blinding_key).unwrap();

    LBtcSwapScript {
        swap_type: SwapType::Submarine,
        side: None,
        funding_addrs: Some(address),
        hashlock,
        receiver_pubkey,
        locktime,
        sender_pubkey,
        blinding_key,
    }
}
