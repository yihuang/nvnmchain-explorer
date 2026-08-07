//! Integration tests against the live chain RPC.
//!
//! These exercise the real node at `TEMPO_RPC` (default
//! `https://rpc.nvnm.canary.mantrachain.dev`): chain metadata, block and tx
//! fetching, indexing into SQLite, and the HTTP API end to end.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tempo_explorer::config::{DEFAULT_CHAIN_ID, DEFAULT_RPC_URL};
use tempo_explorer::db::{self, Db};
use tempo_explorer::indexer::index_block;
use tempo_explorer::rpc::TempoRpc;
use tempo_explorer::web::{self, AppState};

fn temp_db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("explorer.db");
    let conn = db::init_db(path.to_str().unwrap()).expect("init db");
    (dir, Arc::new(Mutex::new(conn)))
}

fn rpc() -> TempoRpc {
    TempoRpc::new(DEFAULT_RPC_URL).expect("rpc client")
}

#[tokio::test]
async fn chain_metadata_matches_mantra() {
    let rpc = rpc();
    let chain_id = rpc.eth_chain_id().await.expect("eth_chainId");
    assert_eq!(chain_id, DEFAULT_CHAIN_ID, "unexpected chain id");

    let head = rpc.eth_block_number().await.expect("eth_blockNumber");
    assert!(head > 0, "chain head should be above zero");

    let block = rpc
        .eth_get_block_by_number(head, true)
        .await
        .expect("block")
        .expect("block exists");
    assert!(block.get("hash").and_then(Value::as_str).is_some());
    assert!(block.get("transactions").is_some());
}

#[tokio::test]
async fn index_recent_blocks_into_sqlite() {
    let rpc = rpc();
    let (_dir, db) = temp_db();
    let head = rpc.eth_block_number().await.expect("head");

    // Index a small window at the tip.
    let start = head.saturating_sub(4);
    for n in start..=head {
        index_block(&rpc, &db, n).await.expect("index block");
    }

    for n in start..=head {
        let block = db::get_block_by_number(&db, n as i64).expect("stored block");
        assert_eq!(block.number as u64, n);
        assert!(!block.hash.is_empty());
        assert!(block.timestamp > 0);
    }
    let latest = db::get_latest_block(&db).expect("latest");
    assert_eq!(latest.number as u64, head);

    // Transactions that exist on the chain must be stored with receipts.
    for n in start..=head {
        let block = db::get_block_by_number(&db, n as i64).unwrap();
        let txs = db::get_block_transactions(&db, block.number);
        assert_eq!(txs.len() as i64, block.tx_count);
        for tx in &txs {
            assert_eq!(tx.block_number, block.number);
            assert!(!tx.from_addr.is_empty());
        }
    }

    // If the tip block has transactions, spot-check a receipt + decoded call.
    let txs = db::get_block_transactions(&db, head as i64);
    if let Some(tx) = txs.first() {
        if tx.receipt_data.is_some() {
            let receipt: Value = serde_json::from_str(tx.receipt_data.as_deref().unwrap()).unwrap();
            assert!(receipt.get("status").is_some());
        }
        if tx.input.len() > 10 {
            assert!(tempo_explorer::decoder::decode_function_call(&tx.input).is_some());
        }
    }
}

#[tokio::test]
async fn web_api_serves_indexed_data() {
    let rpc = rpc();
    let (_dir, db) = temp_db();
    let head = rpc.eth_block_number().await.expect("head");
    index_block(&rpc, &db, head).await.expect("index tip");

    let cfg = tempo_explorer::config::Settings {
        rpc_url: DEFAULT_RPC_URL.into(),
        chain_id: DEFAULT_CHAIN_ID,
        host: "127.0.0.1".into(),
        port: 0,
        db_path: "unused".into(),
        max_cached_blocks: 100_000,
        recent_block_count: 5,
        recent_tx_count: 5,
        poll_seconds: 3.0,
        batch_size: 5,
    };
    let tera = web::build_tera(db.clone()).expect("tera");
    let state = AppState {
        db: db.clone(),
        rpc,
        cfg,
        tera,
    };
    let app = web::app(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let base = format!("http://{addr}");

    let client = reqwest::Client::new();

    // Home JSON.
    let home: Value = client
        .get(format!("{base}/"))
        .query(&[("format", "json")])
        .send()
        .await
        .expect("home")
        .json()
        .await
        .expect("home json");
    assert!(home
        .get("latest_block")
        .and_then(Value::as_object)
        .is_some());
    assert!(home
        .get("recent_blocks")
        .and_then(Value::as_array)
        .is_some());

    // Block JSON by number.
    let block_resp = client
        .get(format!("{base}/block/{head}"))
        .header("Accept", "application/json")
        .send()
        .await
        .expect("block");
    assert_eq!(block_resp.status(), 200);
    let block_json: Value = block_resp.json().await.expect("block json");
    assert_eq!(block_json["block"]["number"], json!(head));

    // Block HTML.
    let html = client
        .get(format!("{base}/block/{head}"))
        .send()
        .await
        .expect("block html");
    assert!(html.status().is_success());
    let body = html.text().await.expect("html body");
    assert!(body.contains("Block"));

    // Search by block number redirects.
    let search = client
        .get(format!("{base}/search"))
        .query(&[("q", head.to_string())])
        .send()
        .await
        .expect("search");
    assert!(search.status().is_redirection() || search.status().is_success());

    // A transaction page if the tip block has transactions.
    let txs = db::get_block_transactions(&db, head as i64);
    if let Some(tx) = txs.first() {
        let tx_resp = client
            .get(format!("{base}/tx/{}", tx.hash))
            .query(&[("format", "json")])
            .send()
            .await
            .expect("tx");
        assert_eq!(tx_resp.status(), 200, "tx page should load for indexed tx");
        let tx_json: Value = tx_resp.json().await.expect("tx json");
        assert_eq!(tx_json["tx"]["hash"], json!(tx.hash));
        assert!(tx_json.get("calls").is_some());
    }

    // 404 for an unknown tx.
    let missing = client
        .get(format!("{base}/tx/0x{}", "00".repeat(32)))
        .query(&[("format", "json")])
        .send()
        .await
        .expect("missing");
    assert_eq!(missing.status(), 404);
}
