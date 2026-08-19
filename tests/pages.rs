//! The pages, end to end.
//!
//! These render the real templates against a real database, so a context key a
//! handler stops sending fails a test rather than a page view. Nothing reaches
//! the network: fixtures carry their own traces and failure reasons, the RPC
//! points at a closed port, and the signature directory is stubbed.

use std::sync::{Arc, Mutex};

use nvnmchain_explorer::db::{self, Db};
use nvnmchain_explorer::decoder::{checksum_address, keccak256, keccak_hex, TRANSFER_TOPIC};
use nvnmchain_explorer::models::{Block, BlockBundle, Transaction, TransferEvent};
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

/// How many transfers the sender is party to — more than one page of 25, so
/// a count taken from the current page is visibly wrong.
const TRANSFER_COUNT: i64 = 30;

/// A block of transfers out of the sender, one unit to each recipient, which
/// makes the holders list and its shares easy to state exactly. The sender
/// was never funded, so its balance goes negative — a non-holding.
fn transfer_bundle() -> BlockBundle {
    let mut block = block();
    block.number = 101;
    block.hash = format!("0x{}", "ef".repeat(32));
    let transfers = (0..TRANSFER_COUNT)
        .map(|i| TransferEvent {
            id: 0,
            tx_hash: TX_HASH.into(),
            block_number: 101,
            log_index: i,
            token_addr: checksum_address(TOKEN),
            from_addr: checksum_address(SENDER),
            to_addr: checksum_address(&format!("0x{:040x}", i + 1)),
            amount: "1000000".into(),
            timestamp: 1_700_000_000,
            created_at: 0,
        })
        .collect();
    BlockBundle {
        block,
        txs: Vec::new(),
        transfers,
        anchored: Vec::new(),
        tokens: Vec::new(),
        registries: Vec::new(),
    }
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
    // More transfers than fit on a page, so the counts and the pager have
    // something to be wrong about.
    db::save_block_bundle(&db, &transfer_bundle()).expect("transfers");

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

// ---------------------------------------------------------------------------
// Address and token pages
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_address_counts_every_transfer_not_just_the_page() {
    let (_dir, base) = serve().await;
    let sender = checksum_address(SENDER);

    // The count is of the whole history; a page holds 25 of them.
    for tab in ["transactions", "transfers"] {
        let page = get_json(&base, &format!("/address/{sender}?tab={tab}")).await;
        assert_eq!(
            page["transfer_count"],
            json!(TRANSFER_COUNT),
            "transfer count on the {tab} tab"
        );
        assert_eq!(page["tx_count"], json!(3), "tx count on the {tab} tab");
    }

    // And the pager knows there is a second page to reach.
    let transfers = get_json(&base, &format!("/address/{sender}?tab=transfers")).await;
    assert_eq!(transfers["total_pages"], json!(2));
    assert_eq!(transfers["html_transactions"].as_array().unwrap().len(), 25);

    let second = get_json(&base, &format!("/address/{sender}?tab=transfers&page=2")).await;
    assert_eq!(
        second["html_transactions"].as_array().unwrap().len(),
        (TRANSFER_COUNT - 25) as usize
    );
}

#[tokio::test]
async fn a_token_lists_its_holders() {
    let (_dir, base) = serve().await;
    let token = checksum_address(TOKEN);
    let page = get_json(&base, &format!("/token/{token}?tab=holders")).await;

    // Every recipient holds one unit; the sender's balance is negative, and a
    // negative balance is not a holding, so the list is exactly the recipients.
    assert_eq!(page["holders"], json!(TRANSFER_COUNT));
    let rows = page["holder_rows"].as_array().expect("holder rows");
    assert_eq!(rows.len(), 25, "one page of holders");
    assert_eq!(rows[0]["rank"], json!(1));
    assert_eq!(rows[0]["formatted"], json!("1"));
    // 1,000,000 of a 1,000,000,000 supply.
    assert_eq!(rows[0]["share"], json!("0.1000"));
    assert_eq!(page["total_pages"], json!(2));

    let second = get_json(&base, &format!("/token/{token}?tab=holders&page=2")).await;
    assert_eq!(
        second["holder_rows"].as_array().unwrap()[0]["rank"],
        json!(26)
    );
}

/// Addresses are copied all day; every page that shows one must offer it.
#[tokio::test]
async fn addresses_are_copyable_and_labelled() {
    let (_dir, base) = serve().await;
    let token = checksum_address(TOKEN);

    let body = reqwest::get(format!("{base}/tx/{TX_HASH}"))
        .await
        .expect("tx page")
        .text()
        .await
        .expect("body");
    assert!(
        body.contains(&format!("data-copy=\"{TX_HASH}\"")),
        "the transaction hash is copyable"
    );
    assert!(
        body.contains(&format!("data-copy=\"{token}\"")),
        "the addresses on it are copyable"
    );
    // And an address the chain has a name for carries it.
    assert!(body.contains("addr-tag"), "known addresses are tagged");
    assert!(body.contains(">pathUSD<"), "the token is named");
}

/// A precompile is named by the built-in table rather than by the database.
#[tokio::test]
async fn precompiles_are_labelled_without_a_database_row() {
    use nvnmchain_explorer::web::address_label;

    let (_dir, db) = temp_db("labels.db");
    assert_eq!(
        address_label(&db, "0xfeEC000000000000000000000000000000000000").as_deref(),
        Some("Fee Manager")
    );
    // A TIP-1022 deposit address says so — nothing else on the page would.
    let deposit = nvnmchain_explorer::tempo_address::virtual_address(&[0xab; 4], &[1; 6]);
    assert_eq!(
        address_label(&db, &deposit).as_deref(),
        Some("Virtual 0xabababab")
    );
    assert_eq!(
        address_label(&db, "0x1111111111111111111111111111111111111111"),
        None
    );
}

// ---------------------------------------------------------------------------
// Search suggestions
// ---------------------------------------------------------------------------

async fn suggest(base: &str, query: &str) -> Vec<Value> {
    let body: Value = reqwest::get(format!("{base}/api/search?q={}", urlencoding_encode(query)))
        .await
        .unwrap_or_else(|e| panic!("suggest {query}: {e}"))
        .json()
        .await
        .unwrap_or_else(|e| panic!("suggest {query} json: {e}"));
    body["results"].as_array().cloned().unwrap_or_default()
}

/// Minimal percent-encoding for the few characters the fixtures use.
fn urlencoding_encode(value: &str) -> String {
    value.replace('#', "%23").replace(' ', "%20")
}

#[tokio::test]
async fn the_search_box_suggests_what_is_being_typed() {
    let (_dir, base) = serve().await;

    // A token by name, before the whole name is typed.
    let by_name = suggest(&base, "path").await;
    assert_eq!(by_name[0]["type"], json!("token"));
    assert_eq!(by_name[0]["label"], json!("pathUSD"));
    assert_eq!(
        by_name[0]["url"],
        json!(format!("/token/{}", checksum_address(TOKEN)))
    );

    // A precompile by name, which no database row describes. Matched as a
    // substring, so a partial second word still finds it.
    for term in ["fee", "fee man", "manager"] {
        let precompile = suggest(&base, term).await;
        assert!(
            precompile
                .iter()
                .any(|r| r["label"] == json!("Fee Manager") && r["type"] == json!("precompile")),
            "`{term}` got {precompile:#?}"
        );
    }

    // A block number, and only one the chain has reached.
    let block = suggest(&base, "100").await;
    assert_eq!(block[0]["type"], json!("block"));
    assert_eq!(block[0]["url"], json!("/block/100"));
    assert!(suggest(&base, "999999").await.is_empty());
    // The same, typed the way a reader refers to a block.
    assert_eq!(suggest(&base, "#100").await[0]["url"], json!("/block/100"));

    // A transaction hash resolves whether or not it is indexed — being told
    // "not found" on the page beats being offered nothing.
    let indexed = suggest(&base, TX_HASH).await;
    assert_eq!(indexed[0]["type"], json!("transaction"));
    assert_eq!(indexed[0]["sublabel"], json!("Block #100"));
    let unknown = suggest(&base, &format!("0x{}", "12".repeat(32))).await;
    assert_eq!(unknown[0]["sublabel"], json!("Not indexed yet"));

    // An address is offered as itself; a token address as both.
    let token_address = suggest(&base, TOKEN).await;
    assert_eq!(token_address[0]["type"], json!("token"));
    assert_eq!(token_address[1]["type"], json!("address"));
    assert_eq!(token_address[1]["label"], json!("pathUSD"));

    assert!(suggest(&base, "").await.is_empty());
}

/// A TIP-1022 deposit address is searchable, and says what it is — nothing
/// else in the explorer would tell the reader that.
#[tokio::test]
async fn a_virtual_address_is_recognised_in_search() {
    let (_dir, base) = serve().await;
    let address = nvnmchain_explorer::tempo_address::virtual_address(&[0xab; 4], &[0xcd; 6]);

    let results = suggest(&base, &address).await;
    assert_eq!(results[0]["type"], json!("address"));
    assert_eq!(results[0]["label"], json!("Virtual 0xabababab"));
    assert_eq!(
        results[0]["sublabel"],
        json!("Virtual address · user tag 0xcdcdcdcdcdcd")
    );
}

/// A search term is bound, never interpolated: a wildcard a reader types is a
/// character to match, not a pattern that matches everything.
#[tokio::test]
async fn search_terms_are_never_patterns() {
    let (_dir, base) = serve().await;
    // Leaked wildcards would make these match pathUSD; escaped, they cannot.
    assert!(suggest(&base, "%path%").await.is_empty());
    assert!(suggest(&base, "_ath").await.is_empty());
    assert!(suggest(&base, "' OR 1=1 --").await.is_empty());
    // And one character matches too much to rank at all.
    assert!(suggest(&base, "p").await.is_empty());
}
