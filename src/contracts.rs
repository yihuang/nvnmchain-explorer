//! Contract identification and labels, mirroring `app/contracts.py`.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::OnceLock;

use crate::decoder::checksum_address;

fn precompile_labels() -> &'static HashMap<String, String> {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| {
        let pairs = [
            ("0xfeEC000000000000000000000000000000000000", "Fee Manager"),
            (
                "0x403c000000000000000000000000000000000000",
                "TIP-403 Registry",
            ),
            (
                "0x20Fc000000000000000000000000000000000000",
                "TIP-20 Factory",
            ),
            (
                "0x4D50500000000000000000000000000000000000",
                "TIP-20 Channel Reserve",
            ),
            (
                "0xDEc0000000000000000000000000000000000000",
                "Stablecoin DEX",
            ),
            (
                "0x4e4F4E4345000000000000000000000000000000",
                "Nonce Manager",
            ),
            (
                "0xCccCcCCC00000000000000000000000000000000",
                "Validator Config",
            ),
            (
                "0xaAAAaaAA00000000000000000000000000000000",
                "Account Keychain",
            ),
            (
                "0xFDC0000000000000000000000000000000000000",
                "Address Registry",
            ),
            (
                "0x5165300000000000000000000000000000000000",
                "Signature Verifier",
            ),
            (
                "0xB10C000000000000000000000000000000000000",
                "Receive Policy Guard",
            ),
            (
                "0x1060000000000000000000000000000000000000",
                "Storage Credits",
            ),
        ];
        pairs
            .iter()
            .map(|(a, label)| (checksum_address(a), label.to_string()))
            .collect()
    })
}

fn known_tokens() -> &'static HashMap<String, TokenInfo> {
    static MAP: OnceLock<HashMap<String, TokenInfo>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map = HashMap::new();
        map.insert(
            checksum_address("0x20C0000000000000000000000000000000000000"),
            TokenInfo {
                name: "pathUSD".into(),
                symbol: "pathUSD".into(),
                currency: "USD".into(),
            },
        );
        map.insert(
            checksum_address("0x20C0000000000000000000000000000000000001"),
            TokenInfo {
                name: "Alpha USD".into(),
                symbol: "ALPHA".into(),
                currency: "USD".into(),
            },
        );
        map.insert(
            checksum_address("0x20C0000000000000000000000000000000000002"),
            TokenInfo {
                name: "Beta USD".into(),
                symbol: "BETA".into(),
                currency: "USD".into(),
            },
        );
        map.insert(
            checksum_address("0x20C0000000000000000000000000000000000003"),
            TokenInfo {
                name: "Theta USD".into(),
                symbol: "THETA".into(),
                currency: "USD".into(),
            },
        );
        map
    })
}

#[derive(Debug, Clone)]
pub struct TokenInfo {
    pub name: String,
    pub symbol: String,
    pub currency: String,
}

/// TIP-20 native token prefix: all StdTokens start with 0x20C000...
pub const TIP20_TOKEN_PREFIX: &str = "0x20c0000000000000000";

pub fn is_precompile_address(addr: &str) -> bool {
    precompile_labels().contains_key(&checksum_address(addr))
}

pub fn get_precompile_name(addr: &str) -> Option<String> {
    precompile_labels().get(&checksum_address(addr)).cloned()
}

pub fn is_tip20_token(addr: &str) -> bool {
    addr.trim().to_lowercase().starts_with(TIP20_TOKEN_PREFIX)
}

pub fn is_contract(addr: &str) -> bool {
    let checksummed = checksum_address(addr);
    precompile_labels().contains_key(&checksummed) || is_tip20_token(addr)
}

pub fn get_contract_name(addr: &str) -> Option<String> {
    let checksummed = checksum_address(addr);
    if let Some(label) = precompile_labels().get(&checksummed) {
        return Some(label.clone());
    }
    if let Some(info) = known_tokens().get(&checksummed) {
        return Some(info.name.clone());
    }
    None
}

pub fn is_eoa(addr: &str) -> bool {
    !is_contract(addr)
}

#[derive(Debug, Serialize)]
pub struct AddressInfo {
    #[serde(rename = "type")]
    pub kind: String,
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
}

pub fn identify_address(addr: &str) -> AddressInfo {
    let checksummed = checksum_address(addr);
    if let Some(label) = precompile_labels().get(&checksummed) {
        return AddressInfo {
            kind: "precompile".into(),
            label: Some(label.clone()),
            symbol: None,
        };
    }
    if let Some(info) = known_tokens().get(&checksummed) {
        return AddressInfo {
            kind: "token".into(),
            label: Some(info.name.clone()),
            symbol: Some(info.symbol.clone()),
        };
    }
    if is_tip20_token(&checksummed) {
        return AddressInfo {
            kind: "token".into(),
            label: None,
            symbol: None,
        };
    }
    AddressInfo {
        kind: "eoa".into(),
        label: None,
        symbol: None,
    }
}

pub fn get_known_token(address: &str) -> Option<TokenInfo> {
    known_tokens().get(&checksum_address(address)).cloned()
}
