use nvnmchain_explorer::decoder::{
    checksum_address, decode_event, decode_function_call, extract_balance_changes, extract_calls,
    flatten_trace, TRANSFER_TOPIC,
};
use nvnmchain_explorer::models::Transaction;
use nvnmchain_explorer::parse::{parse_block, parse_transaction};
use nvnmchain_explorer::tokens::format_token_amount;
use serde_json::json;

fn transfer_calldata(to: &str, amount: u128) -> String {
    format!(
        "0xa9059cbb000000000000000000000000{}{:064x}",
        to.trim_start_matches("0x"),
        amount
    )
}

#[test]
fn checksum_is_stable_and_40_hex() {
    let a = checksum_address("0x20c0000000000000000000000000000000000000");
    assert_eq!(a.len(), 42);
    assert_eq!(checksum_address(&a), a);
    let b = checksum_address("0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
    assert_eq!(b.len(), 42);
    assert_eq!(checksum_address(&b), b);
}

#[test]
fn decode_transfer_call() {
    let data = transfer_calldata("0x0000000000000000000000000000000000000001", 100);
    let call = decode_function_call(&data).expect("decoded");
    assert_eq!(call.name.as_deref(), Some("transfer"));
    assert_eq!(call.signature.as_deref(), Some("transfer(address,uint256)"));
    assert_eq!(call.selector, "0xa9059cbb");
    assert_eq!(call.params.len(), 2);
    assert_eq!(call.params[0].name, "to");
    assert_eq!(call.params[1].name, "amount");
    assert_eq!(call.params[1].value, "100");
}

#[test]
fn decode_approve_and_unknown() {
    let approve = format!(
        "0x095ea7b3{}{}{:064x}",
        "00".repeat(12),
        "ab".repeat(20),
        42u64
    );
    let call = decode_function_call(&approve).expect("decoded");
    assert_eq!(call.name.as_deref(), Some("approve"));
    assert_eq!(call.params[1].value, "42");

    let unknown = decode_function_call("0xdeadbeef1234").expect("unknown decoded");
    assert!(unknown.name.is_none());
    assert_eq!(unknown.selector, "0xdeadbeef");
    assert_eq!(unknown.raw_args, "0x1234");

    assert!(decode_function_call("0x").is_none());
    assert!(decode_function_call("0x1234").is_none());
}

#[test]
fn decode_transfer_event() {
    let from = format!("0x{}", "11".repeat(20));
    let to = format!("0x{}", "22".repeat(20));
    let log = json!({
        "address": format!("0x{}", "cc".repeat(20)),
        "topics": [
            TRANSFER_TOPIC,
            format!("0x{}{}", "00".repeat(12), from.trim_start_matches("0x")),
            format!("0x{}{}", "00".repeat(12), to.trim_start_matches("0x")),
        ],
        "data": format!("0x{:064x}", 1234u64),
        "logIndex": "0x0",
    });
    let event = decode_event(&log).expect("decoded event");
    assert_eq!(event.name.as_deref(), Some("Transfer"));
    assert_eq!(event.params.len(), 3);
    assert_eq!(event.params[0].name, "from");
    assert!(event.params[0].indexed);
    assert_eq!(event.params[2].value, "1234");
    assert_eq!(event.params[1].value, checksum_address(&to));
}

#[test]
fn flatten_trace_nested() {
    let trace = json!({
        "type": "CALL",
        "from": format!("0x{}", "aa".repeat(20)),
        "to": format!("0x{}", "bb".repeat(20)),
        "input": "0x",
        "value": "0x0",
        "gas": "0x186a0",
        "gasUsed": "0xcccc",
        "calls": [
            {"type": "STATICCALL", "from": format!("0x{}", "bb".repeat(20)), "to": format!("0x{}", "cc".repeat(20)), "input": "0x", "value": "0x0", "gas": "0x5208", "gasUsed": "0x100"},
            {"type": "CALL", "from": format!("0x{}", "bb".repeat(20)), "to": format!("0x{}", "dd".repeat(20)), "input": "0x", "value": "0x01", "gas": "0x7530", "gasUsed": "0x2000"}
        ]
    });
    let flat = flatten_trace(&trace);
    assert_eq!(flat.len(), 3);
    assert_eq!(flat[0]["depth"], 0);
    assert_eq!(flat[1]["depth"], 1);
    assert_eq!(flat[2]["depth"], 1);
    assert_eq!(flat[1]["type"], "STATICCALL");
    assert_eq!(flat[0]["children"], json!([1, 2]));
    assert_eq!(flat[1]["gas"], "21000");
    assert_eq!(flat[2]["value"], "1");
}

#[test]
fn format_amounts() {
    assert_eq!(format_token_amount("0", 18), "0");
    assert_eq!(format_token_amount("1000000000000000000", 18), "1");
    assert_eq!(format_token_amount("1234567890123456789", 18), "1.234567");
    assert_eq!(format_token_amount("5000000000000000000", 18), "5");
}

#[test]
fn parse_block_and_transaction() {
    let raw_block = json!({
        "number": "0x10",
        "hash": format!("0x{}", "ab".repeat(32)),
        "parentHash": format!("0x{}", "cd".repeat(32)),
        "timestamp": "0x64",
        "gasUsed": "0x5208",
        "gasLimit": "0x1c9c380",
        "miner": format!("0x{}", "ee".repeat(20)),
        "transactions": [
            {
                "hash": format!("0x{}", "ff".repeat(32)),
                "blockNumber": "0x10",
                "blockHash": format!("0x{}", "ab".repeat(32)),
                "transactionIndex": "0x0",
                "from": format!("0x{}", "11".repeat(20)),
                "to": format!("0x{}", "22".repeat(20)),
                "gas": "0x5208",
                "gasPrice": "0x4a817c800",
                "value": "0xde0b6b3a7640000",
                "nonce": "0x3",
                "type": "0x0",
                "input": "0x",
                "chainId": "0x2b45"
            }
        ]
    });
    let block = parse_block(&raw_block);
    assert_eq!(block.number, 16);
    assert_eq!(block.tx_count, 1);
    assert_eq!(block.timestamp, 100);

    let tx = parse_transaction(&raw_block["transactions"][0], &block);
    assert_eq!(tx.hash, format!("0x{}", "ff".repeat(32)));
    assert_eq!(tx.block_number, 16);
    assert_eq!(tx.value, "1000000000000000000");
    assert_eq!(tx.nonce, 3);
    assert_eq!(tx.gas_price, "0x4a817c800");
    assert_eq!(tx.chain_id, 0x2b45);
    assert_eq!(tx.timestamp, 100);
}

#[test]
fn balance_changes_from_receipt() {
    let from = format!("0x{}", "11".repeat(20));
    let to = format!("0x{}", "22".repeat(20));
    let receipt = json!({
        "logs": [{
            "address": format!("0x{}", "cc".repeat(20)),
            "topics": [
                TRANSFER_TOPIC,
                format!("0x{}{}", "00".repeat(12), from.trim_start_matches("0x")),
                format!("0x{}{}", "00".repeat(12), to.trim_start_matches("0x")),
            ],
            "data": format!("0x{:064x}", 500u64),
        }]
    });
    let tx = Transaction {
        hash: "0x".into(),
        block_number: 0,
        block_hash: "0x".into(),
        position: 0,
        from_addr: from.clone(),
        to_addr: None,
        status: 1,
        gas_limit: 0,
        gas_used: 0,
        gas_price: "0x0".into(),
        max_fee_per_gas: "0x0".into(),
        max_priority_fee_per_gas: "0x0".into(),
        base_fee: "0x0".into(),
        contract_address: None,
        fee_token: None,
        fee_amount: "0".into(),
        nonce: 0,
        nonce_key: None,
        value: "0".into(),
        chain_id: 0,
        tx_type: 0,
        input: "0x".into(),
        raw: None,
        trace_data: None,
        receipt_data: None,
        timestamp: 0,
        created_at: 0,
    };
    let changes = extract_balance_changes(&receipt, &tx);
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0]["change"], "-500");
    assert_eq!(changes[1]["change"], "+500");
    assert_eq!(changes[0]["is_fee"], false);

    let calls = extract_calls(&tx, &[]);
    assert!(calls.is_empty());
}
