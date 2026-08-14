//! Indexing the `Anchored` log.
//!
//! The precompile keeps only the head per `(namespace, key)`, so the indexed
//! table is the entire history. What a payload *means* is not tested here —
//! that moved to the anchoring indexer along with the decoder.

use std::sync::{Arc, Mutex};

use nvnmchain_explorer::anchoring::{
    is_self_verifying, ANCHORED_SIGNATURE, ANCHORED_TOPIC, ANCHORING_ADDRESS,
};
use nvnmchain_explorer::db::{self, Db};
use nvnmchain_explorer::decoder::{decode_event, decode_function_call, keccak_hex};
use nvnmchain_explorer::indexer::anchored_event;
use nvnmchain_explorer::models::{AnchoredEvent, Block, BlockBundle, Transaction};
use serde_json::{json, Value};

/// The registry contract that emitted the fixtures — its address is the namespace,
/// since the precompile partitions by caller. One deployment per registry, so this
/// address is the registry rather than a proxy fronting many of them.
const REGISTRY: &str = "0x44DA54d3f5416A9Ae699d54EcB83c3043c41319E";

/// The explorer stores what was anchored without reading it, so the tests need
/// a key, a commitment and some bytes — not a payload of any particular shape.
const REGISTRY_KEY: &str = "0x173602657603c73bdfa5393aba98fa9e899f7c58898ea2a7d444639768d549d4";
const REGISTRY_COMMITMENT: &str =
    "0xf6f0bcff7207ce080ce3900e9e8c378a31a0faa37441f4ce9f222929db9b9b0e";
const REGISTRY_METADATA: &str = "0x7b2276223a317d";

#[test]
fn anchored_topic_matches_the_signature() {
    assert_eq!(keccak_hex(ANCHORED_SIGNATURE.as_bytes()), ANCHORED_TOPIC);
}

#[test]
fn anchor_and_hash_payloads_are_self_verifying() {
    // The precompile's own guarantee: anchorAndHash commits to the digest of
    // its own metadata, whatever that metadata turns out to mean.
    let metadata = b"{\"v\":1}";
    let commitment = keccak_hex(metadata);
    let hexed = format!("0x{}", hex::encode(metadata));
    assert!(is_self_verifying(&commitment, &hexed));
    assert!(!is_self_verifying(
        &format!("0x{}", "11".repeat(32)),
        &hexed
    ));
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
    let bundle = BlockBundle {
        block,
        txs: vec![tx],
        transfers: vec![],
        anchored: vec![event.clone()],
        tokens: vec![],
        registries: vec![],
    };
    db::save_block_bundle(db, &bundle).expect("save bundle");
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
    let txs = [tx];
    let events = [mine, theirs];
    let bundle = BlockBundle {
        block,
        txs: txs.to_vec(),
        transfers: vec![],
        anchored: events.to_vec(),
        tokens: vec![],
        registries: vec![],
    };
    db::save_block_bundle(&db, &bundle).expect("save bundle");

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

    let namespaces = db::get_anchored_namespaces(&db, None, 1, 25);
    for ns in [REGISTRY, other.as_str()] {
        let entry = namespaces
            .iter()
            .find(|n| n["namespace"] == json!(ns))
            .expect("namespace listed");
        assert_eq!(entry["anchor_count"], json!(1), "{ns}");
        assert_eq!(entry["key_count"], json!(1), "{ns}");
        assert_eq!(entry["last_block"], json!(300), "{ns}");
    }

    // Re-writing an already-indexed block inserts nothing, so neither the
    // summary nor the total may move.
    db::save_block_bundle(&db, &bundle).expect("re-save");
    assert_eq!(db::get_anchored_namespaces(&db, None, 1, 25), namespaces);
    assert_eq!(db::count_anchored(&db), 2);

    // The startup rebuild must land exactly where the incremental fold did.
    db::sync_anchored_namespaces(&db::lock(&db)).expect("sync");
    assert_eq!(db::get_anchored_namespaces(&db, None, 1, 25), namespaces);
    assert_eq!(db::count_anchored(&db), 2);
}

// ---------------------------------------------------------------------------
// Registry labelling
// ---------------------------------------------------------------------------

const FACTORY: &str = "0x00000000000000000000000000000000000FAC70";

/// A `RegistryDeployed` log as the factory emits it: registry, creator and index
/// in the topics, the three strings ABI-encoded in `data`.
fn registry_deployed_log(factory: &str, registry: &str, name: &str) -> Value {
    // abi.encode(string, string, string): three offset words, then each tail as
    // a length word and right-padded bytes.
    let parts: [&[u8]; 3] = [name.as_bytes(), b"docs about docs", b"{}"];
    let word = |n: usize| format!("{n:064x}");
    let mut head = String::from("0x");
    let mut tail = String::new();
    for part in parts {
        head.push_str(&word(3 * 32 + tail.len() / 2));
        tail.push_str(&word(part.len()));
        tail.push_str(&hex::encode(part));
        tail.push_str(&"0".repeat(tail.len().next_multiple_of(64) - tail.len()));
    }
    let data = head + &tail;
    json!({
        "address": factory,
        "topics": [
            nvnmchain_explorer::decoder::REGISTRY_DEPLOYED_TOPIC,
            format!("0x{}{}", "00".repeat(12), registry.trim_start_matches("0x").to_lowercase()),
            format!("0x{}{}", "00".repeat(12), "33".repeat(20)),
        ],
        "data": data,
        "logIndex": "0x0",
    })
}

#[test]
fn registry_deployed_topic_matches_the_signature() {
    assert_eq!(
        keccak_hex(b"RegistryDeployed(address,address,string,string,string)"),
        nvnmchain_explorer::decoder::REGISTRY_DEPLOYED_TOPIC
    );
}

#[test]
fn a_deployment_labels_its_namespace_for_the_configured_factory_only() {
    let (_dir, db) = temp_db();
    let block = test_block(400);
    let tx = test_tx(&block);

    // The registry anchors (so it appears among namespaces)...
    let key = format!("0x{}", "77".repeat(32));
    let anchor = event_from_log(
        &anchored_log(REGISTRY, &key, &format!("0x{}", "09".repeat(32)), "0x"),
        &tx,
        0,
    );
    // ...and its factory announced it.
    let deployed = nvnmchain_explorer::indexer::registry_deployed(
        &registry_deployed_log(FACTORY, REGISTRY, "docs"),
        &tx,
        1,
    )
    .expect("deployment parses");
    assert_eq!(deployed.registry, REGISTRY);
    assert_eq!(deployed.name, "docs");
    assert_eq!(deployed.description, "docs about docs");

    let txs = [tx];
    let bundle = BlockBundle {
        block,
        txs: txs.to_vec(),
        transfers: vec![],
        anchored: vec![anchor],
        tokens: vec![],
        registries: vec![deployed],
    };
    db::save_block_bundle(&db, &bundle).expect("save bundle");

    // Labelled for the factory that deployed it, bare for anyone else.
    let labelled = db::get_anchored_namespaces(&db, Some(FACTORY), 1, 25);
    let row = labelled
        .iter()
        .find(|n| n["namespace"] == json!(REGISTRY))
        .expect("listed");
    assert_eq!(row["name"], json!("docs"));

    let other = format!("0x{}", "44".repeat(20));
    for factory in [None, Some(other.as_str())] {
        let bare = db::get_anchored_namespaces(&db, factory, 1, 25);
        let row = bare
            .iter()
            .find(|n| n["namespace"] == json!(REGISTRY))
            .expect("listed");
        assert_eq!(row["name"], json!(null), "{factory:?} must not label");
    }

    // The namespace page's header row, same trust rule.
    assert!(db::get_registry(&db, FACTORY, REGISTRY).is_some());
    assert!(db::get_registry(&db, &other, REGISTRY).is_none());
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
        stats: std::sync::Arc::new(std::sync::RwLock::new(serde_json::Value::Null)),
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
    // The row is the log row: what was anchored, not what it means.
    assert_eq!(index["recent"][0]["commitment"], json!(REGISTRY_COMMITMENT));

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
    assert_eq!(key["head"]["metadata"], json!(REGISTRY_METADATA));
    assert!(
        key.get("envelope").is_none(),
        "envelopes belong to the indexer"
    );
    // anchorAndHash's guarantee is the precompile's, so the explorer can still
    // say whether a payload verifies itself.
    assert_eq!(key["self_verifying"], json!(false));

    // Every page renders.
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
            assert!(
                body.contains(REGISTRY_COMMITMENT),
                "the commitment is shown"
            );
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

/// The layout, rendered with whatever `page_ctx` would have put in it.
fn nav(anchored_total: i64) -> String {
    let (_dir, db) = temp_db();
    let tera = nvnmchain_explorer::web::build_tera(db).expect("templates load");
    let ctx = tera::Context::from_serialize(json!({
        "native_symbol": "PATH",
        "anchoring_url": Value::Null,
        "anchored_total": anchored_total,
        "latest_block": Value::Null,
        "query": "",
    }))
    .expect("context");
    tera.render("base.html", &ctx).expect("base.html renders")
}

#[test]
fn the_anchoring_tab_waits_for_the_first_anchor() {
    // A chain that has never anchored gets no menu entry for it. The routes stay
    // reachable either way -- a link from elsewhere still resolves, and would
    // start 404ing the day someone anchored if this hid them instead.
    assert!(
        !nav(0).contains(r#"href="/anchoring""#),
        "nav on an unused chain"
    );
    assert!(
        nav(1).contains(r#"href="/anchoring""#),
        "nav once something is anchored"
    );
}
