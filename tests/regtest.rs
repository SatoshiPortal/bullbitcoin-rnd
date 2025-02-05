use bitcoin::absolute::LockTime;
use bitcoin::key::rand::thread_rng;
use bitcoin::key::{Keypair, PublicKey};
use bitcoin::secp256k1::Secp256k1;
use bitcoin::{Amount, OutPoint, TxOut};
use bitcoind::bitcoincore_rpc::json::ScanTxOutRequest;
use bitcoind::bitcoincore_rpc::RpcApi;
use boltz_client::boltz::{SwapTxKind, SwapType};
use boltz_client::fees::Fee;
use boltz_client::network::Chain;
use boltz_client::util::secrets::Preimage;
use boltz_client::{BtcSwapScript, BtcSwapTx, LBtcSwapScript, LBtcSwapTx};
use elements::Address;

mod test_framework;
use test_framework::{BtcTestFramework, LbtcTestFramework};

const FUNDING_AMOUNT: u64 = 10_000;

fn prepare_btc_claim() -> (
    BtcTestFramework,
    ScanTxOutRequest,
    BtcSwapTx,
    Preimage,
    Keypair,
    Vec<(OutPoint, TxOut)>,
) {
    // Init test framework and get a test-wallet
    let test_framework = BtcTestFramework::init();

    // Generate a random preimage and hash it.
    let preimage = Preimage::new();

    // Generate dummy receiver and sender's keypair
    let secp = Secp256k1::new();
    let recvr_keypair = Keypair::new(&secp, &mut thread_rng());
    let sender_keypair = Keypair::new(&secp, &mut thread_rng());

    // create a btc swap script.
    let swap_script = BtcSwapScript {
        swap_type: SwapType::ReverseSubmarine,
        side: None,
        funding_addrs: None,
        hashlock: preimage.hash160,
        receiver_pubkey: PublicKey {
            compressed: true,
            inner: recvr_keypair.public_key(),
        },
        locktime: LockTime::from_height(200).unwrap(),
        sender_pubkey: PublicKey {
            compressed: true,
            inner: sender_keypair.public_key(),
        },
    };

    // Send coin the swapscript address and confirm tx
    let swap_addrs = swap_script.to_address(Chain::BitcoinRegtest).unwrap();
    let spk = swap_addrs.script_pubkey();
    println!("spk: {}", spk);
    test_framework.send_coins(&swap_addrs, Amount::from_sat(FUNDING_AMOUNT));
    test_framework.generate_blocks(1);

    let scan_request = ScanTxOutRequest::Single(format!("addr({})", swap_addrs));

    let scan_result = test_framework
        .as_ref()
        .scan_tx_out_set_blocking(&[scan_request.clone()])
        .unwrap();

    assert_eq!(scan_result.unspents.len(), 1);
    assert_eq!(scan_result.total_amount, Amount::from_sat(FUNDING_AMOUNT));

    // Create a refund spending transaction from the swap
    let utxos: Vec<(OutPoint, TxOut)> = scan_result
        .unspents
        .iter()
        .map(|utxo| {
            let outpoint = OutPoint::new(utxo.txid, utxo.vout);
            let txout = TxOut {
                script_pubkey: utxo.script_pub_key.clone(),
                value: utxo.amount,
            };
            (outpoint, txout)
        })
        .collect();

    let test_wallet = test_framework.get_test_wallet();
    let refund_addrs = test_wallet
        .get_new_address(None, None)
        .unwrap()
        .assume_checked();

    let swap_tx = BtcSwapTx {
        kind: SwapTxKind::Claim,
        swap_script,
        output_address: refund_addrs,
        utxos: utxos.clone(),
    };

    (
        test_framework,
        scan_request,
        swap_tx,
        preimage,
        recvr_keypair,
        utxos,
    )
}

#[test]
fn btc_reverse_claim_size() {
    let (_test_framework, _scan_request, swap_tx, _preimage, recvr_keypair, _utxos) =
        prepare_btc_claim();

    let coop_claim_tx_size = swap_tx.size(&recvr_keypair, true).unwrap();
    assert_eq!(coop_claim_tx_size, 84);

    let non_coop_claim_tx_size = swap_tx.size(&recvr_keypair, false).unwrap();
    assert_eq!(non_coop_claim_tx_size, 140);
}

#[test]
fn btc_reverse_claim() {
    let (test_framework, scan_request, swap_tx, preimage, recvr_keypair, utxos) =
        prepare_btc_claim();
    let test_wallet = test_framework.get_test_wallet();

    let absolute_fee = 1_000;
    let claim_tx = swap_tx
        .sign_claim(&recvr_keypair, &preimage, Fee::Absolute(absolute_fee), None)
        .unwrap();

    let claim_tx_fee = utxos
        .iter()
        .fold(0, |acc, (_, out)| acc + out.value.to_sat())
        - claim_tx.output[0].value.to_sat();
    assert_eq!(claim_tx_fee, absolute_fee);

    test_framework
        .as_ref()
        .send_raw_transaction(&claim_tx)
        .unwrap();
    test_framework.generate_blocks(1);

    let scan_result = test_framework
        .as_ref()
        .scan_tx_out_set_blocking(&[scan_request])
        .unwrap();

    assert_eq!(scan_result.unspents.len(), 0);
    assert_eq!(scan_result.total_amount, Amount::from_sat(0));

    let test_balance = test_wallet.get_balance(None, None).unwrap();
    assert_eq!(test_balance, Amount::from_sat(FUNDING_AMOUNT * 2 - 1_000));
}

#[test]
fn btc_reverse_claim_relative_fee() {
    let (test_framework, scan_request, swap_tx, preimage, recvr_keypair, utxos) =
        prepare_btc_claim();
    let test_wallet = test_framework.get_test_wallet();

    let relative_fee = 1.0;
    let claim_tx = swap_tx
        .sign_claim(&recvr_keypair, &preimage, Fee::Relative(relative_fee), None)
        .unwrap();

    let claim_tx_fee = utxos
        .iter()
        .fold(0, |acc, (_, out)| acc + out.value.to_sat())
        - claim_tx.output[0].value.to_sat();
    assert_eq!(relative_fee, claim_tx_fee as f64 / claim_tx.vsize() as f64);
    assert_eq!(claim_tx_fee, 140);

    test_framework
        .as_ref()
        .send_raw_transaction(&claim_tx)
        .unwrap();
    test_framework.generate_blocks(1);

    let scan_result = test_framework
        .as_ref()
        .scan_tx_out_set_blocking(&[scan_request])
        .unwrap();

    assert_eq!(scan_result.unspents.len(), 0);
    assert_eq!(scan_result.total_amount, Amount::from_sat(0));

    let test_balance = test_wallet.get_balance(None, None).unwrap();
    assert_eq!(
        test_balance,
        Amount::from_sat(FUNDING_AMOUNT * 2 - claim_tx_fee)
    );
}

fn prepare_btc_refund() -> (
    BtcTestFramework,
    ScanTxOutRequest,
    BtcSwapTx,
    Keypair,
    Vec<(OutPoint, TxOut)>,
) {
    // Init test framework and get a test-wallet
    let test_framework = BtcTestFramework::init();

    // Generate dummy receiver and sender's keypair
    let preimage = Preimage::new();
    let secp = Secp256k1::new();
    let recvr_keypair = Keypair::new(&secp, &mut thread_rng());
    let sender_keypair = Keypair::new(&secp, &mut thread_rng());

    // create a btc swap script.
    let swap_script = BtcSwapScript {
        swap_type: SwapType::Submarine,
        side: None,
        funding_addrs: None,
        hashlock: preimage.hash160,
        receiver_pubkey: PublicKey {
            compressed: true,
            inner: recvr_keypair.public_key(),
        },
        locktime: LockTime::from_height(200).unwrap(),
        sender_pubkey: PublicKey {
            compressed: true,
            inner: sender_keypair.public_key(),
        },
    };

    // Send coin the swapscript address and confirm tx
    let swap_addrs = swap_script.to_address(Chain::BitcoinRegtest).unwrap();
    test_framework.send_coins(&swap_addrs, Amount::from_sat(10000));
    test_framework.generate_blocks(1);

    let scan_request = ScanTxOutRequest::Single(format!("addr({})", swap_addrs));

    let scan_result = test_framework
        .as_ref()
        .scan_tx_out_set_blocking(&[scan_request.clone()])
        .unwrap();

    assert_eq!(scan_result.unspents.len(), 1);
    assert_eq!(scan_result.total_amount, Amount::from_sat(10000));

    // Create a refund spending transaction from the swap
    let utxos: Vec<(OutPoint, TxOut)> = scan_result
        .unspents
        .iter()
        .map(|utxo| {
            let outpoint = OutPoint::new(utxo.txid, utxo.vout);
            let txout = TxOut {
                script_pubkey: utxo.script_pub_key.clone(),
                value: utxo.amount,
            };
            (outpoint, txout)
        })
        .collect();

    let test_wallet = test_framework.get_test_wallet();
    let refund_addrs = test_wallet
        .get_new_address(None, None)
        .unwrap()
        .assume_checked();

    let swap_tx = BtcSwapTx {
        kind: SwapTxKind::Refund,
        swap_script,
        output_address: refund_addrs,
        utxos: utxos.clone(),
    };

    (test_framework, scan_request, swap_tx, sender_keypair, utxos)
}

#[test]
fn btc_submarine_refund_size() {
    let (_test_framework, _scan_request, swap_tx, sender_keypair, _utxos) = prepare_btc_refund();

    let coop_refund_tx_size = swap_tx.size(&sender_keypair, true).unwrap();
    assert_eq!(coop_refund_tx_size, 84);

    let non_coop_refund_tx_size = swap_tx.size(&sender_keypair, false).unwrap();
    assert_eq!(non_coop_refund_tx_size, 126);
}

#[test]
fn btc_submarine_refund() {
    let (test_framework, scan_request, swap_tx, sender_keypair, utxos) = prepare_btc_refund();
    let test_wallet = test_framework.get_test_wallet();

    let absolute_fee = 1_000;
    let refund_tx = swap_tx
        .sign_refund(&sender_keypair, Fee::Absolute(absolute_fee), None)
        .unwrap();

    let refund_tx_fee = utxos
        .iter()
        .fold(0, |acc, (_, out)| acc + out.value.to_sat())
        - refund_tx.output[0].value.to_sat();
    assert_eq!(refund_tx_fee, absolute_fee);

    // Make the timelock matured and broadcast the spend
    test_framework.generate_blocks(100);
    test_framework
        .as_ref()
        .send_raw_transaction(&refund_tx)
        .unwrap();
    test_framework.generate_blocks(1);

    let scan_result = test_framework
        .as_ref()
        .scan_tx_out_set_blocking(&[scan_request])
        .unwrap();

    assert_eq!(scan_result.unspents.len(), 0);
    assert_eq!(scan_result.total_amount, Amount::from_sat(0));

    let test_balance = test_wallet.get_balance(None, None).unwrap();

    assert_eq!(
        test_balance,
        Amount::from_sat(FUNDING_AMOUNT * 2 - absolute_fee)
    );
}

#[test]
fn btc_submarine_refund_relative_fee() {
    let (test_framework, scan_request, swap_tx, sender_keypair, utxos) = prepare_btc_refund();
    let test_wallet = test_framework.get_test_wallet();

    let relative_fee = 1.0;
    let refund_tx = swap_tx
        .sign_refund(&sender_keypair, Fee::Relative(relative_fee), None)
        .unwrap();

    let refund_tx_fee = utxos
        .iter()
        .fold(0, |acc, (_, out)| acc + out.value.to_sat())
        - refund_tx.output[0].value.to_sat();
    assert_eq!(
        relative_fee,
        refund_tx_fee as f64 / refund_tx.vsize() as f64
    );
    assert_eq!(refund_tx_fee, 126);

    // Make the timelock matured and broadcast the spend
    test_framework.generate_blocks(100);
    test_framework
        .as_ref()
        .send_raw_transaction(&refund_tx)
        .unwrap();
    test_framework.generate_blocks(1);

    let scan_result = test_framework
        .as_ref()
        .scan_tx_out_set_blocking(&[scan_request])
        .unwrap();

    assert_eq!(scan_result.unspents.len(), 0);
    assert_eq!(scan_result.total_amount, Amount::from_sat(0));

    let test_balance = test_wallet.get_balance(None, None).unwrap();

    assert_eq!(
        test_balance,
        Amount::from_sat(FUNDING_AMOUNT * 2 - refund_tx_fee)
    );
}

fn prepare_lbtc_claim() -> (
    LbtcTestFramework,
    LBtcSwapTx,
    Preimage,
    Keypair,
    elements::secp256k1_zkp::Keypair,
    Address,
    (elements::OutPoint, elements::TxOut),
) {
    // Init test framework and get a test-wallet
    let test_framework = LbtcTestFramework::init();

    // Generate a random preimage and hash it.
    let preimage = Preimage::new();

    // Generate dummy receiver and sender's keypair
    let secp = Secp256k1::new();
    let recvr_keypair = Keypair::new(&secp, &mut thread_rng());
    let sender_keypair = Keypair::new(&secp, &mut thread_rng());
    let blinding_keypair = elements::secp256k1_zkp::Keypair::new(&secp, &mut thread_rng());

    // create a btc swap script.
    let swap_script = LBtcSwapScript {
        swap_type: SwapType::ReverseSubmarine,
        side: None,
        funding_addrs: None,
        hashlock: preimage.hash160,
        receiver_pubkey: PublicKey {
            compressed: true,
            inner: recvr_keypair.public_key(),
        },
        locktime: elements::LockTime::from_height(200).unwrap(),
        sender_pubkey: PublicKey {
            compressed: true,
            inner: sender_keypair.public_key(),
        },
        blinding_key: blinding_keypair,
    };

    // Send coin the swapscript address and confirm tx
    let swap_addrs = swap_script.to_address(Chain::LiquidRegtest).unwrap();

    test_framework.send_coins(&swap_addrs, Amount::from_sat(10000));
    test_framework.generate_blocks(1);

    let utxo = test_framework.fetch_utxo(&swap_addrs).unwrap();

    let refund_addrs = test_framework.get_new_addrs();

    let genesis_hash = test_framework.genesis_hash();

    let swap_tx = LBtcSwapTx {
        kind: SwapTxKind::Claim,
        swap_script,
        output_address: refund_addrs,
        funding_outpoint: utxo.0,
        funding_utxo: utxo.1.clone(),
        genesis_hash,
    };

    (
        test_framework,
        swap_tx,
        preimage,
        recvr_keypair,
        blinding_keypair,
        swap_addrs,
        utxo,
    )
}

#[test]
fn lbtc_reverse_claim_size() {
    let (
        _test_framework,
        swap_tx,
        _preimage,
        recvr_keypair,
        _blinding_keypair,
        _swap_addrs,
        _utxos,
    ) = prepare_lbtc_claim();

    let coop_claim_tx_size = swap_tx.size(&recvr_keypair, true, true).unwrap();
    assert_eq!(coop_claim_tx_size, 165);

    let non_coop_claim_tx_size = swap_tx.size(&recvr_keypair, false, true).unwrap();
    assert_eq!(non_coop_claim_tx_size, 221);
}

#[test]
fn lbtc_reverse_claim() {
    let (test_framework, swap_tx, preimage, recvr_keypair, blinding_keypair, swap_addrs, utxo) =
        prepare_lbtc_claim();

    let absolute_fee = 1_000;
    let claim_tx = swap_tx
        .sign_claim(
            &recvr_keypair,
            &preimage,
            Fee::Absolute(absolute_fee),
            None,
            false,
        )
        .unwrap();
    let secp = Secp256k1::new();
    assert_eq!(
        claim_tx.fee_in(
            utxo.1
                .unblind(&secp, blinding_keypair.secret_key())
                .unwrap()
                .asset
        ),
        absolute_fee
    );

    test_framework.send_tx(&claim_tx);
    test_framework.generate_blocks(1);

    assert!(test_framework.fetch_utxo(&swap_addrs).is_none());
}

#[test]
fn lbtc_reverse_claim_relative_fee() {
    let (test_framework, swap_tx, preimage, recvr_keypair, blinding_keypair, swap_addrs, utxo) =
        prepare_lbtc_claim();

    let relative_fee = 0.1;
    let claim_tx = swap_tx
        .sign_claim(
            &recvr_keypair,
            &preimage,
            Fee::Relative(relative_fee),
            None,
            false,
        )
        .unwrap();
    assert_eq!(
        claim_tx.fee_in(
            utxo.1
                .unblind(&Secp256k1::new(), blinding_keypair.secret_key())
                .unwrap()
                .asset
        ),
        (relative_fee * claim_tx.vsize() as f64).ceil() as u64
    );

    test_framework.send_tx(&claim_tx);
    test_framework.generate_blocks(1);

    assert!(test_framework.fetch_utxo(&swap_addrs).is_none());
}

fn prepare_lbtc_refund() -> (
    LbtcTestFramework,
    LBtcSwapTx,
    Keypair,
    Keypair,
    Address,
    (elements::OutPoint, elements::TxOut),
) {
    // Init test framework and get a test-wallet
    let test_framework = LbtcTestFramework::init();

    // Generate dummy receiver and sender's keypair
    let preimage = Preimage::new();
    let secp = Secp256k1::new();
    let recvr_keypair = Keypair::new(&secp, &mut thread_rng());
    let sender_keypair = Keypair::new(&secp, &mut thread_rng());
    let blinding_keypair = elements::secp256k1_zkp::Keypair::new(&secp, &mut thread_rng());

    // create a btc swap script.
    let swap_script = LBtcSwapScript {
        swap_type: SwapType::Submarine,
        side: None,
        funding_addrs: None,
        hashlock: preimage.hash160,
        receiver_pubkey: PublicKey {
            compressed: true,
            inner: recvr_keypair.public_key(),
        },
        locktime: elements::LockTime::from_height(200).unwrap(),
        sender_pubkey: PublicKey {
            compressed: true,
            inner: sender_keypair.public_key(),
        },
        blinding_key: blinding_keypair,
    };

    // Send coin the swapscript address and confirm tx
    let swap_addrs = swap_script.to_address(Chain::LiquidRegtest).unwrap();
    test_framework.send_coins(&swap_addrs, Amount::from_sat(10000));
    test_framework.generate_blocks(1);

    // Create a refund spending transaction from the swap
    let utxo = test_framework.fetch_utxo(&swap_addrs).unwrap();

    let refund_addrs = test_framework.get_new_addrs();

    let genesis_hash = test_framework.genesis_hash();

    let swap_tx = LBtcSwapTx {
        kind: SwapTxKind::Refund,
        swap_script,
        output_address: refund_addrs,
        funding_outpoint: utxo.0,
        funding_utxo: utxo.1.clone(),
        genesis_hash,
    };

    (
        test_framework,
        swap_tx,
        sender_keypair,
        blinding_keypair,
        swap_addrs,
        utxo,
    )
}

#[test]
fn lbtc_submarine_refund_size() {
    let (_test_framework, swap_tx, sender_keypair, _blinding_keypair, _swap_addrs, _utxos) =
        prepare_lbtc_refund();

    let coop_refund_tx_size = swap_tx.size(&sender_keypair, true, true).unwrap();
    assert_eq!(coop_refund_tx_size, 165);

    let non_coop_refund_tx_size = swap_tx.size(&sender_keypair, false, true).unwrap();
    assert_eq!(non_coop_refund_tx_size, 207);
}

#[test]
fn lbtc_submarine_refund() {
    let (test_framework, swap_tx, sender_keypair, blinding_keypair, swap_addrs, utxo) =
        prepare_lbtc_refund();

    let absolute_fee = 1_000;
    let refund_tx = swap_tx
        .sign_refund(&sender_keypair, Fee::Absolute(absolute_fee), None, false)
        .unwrap();
    assert_eq!(
        refund_tx.fee_in(
            utxo.1
                .unblind(&Secp256k1::new(), blinding_keypair.secret_key())
                .unwrap()
                .asset
        ),
        absolute_fee
    );

    // Make the timelock matured and broadcast the spend
    test_framework.generate_blocks(100);
    test_framework.send_tx(&refund_tx);
    test_framework.generate_blocks(1);

    assert!(test_framework.fetch_utxo(&swap_addrs).is_none());
}

#[test]
fn lbtc_submarine_refund_relative_fee() {
    let (test_framework, swap_tx, sender_keypair, blinding_keypair, swap_addrs, utxo) =
        prepare_lbtc_refund();

    let relative_fee = 0.1;
    let refund_tx = swap_tx
        .sign_refund(&sender_keypair, Fee::Relative(relative_fee), None, false)
        .unwrap();
    assert_eq!(
        refund_tx.fee_in(
            utxo.1
                .unblind(&Secp256k1::new(), blinding_keypair.secret_key())
                .unwrap()
                .asset
        ),
        (relative_fee * refund_tx.vsize() as f64).ceil() as u64
    );

    // Make the timelock matured and broadcast the spend
    test_framework.generate_blocks(100);
    test_framework.send_tx(&refund_tx);
    test_framework.generate_blocks(1);

    assert!(test_framework.fetch_utxo(&swap_addrs).is_none());
}
