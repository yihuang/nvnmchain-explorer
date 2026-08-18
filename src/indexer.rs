//! Background indexer tuned for a sub-second chain.
//!
//! New heads arrive instantly via a WebSocket `newHeads` subscription (with a
//! polling fallback). Blocks are fetched in multi-block JSON-RPC HTTP batches
//! (receipts only when a block has transactions) and written by one serialized
//! SQLite writer. Tip catch-up is coalesced and windowed so the writer is not
//! left idle; backfill yields the RPC when the tip falls behind.

use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::Result;
use rusqlite::params;
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{info, warn};

use crate::anchoring::ANCHORING_ADDRESS;
use crate::config::Settings;
use crate::db::{self, Db};
use crate::decoder::{checksum_address, decode_event};
use crate::models::{AnchoredEvent, BlockBundle, RegistryDeployed, Transaction, TransferEvent};
use crate::parse::{parse_block, parse_transaction};
use crate::rpc::ChainRpc;
use crate::tokens::fetch_token_metadata;

/// How many blocks share one JSON-RPC HTTP request. Sixteen empty blocks
/// (32 methods when receipts are included) measured at ~265ms against the
/// public RPC — one RTT for a range that used to be 16 separate connections.
const RPC_BLOCK_BATCH: u64 = 16;

/// How far the indexed tip may lag before backfill yields the RPC so catch-up
/// is not starved by history.
const TIP_YIELD_LAG: u64 = 16;

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

/// Fetch a block and everything attached to it: transactions, receipts when
/// the block has any, transfers, and fee-token metadata.
pub async fn fetch_block_bundle(rpc: &ChainRpc, block_num: u64) -> Result<Option<BlockBundle>> {
    let mut bundles = fetch_block_bundles(
        rpc,
        Arc::new(Mutex::new(HashSet::new())),
        block_num..=block_num,
    )
    .await?;
    Ok(bundles.pop())
}

/// Split `from..=to` into contiguous chunks of at most `batch` blocks.
fn block_chunks(from: u64, to: u64, batch: u64) -> impl Iterator<Item = RangeInclusive<u64>> {
    let batch = batch.max(1);
    std::iter::successors((from <= to).then_some(from), move |&start| {
        start.checked_add(batch).filter(|&next| next <= to)
    })
    .map(move |start| start..=start.saturating_add(batch - 1).min(to))
}

/// One non-null `eth_getBlockByNumber(full=true)` result plus optional
/// receipts. Token metadata is filled in later — this is pure CPU.
fn assemble_bundle(raw_block: &Value, receipts: Option<&Value>) -> BlockBundle {
    let block = parse_block(raw_block);
    let receipt_by_hash: HashMap<&str, &Value> = match receipts {
        Some(Value::Array(receipts)) => receipts
            .iter()
            .filter_map(|r| {
                r.get("transactionHash")
                    .and_then(Value::as_str)
                    .map(|h| (h, r))
            })
            .collect(),
        _ => HashMap::new(),
    };

    let raw_txs = raw_block
        .get("transactions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut txs = Vec::with_capacity(raw_txs.len());
    let mut transfers = Vec::new();
    let mut anchored = Vec::new();
    let mut registries = Vec::new();
    let mut next_log_index = 0u64;

    for tx_data in raw_txs {
        let tx_hash = tx_data.get("hash").and_then(Value::as_str).unwrap_or("");
        if tx_hash.is_empty() {
            continue;
        }
        let mut tx = parse_transaction(tx_data, &block);
        tx.timestamp = block.timestamp;
        // Re-encode the RPC object with the official tempo primitives; the
        // block already carries the signed tx, so no extra per-tx RPC.
        if let Ok(signed) = serde_json::from_value::<crate::tempo::AASigned>(tx_data.clone()) {
            let mut buf = Vec::new();
            signed.eip2718_encode(&mut buf);
            tx.raw = Some(format!("0x{}", hex::encode(buf)));
        }
        if let Some(receipt) = receipt_by_hash.get(tx_hash) {
            apply_receipt(
                &mut tx,
                receipt,
                &mut transfers,
                &mut anchored,
                &mut registries,
                &mut next_log_index,
            );
        }
        txs.push(tx);
    }

    BlockBundle {
        block,
        txs,
        transfers,
        anchored,
        tokens: Vec::new(),
        registries,
    }
}

/// Fetch `nums` in one HTTP request of `eth_getBlockByNumber`, then a second
/// request of `eth_getBlockReceipts` only for blocks that have transactions.
async fn fetch_raw_blocks(
    rpc: &ChainRpc,
    nums: RangeInclusive<u64>,
) -> Result<Vec<(Value, Option<Value>)>> {
    if nums.is_empty() {
        return Ok(Vec::new());
    }
    let calls: Vec<(String, Value)> = nums
        .clone()
        .map(|n| {
            (
                "eth_getBlockByNumber".into(),
                json!([format!("0x{n:x}"), true]),
            )
        })
        .collect();
    let results = rpc.batch_call(calls).await?;

    let mut blocks = Vec::with_capacity(results.len());
    let mut need_receipts = Vec::new();
    for (num, res) in nums.zip(results) {
        match res {
            Ok(v) if !v.is_null() => {
                let has_txs = v
                    .get("transactions")
                    .and_then(Value::as_array)
                    .is_some_and(|txs| !txs.is_empty());
                if has_txs {
                    need_receipts.push((blocks.len(), num));
                }
                blocks.push((v, None));
            }
            Ok(_) => warn!("block {num} not found"),
            Err(e) => warn!("getBlock({num}) failed: {e}"),
        }
    }
    if need_receipts.is_empty() {
        return Ok(blocks);
    }

    let calls: Vec<(String, Value)> = need_receipts
        .iter()
        .map(|(_, n)| ("eth_getBlockReceipts".into(), json!([format!("0x{n:x}")])))
        .collect();
    match rpc.batch_call(calls).await {
        Ok(results) => {
            for ((idx, num), res) in need_receipts.into_iter().zip(results) {
                match res {
                    Ok(v) => blocks[idx].1 = Some(v),
                    Err(e) => warn!("getBlockReceipts({num}) failed: {e}"),
                }
            }
        }
        Err(e) => warn!("receipts batch failed: {e:#}"),
    }
    Ok(blocks)
}

async fn fetch_block_bundles(
    rpc: &ChainRpc,
    known_tokens: Arc<Mutex<HashSet<String>>>,
    nums: RangeInclusive<u64>,
) -> Result<Vec<BlockBundle>> {
    let mut bundles: Vec<BlockBundle> = fetch_raw_blocks(rpc, nums)
        .await?
        .iter()
        .map(|(raw, receipts)| assemble_bundle(raw, receipts.as_ref()))
        .collect();
    attach_token_metadata(rpc, &known_tokens, &mut bundles).await;
    Ok(bundles)
}

fn bundle_token_addrs(bundle: &BlockBundle) -> impl Iterator<Item = &str> {
    bundle
        .transfers
        .iter()
        .map(|t| t.token_addr.as_str())
        .chain(bundle.txs.iter().filter_map(|t| t.fee_token.as_deref()))
}

async fn attach_token_metadata(
    rpc: &ChainRpc,
    known_tokens: &Arc<Mutex<HashSet<String>>>,
    bundles: &mut [BlockBundle],
) {
    let mut unseen = HashSet::new();
    {
        let known = known_tokens.lock().unwrap_or_else(|e| e.into_inner());
        for bundle in bundles.iter() {
            for addr in bundle_token_addrs(bundle) {
                if !known.contains(addr) {
                    unseen.insert(addr.to_string());
                }
            }
        }
    }
    if unseen.is_empty() {
        return;
    }

    let mut set = tokio::task::JoinSet::new();
    for addr in unseen {
        let rpc = rpc.clone();
        set.spawn(async move { fetch_token_metadata(&rpc, &addr).await });
    }
    let mut metas = HashMap::new();
    while let Some(res) = set.join_next().await {
        match res {
            Ok(meta) => {
                metas.insert(meta.address.clone(), meta);
            }
            Err(e) => warn!("token metadata task failed: {e:#}"),
        }
    }
    known_tokens
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .extend(metas.keys().cloned());

    for bundle in bundles.iter_mut() {
        // Deduped: one token backs every transfer of it in the block, and the
        // rows are upserted by address.
        let tokens: Vec<_> = bundle_token_addrs(bundle)
            .collect::<HashSet<_>>()
            .into_iter()
            .filter_map(|addr| metas.get(addr).cloned())
            .collect();
        bundle.tokens = tokens;
    }
}

fn apply_receipt(
    tx: &mut Transaction,
    receipt: &Value,
    transfers: &mut Vec<TransferEvent>,
    anchored: &mut Vec<AnchoredEvent>,
    registries: &mut Vec<RegistryDeployed>,
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
        tx.fee_token = Some(checksum_address(fee_token));
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

        // Only the precompile's own log carries a trustworthy namespace: the
        // caller it records is the sender it saw, which a contract emitting the
        // same signature could claim to be anyone. Rejected before decoding, so
        // an impostor's payload is never worth decoding.
        let topic0 = log.pointer("/topics/0").and_then(Value::as_str);
        let emitter = log.get("address").and_then(Value::as_str).unwrap_or("");
        if topic0 == Some(crate::decoder::ANCHORED_TOPIC.as_str())
            && !emitter.eq_ignore_ascii_case(ANCHORING_ADDRESS)
        {
            continue;
        }

        let Some(decoded) = decode_event(log) else {
            continue;
        };
        // Both anchoring events are matched on topic0 rather than the decoded
        // display name, which is a UI label: renaming one must not silently
        // stop indexing it.
        if decoded.topic0 == *crate::decoder::REGISTRY_DEPLOYED_TOPIC {
            // The emitting factory rides in the row — reads decide which
            // factory to trust, the way transfer rows record their token.
            if let Some(event) = registry_deployed(&decoded, tx) {
                registries.push(event);
            }
            continue;
        }
        if decoded.topic0 == *crate::decoder::ANCHORED_TOPIC {
            match anchored_event(&decoded, tx, log_index) {
                Some(event) => anchored.push(event),
                None => warn!("undecodable Anchored log {log_index} in {}", tx.hash),
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
            token_addr: token,
            from_addr: from,
            to_addr: to,
            amount,
            timestamp: tx.timestamp,
            created_at: db::now_ts(),
        });
    }
}

/// One decoded argument of a log, by the name `decode_event` gave it.
fn param<'a>(decoded: &'a crate::decoder::DecodedEvent, name: &str) -> Option<&'a str> {
    decoded
        .params
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.value.as_str())
}

/// One decoded `Anchored` log as a storable row, or `None` when the log does
/// not carry a whole commitment — the precompile keeps only the head, so a row
/// here is the only record that this revision ever existed.
pub fn anchored_event(
    decoded: &crate::decoder::DecodedEvent,
    tx: &Transaction,
    log_index: i64,
) -> Option<AnchoredEvent> {
    let arg = |name: &str| param(decoded, name);
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

/// One decoded `RegistryDeployed` log as a storable row, or `None` when it does
/// not carry its addresses — a row keyed on a malformed address would be
/// unreachable, and an empty factory would match the "no factory configured"
/// read. Strings decode leniently: an empty name labels as empty rather than
/// dropping the registry from the listing.
pub fn registry_deployed(
    decoded: &crate::decoder::DecodedEvent,
    tx: &Transaction,
) -> Option<RegistryDeployed> {
    let arg = |name: &str| param(decoded, name).map(str::to_string);
    let address = |name: &str| arg(name).filter(|a| crate::decoder::is_valid_address(a));
    let factory = checksum_address(&decoded.contract);
    if !crate::decoder::is_valid_address(&factory) {
        return None;
    }
    Some(RegistryDeployed {
        factory,
        registry: address("registry")?,
        creator: address("creator")?,
        name: arg("name").unwrap_or_default(),
        description: arg("description").unwrap_or_default(),
        block_number: tx.block_number,
        created_at: db::now_ts(),
    })
}

/// Fetch + persist one block (used by tests and simple callers).
pub async fn index_block(rpc: &ChainRpc, db: &Db, block_num: u64) -> Result<()> {
    let Some(bundle) = fetch_block_bundle(rpc, block_num).await? else {
        return Ok(());
    };
    db::save_block_bundle(db, &bundle)?;
    Ok(())
}

/// Re-fetch metadata for tokens whose stored name/symbol carry control
/// characters — the signature of the pre-fix ABI string decoder. Run once at
/// startup so the deployed database self-heals without manual intervention.
async fn repair_token_metadata(rpc: &ChainRpc, db: &Db) -> Result<()> {
    let corrupt: Vec<String> = db::get_all_token_metas(db)
        .into_iter()
        .filter(|m| {
            crate::tokens::has_control_chars(&m.name) || crate::tokens::has_control_chars(&m.symbol)
        })
        .map(|m| m.address)
        .collect();
    if corrupt.is_empty() {
        return Ok(());
    }
    info!("repairing {} token metadata row(s)", corrupt.len());
    for addr in corrupt {
        let meta = fetch_token_metadata(rpc, &addr).await;
        if let Err(e) = db::save_token_metadata(db, &meta) {
            warn!("failed to repair token metadata for {addr}: {e:#}");
        }
    }
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

/// Shared state for the forward and backfill loops.
#[derive(Clone)]
struct Indexer {
    rpc: ChainRpc,
    db: Db,
    cfg: IndexerConfig,
    known_tokens: Arc<Mutex<HashSet<String>>>,
    bundle_tx: mpsc::Sender<BlockBundle>,
    shutdown: watch::Receiver<bool>,
    tip_lag: Arc<AtomicU64>,
}

impl Indexer {
    fn shutting_down(&self) -> bool {
        *self.shutdown.borrow()
    }

    fn set_tip_lag(&self, lag: u64) {
        self.tip_lag.store(lag, Ordering::Relaxed);
    }

    /// Fetch `from..=to` as multi-block RPC batches, returning bundles in
    /// number order so the caller can emit a gapless live feed.
    async fn fetch_range(&self, from: u64, to: u64) -> Vec<BlockBundle> {
        if from > to {
            return Vec::new();
        }
        let inflight = (self.cfg.concurrency.max(1) as u64).div_ceil(RPC_BLOCK_BATCH) as usize;
        let mut chunks = block_chunks(from, to, RPC_BLOCK_BATCH);
        let mut set = tokio::task::JoinSet::new();
        let mut out = Vec::with_capacity((to - from + 1).min(16384) as usize);
        loop {
            if self.shutting_down() {
                set.abort_all();
                break;
            }
            while set.len() < inflight {
                let Some(chunk) = chunks.next() else {
                    break;
                };
                let rpc = self.rpc.clone();
                let known = self.known_tokens.clone();
                set.spawn(async move { fetch_block_bundles(&rpc, known, chunk).await });
            }
            if set.is_empty() {
                break;
            }
            match set.join_next().await {
                Some(Ok(Ok(bundles))) => out.extend(bundles),
                Some(Ok(Err(e))) => warn!("index range fetch failed: {e:#}"),
                Some(Err(e)) => warn!("index range task failed: {e:#}"),
                None => break,
            }
        }
        out.sort_by_key(|b| b.block.number);
        out
    }

    async fn send_bundles(&self, bundles: impl IntoIterator<Item = BlockBundle>) -> bool {
        for b in bundles {
            if self.bundle_tx.send(b).await.is_err() {
                warn!("writer stopped");
                return false;
            }
        }
        true
    }
}

/// Collapse every queued head into the highest one seen. The watcher polls and
/// subscribes concurrently, so a slow poll can deliver a stale head after a
/// newer one — taking the last would walk the tip backwards.
fn drain_heads(rx: &mut mpsc::Receiver<u64>, mut head: u64) -> u64 {
    while let Ok(n) = rx.try_recv() {
        head = head.max(n);
    }
    head
}

/// Fresh DB: wait for backfill to seed the head so this loop never tries to
/// index the whole history, and so the two loops don't fetch the same blocks.
async fn wait_for_seed(db: &Db) -> u64 {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(b) = db::get_latest_block(db) {
            return b.number as u64;
        }
        if tokio::time::Instant::now() >= deadline {
            return 0;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn forward_loop(ix: Indexer, block_events: broadcast::Sender<Value>) {
    let (head_tx, mut head_rx) = mpsc::channel::<u64>(256);
    tokio::spawn(crate::ws::head_watcher(
        ix.rpc.clone(),
        ix.cfg.ws_url.clone(),
        ix.cfg.index_ws,
        ix.cfg.poll,
        head_tx,
        ix.shutdown.clone(),
    ));

    let mut sent = db::get_latest_block(&ix.db)
        .map(|b| b.number as u64)
        .unwrap_or(0);
    info!("forward loop started from block {sent}");

    while let Some(head) = head_rx.recv().await {
        if ix.shutting_down() {
            break;
        }
        // A fast head feed delivers every block; fold queued heads into one
        // range so we fetch a batch instead of one HTTP request per block.
        let mut head = drain_heads(&mut head_rx, head);
        db::set_chain_head(&ix.db, head as i64);

        if sent == 0 && head > 0 {
            sent = wait_for_seed(&ix.db).await;
            if sent == 0 || sent >= head {
                continue;
            }
        }
        if head > sent + 1 {
            info!("new blocks: {sent} -> {head} (+{})", head - sent);
        }
        // Window the catch-up so the writer starts before a large gap has
        // finished fetching, and so memory stays bounded.
        while sent < head {
            if ix.shutting_down() {
                return;
            }
            head = drain_heads(&mut head_rx, head);
            db::set_chain_head(&ix.db, head as i64);
            let end = sent.saturating_add(ix.cfg.batch).min(head);
            ix.set_tip_lag(head.saturating_sub(sent));
            let bundles = ix.fetch_range(sent + 1, end).await;
            // Live feed is tip-only and in number order: concurrent fetches
            // complete out of order, and backfill must not reach viewers.
            for b in &bundles {
                let _ = block_events.send(crate::models::block_event_json(
                    &b.block,
                    &b.txs,
                    crate::models::STREAM_TX_CAP,
                ));
            }
            if !ix.send_bundles(bundles).await {
                return;
            }
            sent = end;
        }
        ix.set_tip_lag(0);
    }
    warn!("forward loop stopped (head feed closed)");
}

async fn backfill_loop(mut ix: Indexer) {
    info!(
        "backfill loop started (batch {}, concurrency {})",
        ix.cfg.batch, ix.cfg.concurrency
    );

    // In-memory descending frontier: advancing this without waiting for the
    // writer lets the next fetch overlap the previous commit. Writes are
    // idempotent, so a crash just re-fetches the last in-flight batch.
    let mut frontier: Option<u64> = db::get_min_block_number(&ix.db).map(|n| n as u64);
    let mut consecutive_failures = 0u32;
    loop {
        if ix.shutting_down() {
            break;
        }
        if ix.tip_lag.load(Ordering::Relaxed) > TIP_YIELD_LAG {
            if sleep_or_shutdown(&mut ix.shutdown, Duration::from_millis(50)).await {
                break;
            }
            continue;
        }
        let target = match frontier {
            Some(min) if min > 1 => min,
            Some(_) => {
                if sleep_or_shutdown(&mut ix.shutdown, ix.cfg.poll).await {
                    break;
                }
                frontier = db::get_min_block_number(&ix.db).map(|n| n as u64);
                continue;
            }
            None => match ix.rpc.eth_block_number().await {
                Ok(head) => head + 1,
                Err(e) => {
                    warn!("backfill head fetch failed: {e:#}");
                    if sleep_or_shutdown(&mut ix.shutdown, ix.cfg.poll).await {
                        break;
                    }
                    continue;
                }
            },
        };
        let start = target.saturating_sub(ix.cfg.batch).max(1);
        let bundles = ix.fetch_range(start, target - 1).await;
        if bundles.is_empty() {
            consecutive_failures += 1;
            let backoff = Duration::from_millis(200 * consecutive_failures.min(10) as u64);
            if sleep_or_shutdown(&mut ix.shutdown, backoff).await {
                break;
            }
            continue;
        }
        // Blocks the range failed to fetch must stay above the frontier or
        // nothing ever revisits them: it only descends, and a hole below it is
        // out of reach. `fetch_range` sorts, so the first bundle is the lowest.
        let reached = bundles
            .first()
            .map(|b| b.block.number as u64)
            .unwrap_or(start);
        // Tip-first so the head lands in the DB as soon as possible (the
        // forward loop waits for it on a fresh database).
        if !ix.send_bundles(bundles.into_iter().rev()).await {
            return;
        }
        frontier = Some(reached);
        consecutive_failures = 0;
        info!("backfill: {} remaining", reached.saturating_sub(1));
    }
}

/// Run the indexer: one serialized DB writer plus forward and backfill loops.
/// Newly written blocks are broadcast on `block_events` for live viewers.
pub async fn run_forever(
    rpc: ChainRpc,
    db: Db,
    cfg: IndexerConfig,
    block_events: broadcast::Sender<Value>,
    stats: Arc<RwLock<Value>>,
    shutdown: watch::Receiver<bool>,
) {
    let stats_interval = cfg.stats_interval;
    let stats_db = db.clone();
    let stats_events = block_events.clone();
    let stats_shutdown = shutdown.clone();
    tokio::spawn(async move {
        stats_loop(
            stats_db,
            stats,
            stats_events,
            stats_interval,
            stats_shutdown,
        )
        .await
    });

    // Seed the token-metadata cache and repair balances on legacy databases.
    let known_tokens = Arc::new(Mutex::new(
        db::get_all_token_addresses(&db).into_iter().collect(),
    ));
    // Re-fetch token metadata that was stored by the pre-fix ABI string
    // decoder (its name/symbol are NUL-padded control-char garbage). Cheap
    // and best-effort: only rows with control characters are refetched.
    let repair_rpc = rpc.clone();
    let repair_db = db.clone();
    tokio::spawn(async move {
        if let Err(e) = repair_token_metadata(&repair_rpc, &repair_db).await {
            warn!("token metadata repair failed: {e:#}");
        }
    });
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
        // Seed the /anchoring summary for databases that predate it; after
        // this each block maintains it incrementally. Guarded, because the
        // recompute reads every anchor ever written while holding the lock.
        let conn = db::lock(&rebuild_db);
        if db::anchored_summary_is_stale(&conn) {
            if let Err(e) = db::sync_anchored_namespaces(&conn) {
                warn!("anchored namespace sync failed: {e:#}");
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
    // How many queued bundles may share one commit. Only bounds how much one
    // transaction holds: a writer that is keeping up sees an empty queue and
    // commits per block; batching engages only once it falls behind.
    const MAX_BATCH: usize = 64;

    let writer = tokio::spawn(async move {
        while let Some(bundle) = bundle_rx.recv().await {
            // On shutdown, stop draining the queue: in-flight bundles are
            // dropped and re-fetched on the next start.
            if *writer_shutdown.borrow() {
                break;
            }
            // Fold in whatever is already queued; `try_recv` never waits, so
            // this adds no latency.
            let mut batch = vec![bundle];
            while batch.len() < MAX_BATCH {
                let Ok(next) = bundle_rx.try_recv() else {
                    break;
                };
                batch.push(next);
            }
            if let Err(e) = db::save_block_bundles(&writer_db, &batch) {
                let (first, last) = (batch[0].block.number, batch[batch.len() - 1].block.number);
                warn!("db write failed for blocks {first}..={last}: {e:#}");
            }
        }
    });

    let ix = Indexer {
        rpc,
        db,
        cfg,
        known_tokens,
        bundle_tx: bundle_tx.clone(),
        shutdown,
        tip_lag: Arc::new(AtomicU64::new(0)),
    };
    let forward = tokio::spawn(forward_loop(ix.clone(), block_events));
    let backfill = tokio::spawn(backfill_loop(ix));
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
async fn stats_loop(
    db: Db,
    cell: Arc<RwLock<Value>>,
    events: broadcast::Sender<Value>,
    interval: Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            break;
        }
        match compute_and_store_stats(&db) {
            Ok(stats) => {
                *cell.write().unwrap_or_else(|e| e.into_inner()) = stats.clone();
                // Live viewers get the refresh too, tagged the way block
                // payloads are. Droppable: anyone who misses one is corrected
                // by the next tick.
                let _ = events.send(json!({ "type": "stats", "stats": stats }));
            }
            Err(e) => warn!("stats recompute failed: {e:#}"),
        }
        if sleep_or_shutdown(&mut shutdown, interval).await {
            break;
        }
    }
}

fn compute_and_store_stats(db: &Db) -> Result<Value> {
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

    // Index progress, so the home page and the stream never recount blocks.
    // Read through `conn`: a helper taking `&Db` would deadlock on the
    // connection lock this function already holds.
    let chain_head: i64 = conn
        .query_row("SELECT value FROM kv WHERE key='chain_head'", [], |r| {
            r.get::<_, String>(0)
        })
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let index_pct = if chain_head > 0 {
        (total_blocks as f64 / chain_head as f64 * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    // Rides along so the nav's anchoring gate is a memory read on every page
    // view rather than a query. One tick of lag on the tab appearing.
    let has_anchors: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM anchored_namespaces LIMIT 1)",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        != 0;

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
        "chain_head": chain_head,
        "index_pct": index_pct,
        "has_anchors": has_anchors,
        "updated_at": now,
    });
    // Still written to kv: it seeds the in-memory copy across a restart.
    db::set_kv(&conn, "stats", &stats.to_string())?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::TRANSFER_TOPIC;

    /// An `Anchored` log as a node reports it, from `emitter`.
    fn anchored_log(emitter: &str, caller: &str) -> Value {
        let commitment = "22".repeat(32);
        json!({
            "address": emitter,
            "topics": [
                crate::decoder::ANCHORED_TOPIC.as_str(),
                format!("0x{}{}", "00".repeat(12), caller),
                format!("0x{}", "11".repeat(32)),
            ],
            // abi.encode(bytes32 commitment, bytes metadata), metadata empty.
            "data": format!("0x{commitment}{:064x}{:064x}", 0x40, 0),
            "logIndex": "0x0",
        })
    }

    fn tx_with_logs(logs: Vec<Value>) -> (Transaction, Vec<AnchoredEvent>) {
        let mut tx = Transaction {
            hash: format!("0x{}", "ab".repeat(32)),
            block_number: 7,
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
            timestamp: 1_700,
            created_at: 0,
        };
        let receipt = json!({"status": "0x1", "logs": logs});
        let (mut anchored, mut transfers, mut registries) = (Vec::new(), Vec::new(), Vec::new());
        let mut next_log_index = 0u64;
        apply_receipt(
            &mut tx,
            &receipt,
            &mut transfers,
            &mut anchored,
            &mut registries,
            &mut next_log_index,
        );
        (tx, anchored)
    }

    #[test]
    fn only_the_precompiles_own_anchored_logs_are_indexed() {
        // The whole trust argument for a namespace: the precompile reports the
        // caller it actually saw. Any contract can emit the same signature.
        let caller = "44".repeat(20);
        let (_, anchored) = tx_with_logs(vec![anchored_log(ANCHORING_ADDRESS, &caller)]);
        assert_eq!(anchored.len(), 1, "the precompile's own log is an anchor");
        assert_eq!(anchored[0].namespace, checksum_address(&caller));

        let impostor = format!("0x{}", "cc".repeat(20));
        let (_, anchored) = tx_with_logs(vec![anchored_log(&impostor, &caller)]);
        assert!(
            anchored.is_empty(),
            "a claimed namespace is not a namespace"
        );
    }

    #[test]
    fn the_precompile_is_recognised_however_its_address_is_spelled() {
        // The gate compares addresses, not spellings: a node reporting the
        // log address in lowercase still gets its anchors indexed.
        let caller = "44".repeat(20);
        let (_, anchored) = tx_with_logs(vec![anchored_log(
            &ANCHORING_ADDRESS.to_lowercase(),
            &caller,
        )]);
        assert_eq!(anchored.len(), 1);
    }

    fn chunks(from: u64, to: u64, batch: u64) -> Vec<RangeInclusive<u64>> {
        block_chunks(from, to, batch).collect()
    }

    #[test]
    fn block_chunks_splits_range() {
        assert_eq!(chunks(1, 40, 16), vec![1..=16, 17..=32, 33..=40]);
        assert_eq!(chunks(7, 7, 16), vec![7..=7]);
        assert_eq!(chunks(1, 3, 1), vec![1..=1, 2..=2, 3..=3]);
        assert!(chunks(5, 4, 16).is_empty());
        // A zero batch would otherwise chunk forever.
        assert_eq!(chunks(1, 2, 0), vec![1..=1, 2..=2]);
        // The final chunk stops at `to` rather than overflowing past it.
        assert_eq!(
            chunks(u64::MAX - 1, u64::MAX, 16),
            vec![u64::MAX - 1..=u64::MAX]
        );
    }

    #[test]
    fn drain_heads_keeps_the_highest() {
        let (tx, mut rx) = mpsc::channel::<u64>(8);
        // The watcher polls and subscribes concurrently, so a slow poll can
        // deliver a stale head after a newer subscription one.
        for n in [104u64, 107, 105] {
            tx.try_send(n).expect("queue head");
        }
        assert_eq!(drain_heads(&mut rx, 103), 107);
        // Nothing queued leaves the caller's head untouched.
        assert_eq!(drain_heads(&mut rx, 107), 107);
    }

    #[test]
    fn assemble_empty_block_without_receipts() {
        let raw = json!({
            "number": "0x10",
            "hash": format!("0x{}", "ab".repeat(32)),
            "parentHash": format!("0x{}", "cd".repeat(32)),
            "timestamp": "0x64",
            "transactions": [],
        });
        let bundle = assemble_bundle(&raw, None);
        assert_eq!(bundle.block.number, 16);
        assert_eq!(bundle.block.tx_count, 0);
        assert!(bundle.txs.is_empty());
        assert!(bundle.transfers.is_empty());
    }

    #[test]
    fn assemble_decodes_transfer_receipt() {
        let from = format!("0x{}", "1a".repeat(20));
        let to = format!("0x{}", "2b".repeat(20));
        let token = format!("0x{}", "aa".repeat(20));
        let tx_hash = format!("0x{}", "ff".repeat(32));
        let raw = json!({
            "number": "0x10",
            "hash": format!("0x{}", "ab".repeat(32)),
            "parentHash": format!("0x{}", "cd".repeat(32)),
            "timestamp": "0x64",
            "transactions": [{
                "hash": tx_hash,
                "blockNumber": "0x10",
                "transactionIndex": "0x0",
                "from": from,
                "to": token,
                "input": "0x",
            }],
        });
        let receipts = json!([{
            "transactionHash": tx_hash,
            "status": "0x1",
            "gasUsed": "0x5208",
            "logs": [{
                "address": token,
                "logIndex": "0x0",
                "topics": [
                    TRANSFER_TOPIC.as_str(),
                    format!("0x{}{}", "00".repeat(12), from.trim_start_matches("0x")),
                    format!("0x{}{}", "00".repeat(12), to.trim_start_matches("0x")),
                ],
                "data": format!("0x{:064x}", 100u64),
            }],
        }]);
        let bundle = assemble_bundle(&raw, Some(&receipts));
        assert_eq!(bundle.txs.len(), 1);
        assert_eq!(bundle.txs[0].status, 1);
        assert_eq!(bundle.transfers.len(), 1);
        assert_eq!(bundle.transfers[0].from_addr, checksum_address(&from));
        assert_eq!(bundle.transfers[0].to_addr, checksum_address(&to));
        assert_eq!(bundle.transfers[0].amount, "100");
        assert_eq!(bundle.transfers[0].token_addr, checksum_address(&token));
    }
}
