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
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{info, warn};

use crate::anchoring::ANCHORING_ADDRESS;
use crate::config::Settings;
use crate::db::{self, Db};
use crate::decoder::{checksum_address, decode_event, flatten_trace};
use crate::models::{AnchoredEvent, Block, Transaction, TransferEvent};
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
    pub anchored: Vec<AnchoredEvent>,
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
    // Exactly three RPC calls for the whole block, sent as one batched
    // request: block with tx bodies, receipts, trace. Each can fail
    // independently (tracing is unsupported on some chains).
    let num_hex = format!("0x{block_num:x}");
    let results = rpc
        .batch_call(vec![
            (
                "eth_getBlockByNumber".to_string(),
                json!([num_hex.clone(), true]),
            ),
            ("eth_getBlockReceipts".to_string(), json!([num_hex.clone()])),
            (
                "debug_traceBlockByNumber".to_string(),
                json!([num_hex, {"tracer": "callTracer"}]),
            ),
        ])
        .await?;
    let mut iter = results.into_iter();
    let Some(Ok(raw_block)) = iter.next() else {
        warn!("block {block_num} not found");
        return Ok(None);
    };
    if raw_block.is_null() {
        warn!("block {block_num} not found");
        return Ok(None);
    }
    let receipts = iter.next().and_then(|r| r.ok());
    let trace_result = iter.next().and_then(|r| r.ok());

    let block = parse_block(&raw_block);
    let raw_txs = raw_block
        .get("transactions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // Traces (unsupported on this chain — degrades to no trace data).
    let mut trace_map: HashMap<String, Vec<Value>> = HashMap::new();
    if !raw_txs.is_empty() {
        if let Some(traces) = trace_result {
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

    // Receipts by block, keyed by tx hash.
    let receipt_by_hash: HashMap<String, Value> = match receipts {
        Some(Value::Array(receipts)) => receipts
            .iter()
            .filter_map(|r| {
                r.get("transactionHash")
                    .and_then(Value::as_str)
                    .map(|h| (h.to_string(), r.clone()))
            })
            .collect(),
        _ => HashMap::new(),
    };

    let mut txs = Vec::with_capacity(raw_txs.len());
    let mut transfers = Vec::new();
    let mut anchored = Vec::new();
    let mut transfer_tokens: HashSet<String> = HashSet::new();
    // Running log index across the whole block. Receipts normally carry a
    // unique `logIndex`; when one is missing or mangled the counter fills in
    // a unique value so the DB's (block_number, log_index) key never
    // collides within a block.
    let mut next_log_index = 0u64;

    for tx_data in &raw_txs {
        let tx_hash = tx_data.get("hash").and_then(Value::as_str).unwrap_or("");
        if tx_hash.is_empty() {
            continue;
        }
        let mut tx = parse_transaction(tx_data, &block);
        tx.timestamp = block.timestamp;
        // Canonical RLP: re-encode the RPC object with the official tempo
        // primitives (the block response already carries the full signed tx,
        // so no extra per-tx RPC is needed). Byte-identical to the node's
        // `eth_getRawTransactionByHash` for the 0x76 type.
        if let Ok(signed) = serde_json::from_value::<crate::tempo::AASigned>(tx_data.clone()) {
            let mut buf = Vec::new();
            signed.eip2718_encode(&mut buf);
            tx.raw = Some(format!("0x{}", hex::encode(buf)));
        }
        if let Some(flat) = trace_map.get(tx_hash) {
            tx.trace_data = Some(serde_json::to_string(flat).unwrap_or_else(|_| "[]".into()));
        }
        if let Some(receipt) = receipt_by_hash.get(tx_hash) {
            apply_receipt(
                &mut tx,
                receipt,
                &mut transfers,
                &mut anchored,
                &mut transfer_tokens,
                &mut next_log_index,
            );
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
        anchored,
        tokens,
    }))
}

fn apply_receipt(
    tx: &mut Transaction,
    receipt: &Value,
    transfers: &mut Vec<TransferEvent>,
    anchored: &mut Vec<AnchoredEvent>,
    transfer_tokens: &mut HashSet<String>,
    next_log_index: &mut u64,
) {
    tx.receipt_data = Some(serde_json::to_string(receipt).unwrap_or_else(|_| "{}".into()));
    tx.status = receipt
        .get("status")
        .map(crate::rpc::parse_int_any)
        .unwrap_or(1);
    // Some nodes fill receipt `to` with the first call's destination even when
    // the tx itself has no top-level `to` (tempo-style `calls` transactions).
    if tx.to_addr.is_none() {
        if let Some(to) = receipt.get("to").and_then(Value::as_str) {
            tx.to_addr = Some(to.to_string());
        }
    }
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
        // `logIndex` is the per-block unique half of the transfer_events key.
        // Prefer the node's value, but every log still advances the running
        // counter (undecodable logs occupy index slots too), so a missing or
        // unparsable index gets a unique fallback instead of colliding at 0.
        let log_index = log
            .get("logIndex")
            .map(crate::rpc::parse_int_any)
            .filter(|n| *n >= 0)
            .unwrap_or(*next_log_index as i64);
        *next_log_index = (*next_log_index).max(log_index as u64 + 1);

        let Some(decoded) = decode_event(log) else {
            continue;
        };
        if decoded.name.as_deref() == Some("Anchored") {
            // Only the precompile's own log carries a trustworthy namespace:
            // the caller it records is the sender it saw, which a contract
            // emitting the same signature could claim to be anyone.
            if checksum_address(&decoded.contract) == ANCHORING_ADDRESS {
                match anchored_event(&decoded, tx, log_index) {
                    Some(event) => anchored.push(event),
                    None => warn!("undecodable Anchored log {log_index} in {}", tx.hash),
                }
            }
            continue;
        }
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

/// One decoded `Anchored` log as a storable row, or `None` when the log does
/// not carry a whole commitment — the precompile keeps only the head, so a row
/// here is the only record that this revision ever existed.
pub fn anchored_event(
    decoded: &crate::decoder::DecodedEvent,
    tx: &Transaction,
    log_index: i64,
) -> Option<AnchoredEvent> {
    let arg = |name: &str| {
        decoded
            .params
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.value.as_str())
    };
    let (key, commitment) = (arg("key")?, arg("commitment")?);
    // `0x` + 64 hex digits. A truncated log decodes to a short or empty value,
    // which would store a head the chain never wrote.
    if key.len() != 66 || commitment.len() != 66 {
        return None;
    }
    Some(AnchoredEvent {
        tx_hash: tx.hash.clone(),
        block_number: tx.block_number,
        log_index,
        namespace: checksum_address(arg("caller")?),
        key: key.to_string(),
        commitment: commitment.to_string(),
        metadata: arg("metadata").unwrap_or("0x").to_string(),
        timestamp: tx.timestamp,
        created_at: db::now_ts(),
    })
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
        &bundle.anchored,
        &bundle.tokens,
    )?;
    Ok(())
}

/// Sleep for `dur` unless shutdown was requested, in which case return
/// immediately with `true`. Lets background loops stop promptly on Ctrl+C.
async fn sleep_or_shutdown(shutdown: &mut watch::Receiver<bool>, dur: Duration) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(dur) => false,
        r = shutdown.changed() => r.is_err() || *shutdown.borrow(),
    }
}

/// Index every block in `from..=to`, fetching up to `concurrency` in
/// parallel, and return the bundles in number order (the caller decides how
/// to persist/broadcast them — this lets the forward loop emit a gapless live
/// feed regardless of fetch completion order or writer timing).
async fn index_range_concurrent(
    rpc: &ChainRpc,
    from: u64,
    to: u64,
    concurrency: usize,
    known_tokens: Arc<Mutex<HashSet<String>>>,
    shutdown: &watch::Receiver<bool>,
) -> Vec<BlockBundle> {
    if from > to {
        return Vec::new();
    }
    let mut set = tokio::task::JoinSet::new();
    let mut next = to;
    let mut remaining = to - from + 1;
    // Cap the reservation: ranges can be huge after downtime (forward loop
    // catch-up), and each bundle is a sizable struct.
    let mut out = Vec::with_capacity((to - from + 1).min(16384) as usize);
    loop {
        // Abort in-flight fetches as soon as shutdown is requested.
        if *shutdown.borrow() {
            set.abort_all();
            break;
        }
        while remaining > 0 && set.len() < concurrency {
            let rpc = rpc.clone();
            let known = known_tokens.clone();
            let num = next;
            set.spawn(async move {
                match fetch_block_bundle_with_cache(&rpc, known, num).await {
                    Ok(Some(bundle)) => Some(bundle),
                    Ok(None) => {
                        warn!("block {num} not found");
                        None
                    }
                    Err(e) => {
                        warn!("index_block({num}) failed: {e:#}");
                        None
                    }
                }
            });
            next = next.wrapping_sub(1);
            remaining -= 1;
        }
        if set.is_empty() {
            break;
        }
        if let Some(Ok(Some(bundle))) = set.join_next().await {
            out.push(bundle);
        }
    }
    out.sort_by_key(|b| b.block.number);
    out
}

async fn forward_loop(
    rpc: ChainRpc,
    db: Db,
    cfg: IndexerConfig,
    bundle_tx: mpsc::Sender<BlockBundle>,
    known_tokens: Arc<Mutex<HashSet<String>>>,
    shutdown: watch::Receiver<bool>,
    block_events: broadcast::Sender<Value>,
) {
    let (head_tx, mut head_rx) = mpsc::channel::<u64>(256);
    let watcher_rpc = rpc.clone();
    let ws_url = cfg.ws_url.clone();
    let index_ws = cfg.index_ws;
    let poll = cfg.poll;
    let watcher_shutdown = shutdown.clone();
    tokio::spawn(async move {
        crate::ws::head_watcher(
            watcher_rpc,
            ws_url,
            index_ws,
            poll,
            head_tx,
            watcher_shutdown,
        )
        .await;
    });

    let mut sent = db::get_latest_block(&db)
        .map(|b| b.number as u64)
        .unwrap_or(0);
    info!("forward loop started from block {sent}");

    while let Some(head) = head_rx.recv().await {
        if *shutdown.borrow() {
            break;
        }
        // Track the chain tip for the home page's index-progress display.
        db::set_chain_head(&db, head as i64);
        if sent == 0 && head > 0 {
            // Fresh DB: the backfill loop seeds history (including the head).
            // Wait for its first write so the loops don't both fetch the
            // same blocks — and never try to index the whole history here.
            let mut waited = Duration::ZERO;
            while db::get_latest_block(&db).is_none() && waited < Duration::from_secs(5) {
                tokio::time::sleep(Duration::from_millis(100)).await;
                waited += Duration::from_millis(100);
            }
            sent = db::get_latest_block(&db)
                .map(|b| b.number as u64)
                .unwrap_or(0);
            if sent == 0 {
                // Backfill hasn't seeded anything yet; wait for the next head
                // instead of attempting a full-history range.
                continue;
            }
            if sent >= head {
                continue;
            }
        }
        if head > sent {
            info!("new blocks: {sent} -> {head} (+{})", head - sent);
            let bundles = index_range_concurrent(
                &rpc,
                sent + 1,
                head,
                cfg.concurrency,
                known_tokens.clone(),
                &shutdown,
            )
            .await;
            // Live feed, emitted strictly in number order from the fetched
            // bundles: concurrent fetches complete out of order, and the
            // writer persists asynchronously, so neither can be broadcast
            // directly. Backfill history never reaches this loop, so the
            // stream stays tip-only and gapless.
            for b in &bundles {
                let _ = block_events.send(crate::models::block_event_json(
                    &b.block,
                    &b.txs,
                    crate::models::STREAM_TX_CAP,
                ));
            }
            for b in bundles {
                if bundle_tx.send(b).await.is_err() {
                    warn!("writer stopped; forward loop exiting");
                    return;
                }
            }
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
    mut shutdown: watch::Receiver<bool>,
) {
    info!(
        "backfill loop started (batch {}, concurrency {})",
        cfg.batch, cfg.concurrency
    );

    let mut consecutive_failures = 0u32;
    loop {
        if *shutdown.borrow() {
            break;
        }
        let target = match db::get_min_block_number(&db) {
            Some(min) if min > 1 => min as u64,
            Some(_) => {
                // Fully backfilled; nothing to do until history changes.
                if sleep_or_shutdown(&mut shutdown, cfg.poll).await {
                    break;
                }
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
                        if sleep_or_shutdown(&mut shutdown, cfg.poll).await {
                            break;
                        }
                        continue;
                    }
                }
            }
        };
        let start = target.saturating_sub(cfg.batch).max(1);
        let bundles = index_range_concurrent(
            &rpc,
            start,
            target - 1,
            cfg.concurrency,
            known_tokens.clone(),
            &shutdown,
        )
        .await;
        // Send tip-first so the head lands in the DB as soon as possible
        // (the forward loop waits for it on a fresh database).
        for b in bundles.into_iter().rev() {
            if bundle_tx.send(b).await.is_err() {
                warn!("writer stopped; backfill loop exiting");
                return;
            }
        }
        // The writer persists asynchronously; wait until this batch is on
        // disk so the progress check below isn't reading stale state.
        // (`min == 0` means the table is still empty — keep waiting.)
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let min = db::get_min_block_number(&db).unwrap_or(0) as u64;
            let persisted = min != 0 && min <= start;
            if persisted || *shutdown.borrow() || tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let new_min = db::get_min_block_number(&db).unwrap_or(0) as u64;
        if new_min >= target {
            // Nothing was persisted; back off instead of spinning.
            consecutive_failures += 1;
            let backoff = Duration::from_millis(200 * consecutive_failures.min(10) as u64);
            if sleep_or_shutdown(&mut shutdown, backoff).await {
                break;
            }
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
    shutdown: watch::Receiver<bool>,
) {
    let stats_interval = cfg.stats_interval;
    let stats_db = db.clone();
    let stats_shutdown = shutdown.clone();
    tokio::spawn(async move { stats_loop(stats_db, stats_interval, stats_shutdown).await });

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
        } else if has_balances {
            // Backfill token_metadata.holder_count (stale rows written
            // before the BLOB-key fix); cheap and idempotent.
            let conn = db::lock(&rebuild_db);
            if let Err(e) = db::sync_holder_counts(&conn) {
                warn!("holder count sync failed: {e:#}");
            }
        }
    });

    let (bundle_tx, mut bundle_rx) = mpsc::channel::<BlockBundle>(1024);
    let writer_db = db.clone();
    let writer_shutdown = shutdown.clone();
    // The writer only persists. Live streaming happens in the forward loop,
    // which re-reads each freshly indexed range from the DB in number order
    // (concurrent fetches complete out of order, so the writer cannot emit
    // gaplessly, and backfill history must not reach the live feed at all).
    let writer = tokio::spawn(async move {
        while let Some(bundle) = bundle_rx.recv().await {
            // On shutdown, stop draining the queue: in-flight bundles are
            // dropped and re-fetched on the next start.
            if *writer_shutdown.borrow() {
                break;
            }
            if let Err(e) = db::save_block_bundle(
                &writer_db,
                &bundle.block,
                &bundle.txs,
                &bundle.transfers,
                &bundle.anchored,
                &bundle.tokens,
            ) {
                warn!("db write failed for block {}: {e:#}", bundle.block.number);
                continue;
            }
        }
    });

    let forward = tokio::spawn(forward_loop(
        rpc.clone(),
        db.clone(),
        cfg.clone(),
        bundle_tx.clone(),
        known_tokens.clone(),
        shutdown.clone(),
        block_events.clone(),
    ));
    let backfill = tokio::spawn(backfill_loop(
        rpc.clone(),
        db.clone(),
        cfg.clone(),
        bundle_tx.clone(),
        known_tokens.clone(),
        shutdown,
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
async fn stats_loop(db: Db, interval: Duration, mut shutdown: watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            break;
        }
        if let Err(e) = compute_and_store_stats(&db) {
            warn!("stats recompute failed: {e:#}");
        }
        if sleep_or_shutdown(&mut shutdown, interval).await {
            break;
        }
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
    // Timestamps are ms-precision (`timestamp_ms`); the chain produces blocks
    // faster than once per second, so second-granularity timestamps would
    // quantize the block time to 1s.
    let window: i64 = 100;
    let (min_ms, max_ms, tx_sum, gas_sum, gas_den, n): (i64, i64, i64, f64, f64, i64) = conn
        .query_row(
            "SELECT MIN(timestamp_ms), MAX(timestamp_ms), SUM(tx_count),
                    SUM(CASE WHEN gas_limit > 0 THEN gas_used * 1.0 / gas_limit ELSE 0 END),
                    SUM(CASE WHEN gas_limit > 0 THEN 1 ELSE 0 END),
                    COUNT(*)
             FROM (SELECT timestamp_ms, tx_count, gas_used, gas_limit
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

    let span_ms = (max_ms - min_ms).max(1) as f64;
    let avg_block_time_ms = if n > 1 { span_ms / (n - 1) as f64 } else { 0.0 };
    let tps = if span_ms > 0.0 {
        tx_sum as f64 / span_ms * 1000.0
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
