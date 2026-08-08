//! Background indexer tuned for a sub-second chain.
//!
//! New heads arrive instantly via a WebSocket `newHeads` subscription (with a
//! polling fallback). Blocks are *fetched* concurrently (batched receipts,
//! single `debug_traceBlockByNumber` call) and *written* by one serialized
//! SQLite writer, one transaction per block.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;
use tokio::sync::mpsc;
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
}

impl IndexerConfig {
    pub fn from_settings(s: &Settings) -> Self {
        Self {
            poll: Duration::from_secs_f64(s.poll_seconds.max(0.05)),
            batch: s.batch_size.max(1),
            concurrency: s.index_concurrency.max(1),
            ws_url: s.ws_url.clone(),
            index_ws: s.index_ws,
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
    let mut fee_tokens: HashSet<String> = HashSet::new();

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
            apply_receipt(&mut tx, receipt, &mut transfers, &mut fee_tokens);
        }
        txs.push(tx);
    }

    // Fee-token metadata is network I/O; fetch concurrently, best-effort.
    let mut tokens = Vec::new();
    if !fee_tokens.is_empty() {
        let mut set = tokio::task::JoinSet::new();
        for addr in &fee_tokens {
            let rpc = rpc.clone();
            let addr = addr.clone();
            set.spawn(async move { fetch_token_metadata(&rpc, &addr).await });
        }
        while let Some(res) = set.join_next().await {
            match res {
                Ok(meta) => tokens.push(meta),
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
    fee_tokens: &mut HashSet<String>,
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
        tx.contract_address = Some(addr.to_string());
    }
    if let Some(fee_token) = receipt.get("feeToken").and_then(Value::as_str) {
        tx.fee_token = Some(fee_token.to_string());
        fee_tokens.insert(fee_token.to_string());
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
        if decoded.name.as_deref() != Some("Transfer")
            && decoded.name.as_deref() != Some("TransferWithMemo")
        {
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
            token_addr: token,
            from_addr: from,
            to_addr: to,
            amount,
            timestamp: tx.timestamp,
            created_at: db::now_ts(),
        });
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
    block_num: u64,
) -> Result<()> {
    let Some(bundle) = fetch_block_bundle(rpc, block_num).await? else {
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
            let num = next;
            set.spawn(async move {
                if let Err(e) = index_block_send(&rpc, &tx, num).await {
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
            index_range_concurrent(&rpc, &bundle_tx, sent + 1, head, cfg.concurrency).await;
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
        index_range_concurrent(&rpc, &bundle_tx, start, target - 1, cfg.concurrency).await;

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
pub async fn run_forever(rpc: ChainRpc, db: Db, cfg: IndexerConfig) {
    let (bundle_tx, mut bundle_rx) = mpsc::channel::<BlockBundle>(1024);
    let writer_db = db.clone();
    let writer = tokio::spawn(async move {
        while let Some(bundle) = bundle_rx.recv().await {
            if let Err(e) = db::save_block_bundle(
                &writer_db,
                &bundle.block,
                &bundle.txs,
                &bundle.transfers,
                &bundle.tokens,
            ) {
                warn!("db write failed for block {}: {e:#}", bundle.block.number);
            }
        }
    });

    let forward = tokio::spawn(forward_loop(
        rpc.clone(),
        db.clone(),
        cfg.clone(),
        bundle_tx.clone(),
    ));
    let backfill = tokio::spawn(backfill_loop(
        rpc.clone(),
        db.clone(),
        cfg.clone(),
        bundle_tx.clone(),
    ));
    drop(bundle_tx);

    tokio::select! {
        r = forward => { if let Err(e) = r { warn!("forward loop ended: {e:#}"); } }
        r = backfill => { if let Err(e) = r { warn!("backfill loop ended: {e:#}"); } }
    }
    let _ = writer.await;
}
