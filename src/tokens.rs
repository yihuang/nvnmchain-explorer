//! TIP-20 / ERC-20 token metadata fetching and amount formatting,
//! mirroring `app/tokens.py`.

use serde::Serialize;
use serde_json::json;

use crate::contracts::get_known_token;
use crate::decoder::checksum_address;
use crate::rpc::ChainRpc;

const NAME_CALL: &str = "0x06fdde03";
const SYMBOL_CALL: &str = "0x95d89b41";
const DECIMALS_CALL: &str = "0x313ce567";
const TOTAL_SUPPLY_CALL: &str = "0x18160ddd";

#[derive(Debug, Clone, Serialize)]
pub struct TokenMeta {
    pub address: String,
    pub name: String,
    pub symbol: String,
    pub decimals: i64,
    pub currency: String,
    pub total_supply: String,
}

/// Decode an ABI-encoded `string` return value (e.g. `name()` / `symbol()`).
///
/// A dynamic `string` is encoded with an offset indirection, even when it is
/// short:
///
/// ```text
/// word 0 = byte offset to the length word (0x20 for a single string)
/// word 1 = byte length of the UTF-8 payload
/// word 2+ = payload, right-padded to a 32-byte boundary
/// ```
///
/// A few non-standard contracts return the string in place (length in word 0),
/// so that shape is accepted as a fallback.
pub fn decode_string_result(raw: &str) -> String {
    if raw.is_empty() || raw == "0x" {
        return String::new();
    }
    let bytes = match hex::decode(raw.strip_prefix("0x").unwrap_or(raw)) {
        Ok(b) => b,
        Err(_) => return String::new(),
    };
    if bytes.len() < 32 {
        return String::new();
    }
    let offset = u64::from_be_bytes(bytes[24..32].try_into().unwrap_or([0; 8])) as usize;
    // Standard dynamic-string encoding: the first word is an offset into the
    // buffer pointing at the length word, followed by the payload.
    if offset >= 32 && offset + 32 <= bytes.len() {
        let len = u64::from_be_bytes(bytes[offset + 24..offset + 32].try_into().unwrap_or([0; 8]))
            as usize;
        let start = offset + 32;
        let end = (start + len).min(bytes.len());
        return String::from_utf8_lossy(&bytes[start..end]).into_owned();
    }
    // Fallback: short string encoded in place (length in word 0).
    let len = offset.min(bytes.len().saturating_sub(32));
    String::from_utf8_lossy(&bytes[32..32 + len]).into_owned()
}

fn decode_uint_result(raw: &str, default: i64) -> i64 {
    if raw.is_empty() || raw == "0x" {
        return default;
    }
    let bytes = match hex::decode(raw.strip_prefix("0x").unwrap_or(raw)) {
        Ok(b) => b,
        Err(_) => return default,
    };
    if bytes.len() < 32 {
        return default;
    }
    u64::from_be_bytes(bytes[24..32].try_into().unwrap_or([0; 8])) as i64
}

fn decode_uint256_result(raw: &str) -> String {
    if raw.is_empty() || raw == "0x" {
        return "0".into();
    }
    let bytes = match hex::decode(raw.strip_prefix("0x").unwrap_or(raw)) {
        Ok(b) => b,
        Err(_) => return "0".into(),
    };
    if bytes.len() < 32 {
        return "0".into();
    }
    num_bigint::BigInt::from_bytes_be(num_bigint::Sign::Plus, &bytes).to_string()
}

/// Clean a decoded `name()`/`symbol()` string. Real token metadata is
/// printable text; a mis-decoded ABI value (e.g. a length word read as the
/// payload) is full of NUL/control characters and should degrade to the
/// empty string rather than render as garbage.
pub fn sanitize_metadata_text(s: &str) -> String {
    let trimmed = s.trim_end_matches('\0');
    if trimmed.chars().any(|c| c.is_control()) {
        String::new()
    } else {
        trimmed.trim().to_string()
    }
}

/// Whether a stored name/symbol carries control characters — the signature of
/// a value written by the pre-fix string decoder — and therefore needs a
/// re-fetch.
pub fn has_control_chars(s: &str) -> bool {
    s.chars().any(|c| c.is_control())
}

/// Fetch TIP-20 token metadata from the chain, tolerating missing views.
pub async fn fetch_token_metadata(rpc: &ChainRpc, address: &str) -> TokenMeta {
    let checksummed = checksum_address(address);
    let known = get_known_token(&checksummed);

    let name = match &known {
        Some(k) => k.name.clone(),
        None => {
            let raw = rpc
                .eth_call(&checksummed, NAME_CALL, "latest")
                .await
                .unwrap_or_default();
            decode_string_result(&raw)
        }
    };
    let symbol = match &known {
        Some(k) => k.symbol.clone(),
        None => {
            let raw = rpc
                .eth_call(&checksummed, SYMBOL_CALL, "latest")
                .await
                .unwrap_or_default();
            decode_string_result(&raw)
        }
    };
    // Guard against stale/bad decodes: a mis-decoded ABI word must not be
    // stored or rendered as a name/symbol.
    let name = sanitize_metadata_text(&name);
    let symbol = sanitize_metadata_text(&symbol);
    let decimals_raw = rpc
        .eth_call(&checksummed, DECIMALS_CALL, "latest")
        .await
        .unwrap_or_default();
    let decimals = decode_uint_result(&decimals_raw, 18);
    let supply_raw = rpc
        .eth_call(&checksummed, TOTAL_SUPPLY_CALL, "latest")
        .await
        .unwrap_or_default();
    let total_supply = decode_uint256_result(&supply_raw);

    let currency = if let Some(k) = known {
        if k.currency.is_empty() {
            infer_currency(&symbol)
        } else {
            k.currency.clone()
        }
    } else if symbol.is_empty() {
        String::new()
    } else {
        infer_currency(&symbol)
    };

    TokenMeta {
        address: checksummed,
        name,
        symbol,
        decimals,
        currency,
        total_supply,
    }
}

fn infer_currency(symbol: &str) -> String {
    let upper = symbol.to_uppercase();
    if upper.ends_with("USD") || matches!(upper.as_str(), "USDC" | "USDT" | "DAI" | "FRAX") {
        "USD".into()
    } else if upper.ends_with("EUR") {
        "EUR".into()
    } else {
        symbol.to_string()
    }
}

/// Format a token amount with up to six significant decimal places.
pub fn format_token_amount(amount: &str, decimals: i64) -> String {
    let amount = num_bigint::BigInt::parse_bytes(amount.as_bytes(), 10)
        .unwrap_or_else(|| num_bigint::BigInt::from(0));
    if amount.sign() == num_bigint::Sign::NoSign {
        return "0".into();
    }
    let decimals = decimals.clamp(0, 30) as u32;
    let divisor = num_bigint::BigInt::from(10u8).pow(decimals);
    if divisor.sign() == num_bigint::Sign::NoSign {
        return amount.to_string();
    }
    let integer_part = &amount / &divisor;
    let fractional_part = &amount % &divisor;
    if fractional_part.sign() == num_bigint::Sign::NoSign {
        return integer_part.to_string();
    }
    let mut frac = if fractional_part.sign() == num_bigint::Sign::Minus {
        (-&fractional_part).to_string()
    } else {
        fractional_part.to_string()
    };
    if (frac.len() as u32) < decimals {
        frac = format!("{}{}", "0".repeat(decimals as usize - frac.len()), frac);
    }
    while frac.ends_with('0') {
        frac.pop();
    }
    if frac.is_empty() {
        return integer_part.to_string();
    }
    if frac.len() > 6 {
        frac.truncate(6);
    }
    format!("{integer_part}.{frac}")
}

pub fn format_token_amount_with_symbol(amount: &str, decimals: i64, symbol: &str) -> String {
    let formatted = format_token_amount(amount, decimals);
    if symbol.is_empty() {
        formatted
    } else {
        format!("{formatted} {symbol}")
    }
}

/// Convenience JSON wrapper for the token page.
pub fn token_to_json(meta: &TokenMeta) -> serde_json::Value {
    json!(meta)
}
