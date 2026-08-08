//! Normalize raw RPC JSON into storage models (port of the `parse_*`
//! functions in `app/rpc.py`).

use num_bigint::BigInt;
use serde_json::Value;

use crate::db::now_ts;
use crate::models::{Block, Transaction};
use crate::rpc::{int_to_hex_str, parse_int_any, str_field};

pub fn parse_block(raw: &Value) -> Block {
    let number = raw.get("number").map(parse_int_any).unwrap_or(0);
    let tx_count = raw
        .get("transactions")
        .and_then(Value::as_array)
        .map(|a| a.len() as i64)
        .unwrap_or(0);
    let base_fee = raw
        .get("baseFeePerGas")
        .and_then(Value::as_str)
        .and_then(|s| {
            s.strip_prefix("0x")
                .and_then(|h| num_bigint::BigInt::parse_bytes(h.as_bytes(), 16))
                .map(|n| n.to_string())
        })
        .unwrap_or_else(|| "0".into());
    let consensus = raw.get("consensusContext");
    let proposer = consensus
        .and_then(|c| c.get("proposer"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Block {
        number,
        hash: str_field(raw, "hash"),
        parent_hash: str_field(raw, "parentHash"),
        timestamp: raw.get("timestamp").map(parse_int_any).unwrap_or(0),
        gas_used: raw.get("gasUsed").map(parse_int_any).unwrap_or(0),
        gas_limit: raw.get("gasLimit").map(parse_int_any).unwrap_or(0),
        base_fee,
        size: raw.get("size").map(parse_int_any).unwrap_or(0),
        extra_data: str_field(raw, "extraData"),
        epoch: consensus
            .and_then(|c| c.get("epoch"))
            .map(parse_int_any)
            .unwrap_or(0),
        view: consensus
            .and_then(|c| c.get("view"))
            .map(parse_int_any)
            .unwrap_or(0),
        proposer,
        miner: str_field(raw, "miner"),
        tx_count,
        raw: serde_json::to_string(raw).unwrap_or_else(|_| "{}".into()),
        created_at: now_ts(),
    }
}

/// Destination for a transaction. Tempo-style chains put every transfer in a
/// `calls` array instead of a top-level `to` (a true contract creation has
/// neither).
pub fn tx_to_addr(tx: &Value) -> Option<String> {
    tx.get("to")
        .and_then(Value::as_str)
        .map(String::from)
        .or_else(|| {
            tx.get("calls")
                .and_then(Value::as_array)
                .and_then(|c| c.first())
                .and_then(|c| c.get("to"))
                .and_then(Value::as_str)
                .map(String::from)
        })
}

pub fn parse_transaction(tx: &Value, block: &Block) -> Transaction {
    let to_addr = tx_to_addr(tx);
    let gas_price = tx
        .get("gasPrice")
        .cloned()
        .unwrap_or_else(|| Value::String("0x0".into()));
    let max_fee = tx
        .get("maxFeePerGas")
        .cloned()
        .unwrap_or_else(|| gas_price.clone());
    let max_priority = tx
        .get("maxPriorityFeePerGas")
        .cloned()
        .unwrap_or_else(|| Value::String("0x0".into()));

    let value = tx
        .get("value")
        .map(|v| match v {
            Value::String(s) => {
                let s = s.strip_prefix("0x").unwrap_or(s);
                BigInt::parse_bytes(s.as_bytes(), 16)
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "0".into())
            }
            Value::Number(n) => n.as_i64().unwrap_or(0).to_string(),
            _ => "0".into(),
        })
        .unwrap_or_else(|| "0".into());

    let fee_amount = tx
        .get("feeAmount")
        .map(|v| match v {
            Value::String(s) if s.starts_with("0x") => {
                let s = &s[2..];
                BigInt::parse_bytes(s.as_bytes(), 16)
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| s.to_string())
            }
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => "0".into(),
        })
        .unwrap_or_else(|| "0".into());

    let input = {
        let input = str_field(tx, "input");
        let data = str_field(tx, "data");
        if input.is_empty() && !data.is_empty() {
            data
        } else {
            input
        }
    };
    let method_id = if let Some(hex) = input.strip_prefix("0x") {
        if hex.len() >= 8 {
            format!("0x{}", &hex[..8])
        } else {
            "0x".into()
        }
    } else {
        String::new()
    };
    Transaction {
        hash: str_field(tx, "hash"),
        block_number: tx.get("blockNumber").map(parse_int_any).unwrap_or(0),
        block_hash: {
            let h = str_field(tx, "blockHash");
            if h.is_empty() {
                block.hash.clone()
            } else {
                h
            }
        },
        position: tx.get("transactionIndex").map(parse_int_any).unwrap_or(0),
        from_addr: str_field(tx, "from"),
        to_addr,
        status: 1,
        gas_limit: tx.get("gas").map(parse_int_any).unwrap_or(0),
        gas_used: 0,
        gas_price: int_to_hex_str(&gas_price),
        max_fee_per_gas: int_to_hex_str(&max_fee),
        max_priority_fee_per_gas: int_to_hex_str(&max_priority),
        base_fee: "0x0".into(),
        contract_address: None,
        fee_token: None,
        fee_amount,
        nonce: tx.get("nonce").map(parse_int_any).unwrap_or(0),
        nonce_key: tx
            .get("nonceKey")
            .and_then(Value::as_str)
            .map(String::from)
            .or_else(|| Some("0x".into())),
        value,
        chain_id: tx.get("chainId").map(parse_int_any).unwrap_or(0),
        tx_type: tx.get("type").map(parse_int_any).unwrap_or(0x76),
        method_id,
        input,
        raw: serde_json::to_string(tx).ok(),
        trace_data: None,
        receipt_data: None,
        timestamp: block.timestamp,
        created_at: now_ts(),
    }
}
