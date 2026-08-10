use nvnmchain_explorer::decoder::{
    checksum_address, decode_event, decode_function_call, extract_balance_changes, extract_calls,
    flatten_trace, TRANSFER_TOPIC,
};
use nvnmchain_explorer::models::{Transaction, TransferEvent};
use nvnmchain_explorer::parse::{parse_block, parse_transaction};
use nvnmchain_explorer::tokens::format_token_amount;
use serde_json::json;

fn transfer_calldata(to: &str, amount: u128) -> String {
    format!(
        "0xa9059cbb000000000000000000000000{}{:064x}",
        to.trim_start_matches("0x"),
        amount
    )
}

#[test]
fn checksum_is_stable_and_40_hex() {
    let a = checksum_address("0x20c0000000000000000000000000000000000000");
    assert_eq!(a.len(), 42);
    assert_eq!(checksum_address(&a), a);
    let b = checksum_address("0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
    assert_eq!(b.len(), 42);
    assert_eq!(checksum_address(&b), b);
}

#[test]
fn decode_transfer_call() {
    let data = transfer_calldata("0x0000000000000000000000000000000000000001", 100);
    let call = decode_function_call(&data).expect("decoded");
    assert_eq!(call.name.as_deref(), Some("transfer"));
    assert_eq!(call.signature.as_deref(), Some("transfer(address,uint256)"));
    assert_eq!(call.selector, "0xa9059cbb");
    assert_eq!(call.params.len(), 2);
    assert_eq!(call.params[0].name, "to");
    assert_eq!(call.params[1].name, "amount");
    assert_eq!(call.params[1].value, "100");
}

#[test]
fn decode_approve_and_unknown() {
    let approve = format!(
        "0x095ea7b3{}{}{:064x}",
        "00".repeat(12),
        "ab".repeat(20),
        42u64
    );
    let call = decode_function_call(&approve).expect("decoded");
    assert_eq!(call.name.as_deref(), Some("approve"));
    assert_eq!(call.params[1].value, "42");

    let unknown = decode_function_call("0xdeadbeef1234").expect("unknown decoded");
    assert!(unknown.name.is_none());
    assert_eq!(unknown.selector, "0xdeadbeef");
    assert_eq!(unknown.raw_args, "0x1234");

    assert!(decode_function_call("0x").is_none());
    assert!(decode_function_call("0x1234").is_none());
}

#[test]
fn decode_transfer_event() {
    let from = format!("0x{}", "11".repeat(20));
    let to = format!("0x{}", "22".repeat(20));
    let log = json!({
        "address": format!("0x{}", "cc".repeat(20)),
        "topics": [
            TRANSFER_TOPIC,
            format!("0x{}{}", "00".repeat(12), from.trim_start_matches("0x")),
            format!("0x{}{}", "00".repeat(12), to.trim_start_matches("0x")),
        ],
        "data": format!("0x{:064x}", 1234u64),
        "logIndex": "0x0",
    });
    let event = decode_event(&log).expect("decoded event");
    assert_eq!(event.name.as_deref(), Some("Transfer"));
    assert_eq!(event.params.len(), 3);
    assert_eq!(event.params[0].name, "from");
    assert!(event.params[0].indexed);
    assert_eq!(event.params[2].value, "1234");
    assert_eq!(event.params[1].value, checksum_address(&to));
}

#[test]
fn flatten_trace_nested() {
    let trace = json!({
        "type": "CALL",
        "from": format!("0x{}", "aa".repeat(20)),
        "to": format!("0x{}", "bb".repeat(20)),
        "input": "0x",
        "value": "0x0",
        "gas": "0x186a0",
        "gasUsed": "0xcccc",
        "calls": [
            {"type": "STATICCALL", "from": format!("0x{}", "bb".repeat(20)), "to": format!("0x{}", "cc".repeat(20)), "input": "0x", "value": "0x0", "gas": "0x5208", "gasUsed": "0x100"},
            {"type": "CALL", "from": format!("0x{}", "bb".repeat(20)), "to": format!("0x{}", "dd".repeat(20)), "input": "0x", "value": "0x01", "gas": "0x7530", "gasUsed": "0x2000"}
        ]
    });
    let flat = flatten_trace(&trace);
    assert_eq!(flat.len(), 3);
    assert_eq!(flat[0]["depth"], 0);
    assert_eq!(flat[1]["depth"], 1);
    assert_eq!(flat[2]["depth"], 1);
    assert_eq!(flat[1]["type"], "STATICCALL");
    assert_eq!(flat[0]["children"], json!([1, 2]));
    assert_eq!(flat[1]["gas"], "21000");
    assert_eq!(flat[2]["value"], "1");
}

#[test]
fn format_amounts() {
    assert_eq!(format_token_amount("0", 18), "0");
    assert_eq!(format_token_amount("1000000000000000000", 18), "1");
    assert_eq!(format_token_amount("1234567890123456789", 18), "1.234567");
    assert_eq!(format_token_amount("5000000000000000000", 18), "5");
}

#[test]
fn parse_block_and_transaction() {
    let raw_block = json!({
        "number": "0x10",
        "hash": format!("0x{}", "ab".repeat(32)),
        "parentHash": format!("0x{}", "cd".repeat(32)),
        "timestamp": "0x64",
        "gasUsed": "0x5208",
        "gasLimit": "0x1c9c380",
        "miner": format!("0x{}", "ee".repeat(20)),
        "transactions": [
            {
                "hash": format!("0x{}", "ff".repeat(32)),
                "blockNumber": "0x10",
                "blockHash": format!("0x{}", "ab".repeat(32)),
                "transactionIndex": "0x0",
                "from": format!("0x{}", "11".repeat(20)),
                "to": format!("0x{}", "22".repeat(20)),
                "gas": "0x5208",
                "gasPrice": "0x4a817c800",
                "value": "0xde0b6b3a7640000",
                "nonce": "0x3",
                "type": "0x0",
                "input": "0x",
                "chainId": "0x2b45"
            }
        ]
    });
    let block = parse_block(&raw_block);
    assert_eq!(block.number, 16);
    assert_eq!(block.tx_count, 1);
    assert_eq!(block.timestamp, 100);

    let tx = parse_transaction(&raw_block["transactions"][0], &block);
    assert_eq!(tx.hash, format!("0x{}", "ff".repeat(32)));
    assert_eq!(tx.block_number, 16);
    assert_eq!(tx.timestamp, 100);
}

#[test]
fn balance_changes_from_receipt() {
    let from = format!("0x{}", "11".repeat(20));
    let to = format!("0x{}", "22".repeat(20));
    let receipt = json!({
        "logs": [{
            "address": format!("0x{}", "cc".repeat(20)),
            "topics": [
                TRANSFER_TOPIC,
                format!("0x{}{}", "00".repeat(12), from.trim_start_matches("0x")),
                format!("0x{}{}", "00".repeat(12), to.trim_start_matches("0x")),
            ],
            "data": format!("0x{:064x}", 500u64),
        }]
    });
    let tx = Transaction {
        hash: "0x".into(),
        block_number: 0,
        position: 0,
        from_addr: from.clone(),
        to_addr: None,
        status: 1,
        gas_used: 0,
        base_fee: "0x0".into(),
        contract_address: None,
        fee_token: None,
        fee_amount: "0".into(),
        input: "0x".into(),
        raw: None,
        trace_data: None,
        receipt_data: None,
        timestamp: 0,
        created_at: 0,
    };
    let changes = extract_balance_changes(&receipt, &tx);
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0]["change"], "-500");
    assert_eq!(changes[1]["change"], "+500");
    assert_eq!(changes[0]["is_fee"], false);

    let calls = extract_calls(&tx, &[]);
    assert!(calls.is_empty());
}

#[test]
fn fresh_schema_has_all_columns() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("fresh.db");
    let conn = nvnmchain_explorer::db::init_db(path.to_str().unwrap()).expect("init_db");

    // The base is reset between schema iterations, so init_db creates the
    // final shape directly — no migration ladder.
    for (table, expected) in [
        (
            "blocks",
            &[
                "number",
                "hash",
                "parent_hash",
                "timestamp",
                "timestamp_ms",
                "gas_used",
                "gas_limit",
                "miner",
                "tx_count",
                "base_fee",
                "size",
                "extra_data",
                "epoch",
                "view",
                "proposer",
                "created_at",
            ][..],
        ),
        (
            "transactions",
            &[
                "hash",
                "block_number",
                "position",
                "from_addr",
                "to_addr",
                "status",
                "gas_used",
                "base_fee",
                "contract_address",
                "fee_token",
                "fee_amount",
                "input",
                "raw",
                "trace_data",
                "receipt_data",
                "timestamp",
                "created_at",
            ][..],
        ),
    ] {
        let cols: Vec<String> = {
            let mut stmt = conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .expect("pragma");
            let rows = stmt.query_map([], |r| r.get::<_, String>(1)).unwrap();
            rows.filter_map(|c| c.ok()).collect()
        };
        for column in expected {
            assert!(
                cols.iter().any(|c| c == column),
                "column {table}.{column} should exist in the fresh schema"
            );
        }
    }

    for table in [
        "blocks",
        "transactions",
        "token_metadata",
        "contract_labels",
        "transfer_events",
        "kv",
        "token_balances",
    ] {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |r| r.get(0),
            )
            .expect("table lookup");
        assert_eq!(count, 1, "table {table} should exist");
    }
}

#[test]
fn blob_hex_round_trip() {
    use nvnmchain_explorer::db::{self, Db};
    use std::sync::{Arc, Mutex};

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("blob.db");
    let conn = db::init_db(path.to_str().unwrap()).expect("init_db");
    let db: Db = Arc::new(Mutex::new(conn));

    // Block with one transaction (so the tx row is exercised too).
    let hash = format!("0x{}", "ab".repeat(32));
    let parent = format!("0x{}", "cd".repeat(32));
    let proposer = format!("0x{}", "11".repeat(32));
    let miner = format!("0x{}", "22".repeat(20));
    let tx_hash = format!("0x{}", "ff".repeat(32));
    let raw_block = json!({
        "number": "0x10",
        "hash": hash,
        "parentHash": parent,
        "timestamp": "0x64",
        "gasUsed": "0x5208",
        "gasLimit": "0x1c9c380",
        "miner": miner,
        "consensusContext": {"epoch": 1, "view": 2, "proposer": proposer},
        "transactions": [{
            "hash": tx_hash,
            "blockNumber": "0x10",
            "transactionIndex": "0x0",
            "from": format!("0x{}", "33".repeat(20)),
            "to": format!("0x{}", "44".repeat(20)),
            "gas": "0x5208",
            "gasPrice": "0x4a817c800",
            "value": "0x1",
            "nonce": "0x3",
            "chainId": "0x2b45",
            "type": "0x76",
            "signature": {"type": "webAuthn", "r": "0x01", "s": "0x02"},
            "calls": [{"to": format!("0x{}", "55".repeat(20)), "value": "0x0", "input": "0xa9059cbb"}],
        }]
    });
    let block = parse_block(&raw_block);
    // The indexer fills the canonical RLP encoding separately; embed the real
    // bytes of a known tempo 0x76 transaction (block 664125).
    let rlp = include_str!("../fixtures/tx_664125.rlp").trim();
    let mut tx = parse_transaction(&raw_block["transactions"][0], &block);
    tx.raw = Some(rlp.to_string());
    db::save_block_bundle(&db, &block, &[tx], &[], &[]).expect("save bundle");

    // Block reads back with identical hex, and hash lookup is case-insensitive
    // (binary storage normalizes hex case — a bonus over TEXT comparisons).
    let got = db::get_block_by_number(&db, 16).expect("block by number");
    assert_eq!(got.hash, hash);
    assert_eq!(got.parent_hash, parent);
    assert_eq!(got.proposer, proposer);
    assert_eq!(got.miner, miner);
    assert!(db::get_block_by_hash(&db, &hash).is_some());
    let upper = format!("0x{}", hash[2..].to_uppercase());
    assert!(
        db::get_block_by_hash(&db, &upper).is_some(),
        "hash lookup should be case-insensitive with BLOB storage"
    );

    // Transaction reads back: raw is the canonical RLP encoding, and the
    // display fields (calls, signature type, gas) are decoded from it at
    // runtime with the tempo primitives.
    let got_tx = db::get_transaction(&db, &tx_hash).expect("tx");
    assert_eq!(got_tx.raw.as_deref(), Some(rlp));
    let parsed = nvnmchain_explorer::decoder::parse_raw_tx(got_tx.raw.as_deref().unwrap());
    assert_eq!(parsed.sig_type.as_deref(), Some("WebAuthn"));
    assert_eq!(parsed.nonce, Some(0));
    assert_eq!(parsed.gas_limit, Some(319946));
    assert_eq!(parsed.max_fee_per_gas, Some(1200000000));
    assert_eq!(parsed.nonce_key.as_deref(), Some("0"));
    assert_eq!(parsed.calls.len(), 1);
    let call_to = parsed.calls[0]["to"].as_str().unwrap().to_lowercase();
    assert_eq!(call_to, "0x20c0000000000000000000000000000000000000");
}

#[test]
fn duplicate_bundle_is_idempotent() {
    use nvnmchain_explorer::db::{self, Db};
    use std::sync::{Arc, Mutex};

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("dedup.db");
    let conn = db::init_db(path.to_str().unwrap()).expect("init_db");
    let db: Db = Arc::new(Mutex::new(conn));

    let raw_block = json!({
        "number": "0x10",
        "hash": format!("0x{}", "ab".repeat(32)),
        "parentHash": format!("0x{}", "cd".repeat(32)),
        "timestamp": "0x64",
        "gasUsed": "0x5208",
        "gasLimit": "0x1c9c380",
        "miner": format!("0x{}", "22".repeat(20)),
        "consensusContext": {"epoch": 1, "view": 2, "proposer": format!("0x{}", "11".repeat(32))},
        "transactions": [{
            "hash": format!("0x{}", "ff".repeat(32)),
            "blockNumber": "0x10",
            "transactionIndex": "0x0",
            "from": format!("0x{}", "33".repeat(20)),
            "to": format!("0x{}", "44".repeat(20)),
            "gas": "0x5208",
            "gasPrice": "0x4a817c800",
            "value": "0x1",
            "nonce": "0x3",
            "chainId": "0x2b45",
            "type": "0x76",
            "signature": {"type": "webAuthn", "r": "0x01", "s": "0x02"},
            "calls": [{"to": format!("0x{}", "55".repeat(20)), "value": "0x0", "input": "0xa9059cbb"}],
        }]
    });
    let block = parse_block(&raw_block);
    let tx = parse_transaction(&raw_block["transactions"][0], &block);
    let token = checksum_address(&format!("0x{}", "aa".repeat(20)));
    let from = checksum_address(&format!("0x{}", "11".repeat(20)));
    let to = checksum_address(&format!("0x{}", "22".repeat(20)));
    let transfer = TransferEvent {
        id: 0,
        tx_hash: tx.hash.clone(),
        block_number: block.number,
        log_index: 0,
        token_addr: token.clone(),
        from_addr: from.clone(),
        to_addr: to.clone(),
        amount: "100".into(),
        timestamp: block.timestamp,
        created_at: 0,
    };

    // The same block written twice (what the indexer's concurrent fetch/retry
    // races can produce). Blocks and txs upsert; transfers must dedupe.
    db::save_block_bundle(
        &db,
        &block,
        std::slice::from_ref(&tx),
        std::slice::from_ref(&transfer),
        &[],
    )
    .expect("first save");
    db::save_block_bundle(
        &db,
        &block,
        std::slice::from_ref(&tx),
        std::slice::from_ref(&transfer),
        &[],
    )
    .expect("duplicate save");

    let conn = db::lock(&db);
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM transfer_events", [], |r| r.get(0))
        .expect("transfer count");
    assert_eq!(
        count, 1,
        "duplicate bundle must not duplicate transfer rows"
    );

    let unique: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_index_list('transfer_events') WHERE \"unique\"=1",
            [],
            |r| r.get(0),
        )
        .expect("unique index lookup");
    assert_eq!(
        unique, 1,
        "unique constraint on (block_number, log_index) should exist"
    );

    // Balance deltas are applied once per transfer, not once per write.
    let recipient: String = conn
        .query_row(
            "SELECT balance FROM token_balances WHERE token_addr=?1 AND holder_addr=?2",
            rusqlite::params![token, to],
            |r| r.get(0),
        )
        .expect("recipient balance");
    assert_eq!(recipient, "100", "balance must not be double-applied");
    let sender: String = conn
        .query_row(
            "SELECT balance FROM token_balances WHERE token_addr=?1 AND holder_addr=?2",
            rusqlite::params![token, from],
            |r| r.get(0),
        )
        .expect("sender balance");
    assert_eq!(sender, "-100", "sender delta applied exactly once");
}

#[test]
fn blob_storage_queries_match_text_params() {
    // Regression: `transfer_events`/`token_metadata` store addresses as raw
    // bytes (BLOBs), so any query passing a plain TEXT address silently
    // matched nothing — empty transfers tabs, empty holdings, and holder
    // counts stuck at 0. All read/update paths must bind `hex_blob`.
    use nvnmchain_explorer::db::{self, Db};
    use nvnmchain_explorer::tokens::TokenMeta;
    use std::sync::{Arc, Mutex};

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("blob-queries.db");
    let conn = db::init_db(path.to_str().unwrap()).expect("init_db");
    let db: Db = Arc::new(Mutex::new(conn));

    let raw_block = json!({
        "number": "0x10",
        "hash": format!("0x{}", "ab".repeat(32)),
        "parentHash": format!("0x{}", "cd".repeat(32)),
        "timestamp": "0x64",
        "gasUsed": "0x5208",
        "gasLimit": "0x1c9c380",
        "miner": format!("0x{}", "22".repeat(20)),
        "consensusContext": {"epoch": 1, "view": 2, "proposer": format!("0x{}", "11".repeat(32))},
        "transactions": [{
            "hash": format!("0x{}", "ff".repeat(32)),
            "blockNumber": "0x10",
            "transactionIndex": "0x0",
            "from": format!("0x{}", "33".repeat(20)),
            "to": format!("0x{}", "44".repeat(20)),
            "gas": "0x5208",
            "gasPrice": "0x4a817c800",
            "value": "0x1",
            "nonce": "0x3",
            "chainId": "0x2b45",
            "type": "0x76",
            "signature": {"type": "webAuthn", "r": "0x01", "s": "0x02"},
            "calls": [{"to": format!("0x{}", "55".repeat(20)), "value": "0x0", "input": "0xa9059cbb"}],
        }]
    });
    let block = parse_block(&raw_block);
    let tx = parse_transaction(&raw_block["transactions"][0], &block);
    let token = checksum_address(&format!("0x{}", "aa".repeat(20)));
    let from = checksum_address(&format!("0x{}", "11".repeat(20)));
    let to = checksum_address(&format!("0x{}", "22".repeat(20)));
    let transfer = TransferEvent {
        id: 0,
        tx_hash: tx.hash.clone(),
        block_number: block.number,
        log_index: 0,
        token_addr: token.clone(),
        from_addr: from.clone(),
        to_addr: to.clone(),
        amount: "1000000".into(),
        timestamp: block.timestamp,
        created_at: 0,
    };
    let meta = TokenMeta {
        address: token.clone(),
        name: "Path USD".into(),
        symbol: "pathUSD".into(),
        decimals: 6,
        currency: "USD".into(),
        total_supply: "1000000".into(),
    };
    db::save_block_bundle(
        &db,
        &block,
        std::slice::from_ref(&tx),
        std::slice::from_ref(&transfer),
        std::slice::from_ref(&meta),
    )
    .expect("save bundle");

    // Address transfers tab: query binds the address as raw bytes.
    let addr_transfers = db::get_address_transfers(&db, &to, 1, 25);
    assert_eq!(addr_transfers.len(), 1, "address transfers must be found");

    // Token transfers list + count agree.
    let token_transfers = db::get_token_transfers(&db, &token, 1, 25);
    assert_eq!(token_transfers.len(), 1, "token transfers must be found");
    assert_eq!(db::get_token_transfer_count(&db, &token), 1);

    // Holdings: balance row exists (TEXT) and metadata resolves (BLOB via
    // hex_blob IN-clause), so the joined holding is returned.
    let holdings = db::get_address_holdings(&db, &to);
    assert_eq!(holdings.len(), 1, "holdings must be resolved");
    assert_eq!(holdings[0]["symbol"], "pathUSD");
    assert_eq!(holdings[0]["formatted"], "1");

    // Holder count: live count over token_balances plus the cached
    // token_metadata.holder_count refreshed with a hex_blob key. Both the
    // sender (negative balance) and the recipient hold a row, so it's 2.
    assert_eq!(db::get_token_holder_count(&db, &token), 2);
    let meta_back = db::get_token_metadata(&db, &token).expect("token metadata");
    assert_eq!(
        meta_back.holder_count, 2,
        "token_metadata.holder_count must be refreshed via BLOB key"
    );

    // Startup backfill repairs stale holder counts.
    {
        let conn = db::lock(&db);
        conn.execute(
            "UPDATE token_metadata SET holder_count=0 WHERE address=?1",
            rusqlite::params![token],
        )
        .expect("stale holder count");
        db::sync_holder_counts(&conn).expect("sync holder counts");
    }
    let meta_back = db::get_token_metadata(&db, &token).expect("token metadata");
    assert_eq!(
        meta_back.holder_count, 2,
        "sync_holder_counts must backfill stale counts"
    );
}

#[test]
fn huge_page_numbers_do_not_panic() {
    use nvnmchain_explorer::db::{self, Db};
    use std::sync::{Arc, Mutex};

    // `page` is user input; u32 offset math overflowed (a panic under
    // debug assertions, a garbage offset in release).
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("pages.db");
    let db: Db = Arc::new(Mutex::new(
        db::init_db(path.to_str().unwrap()).expect("init_db"),
    ));

    assert!(
        db::get_address_transactions(&db, &format!("0x{}", "11".repeat(20)), u32::MAX, 25)
            .is_empty()
    );
    assert!(
        db::get_token_transfers(&db, &format!("0x{}", "aa".repeat(20)), u32::MAX, 25).is_empty()
    );
    assert!(db::get_all_tokens(&db, u32::MAX, 25).is_empty());
}
