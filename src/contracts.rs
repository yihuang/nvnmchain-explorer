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
                "Validator Config V1 (legacy)",
            ),
            (
                "0xCccCcCCC00000000000000000000000000000001",
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
            (crate::anchoring::ANCHORING_ADDRESS, "Anchoring"),
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

/// Which built-in ABIs describe the contract at `addr`, by the names the
/// decoder's registry loads them under. Some addresses need more than one:
/// the fee manager's interface says nothing about the AMM it runs.
pub fn abis_for_address(addr: &str) -> &'static [&'static str] {
    /// Addresses spelled lowercase, so the table cannot disagree with EIP-55
    /// over a character. Lookups normalize the same way.
    const BY_ADDRESS: &[(&str, &[&str])] = &[
        (
            "0xfeec000000000000000000000000000000000000",
            &["fee_manager", "fee_amm"],
        ),
        (
            "0x403c000000000000000000000000000000000000",
            &["tip403_registry"],
        ),
        (
            "0x20fc000000000000000000000000000000000000",
            &["tip20_factory"],
        ),
        (
            "0x4d50500000000000000000000000000000000000",
            &["tip20_channel_reserve"],
        ),
        (
            "0xdec0000000000000000000000000000000000000",
            &["stablecoin_dex"],
        ),
        ("0x4e4f4e4345000000000000000000000000000000", &["nonce"]),
        (
            "0xcccccccc00000000000000000000000000000000",
            &["validator_config"],
        ),
        (
            "0xcccccccc00000000000000000000000000000001",
            &["validator_config_v2"],
        ),
        (
            "0xaaaaaaaa00000000000000000000000000000000",
            &["account_keychain"],
        ),
        (
            "0xfdc0000000000000000000000000000000000000",
            &["address_registry"],
        ),
        (
            "0x5165300000000000000000000000000000000000",
            &["signature_verifier"],
        ),
        (
            "0xb10c000000000000000000000000000000000000",
            &["receive_policy_guard"],
        ),
        (
            "0x1060000000000000000000000000000000000000",
            &["storage_credits"],
        ),
    ];

    let lowered = addr.trim().to_lowercase();
    if let Some((_, abis)) = BY_ADDRESS.iter().find(|(a, _)| *a == lowered) {
        return abis;
    }
    if lowered == crate::anchoring::ANCHORING_ADDRESS.to_lowercase() {
        return &["local"];
    }
    // Every TIP-20 is the same interface at a different address.
    if is_tip20_token(&lowered) {
        return &["tip20", "tip20_roles_auth"];
    }
    &[]
}

/// Precompiles whose name contains `query`, as `(address, name)` pairs.
/// Sorted by match quality then alphabetically, so the same query always
/// produces the same list — a hash map's order is not one.
pub fn search_precompiles(query: &str, limit: usize) -> Vec<(String, String)> {
    let query = query.trim().to_lowercase();
    if query.len() < 2 {
        return Vec::new();
    }
    let mut matches: Vec<(u8, String, String)> = precompile_labels()
        .iter()
        .filter_map(|(address, name)| {
            let lowered = name.to_lowercase();
            let rank = if lowered == query {
                0
            } else if lowered.starts_with(&query) {
                1
            } else if lowered.contains(&query) {
                2
            } else {
                return None;
            };
            Some((rank, name.clone(), address.clone()))
        })
        .collect();
    matches.sort();
    matches.truncate(limit);
    matches
        .into_iter()
        .map(|(_, name, address)| (address, name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every address the ABI table names must also be a labelled precompile,
    /// and every ABI it names must be one the registry actually loads.
    #[test]
    fn the_abi_table_agrees_with_the_label_table() {
        for label in precompile_labels().keys() {
            let abis = abis_for_address(label);
            assert!(
                !abis.is_empty(),
                "{label} is labelled but has no ABI to show"
            );
            for name in abis {
                assert!(
                    crate::decoder::REGISTRY.contract(name).is_some(),
                    "{label} names `{name}`, which the registry does not load"
                );
            }
        }
    }

    /// The lookup must not care how an address is spelled.
    #[test]
    fn abis_are_found_however_the_address_is_spelled() {
        let fee_manager = "0xfeEC000000000000000000000000000000000000";
        assert_eq!(abis_for_address(fee_manager), ["fee_manager", "fee_amm"]);
        assert_eq!(
            abis_for_address(&fee_manager.to_lowercase()),
            ["fee_manager", "fee_amm"]
        );
        assert_eq!(
            abis_for_address(&fee_manager.to_uppercase().replace("0X", "0x")),
            ["fee_manager", "fee_amm"]
        );
    }

    /// A TIP-20 is recognised by its prefix, not by an entry per token.
    #[test]
    fn every_tip20_gets_the_token_interface() {
        assert_eq!(
            abis_for_address("0x20c0000000000000000000000000000000000042"),
            ["tip20", "tip20_roles_auth"]
        );
        assert!(abis_for_address("0x1111111111111111111111111111111111111111").is_empty());
    }
}
