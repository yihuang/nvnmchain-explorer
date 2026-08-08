//! Axum web application: routes, JSON API, and Tera-rendered HTML.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Json;
use axum::Router;
use chrono::{Local, TimeZone};
use futures_util::stream::unfold;
use num_bigint::BigInt;
use serde_json::{json, Value};
use tera::Tera;
use tokio::sync::broadcast;

use crate::config::Settings;
use crate::contracts::{identify_address, is_contract};
use crate::db::{self, Db};
use crate::decoder::{
    checksum_address, decode_event, decode_function_call, extract_balance_changes, extract_calls,
};
use crate::rpc::ChainRpc;
use crate::tokens::{fetch_token_metadata, format_token_amount, format_token_amount_with_symbol};

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub rpc: ChainRpc,
    pub cfg: Settings,
    pub tera: Arc<Tera>,
    /// Live stream of indexed blocks, fed by the indexer writer task.
    pub block_events: broadcast::Sender<Value>,
}

fn wants_json(headers: &HeaderMap, query: &HashMap<String, String>) -> bool {
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    accept.contains("application/json") || query.get("format").map(|f| f == "json").unwrap_or(false)
}

fn render_html(tera: &Tera, template: &str, ctx: &Value) -> Response {
    match tera::Context::from_serialize(ctx) {
        Ok(tera_ctx) => match tera.render(template, &tera_ctx) {
            Ok(html) => Html(html).into_response(),
            Err(e) => {
                tracing::error!("template {template} render failed: {e:?}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Html("Internal server error"),
                )
                    .into_response()
            }
        },
        Err(e) => {
            tracing::error!("template context serialization failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html("Internal server error"),
            )
                .into_response()
        }
    }
}

fn json_response(ctx: &Value) -> Response {
    Json(ctx.clone()).into_response()
}

fn html_or_json(
    state: &AppState,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    template: &str,
    ctx: &Value,
) -> Response {
    if wants_json(headers, query) {
        json_response(ctx)
    } else {
        render_html(&state.tera, template, ctx)
    }
}

/// Common context keys every page needs (latest block + native symbol).
fn page_ctx(state: &AppState, extra: Value) -> Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "latest_block".into(),
        serde_json::to_value(db::get_latest_block(&state.db)).unwrap_or(Value::Null),
    );
    map.insert("native_symbol".into(), json!(state.cfg.native_symbol));
    if let Value::Object(o) = extra {
        for (k, v) in o {
            map.insert(k, v);
        }
    }
    Value::Object(map)
}

/// Compact method badge for a transaction: decoded name > signature > selector.
fn tx_method_badge(input: &str) -> Option<String> {
    if input.is_empty() || input == "0x" {
        return None;
    }
    decode_function_call(input).map(|d| d.name.or(d.signature).unwrap_or(d.selector))
}

/// Extra descriptors for the tx detail page, derived from the stored receipt
/// and the raw RPC object: (fee payer, signature type, failure reason).
pub fn tx_extras(
    tx: &crate::models::Transaction,
    receipt: Option<&Value>,
) -> (Option<String>, Option<String>, Option<String>) {
    let fee_payer = receipt
        .and_then(|r| r.get("feePayer").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .map(String::from);
    let signature_type = tx
        .raw
        .as_deref()
        .and_then(|r| serde_json::from_str::<Value>(r).ok())
        .and_then(|v| {
            v.get("signature")
                .and_then(|s| s.get("type"))
                .and_then(Value::as_str)
                .map(String::from)
        });
    let fail_reason = if tx.status == 0 {
        receipt
            .and_then(|r| {
                r.get("error")
                    .and_then(Value::as_str)
                    .or_else(|| r.get("revertReason").and_then(Value::as_str))
                    .or_else(|| r.get("outcome").and_then(Value::as_str))
            })
            .map(String::from)
    } else {
        None
    };
    (fee_payer, signature_type, fail_reason)
}

/// Replay every replayable top-level call of a transaction via `eth_call` at
/// its block, as one batched request. Returns the node's error message per
/// call (`None` = succeeded). This is the only per-call outcome source on
/// chains whose receipts record no reason and whose tracing is unsupported.
/// Entries are in `raw.calls` order, skipping calls without a destination.
pub async fn replay_tx_calls(
    rpc: &ChainRpc,
    tx: &crate::models::Transaction,
) -> Vec<Option<String>> {
    let raw: Value = match serde_json::from_str(tx.raw.as_deref().unwrap_or("{}")) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(calls) = raw.get("calls").and_then(Value::as_array) else {
        return Vec::new();
    };
    let from = tx.from_addr.clone();
    let gas = format!("0x{:x}", tx.gas_limit.max(0));
    let block = format!("0x{:x}", tx.block_number.max(0));
    let batch: Vec<(String, Value)> = calls
        .iter()
        .filter_map(|call| {
            let to = call.get("to").and_then(Value::as_str)?;
            if to.is_empty() {
                return None;
            }
            let data = call
                .get("data")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| call.get("input").and_then(Value::as_str))
                .unwrap_or("0x")
                .to_string();
            Some((
                "eth_call".to_string(),
                json!([{"from": from, "to": to, "data": data, "gas": gas}, block]),
            ))
        })
        .collect();
    let Ok(results) = rpc.batch_call(batch).await else {
        return Vec::new();
    };
    results
        .into_iter()
        .map(|r| match r {
            Ok(_) => None,
            Err(e) => Some(e.message.clone()),
        })
        .collect()
}

/// Burnt fees for a block: base fee × gas used, as a wei decimal string.
fn burnt_fees_wei(base_fee: &str, gas_used: i64) -> String {
    let base = BigInt::parse_bytes(base_fee.as_bytes(), 10).unwrap_or_else(|| BigInt::from(0));
    (base * BigInt::from(gas_used.max(0))).to_string()
}

/// SVG polyline points for a 160×32 sparkline.
fn sparkline(values: &[f64]) -> String {
    if values.is_empty() {
        return String::new();
    }
    let max = values.iter().cloned().fold(0.0_f64, f64::max).max(1.0);
    let n = values.len();
    let pts: Vec<String> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let x = if n == 1 {
                80.0
            } else {
                i as f64 / (n - 1) as f64 * 160.0
            };
            let y = 30.0 - (v / max * 26.0);
            format!("{x:.1},{y:.1}")
        })
        .collect();
    pts.join(" ")
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub async fn home(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let latest_block = db::get_latest_block(&state.db);
    let recent_blocks = db::get_recent_blocks(&state.db, state.cfg.recent_block_count);
    let recent_txs = db::get_recent_transactions(&state.db, state.cfg.recent_tx_count);
    let latest_num = latest_block.as_ref().map(|b| b.number).unwrap_or(0);
    let mut stats = db::get_kv(&state.db, "stats")
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    if let Value::Object(ref mut o) = stats {
        let avg_ms = o
            .get("avg_block_time_ms")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let tps = o.get("tps").and_then(Value::as_f64).unwrap_or(0.0);
        let gas = o.get("gas_util_pct").and_then(Value::as_f64).unwrap_or(0.0);
        o.insert(
            "avg_block_time_display".into(),
            json!(format_block_time(avg_ms)),
        );
        o.insert("tps_display".into(), json!(format!("{tps:.2}")));
        o.insert("gas_util_display".into(), json!(format!("{gas:.1}%")));
    }
    let spark_tx = sparkline(
        &recent_blocks
            .iter()
            .map(|b| b.tx_count as f64)
            .collect::<Vec<_>>(),
    );
    let spark_gas = sparkline(
        &recent_blocks
            .iter()
            .map(|b| {
                if b.gas_limit > 0 {
                    b.gas_used as f64 / b.gas_limit as f64 * 100.0
                } else {
                    0.0
                }
            })
            .collect::<Vec<_>>(),
    );
    let ctx = page_ctx(
        &state,
        json!({
            "stats": stats,
            "recent_blocks": recent_blocks,
            "recent_txs": recent_txs,
            "latest_num": latest_num,
            "spark_tx": spark_tx,
            "spark_gas": spark_gas,
            "query": "",
        }),
    );
    html_or_json(&state, &headers, &query, "home.html", &ctx)
}

/// Server-Sent Events endpoint: pushes each newly indexed block to browsers
/// so the home page updates live without polling. Sends the current tip
/// immediately on connect, then every block as the indexer writes it.
pub async fn events(State(state): State<AppState>) -> Response {
    let rx = state.block_events.subscribe();
    let latest = db::get_latest_block(&state.db).map(|b| {
        let txs = db::get_block_transactions(&state.db, b.number);
        crate::models::block_event_json(&b, &txs, crate::models::STREAM_TX_CAP)
    });
    let stream = unfold((rx, latest, false), sse_step);
    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keepalive"),
        )
        .into_response()
}

type SseState = (broadcast::Receiver<Value>, Option<Value>, bool);

async fn sse_step(
    (mut rx, latest, sent_initial): SseState,
) -> Option<(Result<Event, std::convert::Infallible>, SseState)> {
    if !sent_initial {
        let payload = latest
            .clone()
            .map(|b| b.to_string())
            .unwrap_or_else(|| "null".into());
        return Some((
            Ok(Event::default().event("block").data(payload)),
            (rx, latest, true),
        ));
    }
    loop {
        match rx.recv().await {
            Ok(block) => {
                return Some((
                    Ok(Event::default().event("block").data(block.to_string())),
                    (rx, latest, true),
                ));
            }
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return None,
        }
    }
}
pub async fn block_page(
    State(state): State<AppState>,
    Path(block_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let block = if block_id.chars().all(|c| c.is_ascii_digit()) && !block_id.is_empty() {
        block_id
            .parse::<i64>()
            .ok()
            .and_then(|n| db::get_block_by_number(&state.db, n))
    } else {
        db::get_block_by_hash(&state.db, &block_id).or_else(|| {
            block_id
                .strip_prefix("0x")
                .and_then(|h| u64::from_str_radix(h, 16).ok())
                .and_then(|n| db::get_block_by_number(&state.db, n as i64))
        })
    };
    let Some(block) = block else {
        return not_found(&state, &headers, &query, "Block", &block_id);
    };
    let transactions = db::get_block_transactions(&state.db, block.number);
    let gas_pct = block_pct(block.gas_used, block.gas_limit);
    let token_addrs: Vec<String> = transactions
        .iter()
        .filter_map(|t| t.fee_token.clone())
        .collect();
    let metas = db::get_tokens_metadata(&state.db, &token_addrs);
    let transactions: Vec<Value> = transactions
        .iter()
        .map(|t| {
            let mut v = serde_json::to_value(t).unwrap_or(Value::Null);
            if let Some(m) = tx_method_badge(&t.input) {
                v["method"] = json!(m);
            }
            if let Some(meta) = t.fee_token.as_deref().and_then(|f| metas.get(f)) {
                v["fee_token_meta"] = serde_json::to_value(meta).unwrap_or(Value::Null);
                v["fee_formatted"] = json!(format_token_amount_with_symbol(
                    &t.fee_amount,
                    meta.decimals,
                    &meta.symbol,
                ));
            }
            v
        })
        .collect();
    let burnt = burnt_fees_wei(&block.base_fee, block.gas_used);
    let ctx = page_ctx(
        &state,
        json!({
            "block": block,
            "transactions": transactions,
            "gas_pct": gas_pct,
            "base_fee_gwei": format_token_amount(&block.base_fee, 9),
            "burnt_fees": burnt,
            "query": "",
        }),
    );
    html_or_json(&state, &headers, &query, "block.html", &ctx)
}

pub async fn blocks_page(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let per_page: i64 = 25;
    let page: i64 = query
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1)
        .max(1);
    let from: Option<i64> = query.get("from").and_then(|f| f.parse().ok());
    let latest = db::get_latest_block(&state.db);
    let latest_num = latest.as_ref().map(|b| b.number).unwrap_or(0);
    let end = from.unwrap_or_else(|| (latest_num - (page - 1) * per_page).max(0));

    let mut blocks: Vec<Value> = Vec::new();
    let mut i = end;
    while i > end - per_page && i >= 0 {
        if let Some(b) = db::get_block_by_number(&state.db, i) {
            let pct = block_pct(b.gas_used, b.gas_limit);
            let mut v = serde_json::to_value(b).unwrap_or(Value::Null);
            v["gas_pct"] = json!(pct);
            blocks.push(v);
        }
        i -= 1;
    }

    let ctx = json!({
        "blocks": blocks,
        "latest_num": latest_num,
        "total_blocks": latest_num + 1,
        "per_page": per_page,
        "page": page,
        "first_num": blocks.first().and_then(|b| b.get("number")).and_then(Value::as_i64),
        "last_num": blocks.last().and_then(|b| b.get("number")).and_then(Value::as_i64),
        "latest_block": latest,
        "query": "",
    });
    html_or_json(&state, &headers, &query, "blocks.html", &ctx)
}

pub async fn tx_page(
    State(state): State<AppState>,
    Path(tx_hash): Path<String>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(mut tx) = db::get_transaction(&state.db, &tx_hash) else {
        return not_found(&state, &headers, &query, "Transaction", &tx_hash);
    };
    let receipt: Option<Value> = tx
        .receipt_data
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());
    let trace: Option<Vec<Value>> = tx
        .trace_data
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());
    let block = db::get_block_by_number(&state.db, tx.block_number);

    // Rows indexed before `raw` was stored: recover the original RPC object
    // from the block blob, which embeds the full transactions array.
    if tx.raw.is_none() {
        if let Some(b) = &block {
            if let Ok(v) = serde_json::from_str::<Value>(&b.raw) {
                if let Some(raw_tx) =
                    v.get("transactions")
                        .and_then(Value::as_array)
                        .and_then(|a| {
                            a.iter().find(|t| {
                                t.get("hash").and_then(Value::as_str) == Some(tx.hash.as_str())
                            })
                        })
                {
                    tx.raw = Some(raw_tx.to_string());
                }
            }
        }
    }
    // Tempo-style txs carry their destination in `calls[0].to`; fall back to
    // the receipt's `to` (nodes fill it with the first call's destination).
    if tx.to_addr.is_none() {
        if let Some(raw) = tx.raw.as_deref() {
            if let Ok(v) = serde_json::from_str::<Value>(raw) {
                tx.to_addr = crate::parse::tx_to_addr(&v);
            }
        }
        if tx.to_addr.is_none() {
            if let Some(to) = receipt
                .as_ref()
                .and_then(|r| r.get("to"))
                .and_then(Value::as_str)
            {
                tx.to_addr = Some(to.to_string());
            }
        }
    }

    let mut calls = extract_calls(&tx, trace.as_deref().unwrap_or(&[]));
    if calls.is_empty() {
        calls.push(json!({
            "depth": 0,
            "type": "CALL",
            "to": tx.to_addr.clone(),
            "from": tx.from_addr.clone(),
            "data": tx.input.clone(),
            "decoded": decode_function_call(&tx.input).map(|d| d.to_json()).unwrap_or(Value::Null),
            "gas": "0",
            "gas_used": "0",
            "children": [],
        }));
    }

    let mut events = Vec::new();
    if let Some(receipt) = &receipt {
        if let Some(logs) = receipt.get("logs").and_then(Value::as_array) {
            for log in logs {
                if let Some(decoded) = decode_event(log) {
                    events.push(serde_json::to_value(decoded).unwrap_or(Value::Null));
                }
            }
        }
    }
    let mut balance_changes = receipt
        .as_ref()
        .map(|r| extract_balance_changes(r, &tx))
        .unwrap_or_default();
    // Attach symbol + formatted amount to token balance changes.
    {
        let mut token_addrs: Vec<String> = balance_changes
            .iter()
            .filter_map(|c| c.get("token").and_then(Value::as_str).map(String::from))
            .filter(|a| !a.is_empty())
            .collect();
        token_addrs.dedup();
        let metas = db::get_tokens_metadata(&state.db, &token_addrs);
        for c in balance_changes.iter_mut() {
            let Some(token) = c.get("token").and_then(Value::as_str) else {
                continue;
            };
            if let Some(m) = metas.get(token) {
                let raw = c
                    .get("change")
                    .and_then(Value::as_str)
                    .unwrap_or("0")
                    .to_string();
                let (sign, amt) = raw
                    .strip_prefix('+')
                    .map(|a| ("+", a))
                    .or_else(|| raw.strip_prefix('-').map(|a| ("-", a)))
                    .unwrap_or(("", raw.as_str()));
                c["symbol"] = json!(m.symbol);
                c["formatted"] = json!(format!("{sign}{}", format_token_amount(amt, m.decimals)));
            }
        }
    }
    for change in balance_changes.iter_mut() {
        let positive = change
            .get("change")
            .and_then(Value::as_str)
            .map(|c| c.starts_with('+'))
            .unwrap_or(false);
        change["positive"] = json!(positive);
        // Ensure every row has display keys (Tera errors on missing map keys).
        change
            .as_object_mut()
            .expect("balance change is an object")
            .entry("symbol")
            .or_insert_with(|| json!(""));
        let default_formatted = change.get("change").cloned().unwrap_or_else(|| json!(""));
        change
            .as_object_mut()
            .expect("balance change is an object")
            .entry("formatted")
            .or_insert(default_formatted);
    }

    // Indent each call by depth for the tree view.
    for call in calls.iter_mut() {
        let depth = call.get("depth").and_then(Value::as_i64).unwrap_or(0);
        call["indent"] = json!(depth * 20);
    }

    let gas_price = parse_hex_i64(&tx.gas_price);
    let gas_used = tx.gas_used;
    let gas_limit = tx.gas_limit;
    let max_fee = parse_hex_i64(&tx.max_fee_per_gas);
    let max_priority = parse_hex_i64(&tx.max_priority_fee_per_gas);
    let base_fee = parse_hex_i64(&tx.base_fee);
    let tx_type = tx.tx_type;
    let fee_token = tx.fee_token.clone();
    let fee_amount = tx.fee_amount.clone();
    let fee_token_meta = fee_token
        .as_deref()
        .and_then(|f| db::get_token_metadata(&state.db, f));

    let gas_pct = if gas_limit > 0 {
        format!("{:.2}", gas_used as f64 / gas_limit as f64 * 100.0)
    } else {
        String::new()
    };
    let tx_type_hex = format!("{tx_type:02x}");
    let mut method = tx_method_badge(&tx.input);
    if method.is_none() {
        // Tempo-style txs have no top-level input; badge from the first call.
        method = calls
            .first()
            .and_then(|c| c.get("data").and_then(Value::as_str))
            .and_then(tx_method_badge);
    }

    let (fee_payer, signature_type, mut fail_reason) = tx_extras(&tx, receipt.as_ref());

    // Per-call status. Trace-based chains carry the error on the failed trace
    // node; tempo-style chains record nothing, so replay the calls with one
    // batched eth_call to find which one reverted. Best-effort: any RPC
    // hiccup degrades to the generic Failed badge.
    if tx.status == 0 {
        if fail_reason.is_none() {
            if let Some(err) = calls
                .iter()
                .find_map(|c| c.get("error").and_then(Value::as_str))
            {
                fail_reason = Some(err.to_string());
            }
        }
        if fail_reason.is_none() {
            let outcomes = replay_tx_calls(&state.rpc, &tx).await;
            fail_reason = outcomes.iter().find_map(|o| o.clone());
            if !outcomes.is_empty() {
                let mut k = 0;
                let mut seen_failure = false;
                for call in calls.iter_mut() {
                    if call.get("depth").and_then(Value::as_i64) != Some(0) {
                        continue;
                    }
                    if call
                        .get("to")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .is_empty()
                    {
                        continue;
                    }
                    if k >= outcomes.len() {
                        break;
                    }
                    // Calls after the reverting one never executed.
                    if seen_failure {
                        break;
                    }
                    match &outcomes[k] {
                        Some(err) => {
                            call["status"] = json!("failed");
                            call["error"] = json!(err);
                            seen_failure = true;
                        }
                        None => {
                            call["status"] = json!("success");
                        }
                    }
                    k += 1;
                }
            }
        }
    } else {
        // Successful tx on an atomic chain: a lone top-level call executed.
        let top_level: Vec<&Value> = calls
            .iter()
            .filter(|c| c.get("depth").and_then(Value::as_i64) == Some(0))
            .collect();
        if top_level.len() == 1 {
            calls[0]["status"] = json!("success");
        }
    }
    // Synthetic fallback call (no data at all) mirrors the tx result.
    if let Some(first) = calls.first_mut() {
        if first.get("status").is_none() {
            first["status"] = json!(if tx.status == 1 { "success" } else { "failed" });
            if tx.status == 0 {
                if let Some(r) = &fail_reason {
                    first["error"] = json!(r);
                }
            }
        }
    }
    let failed_calls = calls
        .iter()
        .filter(|c| c.get("status").and_then(Value::as_str) == Some("failed"))
        .count() as i64;

    let ctx = page_ctx(
        &state,
        json!({
            "tx": tx,
            "block": block,
            "receipt": receipt,
            "trace": trace,
            "calls": calls,
            "events": events,
            "balance_changes": balance_changes,
            "fee_payer": fee_payer,
            "signature_type": signature_type,
            "fail_reason": fail_reason,
            "failed_calls": failed_calls,
            "gas_price": gas_price,
            "gas_used": gas_used,
            "gas_limit": gas_limit,
            "max_fee": max_fee,
            "max_priority": max_priority,
            "base_fee": base_fee,
            "tx_type": tx_type,
            "tx_type_hex": tx_type_hex,
            "gas_pct": gas_pct,
            "fee_token": fee_token,
            "fee_amount": fee_amount,
            "fee_token_meta": fee_token_meta,
            "method": method,
            "active_tab": query.get("tab").cloned().unwrap_or_else(|| "overview".into()),
            "query": "",
        }),
    );
    html_or_json(&state, &headers, &query, "tx.html", &ctx)
}

pub async fn receipt_page(State(_state): State<AppState>, Path(tx_hash): Path<String>) -> Response {
    Redirect::to(&format!("/tx/{tx_hash}")).into_response()
}

pub async fn address_page(
    State(state): State<AppState>,
    Path(address): Path<String>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let checksummed = checksum_address(&address);
    if !is_valid_address(&checksummed) {
        return if wants_json(&headers, &query) {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("Invalid address: {address}")})),
            )
                .into_response()
        } else {
            not_found_html(&state, "Address", &address, "Invalid address")
        };
    }

    let tab = query
        .get("tab")
        .cloned()
        .unwrap_or_else(|| "transactions".into());
    let page: u32 = query
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1)
        .max(1);
    let per_page: u32 = 25;

    let (transactions, html_transactions, tx_count, total_pages, transfer_count) =
        if tab == "transfers" {
            let mut transfers = db::get_address_transfers(&state.db, &checksummed, page, per_page);
            enrich_transfers(&state, &mut transfers);
            let total = transfers.len() as i64;
            (transfers.clone(), transfers, 0, 0, total)
        } else {
            let txs = db::get_address_transactions(&state.db, &checksummed, page, per_page);
            let count = db::get_address_transaction_count(&state.db, &checksummed);
            let total_pages = ((count as f64) / (per_page as f64)).ceil().max(1.0) as u32;
            let html_txs: Vec<Value> = txs
                .iter()
                .map(|t| {
                    json!({
                        "tx_hash": t.hash,
                        "tx_from": t.from_addr,
                        "tx_to": t.to_addr,
                        "tx_timestamp": t.timestamp,
                        "tx_status": t.status,
                        "tx_block": t.block_number,
                        "tx_value": t.value,
                        "tx_method": tx_method_badge(&t.input),
                    })
                })
                .collect();
            (
                txs.into_iter()
                    .map(|t| serde_json::to_value(t).unwrap_or(Value::Null))
                    .collect::<Vec<Value>>(),
                html_txs,
                count,
                total_pages,
                0,
            )
        };

    let addr_info = identify_address(&checksummed);
    let is_token_addr = db::get_token_metadata(&state.db, &checksummed).is_some()
        || crate::contracts::is_tip20_token(&checksummed);
    let kind = if addr_info.kind == "eoa" && (is_contract(&checksummed) || is_token_addr) {
        "contract"
    } else {
        addr_info.kind.as_str()
    };
    let label = addr_info.label.clone().or_else(|| {
        if kind == "contract" {
            db::get_contract_label(&state.db, &checksummed).filter(|n| !n.is_empty())
        } else {
            None
        }
    });
    let ctx = page_ctx(
        &state,
        json!({
            "address": checksummed,
            "addr_info": addr_info,
            "type": kind,
            "label": label,
            "transactions": transactions,
            "html_transactions": html_transactions,
            "holdings": db::get_address_holdings(&state.db, &checksummed),
            "tx_count": tx_count,
            "transfer_count": transfer_count,
            "page": page,
            "total_pages": total_pages,
            "per_page": per_page,
            "active_tab": tab,
            "query": "",
        }),
    );
    html_or_json(&state, &headers, &query, "address.html", &ctx)
}

pub async fn token_page(
    State(state): State<AppState>,
    Path(address): Path<String>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let checksummed = checksum_address(&address);
    if !is_valid_address(&checksummed) {
        return if wants_json(&headers, &query) {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("Invalid address: {address}")})),
            )
                .into_response()
        } else {
            not_found_html(&state, "Token", &address, "Invalid address")
        };
    }

    let meta = match db::get_token_metadata(&state.db, &checksummed) {
        Some(m) => m,
        None => {
            let fetched = fetch_token_metadata(&state.rpc, &checksummed).await;
            let _ = db::save_token_metadata(&state.db, &fetched);
            db::get_token_metadata(&state.db, &checksummed).unwrap_or_else(|| {
                // Fall back to a minimal descriptor if the save failed.
                crate::models::TokenMetadata {
                    address: checksummed.clone(),
                    name: fetched.name,
                    symbol: fetched.symbol,
                    decimals: fetched.decimals,
                    currency: fetched.currency,
                    total_supply: fetched.total_supply,
                    logo_uri: String::new(),
                    holder_count: 0,
                    created_at: db::now_ts(),
                    updated_at: db::now_ts(),
                }
            })
        }
    };

    let tab = query
        .get("tab")
        .cloned()
        .unwrap_or_else(|| "transactions".into());
    let page: u32 = query
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1)
        .max(1);
    let per_page: u32 = 25;
    let transfers = if tab == "transfers" {
        db::get_token_transfers(&state.db, &checksummed, page, per_page)
    } else {
        Vec::new()
    };

    let holders = db::get_token_holder_count(&state.db, &checksummed);
    let transfer_count = db::get_token_transfer_count(&state.db, &checksummed);
    let ctx = page_ctx(
        &state,
        json!({
            "token": meta,
            "transfers": transfers,
            "holders": holders,
            "transfer_count": transfer_count,
            "page": page,
            "per_page": per_page,
            "active_tab": tab,
            "query": "",
        }),
    );
    html_or_json(&state, &headers, &query, "token.html", &ctx)
}

pub async fn tokens_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let page: u32 = query
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1)
        .max(1);
    let per_page: u32 = 25;
    let tokens = db::get_all_tokens(&state.db, page, per_page);
    let total = db::get_token_count(&state.db);
    let total_pages = ((total as f64) / (per_page as f64)).ceil().max(1.0) as u32;
    let ctx = page_ctx(
        &state,
        json!({
            "tokens": tokens,
            "total": total,
            "page": page,
            "total_pages": total_pages,
            "per_page": per_page,
            "query": "",
        }),
    );
    html_or_json(&state, &headers, &query, "tokens.html", &ctx)
}

pub async fn search_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let q = query.get("q").cloned().unwrap_or_default();
    let q = q.trim().to_string();
    if q.is_empty() {
        if wants_json(&headers, &query) {
            return Json(json!({"query": q, "match": Value::Null})).into_response();
        }
        return Redirect::to("/").into_response();
    }

    let mut found: Option<Value> = None;
    if q.chars().all(|c| c.is_ascii_digit()) && !q.is_empty() {
        if let Ok(n) = q.parse::<i64>() {
            if db::get_block_by_number(&state.db, n).is_some() {
                found = Some(json!({"type": "block", "id": q, "url": format!("/block/{q}")}));
            }
        }
    }
    if found.is_none() && db::get_transaction(&state.db, &q).is_some() {
        found = Some(json!({"type": "transaction", "id": q, "url": format!("/tx/{q}")}));
    }
    if found.is_none() {
        if let Some(b) = db::get_block_by_hash(&state.db, &q) {
            found = Some(json!({
                "type": "block",
                "id": b.number.to_string(),
                "url": format!("/block/{}", b.number),
            }));
        }
    }
    if found.is_none() {
        let checksummed = checksum_address(&q);
        if is_valid_address(&checksummed) {
            found = Some(json!({
                "type": "address",
                "id": checksummed,
                "url": format!("/address/{checksummed}"),
            }));
        }
        if found.is_none() {
            if let Some(meta) = db::get_token_metadata(&state.db, &q) {
                found = Some(json!({
                    "type": "token",
                    "id": meta.address,
                    "url": format!("/token/{}", meta.address),
                }));
            }
        }
    }
    if found.is_none() {
        if let Some(meta) = db::get_token_by_symbol_or_name(&state.db, &q) {
            found = Some(json!({
                "type": "token",
                "id": meta.address,
                "url": format!("/token/{}", meta.address),
            }));
        }
    }

    if wants_json(&headers, &query) {
        return Json(json!({"query": q, "match": found})).into_response();
    }
    if let Some(m) = found {
        let url = m
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("/")
            .to_string();
        return Redirect::to(&url).into_response();
    }
    let ctx = json!({
        "query": q,
        "results": [],
        "latest_block": db::get_latest_block(&state.db),
        "native_symbol": state.cfg.native_symbol,
    });
    render_html(&state.tera, "search.html", &ctx)
}

// ---------------------------------------------------------------------------
// Error helpers
// ---------------------------------------------------------------------------

fn not_found(
    state: &AppState,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    kind: &str,
    id: &str,
) -> Response {
    if wants_json(headers, query) {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("{kind} not found")})),
        )
            .into_response()
    } else {
        not_found_html(state, kind, id, &format!("{kind} not found"))
    }
}

fn not_found_html(state: &AppState, kind: &str, id: &str, message: &str) -> Response {
    let ctx = json!({"type": kind, "id": id, "message": message});
    match tera::Context::from_serialize(&ctx) {
        Ok(tera_ctx) => match state.tera.render("404.html", &tera_ctx) {
            Ok(html) => (StatusCode::NOT_FOUND, Html(html)).into_response(),
            Err(e) => {
                tracing::error!("404 template render failed: {e}");
                (StatusCode::NOT_FOUND, Html(message.to_string())).into_response()
            }
        },
        Err(e) => {
            tracing::error!("404 context failed: {e}");
            (StatusCode::NOT_FOUND, Html(message.to_string())).into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub fn is_valid_address(addr: &str) -> bool {
    let s = addr.strip_prefix("0x").unwrap_or(addr);
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn parse_hex_i64(s: &str) -> i64 {
    if let Some(h) = s.strip_prefix("0x") {
        i64::from_str_radix(h, 16).unwrap_or(0)
    } else {
        s.parse().unwrap_or(0)
    }
}

fn block_pct(gas_used: i64, gas_limit: i64) -> String {
    if gas_limit > 0 {
        format!("{:.1}", gas_used as f64 / gas_limit as f64 * 100.0)
    } else {
        "0".into()
    }
}

fn format_block_time(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.2}s", ms / 1000.0)
    } else {
        format!("{ms:.0} ms")
    }
}

/// Attach token symbol/decimals + formatted amounts to transfer rows.
fn enrich_transfers(state: &AppState, transfers: &mut [Value]) {
    let addrs: Vec<String> = transfers
        .iter()
        .filter_map(|t| {
            t.get("token_addr")
                .and_then(Value::as_str)
                .map(String::from)
        })
        .collect();
    let metas = db::get_tokens_metadata(&state.db, &addrs);
    for t in transfers.iter_mut() {
        let Some(addr) = t.get("token_addr").and_then(Value::as_str) else {
            continue;
        };
        let amount = t
            .get("amount")
            .and_then(Value::as_str)
            .unwrap_or("0")
            .to_string();
        let (symbol, decimals) = metas
            .get(addr)
            .map(|m| (m.symbol.clone(), m.decimals))
            .unwrap_or_else(|| (String::new(), 18));
        t["token_symbol"] = json!(symbol);
        t["token_decimals"] = json!(decimals);
        t["amount_formatted"] = json!(format_token_amount(&amount, decimals));
    }
}

// ---------------------------------------------------------------------------
// Tera setup
// ---------------------------------------------------------------------------

pub fn build_tera(db: Db) -> Result<Arc<Tera>> {
    let mut tera = Tera::new("templates/**/*.html").context("load templates")?;

    tera.register_filter(
        "to_checksum",
        |value: &Value, _: &HashMap<String, Value>| {
            Ok(Value::String(checksum_address(
                value.as_str().unwrap_or(""),
            )))
        },
    );
    tera.register_filter(
        "timestamp_to_date",
        |value: &Value, _: &HashMap<String, Value>| {
            let ts = value.as_i64().unwrap_or(0);
            let formatted = Local
                .timestamp_opt(ts, 0)
                .single()
                .map(|dt| dt.format("%m/%d/%y %H:%M:%S").to_string())
                .unwrap_or_else(|| "unknown".into());
            Ok(Value::String(formatted))
        },
    );
    // Use the fallback when the value is null or an empty string (Tera's
    // built-in `default` only handles null/undefined).
    tera.register_filter(
        "fallback",
        |value: &Value, args: &HashMap<String, Value>| {
            let empty = value.is_null() || value.as_str().map(|s| s.is_empty()).unwrap_or(false);
            if empty {
                Ok(args.get("value").cloned().unwrap_or(Value::Null))
            } else {
                Ok(value.clone())
            }
        },
    );

    tera.register_function("truncate_hash", |args: &HashMap<String, Value>| {
        let h = args.get("h").and_then(Value::as_str).unwrap_or("");
        let prefix = args.get("prefix").and_then(Value::as_i64).unwrap_or(8) as usize;
        let suffix = args.get("suffix").and_then(Value::as_i64).unwrap_or(4) as usize;
        Ok(Value::String(truncate_hash(h, prefix, suffix)))
    });
    tera.register_function("format_time_ago", |args: &HashMap<String, Value>| {
        let ts = args.get("timestamp").and_then(Value::as_i64).unwrap_or(0);
        Ok(Value::String(format_time_ago(ts)))
    });
    tera.register_function("get_block_url", move |args: &HashMap<String, Value>| {
        let id = args.get("block_id").and_then(Value::as_str).unwrap_or("");
        let url = if id.chars().all(|c| c.is_ascii_digit()) && !id.is_empty() {
            format!("/block/{id}")
        } else {
            db::get_block_by_hash(&db, id)
                .map(|b| format!("/block/{}", b.number))
                .unwrap_or_else(|| format!("/block/{id}"))
        };
        Ok(Value::String(url))
    });
    tera.register_function("get_tx_url", |args: &HashMap<String, Value>| {
        let h = args.get("tx_hash").and_then(Value::as_str).unwrap_or("");
        Ok(Value::String(format!("/tx/{h}")))
    });
    tera.register_function("get_address_url", |args: &HashMap<String, Value>| {
        let a = args.get("address").and_then(Value::as_str).unwrap_or("");
        let url = if is_valid_address(&checksum_address(a)) {
            format!("/address/{}", checksum_address(a))
        } else {
            format!("/address/{a}")
        };
        Ok(Value::String(url))
    });
    tera.register_function("get_token_url", |args: &HashMap<String, Value>| {
        let a = args.get("address").and_then(Value::as_str).unwrap_or("");
        let url = if is_valid_address(&checksum_address(a)) {
            format!("/token/{}", checksum_address(a))
        } else {
            format!("/token/{a}")
        };
        Ok(Value::String(url))
    });
    tera.register_function("format_token_amount", |args: &HashMap<String, Value>| {
        let amount = args.get("amount").and_then(Value::as_str).unwrap_or("0");
        let decimals = args.get("decimals").and_then(Value::as_i64).unwrap_or(18);
        Ok(Value::String(format_token_amount(amount, decimals)))
    });
    tera.register_function(
        "format_token_amount_with_symbol",
        |args: &HashMap<String, Value>| {
            let amount = args.get("amount").and_then(Value::as_str).unwrap_or("0");
            let decimals = args.get("decimals").and_then(Value::as_i64).unwrap_or(18);
            let symbol = args.get("symbol").and_then(Value::as_str).unwrap_or("");
            Ok(Value::String(format_token_amount_with_symbol(
                amount, decimals, symbol,
            )))
        },
    );

    Ok(Arc::new(tera))
}

pub fn truncate_hash(h: &str, prefix: usize, suffix: usize) -> String {
    if h.is_empty() {
        return String::new();
    }
    if h.len() > prefix + suffix + 3 {
        format!("{}…{}", &h[..prefix + 2], &h[h.len() - suffix..])
    } else {
        h.to_string()
    }
}

pub fn format_time_ago(ts: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    let diff = now - ts;
    if diff < 0 {
        "just now".into()
    } else if diff < 60 {
        format!("{diff}s ago")
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else if diff < 2592000 {
        format!("{}d ago", diff / 86400)
    } else if diff < 31536000 {
        format!("{}mo ago", diff / 2592000)
    } else {
        format!("{}y ago", diff / 31536000)
    }
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/api/events", get(events))
        .route("/block/{block_id}", get(block_page))
        .route("/blocks", get(blocks_page))
        .route("/tx/{tx_hash}", get(tx_page))
        .route("/receipt/{tx_hash}", get(receipt_page))
        .route("/address/{address}", get(address_page))
        .route("/token/{address}", get(token_page))
        .route("/tokens", get(tokens_page))
        .route("/search", get(search_page))
        .with_state(state)
}
