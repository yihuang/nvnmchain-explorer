//! Background indexer: polls the chain head, indexes blocks forward and
//! backfills history, and fills the `transfer_events` table.

use std::time::Duration;

use anyhow::Result;
use serde_json::Value;
use tracing::{info, warn};

use crate::db::{self, Db};
use crate::decoder::{checksum_address, decode_event, flatten_trace};
use crate::models::TransferEvent;
use crate::parse::{parse_block, parse_transaction};
use crate::rpc::TempoRpc;
use crate::tokens::fetch_token_metadata;

pub async fn index_block(rpc: &TempoRpc, db: &Db, block_num: u64) -> Result<()> {
    // 1. Block with full transaction objects.
    let Some(raw_block) = rpc.eth_get_block_by_number(block_num, true).await? else {
        warn!("block {block_num} not found");
        return Ok(());
    };
    let block = parse_block(&raw_block);
    db::save_block(db, &block)?;

    // 2. One batched trace call for the whole block.
    let traces = rpc.debug_trace_block(block_num).await;
    let mut trace_map: std::collections::HashMap<String, Vec<Value>> = Default::default();
    if let Some(traces) = traces {
        if let Some(entries) = traces.as_array() {
            for entry in entries {
                let tx_hash = entry
                    .get("txHash")
                    .or_else(|| entry.get("transactionHash"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if tx_hash.is_empty() {
                    continue;
                }
                let result = entry.get("result").unwrap_or(entry);
                trace_map.insert(tx_hash, flatten_trace(result));
            }
        }
    }

    // 3. Receipts (one per tx) and everything else.
    let txs = raw_block
        .get("transactions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for tx_data in &txs {
        let tx_hash = tx_data.get("hash").and_then(Value::as_str).unwrap_or("");
        if tx_hash.is_empty() {
            continue;
        }
        let mut tx = parse_transaction(tx_data, &block);
        tx.timestamp = block.timestamp;

        if let Some(flat) = trace_map.get(tx_hash) {
            tx.trace_data = Some(serde_json::to_string(flat).unwrap_or_else(|_| "[]".into()));
        }

        if let Ok(Some(receipt)) = rpc.eth_get_transaction_receipt(tx_hash).await {
            tx.receipt_data = Some(serde_json::to_string(&receipt).unwrap_or_else(|_| "{}".into()));
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
                let meta = fetch_token_metadata(rpc, fee_token).await;
                if let Err(e) = db::save_token_metadata(db, &meta) {
                    warn!("token metadata save failed for {fee_token}: {e:#}");
                }
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

            save_transfer_events(db, &receipt, &tx);
        }

        if let Err(e) = db::save_transaction(db, &tx) {
            warn!("failed to save tx {tx_hash}: {e:#}");
        }
    }

    Ok(())
}

/// Decode Transfer / TransferWithMemo logs and store them so the address and
/// token transfer tabs have data.
fn save_transfer_events(db: &Db, receipt: &Value, tx: &crate::models::Transaction) {
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
        let transfer = TransferEvent {
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
        };
        if let Err(e) = db::save_transfer(db, &transfer) {
            warn!("failed to save transfer event: {e:#}");
        }
    }
}

pub async fn run_forever(rpc: TempoRpc, db: Db, poll_seconds: f64, batch: u64) {
    let mut highest: u64 = db::get_latest_block(&db)
        .map(|b| b.number as u64)
        .unwrap_or(0);
    let mut backfill_target: u64 = 0;
    let mut initialised = highest != 0;
    if initialised {
        // Resume an interrupted backfill from the lowest stored block rather
        // than assuming the whole history below the tip is already indexed.
        if let Some(min) = db::get_min_block_number(&db) {
            if min > 1 {
                backfill_target = min as u64;
                info!("resuming backfill from block {min}");
            }
        }
    }
    let mut total_indexed = 0u64;

    info!(
        "indexer started, polling every {poll_seconds}s (batch {batch}); resuming from {highest}"
    );

    loop {
        let head = match rpc.eth_block_number().await {
            Ok(h) => h,
            Err(e) => {
                warn!("indexer poll cycle failed; retrying: {e:#}");
                tokio::time::sleep(Duration::from_secs_f64(poll_seconds)).await;
                continue;
            }
        };

        if !initialised {
            highest = head;
            backfill_target = head + 1;
            initialised = true;
            info!("initialised: tip={head}, backfilling down to block 1");
        }

        // Phase 1: index new blocks at the tip.
        if head > highest {
            let end = (highest + batch).min(head);
            info!("new blocks: {highest} -> {end} (+{})", end - highest);
            for num in highest + 1..=end {
                if let Err(e) = index_block(&rpc, &db, num).await {
                    warn!("index_block({num}) failed: {e:#}");
                }
            }
            total_indexed += end - highest;
            highest = end;
        }

        // Phase 2: backfill older blocks, descending.
        if backfill_target > 1 {
            let start = (backfill_target.saturating_sub(batch)).max(1);
            info!(
                "backfill: blocks {start}-{} ({} remaining)",
                backfill_target - 1,
                start - 1
            );
            for num in (start..=backfill_target - 1).rev() {
                if let Err(e) = index_block(&rpc, &db, num).await {
                    warn!("index_block({num}) failed: {e:#}");
                }
            }
            total_indexed += backfill_target - start;
            backfill_target = start;
        }

        info!(
            "progress: {total_indexed} blocks indexed, tip={head}, backfill_remaining={}",
            backfill_target.saturating_sub(1)
        );

        tokio::time::sleep(Duration::from_secs_f64(poll_seconds)).await;
    }
}
