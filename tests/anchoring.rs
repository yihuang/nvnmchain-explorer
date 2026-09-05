//! Indexing the precompile's appends.
//!
//! The precompile keeps one MMR per caller, so a row is an append at a leaf
//! index rather than a write to a key. What a payload *means* is not tested
//! here — that moved to the anchoring indexer along with the decoder.

use std::sync::{Arc, Mutex};

use ethers_core::abi::Token;
use nvnmchain_explorer::anchoring::{is_self_verifying, ANCHORING_ADDRESS};
use nvnmchain_explorer::config::Settings;
use nvnmchain_explorer::db::{self, Db};
use nvnmchain_explorer::decoder::{
    decode_event, decode_function_call, keccak_hex, LEAF_APPENDED_TOPIC, LEAVES_APPENDED_TOPIC,
};
use nvnmchain_explorer::indexer::anchored_event;
use nvnmchain_explorer::models::{
    AnchoredEvent, Block, BlockBundle, RegistryDeployed, Transaction,
};
use serde_json::{json, Value};

/// The registry contract that emitted the fixtures — its address is the namespace,
/// since the precompile partitions by caller. One deployment per registry, so this
/// address is the registry rather than a proxy fronting many of them.
const REGISTRY: &str = "0x44DA54d3f5416A9Ae699d54EcB83c3043c41319E";

/// The explorer stores what was appended without reading it, so the tests need
/// a commitment, a root and some bytes — not a payload of any particular shape.
const REGISTRY_COMMITMENT: &str =
    "0xf6f0bcff7207ce080ce3900e9e8c378a31a0faa37441f4ce9f222929db9b9b0e";
const REGISTRY_ROOT: &str = "0x173602657603c73bdfa5393aba98fa9e899f7c58898ea2a7d444639768d549d4";
const REGISTRY_METADATA: &str = "0x7b2276223a317d";

#[test]
fn record_leaf_payloads_are_self_verifying() {
    // What a registry commits to: the digest of the envelope it appended,
    // whatever that envelope turns out to mean.
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
    let commitment = "22".repeat(32);
    let calldata = format!("0xe5435d9a{commitment}{:064x}{:064x}", 0x40, 0);
    let call = decode_function_call(&calldata).expect("appendLeaf call");
    assert_eq!(call.name.as_deref(), Some("appendLeaf"));
    assert_eq!(call.params[0].value, format!("0x{commitment}"));
}

// ---------------------------------------------------------------------------
// Indexing
// ---------------------------------------------------------------------------

fn bytes_of(hexed: &str) -> Vec<u8> {
    hex::decode(hexed.strip_prefix("0x").unwrap_or(hexed)).expect("hex")
}

/// A precompile log as the node reports it: topic0, then the two indexed
/// arguments every append shares, then the rest ABI-encoded in `data`.
fn append_log(topic: &str, namespace: &str, indexed: u64, data: &[Token]) -> Value {
    let topics = [
        topic.to_string(),
        format!("0x{}{}", "00".repeat(12), hex::encode(bytes_of(namespace))),
        format!("0x{indexed:064x}"),
    ];
    json!({
        "address": ANCHORING_ADDRESS,
        "topics": topics,
        "data": format!("0x{}", hex::encode(ethers_core::abi::encode(data))),
        "logIndex": "0x0",
    })
}

/// `LeafAppended`: what one leaf committed to, and the root it left.
fn leaf_log(namespace: &str, index: u64, commitment: &str, root: &str, metadata: &str) -> Value {
    append_log(
        &LEAF_APPENDED_TOPIC,
        namespace,
        index,
        &[
            Token::FixedBytes(bytes_of(commitment)),
            Token::FixedBytes(bytes_of(root)),
            Token::Array(vec![]), // peaks
            Token::Bytes(bytes_of(metadata)),
        ],
    )
}

/// `LeavesAppended`: a span starting at `first`, with `count` the tree's size
/// afterwards. No commitment of its own — its leaves reached the chain as the
/// roots of subtrees, which is what the two empty arrays would carry.
fn leaves_log(namespace: &str, first: u64, count: u64, root: &str, metadata: &str) -> Value {
    append_log(
        &LEAVES_APPENDED_TOPIC,
        namespace,
        first,
        &[
            Token::Uint(count.into()),
            Token::Array(vec![]), // chunkRoots
            Token::Array(vec![]), // chunkHeights
            Token::FixedBytes(bytes_of(root)),
            Token::Array(vec![]), // peaks
            Token::Bytes(bytes_of(metadata)),
        ],
    )
}

fn event_from_log(log: &Value, tx: &Transaction, log_index: i64) -> AnchoredEvent {
    let decoded = decode_event(log).expect("decoded log");
    assert!(matches!(
        decoded.name.as_deref(),
        Some("LeafAppended") | Some("LeavesAppended")
    ));
    anchored_event(&decoded, tx, log_index).expect("append row")
}

fn deployment_from_log(log: &Value, tx: &Transaction) -> RegistryDeployed {
    let decoded = decode_event(log).expect("decoded log");
    assert_eq!(decoded.name.as_deref(), Some("RegistryDeployed"));
    nvnmchain_explorer::indexer::registry_deployed(&decoded, tx).expect("deployment row")
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

/// Index one append into `db` and return the row that was written.
fn index_append(db: &Db, number: i64, log: Value) -> AnchoredEvent {
    let block = test_block(number);
    let tx = test_tx(&block);
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
fn a_leaf_log_becomes_a_row() {
    let (_dir, db) = temp_db();
    let event = index_append(
        &db,
        100,
        leaf_log(
            REGISTRY,
            4,
            REGISTRY_COMMITMENT,
            REGISTRY_ROOT,
            REGISTRY_METADATA,
        ),
    );
    assert_eq!(event.namespace, REGISTRY);

    let appends = db::get_namespace_appends(&db, REGISTRY, 1, 25);
    assert_eq!(appends.len(), 1);
    let row = &appends[0];
    assert_eq!(row.namespace, REGISTRY);
    assert_eq!((row.index, row.leaves), (4, 1));
    assert_eq!(row.commitment, REGISTRY_COMMITMENT);
    assert_eq!(row.root, REGISTRY_ROOT);
    assert_eq!(row.metadata, REGISTRY_METADATA);
    assert_eq!(row.block_number, 100);
    assert_eq!(row.timestamp, 1_800);
    assert_eq!(db::count_anchored(&db), 1);

    // The summary is the tree: five leaves once index 4 is in, at that root.
    assert_eq!(
        db::get_namespace_mmr(&db, REGISTRY),
        (5, REGISTRY_ROOT.to_string())
    );
}

#[test]
fn a_batch_is_one_row_over_the_span_it_added() {
    // Its rows never reached the chain one at a time, so it carries no
    // commitment — and every index in the span resolves to it.
    let (_dir, db) = temp_db();
    let event = index_append(&db, 110, leaves_log(REGISTRY, 0, 13, REGISTRY_ROOT, "0x"));
    assert_eq!((event.index, event.leaves), (0, 13));
    assert_eq!(event.commitment, "");

    assert_eq!(
        db::get_namespace_mmr(&db, REGISTRY),
        (13, REGISTRY_ROOT.to_string())
    );
    for index in [0, 6, 12] {
        let found = db::get_leaf(&db, REGISTRY, index).expect("covered by the batch");
        assert_eq!((found.index, found.leaves), (0, 13));
    }
    assert!(db::get_leaf(&db, REGISTRY, 13).is_none(), "past the span");
}

#[test]
fn a_leaf_after_a_batch_is_found_on_its_own() {
    let (_dir, db) = temp_db();
    index_append(
        &db,
        120,
        leaves_log(REGISTRY, 0, 13, &format!("0x{}", "01".repeat(32)), "0x"),
    );
    index_append(
        &db,
        121,
        leaf_log(REGISTRY, 13, REGISTRY_COMMITMENT, REGISTRY_ROOT, "0x"),
    );

    let found = db::get_leaf(&db, REGISTRY, 13).expect("the leaf after the batch");
    assert_eq!(found.commitment, REGISTRY_COMMITMENT);
    assert_eq!(
        db::get_leaf(&db, REGISTRY, 12)
            .expect("in the batch")
            .leaves,
        13
    );
    // Fourteen leaves now, at the root the newest append left.
    assert_eq!(
        db::get_namespace_mmr(&db, REGISTRY),
        (14, REGISTRY_ROOT.to_string())
    );
}

#[test]
fn logs_from_other_contracts_are_not_appends() {
    let block = test_block(101);
    let tx = test_tx(&block);
    let foreign = json!({
        "address": format!("0x{}", "cc".repeat(20)),
        "topics": [format!("0x{}", "de".repeat(32))],
        "data": "0x",
        "logIndex": "0x0",
    });
    let decoded = decode_event(&foreign).expect("decoded");
    assert_ne!(decoded.name.as_deref(), Some("LeafAppended"));
    // A log that claims the signature but carries no root yields no row.
    let truncated = json!({
        "address": ANCHORING_ADDRESS,
        "topics": [LEAF_APPENDED_TOPIC.as_str()],
        "data": "0x",
        "logIndex": "0x0",
    });
    let decoded = decode_event(&truncated).expect("decoded");
    assert!(anchored_event(&decoded, &tx, 0).is_none());
}

#[test]
fn reindexing_a_block_does_not_duplicate() {
    let (_dir, db) = temp_db();
    let log = || {
        leaf_log(
            REGISTRY,
            0,
            REGISTRY_COMMITMENT,
            REGISTRY_ROOT,
            REGISTRY_METADATA,
        )
    };
    index_append(&db, 102, log());
    index_append(&db, 102, log());
    assert_eq!(db::count_anchored(&db), 1);
    assert_eq!(db::get_namespace_appends(&db, REGISTRY, 1, 25).len(), 1);
    // And the summary counted it once: the second insert moved nothing.
    assert_eq!(db::get_namespace_mmr(&db, REGISTRY).0, 1);
}

/// A database on the old shape is re-indexed from the chain, not merely emptied:
/// forgetting the blocks is what makes the backfill revisit every one.
#[test]
fn a_database_on_the_old_shape_starts_over_from_the_chain() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("anchoring.db");
    let path = path.to_str().unwrap();
    let db: Db = Arc::new(Mutex::new(db::init_db(path).expect("init db")));
    index_append(
        &db,
        100,
        leaf_log(REGISTRY, 0, REGISTRY_COMMITMENT, REGISTRY_ROOT, "0x"),
    );
    assert_eq!(db::get_min_block_number(&db), Some(100));
    // The old shape's tell-tale: a `key` column.
    db.lock()
        .unwrap()
        .execute("ALTER TABLE anchored_events ADD COLUMN key BLOB", [])
        .expect("the old shape");
    drop(db);

    let reopened: Db = Arc::new(Mutex::new(db::init_db(path).expect("reopen")));
    assert_eq!(
        db::get_min_block_number(&reopened),
        None,
        "every block forgotten, so the backfill starts from the head"
    );
    assert_eq!(db::count_anchored(&reopened), 0);
    let keyed: i64 = reopened
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('anchored_events') WHERE name = 'key'",
            [],
            |r| r.get(0),
        )
        .expect("pragma");
    assert_eq!(keyed, 0, "recreated on the new shape");
}

#[test]
fn appends_are_newest_first_and_the_root_is_the_last_one() {
    let (_dir, db) = temp_db();
    let (first, second) = (
        format!("0x{}", "01".repeat(32)),
        format!("0x{}", "02".repeat(32)),
    );
    index_append(
        &db,
        200,
        leaf_log(REGISTRY, 0, REGISTRY_COMMITMENT, &first, "0x"),
    );
    index_append(
        &db,
        201,
        leaf_log(REGISTRY, 1, REGISTRY_COMMITMENT, &second, "0x"),
    );

    let appends = db::get_namespace_appends(&db, REGISTRY, 1, 25);
    assert_eq!(
        appends.iter().map(|r| r.block_number).collect::<Vec<_>>(),
        vec![201, 200]
    );
    // A leaf never moves, so both stay, and the tree is what the newest left.
    assert_eq!(db::get_namespace_mmr(&db, REGISTRY), (2, second.clone()));
    assert_eq!(db::get_leaf(&db, REGISTRY, 0).expect("leaf 0").root, first);
    assert_eq!(db::get_leaf(&db, REGISTRY, 1).expect("leaf 1").root, second);
}

#[test]
fn a_rebuild_matches_what_the_inserts_maintained() {
    // `sync_anchored_namespaces` runs at startup for databases that predate the
    // summary; it must land on what the incremental path already had. A batch
    // is where the two could disagree: it is one append but many leaves.
    let (_dir, db) = temp_db();
    index_append(
        &db,
        210,
        leaves_log(REGISTRY, 0, 13, &format!("0x{}", "0a".repeat(32)), "0x"),
    );
    index_append(
        &db,
        211,
        leaf_log(REGISTRY, 13, REGISTRY_COMMITMENT, REGISTRY_ROOT, "0x"),
    );
    let incremental = db::get_anchored_namespaces(&db, None, 1, 25);
    assert_eq!(incremental[0]["anchor_count"], 2);
    assert_eq!(incremental[0]["leaf_count"], 14);

    db::sync_anchored_namespaces(&db.lock().unwrap()).expect("rebuild");
    assert_eq!(db::get_anchored_namespaces(&db, None, 1, 25), incremental);
    // The tree itself is read off the newest append, so a rebuild cannot move it.
    assert_eq!(
        db::get_namespace_mmr(&db, REGISTRY),
        (14, REGISTRY_ROOT.to_string())
    );
}

#[test]
fn namespaces_are_partitioned_by_caller() {
    let (_dir, db) = temp_db();
    let other = format!("0x{}", "b2".repeat(20));
    let (mine_root, theirs_root) = (
        format!("0x{}", "07".repeat(32)),
        format!("0x{}", "08".repeat(32)),
    );

    let block = test_block(300);
    let tx = test_tx(&block);
    // Both append their first leaf, so both are at index 0.
    let mine = event_from_log(
        &leaf_log(REGISTRY, 0, REGISTRY_COMMITMENT, &mine_root, "0x"),
        &tx,
        0,
    );
    let theirs = event_from_log(
        &leaf_log(&other, 0, REGISTRY_COMMITMENT, &theirs_root, "0x"),
        &tx,
        1,
    );
    let bundle = BlockBundle {
        block,
        txs: vec![tx],
        transfers: vec![],
        anchored: vec![mine, theirs],
        tokens: vec![],
        registries: vec![],
    };
    db::save_block_bundle(&db, &bundle).expect("save bundle");

    // Same leaf index, different callers: the trees never collide.
    let other = nvnmchain_explorer::decoder::checksum_address(&other);
    assert_eq!(
        db::get_leaf(&db, REGISTRY, 0).expect("mine").root,
        mine_root
    );
    assert_eq!(
        db::get_leaf(&db, &other, 0).expect("theirs").root,
        theirs_root
    );

    let namespaces = db::get_anchored_namespaces(&db, None, 1, 25);
    for ns in [REGISTRY, other.as_str()] {
        let entry = namespaces
            .iter()
            .find(|n| n["namespace"] == json!(ns))
            .expect("namespace listed");
        assert_eq!(entry["anchor_count"], json!(1), "{ns}");
        assert_eq!(entry["leaf_count"], json!(1), "{ns}");
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
            nvnmchain_explorer::decoder::REGISTRY_DEPLOYED_TOPIC.as_str(),
            format!("0x{}{}", "00".repeat(12), registry.trim_start_matches("0x").to_lowercase()),
            format!("0x{}{}", "00".repeat(12), "33".repeat(20)),
        ],
        "data": data,
        "logIndex": "0x0",
    })
}

#[test]
fn a_deployment_labels_its_namespace_for_the_configured_factory_only() {
    let (_dir, db) = temp_db();
    let block = test_block(400);
    let tx = test_tx(&block);

    // The registry appends (so it appears among namespaces)...
    let anchor = event_from_log(
        &leaf_log(
            REGISTRY,
            0,
            REGISTRY_COMMITMENT,
            &format!("0x{}", "09".repeat(32)),
            "0x",
        ),
        &tx,
        0,
    );
    // ...and its factory announced it.
    let deployed = deployment_from_log(&registry_deployed_log(FACTORY, REGISTRY, "docs"), &tx);
    assert_eq!(deployed.registry, REGISTRY);
    assert_eq!(deployed.name, "docs");
    assert_eq!(deployed.description, "docs about docs");

    let bundle = BlockBundle {
        block,
        txs: vec![tx],
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

#[test]
fn an_impostors_deployment_cannot_unlabel_a_registry() {
    // RegistryDeployed is recorded from whoever emits it, so a contract can
    // announce someone else's address. Recording that claim must not overwrite
    // the trusted factory's: the row is keyed by both.
    let (_dir, db) = temp_db();
    let block = test_block(500);
    let tx = test_tx(&block);
    let deployed = |factory: &str, name: &str| {
        deployment_from_log(&registry_deployed_log(factory, REGISTRY, name), &tx)
    };
    let impostor = format!("0x{}", "ee".repeat(20));
    let bundle = BlockBundle {
        block,
        txs: vec![tx.clone()],
        transfers: vec![],
        anchored: vec![],
        tokens: vec![],
        registries: vec![deployed(FACTORY, "docs"), deployed(&impostor, "not docs")],
    };
    db::save_block_bundle(&db, &bundle).expect("save bundle");

    let trusted = db::get_registry(&db, FACTORY, REGISTRY).expect("the real deployment survives");
    assert_eq!(trusted["name"], json!("docs"));
    // The claim is still on record, readable only by trusting its emitter.
    assert_eq!(
        db::get_registry(&db, &impostor, REGISTRY).expect("recorded")["name"],
        json!("not docs")
    );
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// A server over a DB holding one registry leaf, plus its base URL.
async fn serve() -> (tempfile::TempDir, String) {
    let (dir, db) = temp_db();
    index_append(
        &db,
        400,
        leaf_log(
            REGISTRY,
            0,
            REGISTRY_COMMITMENT,
            REGISTRY_ROOT,
            REGISTRY_METADATA,
        ),
    );
    let base = serve_db(db).await;
    (dir, base)
}

/// A server over `db`, and its base URL. The caller keeps the TempDir alive.
async fn serve_db(db: Db) -> String {
    serve_configured(db, |_| {}).await
}

/// The same, with the settings an operator would have set. Taken as a closure
/// rather than through the environment, which these tests share.
async fn serve_configured(db: Db, configure: impl FnOnce(&mut Settings)) -> String {
    use nvnmchain_explorer::web::{self, AppState};

    let mut cfg = nvnmchain_explorer::config::Settings::from_env();
    configure(&mut cfg);
    let tera = web::build_tera(db.clone()).expect("templates");
    let state = AppState {
        db,
        rpc: nvnmchain_explorer::rpc::ChainRpc::from_settings(&cfg).expect("rpc"),
        cfg,
        tera,
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
    format!("http://{addr}")
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
    assert_eq!(
        namespace["appends"][0]["commitment"],
        json!(REGISTRY_COMMITMENT)
    );
    // The tree the appends built, from the summary row rather than a walk.
    assert_eq!(namespace["leaves"], json!(1));
    assert_eq!(namespace["root"], json!(REGISTRY_ROOT));

    let leaf: Value = client
        .get(format!("{base}/anchoring/{REGISTRY}/0?format=json"))
        .send()
        .await
        .expect("leaf")
        .json()
        .await
        .expect("leaf json");
    assert_eq!(leaf["append"]["commitment"], json!(REGISTRY_COMMITMENT));
    assert_eq!(leaf["append"]["root"], json!(REGISTRY_ROOT));
    assert_eq!(leaf["append"]["metadata"], json!(REGISTRY_METADATA));
    assert_eq!(leaf["index"], json!(0));
    assert!(
        leaf.get("envelope").is_none(),
        "envelopes belong to the indexer"
    );
    // A record leaf commits to the digest of its envelope, so the explorer can
    // still say whether a payload verifies itself.
    assert_eq!(leaf["self_verifying"], json!(false));

    // Every page renders.
    for path in [
        "/anchoring".to_string(),
        format!("/anchoring/{REGISTRY}"),
        format!("/anchoring/{REGISTRY}/0"),
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
        if path.ends_with("/0") {
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

/// Every page extends the layout, so a context assembled by hand is a page
/// that cannot render. These are the routes that build one without a listing
/// to page — the ones a template-only change breaks silently.
#[tokio::test]
async fn pages_without_rows_still_render_the_layout() {
    let (_dir, base) = serve().await;
    let client = reqwest::Client::new();

    for (path, status) in [
        ("/search?q=matchesnothing", 200),
        ("/anchoring/not-an-address", 404),
        ("/blocks", 200),
        ("/tx/0xdeadbeef", 404),
    ] {
        let resp = client
            .get(format!("{base}{path}"))
            .send()
            .await
            .unwrap_or_else(|e| panic!("GET {path}: {e}"));
        assert_eq!(resp.status(), status, "GET {path}");
        let body = resp.text().await.expect("body");
        assert!(
            body.contains("</nav>"),
            "GET {path} rendered the layout, not a bare fallback"
        );
    }
}

#[tokio::test]
async fn a_listing_longer_than_a_page_is_paged() {
    // Every listing shares one pager macro, so this covers the markup the
    // token and address pages render too.
    let (_dir, db) = temp_db();
    for i in 0..26 {
        index_append(
            &db,
            600 + i,
            leaf_log(REGISTRY, i as u64, REGISTRY_COMMITMENT, REGISTRY_ROOT, "0x"),
        );
    }
    let base = serve_db(db).await;
    let client = reqwest::Client::new();
    let page = |n: u32| {
        let (client, base) = (client.clone(), base.clone());
        async move {
            client
                .get(format!("{base}/anchoring/{REGISTRY}?page={n}"))
                .send()
                .await
                .expect("page")
                .text()
                .await
                .expect("body")
        }
    };

    let first = page(1).await;
    assert!(first.contains("Page 1 of 2"), "first page is paged");
    assert!(first.contains("?page=2"), "and offers the next one");
    assert!(!first.contains("Previous"), "with nowhere back to go");

    let second = page(2).await;
    assert!(second.contains("Page 2 of 2"));
    assert!(second.contains("?page=1"), "the way back");
    assert!(!second.contains("Next"), "and no page three");
}

/// A registry holding one leaf, announced by the configured factory, with a
/// decoder to link out to.
async fn serve_registry() -> (tempfile::TempDir, String) {
    let (dir, db) = temp_db();
    let block = test_block(700);
    let tx = test_tx(&block);
    let anchor = event_from_log(
        &leaf_log(REGISTRY, 0, REGISTRY_COMMITMENT, REGISTRY_ROOT, "0x"),
        &tx,
        0,
    );
    let deployed = deployment_from_log(&registry_deployed_log(FACTORY, REGISTRY, "docs"), &tx);
    db::save_block_bundle(
        &db,
        &BlockBundle {
            block,
            txs: vec![tx],
            transfers: vec![],
            anchored: vec![anchor],
            tokens: vec![],
            registries: vec![deployed],
        },
    )
    .expect("save bundle");
    let base = serve_configured(db, |cfg| {
        cfg.anchoring_url = Some("http://decoder.test".into());
        cfg.registry_factory = Some(FACTORY.into());
    })
    .await;
    (dir, base)
}

async fn html_at(url: String) -> String {
    reqwest::get(url)
        .await
        .expect("page")
        .text()
        .await
        .expect("body")
}

async fn json_at(url: String) -> Value {
    reqwest::get(url)
        .await
        .expect("page")
        .json()
        .await
        .expect("json")
}

/// Anyone may anchor under any key, and the decoder answers per registry, so
/// only a namespace the factory announced gets the link.
#[tokio::test]
async fn only_a_registry_links_out_to_the_decoder() {
    let (_dir, base) = serve_registry().await;

    // Tera escapes the URL's slashes into the attribute, so the path is what
    // says which projection a link reaches.
    let namespace = html_at(format!("{base}/anchoring/{REGISTRY}")).await;
    for projection in ["records", "roles"] {
        assert!(
            namespace.contains(&format!("decoder.test/registries/{REGISTRY}/{projection}")),
            "the {projection} projection"
        );
    }
    assert!(
        html_at(format!("{base}/anchoring/{REGISTRY}/0"))
            .await
            .contains("the anchoring decoder"),
        "the leaf page's link"
    );

    // The same leaf under a factory that never announced it: no link, rather
    // than one that leads to a 404.
    let (_dir, db) = temp_db();
    index_append(
        &db,
        800,
        leaf_log(REGISTRY, 0, REGISTRY_COMMITMENT, REGISTRY_ROOT, "0x"),
    );
    let base = serve_configured(db, |cfg| {
        cfg.anchoring_url = Some("http://decoder.test".into());
        cfg.registry_factory = Some(format!("0x{}", "5e".repeat(20)));
    })
    .await;
    let page = html_at(format!("{base}/anchoring/{REGISTRY}/0")).await;
    assert!(
        !page.contains("decoder.test"),
        "no link for a bare namespace"
    );
}

/// A registry lands wherever `CREATE` puts it, so its interface — and the fact
/// that it is a contract at all — come from the factory's log.
#[tokio::test]
async fn a_registry_address_shows_the_registry_interface() {
    let (_dir, base) = serve_registry().await;

    let page = json_at(format!("{base}/address/{REGISTRY}?format=json")).await;
    assert_eq!(page["interface"]["abis"], json!(["registry"]));
    assert_eq!(page["type"], json!("contract"));
    let writes = page["interface"]["writes"].as_array().expect("writes");
    assert!(
        writes.iter().any(|f| f["name"] == "addRecord"),
        "{writes:?}"
    );

    let factory = json_at(format!("{base}/address/{FACTORY}?format=json")).await;
    assert_eq!(factory["interface"]["abis"], json!(["registry_factory"]));

    // The leaf half of the same interface, which no record list will show.
    assert!(
        writes.iter().any(|f| f["name"] == "appendLeaves"),
        "{writes:?}"
    );
    let reads = page["interface"]["reads"].as_array().expect("reads");
    assert!(reads.iter().any(|f| f["name"] == "mmrRoot"), "{reads:?}");
}

/// The layout, rendered with whatever `page_ctx` would have put in it.
fn nav(has_anchors: bool) -> String {
    let (_dir, db) = temp_db();
    let tera = nvnmchain_explorer::web::build_tera(db).expect("templates load");
    let ctx = tera::Context::from_serialize(json!({
        "native_symbol": "PATH",
        "anchoring_url": Value::Null,
        "has_anchors": has_anchors,
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
        !nav(false).contains(r#"href="/anchoring""#),
        "nav on an unused chain"
    );
    assert!(
        nav(true).contains(r#"href="/anchoring""#),
        "nav once something is anchored"
    );
}
