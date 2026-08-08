//! Background indexer tuned for a sub-second chain.
//!
//! New heads arrive instantly via a WebSocket `newHeads` subscription (with a
//! polling fallback). Blocks are *fetched* concurrently (batched receipts,
//! single `debug_traceBlockByNumber` call) and *written* by one serialized
//! SQLite writer, one transaction per block.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use rusqlite::params;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};

use crate::config::Settings;
use crate::db::{self, Db};
use crate::decoder::{checksum_address, decode_event, flatten_trace};
use crate::models::{Block, Transaction, TransferEvent};
use crate::parse::{parse_block, parse_transaction};
use crate::rpc::ChainRpc;
use crate::tokens::{fetch_token_metadata, TokenMeta};

#[derive(Debug, Clone)]
pub struct IndexerConfig {
    pub poll: Duration,
    pub batch: u64,
    pub concurrency: usize,
    pub ws_url: String,
    pub index_ws: bool,
    pub stats_interval: Duration,
}

impl IndexerConfig {
    pub fn from_settings(s: &Settings) -> Self {
        Self {
            poll: Duration::from_secs_f64(s.poll_seconds.max(0.05)),
            batch: s.batch_size.max(1),
            concurrency: s.index_concurrency.max(1),
            ws_url: s.ws_url.clone(),
            index_ws: s.index_ws,
            stats_interval: Duration::from_secs_f64(s.stats_interval_seconds.max(1.0)),
        }
    }
}

/// Everything needed to persist one block in a single SQLite transaction.
pub struct BlockBundle {
    pub block: Block,
    pub txs: Vec<Transaction>,
    pub transfers: Vec<TransferEvent>,
    pub tokens: Vec<TokenMeta>,
}

/// Fetch a block and everything attached to it: transactions, receipts
/// (one batched call), traces (one call), transfers, and fee-token metadata.
pub async fn fetch_block_bundle(rpc: &ChainRpc, block_num: u64) -> Result<Option<BlockBundle>> {
    fetch_block_bundle_with_cache(rpc, Arc::new(Mutex::new(HashSet::new())), block_num).await
}

/// Like [`fetch_block_bundle`], but skips token-metadata fetches for addresses
/// already known to the indexer (shared across concurrent block tasks).
pub async fn fetch_block_bundle_with_cache(
    rpc: &ChainRpc,
    known_tokens: Arc<Mutex<HashSet<String>>>,
    block_num: u64,
) -> Result<Option<BlockBundle>> {
    let Some(raw_block) = rpc.eth_get_block_by_number(block_num, true).await? else {
        warn!("block {block_num} not found");
        return Ok(None);
    };
    let block = parse_block(&raw_block);
    let raw_txs = raw_block
        .get("transactions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // Traces: one call for the whole block, skipped for empty blocks.
    let mut trace_map: HashMap<String, Vec<Value>> = HashMap::new();
    if !raw_txs.is_empty() {
        if let Some(traces) = rpc.debug_trace_block(block_num).await {
            if let Some(entries) = traces.as_array() {
                for entry in entries {
                    let tx_hash = entry
                        .get("txHash")
                        .or_else(|| entry.get("transactionHash"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if tx_hash.is_empty() {
                        continue;
                    }
                    let result = entry.get("result").unwrap_or(entry);
                    trace_map.insert(tx_hash.to_string(), flatten_trace(result));
                }
            }
        }
    }

    // Receipts: one `eth_getBlockReceipts` call (or one batched request).
    let tx_hashes: Vec<String> = raw_txs
        .iter()
        .filter_map(|t| t.get("hash").and_then(Value::as_str).map(String::from))
        .collect();
    let receipt_by_hash: HashMap<String, Value> =
        match rpc.fetch_block_receipts(block_num, &tx_hashes).await? {
            Some(receipts) => receipts
                .iter()
                .filter_map(|r| {
                    r.get("transactionHash")
                        .and_then(Value::as_str)
                        .map(|h| (h.to_string(), r.clone()))
                })
                .collect(),
            None => HashMap::new(),
        };

    let mut txs = Vec::with_capacity(raw_txs.len());
    let mut transfers = Vec::new();
    let mut transfer_tokens: HashSet<String> = HashSet::new();

    for tx_data in &raw_txs {
        let tx_hash = tx_data.get("hash").and_then(Value::as_str).unwrap_or("");
        if tx_hash.is_empty() {
            continue;
        }
        let mut tx = parse_transaction(tx_data, &block);
        tx.timestamp = block.timestamp;
        if let Some(flat) = trace_map.get(tx_hash) {
            tx.trace_data = Some(serde_json::to_string(flat).unwrap_or_else(|_| "[]".into()));
        }
        if let Some(receipt) = receipt_by_hash.get(tx_hash) {
            apply_receipt(&mut tx, receipt, &mut transfers, &mut transfer_tokens);
        }
        txs.push(tx);
    }

    // Token metadata (fee tokens + transfer tokens) is network I/O; fetch
    // concurrently for addresses the indexer hasn't seen yet, best-effort.
    let mut tokens = Vec::new();
    let mut unseen: Vec<String> = Vec::new();
    for addr in &transfer_tokens {
        if !known_tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(addr)
        {
            unseen.push(addr.clone());
        }
    }
    if !unseen.is_empty() {
        let mut set = tokio::task::JoinSet::new();
        for addr in &unseen {
            let rpc = rpc.clone();
            let addr = addr.clone();
            set.spawn(async move { fetch_token_metadata(&rpc, &addr).await });
        }
        while let Some(res) = set.join_next().await {
            match res {
                Ok(meta) => {
                    known_tokens
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(meta.address.clone());
                    tokens.push(meta);
                }
                Err(e) => warn!("token metadata task failed: {e:#}"),
            }
        }
    }

    Ok(Some(BlockBundle {
        block,
        txs,
        transfers,
        tokens,
    }))
}

fn apply_receipt(
    tx: &mut Transaction,
    receipt: &Value,
    transfers: &mut Vec<TransferEvent>,
    transfer_tokens: &mut HashSet<String>,
) {
    tx.receipt_data = Some(serde_json::to_string(receipt).unwrap_or_else(|_| "{}".into()));
    tx.status = receipt
        .get("status")
        .map(crate::rpc::parse_int_any)
        .unwrap_or(1);
    tx.gas_used = receipt
        .get("gasUsed")
        .map(crate::rpc::parse_int_any)
        .unwrap_or(0);
    if let Some(addr) = receipt.get("contractAddress").and_then(Value::as_str) {
        tx.contract_address = Some(checksum_address(addr));
    }
    if let Some(fee_token) = receipt.get("feeToken").and_then(Value::as_str) {
        let fee_token = checksum_address(fee_token);
        tx.fee_token = Some(fee_token.clone());
        transfer_tokens.insert(fee_token);
    }
    if let Some(fee_amount) = receipt.get("feeAmount") {
        tx.fee_amount = match fee_amount {
            Value::String(s) if s.starts_with("0x") => {
                num_bigint::BigInt::parse_bytes(&s.as_bytes()[2..], 16)
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| s.clone())
            }
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => tx.fee_amount.clone(),
        };
    }
    if let Some(egp) = receipt.get("effectiveGasPrice") {
        tx.base_fee = match egp {
            Value::String(s) => s.clone(),
            _ => crate::rpc::int_to_hex_str(egp),
        };
    }

    // Decode Transfer / TransferWithMemo logs so address/token transfer tabs
    // have data.
    let logs = receipt
        .get("logs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for log in &logs {
        let Some(decoded) = decode_event(log) else {
            continue;
        };
        if !matches!(
            decoded.name.as_deref(),
            Some("Transfer") | Some("TransferWithMemo")
        ) {
            continue;
        }
        let mut from = String::new();
        let mut to = String::new();
        let mut amount = String::new();
        for p in &decoded.params {
            match p.name.as_str() {
                "from" => from = p.value.clone(),
                "to" => to = p.value.clone(),
                "amount" => amount = p.value.clone(),
                _ => {}
            }
        }
        // The receipt often omits `feeAmount`; the fee is settled as a
        // Transfer to the Fee Manager, so derive it from that log.
        if tx.fee_amount.parse::<i64>().map(|n| n <= 0).unwrap_or(true) {
            let token = log
                .get("address")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            let fee_manager = crate::decoder::FEE_MANAGER_ADDRESS.to_lowercase();
            let fee_token_matches = tx
                .fee_token
                .as_deref()
                .map(|f| f.to_lowercase() == token)
                .unwrap_or(false);
            if fee_token_matches && to.to_lowercase() == fee_manager && !amount.is_empty() {
                tx.fee_amount = amount.clone();
            }
        }
        if from.is_empty() || to.is_empty() || amount.is_empty() {
            continue;
        }
        let token = checksum_address(log.get("address").and_then(Value::as_str).unwrap_or(""));
        let log_index = log
            .get("logIndex")
            .and_then(Value::as_str)
            .and_then(|s| u64::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16).ok())
            .unwrap_or(0) as i64;
        transfers.push(TransferEvent {
            id: 0,
            tx_hash: tx.hash.clone(),
            block_number: tx.block_number,
            log_index,
            token_addr: token.clone(),
            from_addr: from,
            to_addr: to,
            amount,
            timestamp: tx.timestamp,
            created_at: db::now_ts(),
        });
        transfer_tokens.insert(token.clone());
    }
}

/// Fetch + persist one block (used by tests and simple callers).
pub async fn index_block(rpc: &ChainRpc, db: &Db, block_num: u64) -> Result<()> {
    let Some(bundle) = fetch_block_bundle(rpc, block_num).await? else {
        return Ok(());
    };
    db::save_block_bundle(
        db,
        &bundle.block,
        &bundle.txs,
        &bundle.transfers,
        &bundle.tokens,
    )?;
    Ok(())
}

/// Fetch a block and hand the bundle to the writer task.
async fn index_block_send(
    rpc: &ChainRpc,
    tx: &mpsc::Sender<BlockBundle>,
    known_tokens: Arc<Mutex<HashSet<String>>>,
    block_num: u64,
) -> Result<()> {
    let Some(bundle) = fetch_block_bundle_with_cache(rpc, known_tokens, block_num).await? else {
        return Ok(());
    };
    tx.send(bundle)
        .await
        .map_err(|_| anyhow::anyhow!("indexer writer task stopped"))?;
    Ok(())
}

/// Index every block in `from..=to`, fetching up to `concurrency` in parallel.
/// Blocks are spawned tip-first (`to` down to `from`) so the newest block is
/// written as soon as possible.
async fn index_range_concurrent(
    rpc: &ChainRpc,
    tx: &mpsc::Sender<BlockBundle>,
    from: u64,
    to: u64,
    concurrency: usize,
    known_tokens: Arc<Mutex<HashSet<String>>>,
) {
    if from > to {
        return;
    }
    let mut set = tokio::task::JoinSet::new();
    let mut next = to;
    let mut remaining = to - from + 1;
    loop {
        while remaining > 0 && set.len() < concurrency {
            let rpc = rpc.clone();
            let tx = tx.clone();
            let known = known_tokens.clone();
            let num = next;
            set.spawn(async move {
                if let Err(e) = index_block_send(&rpc, &tx, known, num).await {
                    warn!("index_block({num}) failed: {e:#}");
                }
            });
            next = next.wrapping_sub(1);
            remaining -= 1;
        }
        if set.is_empty() {
            break;
        }
        if let Some(Err(e)) = set.join_next().await {
            warn!("index task panicked: {e:#}");
        }
    }
}

async fn forward_loop(
    rpc: ChainRpc,
    db: Db,
    cfg: IndexerConfig,
    bundle_tx: mpsc::Sender<BlockBundle>,
    known_tokens: Arc<Mutex<HashSet<String>>>,
) {
    let (head_tx, mut head_rx) = mpsc::channel::<u64>(256);
    let watcher_rpc = rpc.clone();
    let ws_url = cfg.ws_url.clone();
    let index_ws = cfg.index_ws;
    let poll = cfg.poll;
    tokio::spawn(async move {
        crate::ws::head_watcher(watcher_rpc, ws_url, index_ws, poll, head_tx).await;
    });

    let mut sent = db::get_latest_block(&db)
        .map(|b| b.number as u64)
        .unwrap_or(0);
    info!("forward loop started from block {sent}");

    while let Some(head) = head_rx.recv().await {
        if sent == 0 && head > 0 {
            // Fresh DB: the backfill loop seeds history (including the head).
            // Wait a moment for its first write so the loops don't both fetch
            // the same blocks.
            let mut waited = Duration::ZERO;
            while db::get_latest_block(&db).is_none() && waited < Duration::from_secs(2) {
                tokio::time::sleep(Duration::from_millis(100)).await;
                waited += Duration::from_millis(100);
            }
            sent = db::get_latest_block(&db)
                .map(|b| b.number as u64)
                .unwrap_or(0);
            if sent >= head {
                continue;
            }
        }
        if head > sent {
            info!("new blocks: {sent} -> {head} (+{})", head - sent);
            index_range_concurrent(
                &rpc,
                &bundle_tx,
                sent + 1,
                head,
                cfg.concurrency,
                known_tokens.clone(),
            )
            .await;
            sent = head;
        }
    }
    warn!("forward loop stopped (head feed closed)");
}

async fn backfill_loop(
    rpc: ChainRpc,
    db: Db,
    cfg: IndexerConfig,
    bundle_tx: mpsc::Sender<BlockBundle>,
    known_tokens: Arc<Mutex<HashSet<String>>>,
) {
    info!(
        "backfill loop started (batch {}, concurrency {})",
        cfg.batch, cfg.concurrency
    );

    let mut consecutive_failures = 0u32;
    loop {
        let target = match db::get_min_block_number(&db) {
            Some(min) if min > 1 => min as u64,
            Some(_) => {
                // Fully backfilled; nothing to do until history changes.
                tokio::time::sleep(cfg.poll).await;
                continue;
            }
            None => {
                // Fresh DB: seed the descending frontier at the head so
                // history is indexed down to block 1 (the head itself is
                // written first, which the forward loop waits for).
                match rpc.eth_block_number().await {
                    Ok(head) => head + 1,
                    Err(e) => {
                        warn!("backfill head fetch failed: {e:#}");
                        tokio::time::sleep(cfg.poll).await;
                        continue;
                    }
                }
            }
        };
        let start = target.saturating_sub(cfg.batch).max(1);
        index_range_concurrent(
            &rpc,
            &bundle_tx,
            start,
            target - 1,
            cfg.concurrency,
            known_tokens.clone(),
        )
        .await;

        let new_min = db::get_min_block_number(&db).unwrap_or(0) as u64;
        if new_min >= target {
            // Nothing was persisted; back off instead of spinning.
            consecutive_failures += 1;
            tokio::time::sleep(Duration::from_millis(
                200 * consecutive_failures.min(10) as u64,
            ))
            .await;
        } else {
            consecutive_failures = 0;
            info!("backfill: {} remaining", new_min.saturating_sub(1));
        }
    }
}

/// Run the indexer: one serialized DB writer plus forward and backfill loops.
/// Newly written blocks are broadcast on `block_events` for live viewers.
pub async fn run_forever(
    rpc: ChainRpc,
    db: Db,
    cfg: IndexerConfig,
    block_events: broadcast::Sender<Value>,
) {
    let stats_interval = cfg.stats_interval;
    let stats_db = db.clone();
    tokio::spawn(async move { stats_loop(stats_db, stats_interval).await });

    // Seed the token-metadata cache and repair balances on legacy databases.
    let known_tokens = Arc::new(Mutex::new(
        db::get_all_token_addresses(&db).into_iter().collect(),
    ));
    let rebuild_db = db.clone();
    tokio::spawn(async move {
        let conn = db::lock(&rebuild_db);
        let has_transfers = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM transfer_events LIMIT 1)",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n != 0)
            .unwrap_or(false);
        let has_balances = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM token_balances LIMIT 1)",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n != 0)
            .unwrap_or(false);
        drop(conn);
        // Legacy databases have transfers but no incremental balances yet;
        // rebuild once so holder counts and holdings stay correct.
        if has_transfers && !has_balances {
            let conn = db::lock(&rebuild_db);
            if let Err(e) = db::rebuild_token_balances(&conn) {
                warn!("token balance rebuild failed: {e:#}");
            }
        }
    });

    let (bundle_tx, mut bundle_rx) = mpsc::channel::<BlockBundle>(1024);
    let writer_db = db.clone();
    let writer = tokio::spawn(async move {
        // Live feed = new tip blocks only; backfill writes are older numbers
        // and would just duplicate/reorder the home page, so track the max.
        let mut max_block = -1i64;
        while let Some(bundle) = bundle_rx.recv().await {
            if let Err(e) = db::save_block_bundle(
                &writer_db,
                &bundle.block,
                &bundle.txs,
                &bundle.transfers,
                &bundle.tokens,
            ) {
                warn!("db write failed for block {}: {e:#}", bundle.block.number);
                continue;
            }
            // Notify live viewers as soon as the block is durably written.
            if bundle.block.number > max_block {
                max_block = bundle.block.number;
                let _ = block_events.send(crate::models::block_event_json(
                    &bundle.block,
                    &bundle.txs,
                    crate::models::STREAM_TX_CAP,
                ));
            }
        }
    });

    let forward = tokio::spawn(forward_loop(
        rpc.clone(),
        db.clone(),
        cfg.clone(),
        bundle_tx.clone(),
        known_tokens.clone(),
    ));
    let backfill = tokio::spawn(backfill_loop(
        rpc.clone(),
        db.clone(),
        cfg.clone(),
        bundle_tx.clone(),
        known_tokens.clone(),
    ));
    drop(bundle_tx);

    tokio::select! {
        r = forward => { if let Err(e) = r { warn!("forward loop ended: {e:#}"); } }
        r = backfill => { if let Err(e) = r { warn!("backfill loop ended: {e:#}"); } }
    }
    let _ = writer.await;
}

// ---------------------------------------------------------------------------
// Precomputed network stats
// ---------------------------------------------------------------------------

/// Recompute the home-page stats blob into the `kv` table every interval so
/// the web layer never has to scan history at request time.
async fn stats_loop(db: Db, interval: Duration) {
    loop {
        if let Err(e) = compute_and_store_stats(&db) {
            warn!("stats recompute failed: {e:#}");
        }
        tokio::time::sleep(interval).await;
    }
}

fn compute_and_store_stats(db: &Db) -> Result<()> {
    let conn = db::lock(db);
    let now = db::now_ts();

    let total_blocks: i64 = conn.query_row("SELECT COUNT(*) FROM blocks", [], |r| r.get(0))?;
    let total_txns: i64 = conn.query_row("SELECT COUNT(*) FROM transactions", [], |r| r.get(0))?;
    let token_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM token_metadata", [], |r| r.get(0))?;
    let txns_24h: i64 = conn.query_row(
        "SELECT COUNT(*) FROM transactions WHERE timestamp >= ?1",
        params![now - 86400],
        |r| r.get(0),
    )?;
    let blocks_24h: i64 = conn.query_row(
        "SELECT COUNT(*) FROM blocks WHERE timestamp >= ?1",
        params![now - 86400],
        |r| r.get(0),
    )?;

    // Rolling window over the newest blocks (cheap PK scan, no history sweep).
    let window: i64 = 100;
    let (min_ts, max_ts, tx_sum, gas_sum, gas_den, n): (i64, i64, i64, f64, f64, i64) = conn
        .query_row(
            "SELECT MIN(timestamp), MAX(timestamp), SUM(tx_count),
                    SUM(CASE WHEN gas_limit > 0 THEN gas_used * 1.0 / gas_limit ELSE 0 END),
                    SUM(CASE WHEN gas_limit > 0 THEN 1 ELSE 0 END),
                    COUNT(*)
             FROM (SELECT timestamp, tx_count, gas_used, gas_limit
                   FROM blocks ORDER BY number DESC LIMIT ?1)",
            params![window],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                    r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                    r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                ))
            },
        )
        .unwrap_or((0, 0, 0, 0.0, 0.0, 0));

    let span_secs = (max_ts - min_ts).max(1) as f64;
    let avg_block_time_ms = if n > 1 {
        span_secs / (n - 1) as f64 * 1000.0
    } else {
        0.0
    };
    let tps = if span_secs > 0.0 {
        tx_sum as f64 / span_secs
    } else {
        0.0
    };
    let gas_util_pct = if gas_den > 0.0 {
        gas_sum / gas_den * 100.0
    } else {
        0.0
    };
    let latest_block = conn.query_row("SELECT MAX(number) FROM blocks", [], |r| {
        r.get::<_, Option<i64>>(0)
    })?;

    let stats = serde_json::json!({
        "latest_block": latest_block,
        "total_blocks": total_blocks,
        "total_txns": total_txns,
        "token_count": token_count,
        "txns_24h": txns_24h,
        "blocks_24h": blocks_24h,
        "avg_block_time_ms": avg_block_time_ms,
        "tps": tps,
        "gas_util_pct": gas_util_pct,
        "updated_at": now,
    });
    db::set_kv(&conn, "stats", &stats.to_string())?;
    Ok(())
}
