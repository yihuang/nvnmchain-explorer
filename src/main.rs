use std::sync::{Arc, Mutex};

use anyhow::Context;
use tokio::net::TcpListener;
use tracing::info;

use nvnmchain_explorer::config::Settings;
use nvnmchain_explorer::db::{self, Db};
use nvnmchain_explorer::indexer::{self, IndexerConfig};
use nvnmchain_explorer::rpc::ChainRpc;
use nvnmchain_explorer::web;
use tokio::sync::broadcast;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nvnmchain_explorer=info".into()),
        )
        .init();

    let cfg = Settings::from_env();
    info!(
        "starting nvnmchain Explorer (rpc={}, db={})",
        cfg.rpc_url, cfg.db_path
    );

    let conn = db::init_db(&cfg.db_path).context("initialize database")?;
    let db: Db = Arc::new(Mutex::new(conn));
    let rpc = ChainRpc::from_settings(&cfg)?;
    let tera = web::build_tera(db.clone())?;

    // Background indexer: instant heads via WebSocket (poll fallback),
    // concurrent block fetching, serialized SQLite writes.
    let indexer_rpc = ChainRpc::from_settings(&cfg)?;
    let indexer_db = db.clone();
    let indexer_cfg = IndexerConfig::from_settings(&cfg);
    let ws_url = cfg.ws_url.clone();
    let (block_tx, _) = broadcast::channel::<serde_json::Value>(256);
    let indexer_block_tx = block_tx.clone();
    tokio::spawn(async move {
        info!("indexer websocket feed: {ws_url}");
        indexer::run_forever(indexer_rpc, indexer_db, indexer_cfg, indexer_block_tx).await
    });

    let state = web::AppState {
        db,
        rpc,
        cfg: cfg.clone(),
        tera,
        block_events: block_tx,
    };
    let app = web::app(state);

    let addr = format!("{}:{}", cfg.host, cfg.port);
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    info!("listening on http://{addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    info!("shutting down");
}
