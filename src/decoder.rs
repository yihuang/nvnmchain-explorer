//! ABI decoding for calls, events, traces, and balance changes.
//!
//! A self-contained ABIv2 decoder (no alloy dependency) plus built-in TIP-20 /
//! ERC-20 token metadata and labels.

use num_bigint::{BigInt, Sign};
use serde::Serialize;
use serde_json::{json, Value};
use sha3::{Digest, Keccak256};

use crate::models::Transaction;

// ---------------------------------------------------------------------------
// Keccak / checksum helpers
// ---------------------------------------------------------------------------

pub fn keccak256(input: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(input);
    hasher.finalize().into()
}

pub fn keccak_hex(input: &[u8]) -> String {
    format!("0x{}", hex::encode(keccak256(input)))
}

/// EIP-55 checksummed address from any 40-hex-digit input (with or without 0x).
pub fn checksum_address(addr: &str) -> String {
    let lower = addr.trim().to_lowercase();
    let lower = lower.strip_prefix("0x").unwrap_or(&lower);
    if lower.len() != 40 || !lower.chars().all(|c| c.is_ascii_hexdigit()) {
        return addr.trim().to_string();
    }
    let hash = keccak256(lower.as_bytes());
    let mut out = String::with_capacity(42);
    out.push_str("0x");
    for (i, c) in lower.chars().enumerate() {
        let nibble = hash[i / 2] >> (if i % 2 == 0 { 4 } else { 0 }) & 0x0f;
        if c.is_ascii_alphabetic() && nibble >= 8 {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Decoded models (serde-serialized for the JSON API)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct DecodedParam {
    #[serde(rename = "type")]
    pub ty: String,
    pub name: String,
    pub value: String,
    pub indexed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DecodedCall {
    pub name: Option<String>,
    pub signature: Option<String>,
    pub params: Vec<DecodedParam>,
    pub selector: String,
    pub raw_args: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DecodedEvent {
    pub name: Option<String>,
    pub signature: Option<String>,
    pub contract: String,
    pub params: Vec<DecodedParam>,
    pub topic0: String,
    pub log_index: Option<String>,
    pub transaction_hash: Option<String>,
}

impl DecodedCall {
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

// ---------------------------------------------------------------------------
// ABI type parser + decoder
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum AbiType {
    Address,
    Bool,
    Uint(usize),
    Int(usize),
    FixedBytes(usize),
    Bytes,
    String,
    Array(Box<AbiType>, Option<usize>),
    Tuple(Vec<AbiType>),
    Function,
}

impl AbiType {
    fn parse(s: &str) -> Option<AbiType> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        if s == "address" {
            return Some(AbiType::Address);
        }
        if s == "bool" {
            return Some(AbiType::Bool);
        }
        if s == "string" {
            return Some(AbiType::String);
        }
        if s == "bytes" {
            return Some(AbiType::Bytes);
        }
        if s == "function" {
            return Some(AbiType::Function);
        }
        if let Some(n) = s.strip_prefix("uint") {
            if let Ok(bits) = n.parse::<usize>() {
                if (8..=256).contains(&bits) && bits % 8 == 0 {
                    return Some(AbiType::Uint(bits));
                }
            }
        }
        if let Some(n) = s.strip_prefix("int") {
            if let Ok(bits) = n.parse::<usize>() {
                if (8..=256).contains(&bits) && bits % 8 == 0 {
                    return Some(AbiType::Int(bits));
                }
            }
        }
        if let Some(n) = s.strip_prefix("bytes") {
            if let Ok(len) = n.parse::<usize>() {
                if (1..=32).contains(&len) {
                    return Some(AbiType::FixedBytes(len));
                }
            }
        }
        if s.starts_with('(') {
            let inner = {
                let i = find_tuple_end(s)?;
                &s[1..i]
            };
            let mut types = Vec::new();
            for part in split_top_level(inner) {
                types.push(AbiType::parse(part)?);
            }
            return Some(AbiType::Tuple(types));
        }
        // Array suffixes: T[] or T[k]
        if s.ends_with(']') {
            let open = s.rfind('[')?;
            let inner = &s[..open];
            let dim = &s[open + 1..s.len() - 1];
            let elem = Box::new(AbiType::parse(inner)?);
            let len = if dim.is_empty() {
                None
            } else {
                Some(dim.parse::<usize>().ok()?)
            };
            return Some(AbiType::Array(elem, len));
        }
        None
    }

    fn is_dynamic(&self) -> bool {
        match self {
            AbiType::Bytes | AbiType::String => true,
            AbiType::Array(_, None) => true,
            AbiType::Array(elem, Some(_)) => elem.is_dynamic(),
            AbiType::Tuple(types) => types.iter().any(|t| t.is_dynamic()),
            _ => false,
        }
    }

    /// Number of 32-byte head words a value of this type occupies (only valid
    /// for static types; dynamic values always occupy one head word).
    fn head_words(&self) -> usize {
        match self {
            AbiType::Array(elem, Some(k)) => k * elem.head_words(),
            AbiType::Tuple(types) => types.iter().map(|t| t.head_words()).sum(),
            _ => 1,
        }
    }
}

fn find_tuple_end(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Split `a,b,(c,d),e[]` on top-level commas.
fn split_top_level(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

fn word(bytes: &[u8]) -> &[u8] {
    &bytes[..bytes.len().min(32)]
}

fn big_from_word(bytes: &[u8]) -> BigInt {
    let w = word(bytes);
    let mut buf = [0u8; 32];
    buf[..w.len()].copy_from_slice(w);
    BigInt::from_bytes_be(Sign::Plus, &buf)
}

fn hex_bytes(value: &[u8]) -> String {
    format!("0x{}", hex::encode(value))
}

/// Format a decoded value: addresses checksummed, ints decimal, bytes hex,
/// bools lowercase.
fn format_value(ty: &AbiType, value: &[u8], _ty_str: &str) -> String {
    match ty {
        AbiType::Address => {
            checksum_address(&hex::encode(&value[value.len().saturating_sub(20)..]))
        }
        AbiType::Bool => {
            if big_from_word(value).sign() == Sign::Plus && big_from_word(value) != BigInt::from(0)
            {
                "true".into()
            } else {
                "false".into()
            }
        }
        AbiType::Uint(_) => big_from_word(value).to_string(),
        AbiType::Int(bits) => {
            let n = big_from_word(value);
            let mask = BigInt::from(1) << (bits - 1);
            if &n & &mask != BigInt::from(0) {
                (n - (BigInt::from(1) << bits)).to_string()
            } else {
                n.to_string()
            }
        }
        AbiType::FixedBytes(len) => hex_bytes(&value[..*len.min(&value.len())]),
        AbiType::Bytes => {
            // len word + payload
            let len = big_from_word(value)
                .to_string()
                .parse::<usize>()
                .unwrap_or(0);
            let payload = value.get(32..32 + len).unwrap_or(&[]);
            hex_bytes(payload)
        }
        AbiType::String => {
            let len = big_from_word(value)
                .to_string()
                .parse::<usize>()
                .unwrap_or(0);
            let payload = value.get(32..32 + len).unwrap_or(&[]);
            String::from_utf8_lossy(payload).into_owned()
        }
        AbiType::Function => hex_bytes(&value[..24.min(value.len())]),
        AbiType::Array(_, _) | AbiType::Tuple(_) => {
            // Decoded higher up; fall back to raw hex.
            hex_bytes(value)
        }
    }
}

/// Decode a tuple (or top-level argument list) of `types` from `data`.
fn decode_tuple(types: &[AbiType], data: &[u8]) -> Vec<String> {
    let mut values = Vec::with_capacity(types.len());
    let mut head_off = 0usize;
    for ty in types {
        if ty.is_dynamic() {
            let off_bytes = word(data.get(head_off * 32..head_off * 32 + 32).unwrap_or(&[]));
            let off = big_from_word(off_bytes)
                .to_string()
                .parse::<usize>()
                .unwrap_or(0);
            let tail = data.get(off..).unwrap_or(&[]);
            values.push(decode_dynamic(ty, tail));
        } else {
            let words = ty.head_words();
            let start = head_off * 32;
            let slice = data.get(start..start + words * 32).unwrap_or(&[]);
            values.push(decode_static(ty, slice));
        }
        head_off += ty.head_words();
    }
    values
}

fn decode_static(ty: &AbiType, slice: &[u8]) -> String {
    match ty {
        AbiType::Array(elem, Some(k)) => {
            let mut parts = Vec::new();
            let stride = elem.head_words() * 32;
            for i in 0..*k {
                let start = i * stride;
                let item = slice.get(start..start + stride).unwrap_or(&[]);
                if elem.is_dynamic() {
                    parts.push(decode_dynamic(elem, item));
                } else {
                    parts.push(decode_static(elem, item));
                }
            }
            format!("[{}]", parts.join(", "))
        }
        AbiType::Tuple(types) => format!("({})", decode_tuple(types, slice).join(", ")),
        _ => format_value(ty, slice, ""),
    }
}

fn decode_dynamic(ty: &AbiType, data: &[u8]) -> String {
    match ty {
        AbiType::Bytes | AbiType::String => format_value(ty, data, ""),
        AbiType::Array(elem, _) => {
            let count = big_from_word(word(data))
                .to_string()
                .parse::<usize>()
                .unwrap_or(0);
            let body = data.get(32..).unwrap_or(&[]);
            let mut parts = Vec::new();
            let mut head_off = 0usize;
            for _ in 0..count {
                if elem.is_dynamic() {
                    let off = big_from_word(word(
                        body.get(head_off * 32..head_off * 32 + 32).unwrap_or(&[]),
                    ))
                    .to_string()
                    .parse::<usize>()
                    .unwrap_or(0);
                    parts.push(decode_dynamic(elem, body.get(off..).unwrap_or(&[])));
                } else {
                    let words = elem.head_words();
                    let start = head_off * 32;
                    parts.push(decode_static(
                        elem,
                        body.get(start..start + words * 32).unwrap_or(&[]),
                    ));
                }
                head_off += elem.head_words();
            }
            format!("[{}]", parts.join(", "))
        }
        AbiType::Tuple(types) => format!("({})", decode_tuple(types, data).join(", ")),
        _ => format_value(ty, data, ""),
    }
}

/// Decode ABI-encoded arguments for `types` from raw calldata (after selector).
pub fn decode_abi_args(types: &[&str], data: &[u8]) -> Vec<String> {
    let parsed: Vec<AbiType> = types.iter().filter_map(|t| AbiType::parse(t)).collect();
    if parsed.len() != types.len() {
        return Vec::new();
    }
    decode_tuple(&parsed, data)
}

// ---------------------------------------------------------------------------
// Known selectors / signatures
// ---------------------------------------------------------------------------

struct FnDef {
    name: &'static str,
    canonical: &'static str,
    /// (type, name) input pairs.
    inputs: &'static [(&'static str, &'static str)],
}

fn tip20_fns() -> &'static [FnDef] {
    &[
        FnDef {
            name: "transfer",
            canonical: "transfer(address,uint256)",
            inputs: &[("address", "to"), ("uint256", "amount")],
        },
        FnDef {
            name: "transferWithMemo",
            canonical: "transferWithMemo(address,uint256,bytes32)",
            inputs: &[
                ("address", "to"),
                ("uint256", "amount"),
                ("bytes32", "memo"),
            ],
        },
        FnDef {
            name: "transferFrom",
            canonical: "transferFrom(address,address,uint256)",
            inputs: &[
                ("address", "sender"),
                ("address", "to"),
                ("uint256", "amount"),
            ],
        },
        FnDef {
            name: "transferFromWithMemo",
            canonical: "transferFromWithMemo(address,address,uint256,bytes32)",
            inputs: &[
                ("address", "sender"),
                ("address", "to"),
                ("uint256", "amount"),
                ("bytes32", "memo"),
            ],
        },
        FnDef {
            name: "approve",
            canonical: "approve(address,uint256)",
            inputs: &[("address", "spender"), ("uint256", "amount")],
        },
        FnDef {
            name: "mint",
            canonical: "mint(address,uint256)",
            inputs: &[("address", "to"), ("uint256", "amount")],
        },
        FnDef {
            name: "mintWithMemo",
            canonical: "mintWithMemo(address,uint256,bytes32)",
            inputs: &[
                ("address", "to"),
                ("uint256", "amount"),
                ("bytes32", "memo"),
            ],
        },
        FnDef {
            name: "burn",
            canonical: "burn(uint256)",
            inputs: &[("uint256", "amount")],
        },
        FnDef {
            name: "burnWithMemo",
            canonical: "burnWithMemo(uint256,bytes32)",
            inputs: &[("uint256", "amount"), ("bytes32", "memo")],
        },
    ]
}

fn additional_sigs() -> &'static [(&'static str, &'static str)] {
    // selector -> "signature with parameter names"
    &[
        ("0x70a08231", "balanceOf(address account)"),
        ("0x18160ddd", "totalSupply()"),
        ("0x06fdde03", "name()"),
        ("0x95d89b41", "symbol()"),
        ("0x313ce567", "decimals()"),
        ("0xdd62ed3e", "allowance(address owner, address spender)"),
        ("0x40c10f19", "mint(address to, uint256 amount)"),
        ("0x42966c68", "burn(uint256 amount)"),
        ("0x0b631400", "authorizeKey(address,uint8,tuple)"),
        ("0x18783a95", "revokeKey(address keyId)"),
    ]
}

fn selector_hex(sig: &str) -> String {
    let hash = keccak256(sig.as_bytes());
    format!("0x{}", hex::encode(&hash[..4]))
}

fn param_names_from_sig(sig: &str) -> Vec<String> {
    let open = sig.find('(').unwrap_or(0);
    let close = sig.rfind(')').unwrap_or(sig.len());
    let inner = &sig[open + 1..close];
    let mut names = Vec::new();
    for part in split_top_level(inner) {
        let part = part.trim();
        if part.is_empty() {
            names.push(String::new());
        } else if let Some(sp) = part.rfind(' ') {
            names.push(part[sp + 1..].trim().to_string());
        } else {
            names.push(String::new());
        }
    }
    names
}

fn types_from_sig(sig: &str) -> Vec<String> {
    let open = sig.find('(').unwrap_or(0);
    let close = sig.rfind(')').unwrap_or(sig.len());
    let inner = &sig[open + 1..close];
    split_top_level(inner)
        .into_iter()
        .map(|p| {
            let p = p.trim();
            match p.rfind(' ') {
                Some(i) => p[..i].trim().to_string(),
                None => p.to_string(),
            }
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// Decode 0x-prefixed calldata into a call descriptor (None for empty/trivial).
pub fn decode_function_call(data: &str) -> Option<DecodedCall> {
    let data = data.strip_prefix("0x").unwrap_or(data);
    if data.is_empty() {
        return None;
    }
    let raw = hex::decode(data).ok()?;
    if raw.len() < 4 {
        return None;
    }
    let selector = &raw[..4];
    let sel_hex = format!("0x{}", hex::encode(selector));
    let args = &raw[4..];

    for def in tip20_fns() {
        if selector_hex(def.canonical) == sel_hex {
            let types: Vec<&str> = def.inputs.iter().map(|(t, _)| *t).collect();
            let values = decode_abi_args(&types, args);
            let params: Vec<DecodedParam> = def
                .inputs
                .iter()
                .enumerate()
                .map(|(i, (t, n))| DecodedParam {
                    ty: t.to_string(),
                    name: n.to_string(),
                    value: values.get(i).cloned().unwrap_or_default(),
                    indexed: false,
                })
                .collect();
            return Some(DecodedCall {
                name: Some(def.name.to_string()),
                signature: Some(def.canonical.to_string()),
                params,
                selector: sel_hex,
                raw_args: format!("0x{}", hex::encode(args)),
            });
        }
    }

    if let Some((_, sig)) = additional_sigs().iter().find(|(s, _)| s == &sel_hex) {
        let types = types_from_sig(sig);
        let type_refs: Vec<&str> = types.iter().map(|s| s.as_str()).collect();
        let values = decode_abi_args(&type_refs, args);
        let names = param_names_from_sig(sig);
        let params: Vec<DecodedParam> = types
            .iter()
            .enumerate()
            .map(|(i, t)| DecodedParam {
                ty: t.clone(),
                name: names.get(i).cloned().unwrap_or_else(|| format!("arg{i}")),
                value: values.get(i).cloned().unwrap_or_default(),
                indexed: false,
            })
            .collect();
        return Some(DecodedCall {
            name: Some(sig.split('(').next().unwrap_or("").to_string()),
            signature: Some(sig.to_string()),
            params,
            selector: sel_hex,
            raw_args: format!("0x{}", hex::encode(args)),
        });
    }

    Some(DecodedCall {
        name: None,
        signature: None,
        params: Vec::new(),
        selector: sel_hex,
        raw_args: format!("0x{}", hex::encode(args)),
    })
}

// ---------------------------------------------------------------------------
// Event decoding
// ---------------------------------------------------------------------------

pub const TRANSFER_TOPIC: &str =
    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
pub const TRANSFER_WITH_MEMO_TOPIC: &str =
    "0xab2461e5dc8495f413774182e5eb0e9f0f30a81bf32c4b7a4a1d70c3c4e2f0a";
pub const APPROVAL_TOPIC: &str =
    "0x8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925";

fn address_from_topic(topic: &str) -> String {
    let hexed = topic.strip_prefix("0x").unwrap_or(topic);
    let addr = &hexed[hexed.len().saturating_sub(40)..];
    checksum_address(addr)
}

fn uint256_from_data(data: &str, offset: usize) -> String {
    let bytes = hex::decode(data.strip_prefix("0x").unwrap_or(data)).unwrap_or_default();
    let chunk = bytes.get(offset * 32..offset * 32 + 32).unwrap_or(&[]);
    big_from_word(chunk).to_string()
}

fn bytes32_from_data(data: &str, offset: usize) -> String {
    let bytes = hex::decode(data.strip_prefix("0x").unwrap_or(data)).unwrap_or_default();
    let chunk = bytes.get(offset * 32..offset * 32 + 32).unwrap_or(&[]);
    hex_bytes(chunk)
}

pub fn decode_event(log: &Value) -> Option<DecodedEvent> {
    let topics = log.get("topics")?.as_array()?;
    if topics.is_empty() {
        return None;
    }
    let topic0 = topics[0].as_str().unwrap_or("").to_string();
    let contract = log
        .get("address")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let log_index = log
        .get("logIndex")
        .and_then(Value::as_str)
        .map(String::from);
    let transaction_hash = log
        .get("transactionHash")
        .and_then(Value::as_str)
        .map(String::from);
    let data = log
        .get("data")
        .and_then(Value::as_str)
        .unwrap_or("0x")
        .to_string();

    let make =
        |name: &'static str, signature: &'static str, params: Vec<DecodedParam>| DecodedEvent {
            name: Some(name.to_string()),
            signature: Some(signature.to_string()),
            contract: contract.clone(),
            params,
            topic0: topic0.clone(),
            log_index: log_index.clone(),
            transaction_hash: transaction_hash.clone(),
        };

    match topic0.as_str() {
        TRANSFER_TOPIC => {
            if topics.len() < 3 {
                return Some(DecodedEvent {
                    name: Some("Transfer".into()),
                    signature: Some("Transfer(address,address,uint256)".into()),
                    contract,
                    params: Vec::new(),
                    topic0,
                    log_index,
                    transaction_hash,
                });
            }
            let from = topics[1].as_str().unwrap_or("");
            let to = topics[2].as_str().unwrap_or("");
            Some(make(
                "Transfer",
                "Transfer(address,address,uint256)",
                vec![
                    DecodedParam {
                        ty: "address".into(),
                        name: "from".into(),
                        value: address_from_topic(from),
                        indexed: true,
                    },
                    DecodedParam {
                        ty: "address".into(),
                        name: "to".into(),
                        value: address_from_topic(to),
                        indexed: true,
                    },
                    DecodedParam {
                        ty: "uint256".into(),
                        name: "amount".into(),
                        value: uint256_from_data(&data, 0),
                        indexed: false,
                    },
                ],
            ))
        }
        TRANSFER_WITH_MEMO_TOPIC => {
            if topics.len() < 3 {
                return Some(DecodedEvent {
                    name: Some("TransferWithMemo".into()),
                    signature: Some("TransferWithMemo(address,address,uint256,bytes32)".into()),
                    contract,
                    params: Vec::new(),
                    topic0,
                    log_index,
                    transaction_hash,
                });
            }
            let from = topics[1].as_str().unwrap_or("");
            let to = topics[2].as_str().unwrap_or("");
            Some(make(
                "TransferWithMemo",
                "TransferWithMemo(address,address,uint256,bytes32)",
                vec![
                    DecodedParam {
                        ty: "address".into(),
                        name: "from".into(),
                        value: address_from_topic(from),
                        indexed: true,
                    },
                    DecodedParam {
                        ty: "address".into(),
                        name: "to".into(),
                        value: address_from_topic(to),
                        indexed: true,
                    },
                    DecodedParam {
                        ty: "uint256".into(),
                        name: "amount".into(),
                        value: uint256_from_data(&data, 0),
                        indexed: false,
                    },
                    DecodedParam {
                        ty: "bytes32".into(),
                        name: "memo".into(),
                        value: bytes32_from_data(&data, 1),
                        indexed: false,
                    },
                ],
            ))
        }
        APPROVAL_TOPIC => {
            if topics.len() < 3 {
                return Some(DecodedEvent {
                    name: Some("Approval".into()),
                    signature: Some("Approval(address,address,uint256)".into()),
                    contract,
                    params: Vec::new(),
                    topic0,
                    log_index,
                    transaction_hash,
                });
            }
            let owner = topics[1].as_str().unwrap_or("");
            let spender = topics[2].as_str().unwrap_or("");
            Some(make(
                "Approval",
                "Approval(address,address,uint256)",
                vec![
                    DecodedParam {
                        ty: "address".into(),
                        name: "owner".into(),
                        value: address_from_topic(owner),
                        indexed: true,
                    },
                    DecodedParam {
                        ty: "address".into(),
                        name: "spender".into(),
                        value: address_from_topic(spender),
                        indexed: true,
                    },
                    DecodedParam {
                        ty: "uint256".into(),
                        name: "amount".into(),
                        value: uint256_from_data(&data, 0),
                        indexed: false,
                    },
                ],
            ))
        }
        _ => {
            let value = if data.len() > 200 {
                format!("{}...", &data[..200])
            } else {
                data.clone()
            };
            Some(DecodedEvent {
                name: None,
                signature: None,
                contract,
                params: vec![DecodedParam {
                    ty: "bytes".into(),
                    name: "data".into(),
                    value,
                    indexed: false,
                }],
                topic0,
                log_index,
                transaction_hash,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Trace flattening
// ---------------------------------------------------------------------------

fn hex_to_dec(value: &Value, default: &str) -> String {
    match value {
        Value::String(s) => {
            let s = s.strip_prefix("0x").unwrap_or(s);
            if s.is_empty() {
                default.into()
            } else {
                BigInt::parse_bytes(s.as_bytes(), 16)
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| default.into())
            }
        }
        Value::Number(n) => n
            .as_i64()
            .map(|i| i.to_string())
            .unwrap_or_else(|| default.into()),
        _ => default.into(),
    }
}

fn walk_trace(node: &Value, depth: usize, result: &mut Vec<Value>) {
    let input = node.get("input").and_then(Value::as_str).unwrap_or("0x");
    let decoded = decode_function_call(input).map(|d| d.to_json());
    let error = node
        .get("error")
        .or_else(|| node.get("revertReason"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let mut flat = json!({
        "depth": depth,
        "type": node.get("type").and_then(Value::as_str).unwrap_or("CALL"),
        "from": node.get("from").and_then(Value::as_str).unwrap_or(""),
        "to": node.get("to").and_then(Value::as_str).unwrap_or(""),
        "data": input,
        "output": node.get("output").and_then(Value::as_str).unwrap_or(""),
        "value": hex_to_dec(node.get("value").unwrap_or(&Value::Null), "0"),
        "gas": hex_to_dec(node.get("gas").unwrap_or(&Value::Null), "0"),
        "gas_used": hex_to_dec(node.get("gasUsed").unwrap_or(&Value::Null), "0"),
        "decoded": decoded.unwrap_or(Value::Null),
        "children": [],
    });
    if let Some(err) = error {
        flat["status"] = json!("failed");
        flat["error"] = json!(err);
    } else {
        flat["status"] = json!("success");
    }
    let idx = result.len();
    result.push(flat);
    let children = node
        .get("calls")
        .or_else(|| node.get("children"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut child_indices = Vec::new();
    for child in &children {
        child_indices.push(result.len());
        walk_trace(child, depth + 1, result);
    }
    if let Some(entry) = result.get_mut(idx) {
        entry["children"] = json!(child_indices);
    }
}

/// Flatten a nested callTracer tree into DFS order with depth + child indices.
pub fn flatten_trace(trace: &Value) -> Vec<Value> {
    let mut result = Vec::new();
    match trace {
        Value::Null => return result,
        Value::Array(nodes) => {
            for node in nodes {
                walk_trace(node, 0, &mut result);
            }
        }
        Value::Object(_) => walk_trace(trace, 0, &mut result),
        _ => {}
    }
    result
}

// ---------------------------------------------------------------------------
// Balance changes / calls extraction
// ---------------------------------------------------------------------------

pub const FEE_MANAGER_ADDRESS: &str = "0xfeEC000000000000000000000000000000000000";

pub fn extract_balance_changes(receipt: &Value, tx: &Transaction) -> Vec<Value> {
    let mut changes = Vec::new();
    let logs = receipt
        .get("logs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for log in &logs {
        if let Some(decoded) = decode_event(log) {
            if decoded.name.as_deref() == Some("Transfer") {
                let token = log
                    .get("address")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
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
                changes.push(json!({
                    "address": from,
                    "token": token,
                    "change": format!("-{amount}"),
                    "is_fee": false,
                }));
                changes.push(json!({
                    "address": to,
                    "token": token,
                    "change": format!("+{amount}"),
                    "is_fee": false,
                }));
            }
        }
    }

    if let Some(fee_token) = tx.fee_token.as_deref() {
        let fee_ok = tx.fee_amount.parse::<i64>().map(|n| n > 0).unwrap_or(false);
        if fee_ok {
            for c in changes.iter_mut() {
                let addr = c
                    .get("address")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_lowercase();
                let token = c
                    .get("token")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_lowercase();
                if addr == FEE_MANAGER_ADDRESS.to_lowercase() && token == fee_token.to_lowercase() {
                    c["is_fee"] = json!(true);
                    c["change_type"] = json!("fee");
                }
            }
        }
    }
    changes
}

/// Extract internal calls: use the flattened trace when available, otherwise
/// fall back to the top-level `calls` field of the raw transaction.
pub fn extract_calls(tx: &Transaction, trace: &[Value]) -> Vec<Value> {
    if !trace.is_empty() {
        let mut out = trace.to_vec();
        for call in out.iter_mut() {
            if call.get("input").is_some() && call.get("data").is_none() {
                call["data"] = call["input"].clone();
            }
        }
        return out;
    }

    let raw: Value = tx
        .raw
        .as_deref()
        .and_then(|r| serde_json::from_str(r).ok())
        .unwrap_or_else(|| json!({}));
    let calls = raw
        .get("calls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    calls
        .into_iter()
        .map(|call| {
            let to = call
                .get("to")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let value = call
                .get("value")
                .and_then(Value::as_str)
                .map(|s| parse_decimal_or_hex(s).to_string())
                .unwrap_or_else(|| "0".into());
            let data = call
                .get("data")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| call.get("input").and_then(Value::as_str))
                .unwrap_or("0x")
                .to_string();
            let decoded = decode_function_call(&data).map(|d| d.to_json());
            json!({
                "depth": 0,
                "type": "CALL",
                "to": to,
                "from": tx.from_addr,
                "value": value,
                "data": data,
                "decoded": decoded.unwrap_or(Value::Null),
                "gas": "0",
                "gas_used": "0",
                "children": [],
            })
        })
        .collect()
}

pub fn parse_decimal_or_hex(s: &str) -> i128 {
    if let Some(h) = s.strip_prefix("0x") {
        i128::from_str_radix(h, 16).unwrap_or(0)
    } else {
        s.parse().unwrap_or(0)
    }
}
