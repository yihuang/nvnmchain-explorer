//! The pages, end to end.
//!
//! These render the real templates against a real database, so a context key a
//! handler stops sending fails a test rather than a page view. Nothing reaches
//! the network: fixtures carry their own traces and failure reasons, the RPC
//! points at a closed port, and the signature directory is stubbed.

use std::sync::{Arc, Mutex};

use nvnmchain_explorer::db::{self, Db};
use nvnmchain_explorer::decoder::{checksum_address, keccak256, keccak_hex, TRANSFER_TOPIC};
use nvnmchain_explorer::models::{Block, Transaction};
use nvnmchain_explorer::tokens::TokenMeta;
use serde_json::{json, Value};

const TOKEN: &str = "0x20c0000000000000000000000000000000000000";
const SENDER: &str = "0x1111111111111111111111111111111111111111";
const RECIPIENT: &str = "0x2222222222222222222222222222222222222222";
const TX_HASH: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const FAILED_TX_HASH: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const UNKNOWN_LOG_TX_HASH: &str =
    "0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

fn temp_db(name: &str) -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(name);
    let conn = db::init_db(path.to_str().unwrap()).expect("init_db");
    (dir, Arc::new(Mutex::new(conn)))
}

fn topic(address: &str) -> String {
    format!("0x{}{}", "00".repeat(12), address.trim_start_matches("0x"))
}

/// `transfer(to, amount)` calldata — the failing call's input, so the summary
/// has a function name to blame.
fn transfer_calldata(to: &str, amount: u128) -> String {
    format!(
        "0xa9059cbb000000000000000000000000{}{amount:064x}",
        to.trim_start_matches("0x")
    )
}

fn block() -> Block {
    Block {
        number: 100,
        hash: format!("0x{}", "ab".repeat(32)),
        parent_hash: format!("0x{}", "cd".repeat(32)),
        timestamp: 1_700_000_000,
        timestamp_ms: 1_700_000_000_000,
        gas_used: 21_000,
        gas_limit: 30_000_000,
        base_fee: "1000000000".into(),
        size: 512,
        extra_data: String::new(),
        epoch: 1,
        view: 1,
        proposer: format!("0x{}", "33".repeat(20)),
        miner: format!("0x{}", "33".repeat(20)),
        tx_count: 2,
        created_at: 0,
    }
}

/// A transaction carrying `receipt` and no trace to fetch.
fn transaction(hash: &str, status: i64, receipt: Value) -> Transaction {
    Transaction {
        hash: hash.into(),
        block_number: 100,
        position: 0,
        from_addr: checksum_address(SENDER),
        to_addr: Some(checksum_address(TOKEN)),
        status,
        gas_used: 21_000,
        base_fee: "0x3b9aca00".into(),
        contract_address: None,
        fee_token: Some(checksum_address(TOKEN)),
        fee_amount: "2500".into(),
        input: transfer_calldata(RECIPIENT, 1_500_000),
        raw: None,
        // Empty rather than absent: a missing trace sends the handler to the RPC.
        trace_data: Some("[]".into()),
        receipt_data: Some(receipt.to_string()),
        timestamp: 1_700_000_000,
        created_at: 0,
    }
}

/// A receipt whose logs are a fee transfer and the payment it paid for, with a
/// memo — the shape a real TIP-20 payment has.
fn successful_receipt() -> Value {
    json!({
        "status": "0x1",
        "effectiveGasPrice": "0x4a817c800",
        "logs": [
            {
                "address": TOKEN,
                "topics": [TRANSFER_TOPIC.as_str(), topic(SENDER), topic("0xfeEC000000000000000000000000000000000000")],
                "data": format!("0x{:064x}", 2_500u128),
                "logIndex": "0x0",
            },
            {
                "address": TOKEN,
                "topics": [
                    nvnmchain_explorer::decoder::TRANSFER_WITH_MEMO_TOPIC.as_str(),
                    topic(SENDER),
                    topic(RECIPIENT),
                    format!("0x{}{}", hex::encode("invoice 42"), "00".repeat(22)),
                ],
                "data": format!("0x{:064x}", 1_500_000u128),
                "logIndex": "0x1",
            },
        ],
    })
}

/// A receipt for a transfer that ran out of money, with the revert data the
/// node embedded in its message.
fn failed_receipt() -> Value {
    let args = ethers_core::abi::encode(&[
        ethers_core::abi::Token::Uint(1_000_000u64.into()),
        ethers_core::abi::Token::Uint(2_500_000u64.into()),
        ethers_core::abi::Token::Address(TOKEN.parse().unwrap()),
    ]);
    let selector = &keccak256(b"InsufficientBalance(uint256,uint256,address)")[..4];
    json!({
        "status": "0x0",
        "effectiveGasPrice": "0x4a817c800",
        "revertReason": format!(
            "execution reverted: 0x{}{}",
            hex::encode(selector),
            hex::encode(args)
        ),
        "logs": [],
    })
}

/// A receipt whose only log no built-in ABI describes.
fn unknown_log_receipt() -> Value {
    json!({
        "status": "0x1",
        "effectiveGasPrice": "0x4a817c800",
        "logs": [{
            "address": TOKEN,
            "topics": [keccak_hex(b"SomethingNobodyDeclared(uint256)")],
            "data": format!("0x{:064x}", 7u64),
            "logIndex": "0x0",
        }],
    })
}

async fn serve() -> (tempfile::TempDir, String) {
    use nvnmchain_explorer::web::{self, AppState};

    let (dir, db) = temp_db("pages.db");
    db::save_block(&db, &block()).expect("block");
    db::save_transaction(&db, &transaction(TX_HASH, 1, successful_receipt())).expect("tx");
    db::save_transaction(&db, &transaction(FAILED_TX_HASH, 0, failed_receipt()))
        .expect("failed tx");
    db::save_transaction(
        &db,
        &transaction(UNKNOWN_LOG_TX_HASH, 1, unknown_log_receipt()),
    )
    .expect("unknown-log tx");
    db::save_token_metadata(
        &db,
        &TokenMeta {
            address: checksum_address(TOKEN),
            name: "pathUSD".into(),
            symbol: "pathUSD".into(),
            decimals: 6,
            currency: "USD".into(),
            total_supply: "1000000000".into(),
        },
    )
    .expect("token");

    let cfg = nvnmchain_explorer::config::Settings::from_env();
    let tera = web::build_tera(db.clone()).expect("templates");
    let state = AppState {
        db,
        rpc: nvnmchain_explorer::rpc::ChainRpc::from_settings(&cfg).expect("rpc"),
        cfg,
        tera,
        block_events: tokio::sync::broadcast::channel(16).0,
        stats: Arc::new(std::sync::RwLock::new(Value::Null)),
        shutdown: tokio::sync::watch::channel(false).1,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, web::app(state)).await;
    });
    (dir, format!("http://{addr}"))
}

async fn get_json(base: &str, path: &str) -> Value {
    reqwest::get(format!("{base}{path}&format=json"))
        .await
        .unwrap_or_else(|e| panic!("GET {path}: {e}"))
        .json()
        .await
        .unwrap_or_else(|e| panic!("GET {path} json: {e}"))
}

#[tokio::test]
async fn a_successful_transaction_says_what_it_did() {
    let (_dir, base) = serve().await;
    let page = get_json(&base, &format!("/tx/{TX_HASH}?")).await;

    // The fee transfer came first in the receipt; the payment is the point.
    assert_eq!(
        page["summary"]["headline"],
        json!("Send 1.5 pathUSD to 0x2222…2222.")
    );
    assert_eq!(page["summary"]["tone"], json!("success"));

    let events = page["events"].as_array().expect("events");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["known"]["kind"], json!("fee transfer"));
    assert_eq!(events[0]["known"]["is_fee"], json!(true));
    assert_eq!(
        events[1]["known"]["headline"],
        json!("Send 1.5 pathUSD to 0x2222…2222")
    );
    // The memo is the reason TransferWithMemo exists; it must reach the page.
    assert_eq!(events[1]["known"]["note"], json!("invoice 42"));
    // And the decoded parameters stay underneath the sentence.
    assert_eq!(events[1]["name"], json!("TransferWithMemo"));
    assert_eq!(events[1]["params"][2]["value"], json!("1500000"));
}

#[tokio::test]
async fn a_failed_transaction_says_why() {
    let (_dir, base) = serve().await;
    let page = get_json(&base, &format!("/tx/{FAILED_TX_HASH}?")).await;

    assert_eq!(page["summary"]["tone"], json!("failure"));
    assert_eq!(
        page["summary"]["headline"],
        json!(
            "Transfer failed: insufficient pathUSD balance. \
               Available 1 pathUSD, required 2.5 pathUSD."
        )
    );
    assert_eq!(
        page["summary"]["error"],
        json!("insufficient pathUSD balance")
    );
}

#[tokio::test]
async fn the_fee_breakdown_splits_the_gas_cost() {
    let (_dir, base) = serve().await;
    let page = get_json(&base, &format!("/tx/{TX_HASH}?")).await;
    let fee = &page["fee_breakdown"];

    // 21,000 gas at 20 gwei, of which 1 gwei per gas was burnt as base fee.
    assert_eq!(fee["total_wei"], json!("420000000000000"));
    assert_eq!(fee["burnt_wei"], json!("21000000000000"));
    assert_eq!(fee["tip_wei"], json!("399000000000000"));
    // What was actually charged is a TIP-20 amount, not wei.
    assert_eq!(fee["charged"], json!("0.0025 pathUSD"));
}

/// Every template the transaction page can render must render.
#[tokio::test]
async fn every_transaction_tab_renders() {
    let (_dir, base) = serve().await;
    for hash in [TX_HASH, FAILED_TX_HASH] {
        for tab in ["overview", "balances", "calls", "events", "raw"] {
            let path = format!("/tx/{hash}?tab={tab}");
            let response = reqwest::get(format!("{base}{path}"))
                .await
                .unwrap_or_else(|e| panic!("GET {path}: {e}"));
            assert_eq!(response.status(), 200, "GET {path}");
            let body = response.text().await.expect("body");
            if tab == "overview" {
                assert!(body.contains("summary-headline"), "GET {path}");
            }
            if hash == TX_HASH && tab == "events" {
                assert!(body.contains("invoice 42"), "the memo is shown");
            }
        }
    }
}

/// A log no ABI describes must still list, rather than disappearing from a
/// page that claims to show every event.
#[tokio::test]
async fn an_unknown_log_still_appears() {
    let (_dir, base) = serve().await;
    let page = get_json(&base, &format!("/tx/{UNKNOWN_LOG_TX_HASH}?")).await;

    let events = page["events"].as_array().expect("events");
    assert_eq!(events.len(), 1);
    assert!(events[0]["name"].is_null(), "no ABI names this event");
    assert!(
        events[0].get("known").is_none(),
        "and nothing to say about it"
    );
    assert_eq!(events[0]["params"][0]["name"], json!("data"));
    // With nothing interpretable, the summary says exactly that.
    assert_eq!(page["summary"]["headline"], json!("Transaction succeeded."));
}
