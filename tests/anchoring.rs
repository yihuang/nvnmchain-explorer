//! Indexing the `Anchored` log and reading the envelopes anchored through it.
//!
//! The precompile keeps only the head per `(namespace, key)`, so the indexed
//! table is the entire history — these tests pin both halves: the log becomes a
//! row, and a row that is an `AnchoringRegistry` envelope reads as itself.
//!
//! The payloads below are what `AnchoringRegistry.sol` actually emits, dumped
//! from a forge run against the shipped contracts rather than re-encoded here;
//! a decoder checked against its own guess proves nothing.

use std::sync::{Arc, Mutex};

use nvnmchain_explorer::anchoring::{
    decode_envelope, is_self_verifying, ANCHORED_SIGNATURE, ANCHORED_TOPIC, ANCHORING_ADDRESS,
};
use nvnmchain_explorer::db::{self, Db};
use nvnmchain_explorer::decoder::{decode_event, decode_function_call, keccak_hex};
use nvnmchain_explorer::indexer::anchored_event;
use nvnmchain_explorer::models::{AnchoredEvent, Block, Transaction};
use serde_json::{json, Value};

/// The `AnchoringRegistry` proxy that emitted the fixtures — its address is the
/// namespace, since the precompile partitions by caller.
const REGISTRY: &str = "0x44DA54d3f5416A9Ae699d54EcB83c3043c41319E";

const REGISTRY_KEY: &str = "0x173602657603c73bdfa5393aba98fa9e899f7c58898ea2a7d444639768d549d4";
const REGISTRY_COMMITMENT: &str =
    "0xf6f0bcff7207ce080ce3900e9e8c378a31a0faa37441f4ce9f222929db9b9b0e";
const REGISTRY_METADATA: &str = "0x\
0000000000000000000000000000000000000000000000000000000000000001\
00000000000000000000000000000000000000000000000000000000000000c0\
0000000000000000000000000000000000000000000000000000000000000100\
0000000000000000000000000000000000000000000000000000000000000140\
0000000000000000000000002190d584e30f4a2396c1487aa784428f2068cbe8\
0000000000000000000000000000000000000000000000000000000000000001\
0000000000000000000000000000000000000000000000000000000000000004\
446f637300000000000000000000000000000000000000000000000000000000\
000000000000000000000000000000000000000000000000000000000000000d\
696e7465726e616c20646f637300000000000000000000000000000000000000\
0000000000000000000000000000000000000000000000000000000000000002\
7b7d000000000000000000000000000000000000000000000000000000000000";

const RECORD_KEY: &str = "0x50533b0f6489b8e319f1bd0705b9595a20a11ae5e9e54d1913d081c0232a880e";
const RECORD_COMMITMENT: &str =
    "0x9b60783ba653e4f1b87ddf01d26063b937c1a678365a21f4eb7d21abfaa349f3";
const RECORD_METADATA: &str = "0x\
0000000000000000000000000000000000000000000000000000000000000001\
0000000000000000000000000000000000000000000000000000000000000001\
0000000000000000000000000000000000000000000000000000000000000001\
0000000000000000000000000000000000000000000000000000000000000100\
0000000000000000000000000000000000000000000000000000000000000140\
0000000000000000000000000000000000000000000000000000000000000180\
00000000000000000000000000000000000000000000000000000000000001c0\
0000000000000000000000000000000000000000000000000000000000000001\
000000000000000000000000000000000000000000000000000000000000000a\
697066733a2f2f63696400000000000000000000000000000000000000000000\
0000000000000000000000000000000000000000000000000000000000000005\
3078616263000000000000000000000000000000000000000000000000000000\
0000000000000000000000000000000000000000000000000000000000000006\
7368613235360000000000000000000000000000000000000000000000000000\
0000000000000000000000000000000000000000000000000000000000000002\
7b7d000000000000000000000000000000000000000000000000000000000000";

const STATUS_KEY: &str = "0x6150b08c0c9138bbfbfff7e0da81375120124ecd0ec54b8bb8cb4a109966aae8";
const STATUS_COMMITMENT: &str =
    "0xa69b155a1ebf01b9f3bccc9a86e18fd1054bd2ab02e6aecc43184fa03662dbe8";
const STATUS_METADATA: &str = "0x\
0000000000000000000000000000000000000000000000000000000000000001\
0000000000000000000000000000000000000000000000000000000000000001\
0000000000000000000000000000000000000000000000000000000000000001\
00000000000000000000000000000000000000000000000000000000000000a0\
0000000000000000000000000000000000000000000000000000000000000001\
0000000000000000000000000000000000000000000000000000000000000008\
617070726f766564000000000000000000000000000000000000000000000000";

fn field(envelope: &nvnmchain_explorer::anchoring::Envelope, name: &str) -> String {
    envelope
        .fields
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("field {name} missing from {} envelope", envelope.kind))
        .value
        .clone()
}

// ---------------------------------------------------------------------------
// Envelopes
// ---------------------------------------------------------------------------

#[test]
fn anchored_topic_matches_the_signature() {
    assert_eq!(keccak_hex(ANCHORED_SIGNATURE.as_bytes()), ANCHORED_TOPIC);
}

#[test]
fn registry_envelope_decodes() {
    let env = decode_envelope(REGISTRY_KEY, REGISTRY_METADATA).expect("registry envelope");
    assert_eq!(env.kind, "registry");
    assert_eq!(field(&env, "id"), "1");
    assert_eq!(field(&env, "name"), "Docs");
    assert_eq!(field(&env, "description"), "internal docs");
    assert_eq!(
        field(&env, "creator"),
        "0x2190d584E30F4a2396C1487Aa784428f2068CBE8"
    );
    assert_eq!(env.summary, "Registry #1 — Docs");
}

#[test]
fn record_envelope_decodes() {
    let env = decode_envelope(RECORD_KEY, RECORD_METADATA).expect("record envelope");
    assert_eq!(env.kind, "record");
    assert_eq!(field(&env, "registry_id"), "1");
    assert_eq!(field(&env, "uri"), "ipfs://cid");
    assert_eq!(field(&env, "checksum_algo"), "sha256");
    assert_eq!(env.summary, "Record #1 v1 — 0xabc");
}

#[test]
fn status_envelope_decodes() {
    let env = decode_envelope(STATUS_KEY, STATUS_METADATA).expect("status envelope");
    assert_eq!(env.kind, "status");
    assert_eq!(field(&env, "status"), "approved");
    // The sequence number is what makes re-asserting the same status a fresh anchor.
    assert_eq!(field(&env, "seq"), "1");
    assert_eq!(env.summary, "Status of record #1 v1 — approved");
}

#[test]
fn envelopes_are_identified_by_the_key_they_are_anchored_under() {
    // The payloads carry nothing naming their shape; the key does, and each is
    // `keccak256(abi.encode(kind, ids…))` over ids the payload itself repeats.
    // Under any other key the same bytes are just bytes.
    assert!(decode_envelope(RECORD_KEY, REGISTRY_METADATA).is_none());
    assert!(decode_envelope(STATUS_KEY, RECORD_METADATA).is_none());
    let wrong = format!("0x{}", "11".repeat(32));
    assert!(decode_envelope(&wrong, STATUS_METADATA).is_none());
}

#[test]
fn foreign_payloads_are_not_envelopes() {
    // Anything may be anchored, so a payload that is not an envelope is the
    // ordinary case, not an error.
    for payload in [
        "0x",
        "0xdeadbeef",
        &format!("0x{}", "ab".repeat(64)),
        // Registry-shaped head, but the tail a string offset points at is absent.
        &format!("0x{}", "00".repeat(32 * 6)),
    ] {
        assert!(
            decode_envelope(REGISTRY_KEY, payload).is_none(),
            "payload {payload} should not decode as an envelope"
        );
    }
}

#[test]
fn anchor_and_hash_payloads_are_self_verifying() {
    // `anchorAndHash` commits to `keccak256(metadata)`, which is what every
    // AnchoringRegistry write uses.
    assert!(is_self_verifying(REGISTRY_COMMITMENT, REGISTRY_METADATA));
    assert!(is_self_verifying(RECORD_COMMITMENT, RECORD_METADATA));
    assert!(is_self_verifying(STATUS_COMMITMENT, STATUS_METADATA));
    assert!(!is_self_verifying(REGISTRY_COMMITMENT, RECORD_METADATA));
}

#[test]
fn precompile_calls_decode() {
    // Selectors come from the canonical signatures, so a typo here fails loudly
    // rather than silently mislabelling calldata.
    let key = "11".repeat(32);
    let commitment = "22".repeat(32);
    let calldata = format!("0x0a3bd8ec{key}{commitment}{:064x}{:064x}", 0x60, 0);
    let call = decode_function_call(&calldata).expect("anchor call");
    assert_eq!(call.name.as_deref(), Some("anchor"));
    assert_eq!(call.params[0].value, format!("0x{key}"));
    assert_eq!(call.params[1].value, format!("0x{commitment}"));
}

// ---------------------------------------------------------------------------
// Indexing
// ---------------------------------------------------------------------------

/// An `Anchored` log as the node reports it: caller and key indexed, the
/// commitment and payload ABI-encoded in `data`.
fn anchored_log(caller: &str, key: &str, commitment: &str, metadata: &str) -> Value {
    let raw = hex::decode(metadata.strip_prefix("0x").unwrap_or(metadata)).expect("metadata hex");
    let mut data = String::from("0x");
    data.push_str(commitment.strip_prefix("0x").unwrap_or(commitment));
    data.push_str(&format!("{:064x}", 0x40)); // offset of the `bytes` tail
    data.push_str(&format!("{:064x}", raw.len()));
    let mut padded = hex::encode(&raw);
    while !padded.len().is_multiple_of(64) {
        padded.push('0');
    }
    data.push_str(&padded);
    json!({
        "address": ANCHORING_ADDRESS,
        "topics": [
            ANCHORED_TOPIC,
            format!("0x{}{}", "00".repeat(12), caller.trim_start_matches("0x").to_lowercase()),
            key,
        ],
        "data": data,
        "logIndex": "0x0",
    })
}

fn event_from_log(log: &Value, tx: &Transaction, log_index: i64) -> AnchoredEvent {
    let decoded = decode_event(log).expect("decoded log");
    assert_eq!(decoded.name.as_deref(), Some("Anchored"));
    anchored_event(&decoded, tx, log_index).expect("anchored row")
}

fn test_block(number: i64) -> Block {
    Block {
        number,
        hash: format!("0x{:064x}", number),
        parent_hash: format!("0x{:064x}", number - 1),
        timestamp: 1_700 + number,
        timestamp_ms: (1_700 + number) * 1000,
        gas_used: 0,
        gas_limit: 0,
        base_fee: "0".into(),
        size: 0,
        extra_data: String::new(),
        epoch: 0,
        view: 0,
        proposer: format!("0x{}", "00".repeat(20)),
        miner: format!("0x{}", "00".repeat(20)),
        tx_count: 1,
        created_at: 0,
    }
}

fn test_tx(block: &Block) -> Transaction {
    Transaction {
        hash: format!("0x{:064x}", block.number * 7),
        block_number: block.number,
        position: 0,
        from_addr: format!("0x{}", "33".repeat(20)),
        to_addr: Some(ANCHORING_ADDRESS.to_string()),
        status: 1,
        gas_used: 0,
        base_fee: "0".into(),
        contract_address: None,
        fee_token: None,
        fee_amount: "0".into(),
        input: "0x".into(),
        raw: None,
        trace_data: None,
        receipt_data: None,
        timestamp: block.timestamp,
        created_at: 0,
    }
}

fn temp_db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("anchoring.db");
    let conn = db::init_db(path.to_str().unwrap()).expect("init db");
    (dir, Arc::new(Mutex::new(conn)))
}

/// Index one anchor into `db` and return the row that was written.
fn index_anchor(
    db: &Db,
    number: i64,
    key: &str,
    commitment: &str,
    metadata: &str,
) -> AnchoredEvent {
    let block = test_block(number);
    let tx = test_tx(&block);
    let log = anchored_log(REGISTRY, key, commitment, metadata);
    let event = event_from_log(&log, &tx, 0);
    db::save_block_bundle(db, &block, &[tx], &[], std::slice::from_ref(&event), &[])
        .expect("save bundle");
    event
}

#[test]
fn anchored_log_becomes_a_row() {
    let (_dir, db) = temp_db();
    let event = index_anchor(
        &db,
        100,
        REGISTRY_KEY,
        REGISTRY_COMMITMENT,
        REGISTRY_METADATA,
    );
    assert_eq!(event.namespace, REGISTRY);

    let history = db::get_key_history(&db, REGISTRY, REGISTRY_KEY);
    assert_eq!(history.len(), 1);
    let row = &history[0];
    assert_eq!(row.namespace, REGISTRY);
    assert_eq!(row.key, REGISTRY_KEY);
    assert_eq!(row.commitment, REGISTRY_COMMITMENT);
    assert_eq!(row.metadata, REGISTRY_METADATA);
    assert_eq!(row.block_number, 100);
    assert_eq!(row.timestamp, 1_800);
    assert_eq!(db::count_anchored(&db), 1);
}

#[test]
fn logs_from_other_contracts_are_not_anchors() {
    let block = test_block(101);
    let tx = test_tx(&block);
    let foreign = json!({
        "address": format!("0x{}", "cc".repeat(20)),
        "topics": [format!("0x{}", "de".repeat(32))],
        "data": "0x",
        "logIndex": "0x0",
    });
    let decoded = decode_event(&foreign).expect("decoded");
    assert_ne!(decoded.name.as_deref(), Some("Anchored"));
    // A log that claims the signature but carries no key/commitment yields no row.
    let truncated = json!({
        "address": ANCHORING_ADDRESS,
        "topics": [ANCHORED_TOPIC],
        "data": "0x",
        "logIndex": "0x0",
    });
    let decoded = decode_event(&truncated).expect("decoded");
    assert!(anchored_event(&decoded, &tx, 0).is_none());
}

#[test]
fn reindexing_a_block_does_not_duplicate() {
    let (_dir, db) = temp_db();
    index_anchor(
        &db,
        102,
        REGISTRY_KEY,
        REGISTRY_COMMITMENT,
        REGISTRY_METADATA,
    );
    index_anchor(
        &db,
        102,
        REGISTRY_KEY,
        REGISTRY_COMMITMENT,
        REGISTRY_METADATA,
    );
    assert_eq!(db::count_anchored(&db), 1);
    assert_eq!(db::get_key_history(&db, REGISTRY, REGISTRY_KEY).len(), 1);
}

#[test]
fn history_is_newest_first_and_the_head_is_the_last_write() {
    let (_dir, db) = temp_db();
    let key = format!("0x{}", "55".repeat(32));
    index_anchor(&db, 200, &key, &format!("0x{}", "01".repeat(32)), "0x");
    index_anchor(&db, 201, &key, &format!("0x{}", "02".repeat(32)), "0x");

    let history = db::get_key_history(&db, REGISTRY, &key);
    assert_eq!(
        history.iter().map(|r| r.block_number).collect::<Vec<_>>(),
        vec![201, 200]
    );
    assert_eq!(history[0].commitment, format!("0x{}", "02".repeat(32)));

    // The namespace listing shows that head, not the superseded revision.
    let keys = db::get_namespace_keys(&db, REGISTRY, 1, 25);
    let listed = keys
        .iter()
        .find(|r| r["key"] == json!(key))
        .expect("key listed");
    assert_eq!(
        listed["commitment"],
        json!(format!("0x{}", "02".repeat(32)))
    );
    assert_eq!(listed["revisions"], json!(2));
}

#[test]
fn namespaces_are_partitioned_by_caller() {
    let (_dir, db) = temp_db();
    let other = format!("0x{}", "b2".repeat(20));
    let key = format!("0x{}", "66".repeat(32));

    let block = test_block(300);
    let tx = test_tx(&block);
    let mine = event_from_log(
        &anchored_log(REGISTRY, &key, &format!("0x{}", "07".repeat(32)), "0x"),
        &tx,
        0,
    );
    let theirs = event_from_log(
        &anchored_log(&other, &key, &format!("0x{}", "08".repeat(32)), "0x"),
        &tx,
        1,
    );
    db::save_block_bundle(&db, &block, &[tx], &[], &[mine, theirs], &[]).expect("save bundle");

    // Same key, different callers: the writes never collide.
    let other = nvnmchain_explorer::decoder::checksum_address(&other);
    assert_eq!(
        db::get_key_history(&db, REGISTRY, &key)[0].commitment,
        format!("0x{}", "07".repeat(32))
    );
    assert_eq!(
        db::get_key_history(&db, &other, &key)[0].commitment,
        format!("0x{}", "08".repeat(32))
    );

    let namespaces = db::get_anchored_namespaces(&db, 1, 25);
    let listed: Vec<&Value> = namespaces.iter().map(|n| &n["namespace"]).collect();
    assert!(listed.contains(&&json!(REGISTRY)));
    assert!(listed.contains(&&json!(other)));
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// A server over a DB holding one registry anchor, plus its base URL.
async fn serve() -> (tempfile::TempDir, String) {
    use nvnmchain_explorer::web::{self, AppState};

    let (dir, db) = temp_db();
    index_anchor(
        &db,
        400,
        REGISTRY_KEY,
        REGISTRY_COMMITMENT,
        REGISTRY_METADATA,
    );

    let cfg = nvnmchain_explorer::config::Settings::from_env();
    let state = AppState {
        db,
        rpc: nvnmchain_explorer::rpc::ChainRpc::from_settings(&cfg).expect("rpc"),
        cfg,
        tera: web::build_tera(temp_db().1).expect("templates"),
        block_events: tokio::sync::broadcast::channel(16).0,
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

#[tokio::test]
async fn anchoring_pages_serve_json_and_html() {
    let (_dir, base) = serve().await;
    let client = reqwest::Client::new();

    let index: Value = client
        .get(format!("{base}/anchoring?format=json"))
        .send()
        .await
        .expect("index")
        .json()
        .await
        .expect("index json");
    assert_eq!(index["total"], json!(1));
    assert_eq!(index["namespaces"][0]["namespace"], json!(REGISTRY));
    assert_eq!(index["recent"][0]["label"], json!("Registry #1 — Docs"));

    let namespace: Value = client
        .get(format!("{base}/anchoring/{REGISTRY}?format=json"))
        .send()
        .await
        .expect("namespace")
        .json()
        .await
        .expect("namespace json");
    assert_eq!(namespace["namespace"], json!(REGISTRY));
    assert_eq!(namespace["keys"][0]["key"], json!(REGISTRY_KEY));

    let key: Value = client
        .get(format!(
            "{base}/anchoring/{REGISTRY}/{REGISTRY_KEY}?format=json"
        ))
        .send()
        .await
        .expect("key")
        .json()
        .await
        .expect("key json");
    assert_eq!(key["head"]["commitment"], json!(REGISTRY_COMMITMENT));
    assert_eq!(key["head"], key["history"][0]);
    assert_eq!(key["envelope"]["kind"], json!("registry"));
    assert_eq!(key["self_verifying"], json!(true));

    // Every page also renders, with the decoded envelope on the key page.
    for path in [
        "/anchoring".to_string(),
        format!("/anchoring/{REGISTRY}"),
        format!("/anchoring/{REGISTRY}/{REGISTRY_KEY}"),
    ] {
        let resp = client
            .get(format!("{base}{path}"))
            .send()
            .await
            .unwrap_or_else(|e| panic!("GET {path}: {e}"));
        assert_eq!(resp.status(), 200, "GET {path}");
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(content_type.contains("text/html"), "GET {path}");
        let body = resp.text().await.expect("body");
        if path.contains(REGISTRY_KEY) {
            assert!(body.contains("Registry #1 — Docs"), "envelope summary");
            assert!(body.contains("internal docs"), "envelope fields");
        }
    }
}

#[tokio::test]
async fn malformed_namespace_and_unknown_key_are_rejected() {
    let (_dir, base) = serve().await;
    let client = reqwest::Client::new();

    let bad = client
        .get(format!("{base}/anchoring/not-an-address?format=json"))
        .send()
        .await
        .expect("bad namespace");
    assert_eq!(bad.status(), 400);

    let missing = client
        .get(format!(
            "{base}/anchoring/{REGISTRY}/0x{}?format=json",
            "99".repeat(32)
        ))
        .send()
        .await
        .expect("missing key");
    assert_eq!(missing.status(), 404);
}
