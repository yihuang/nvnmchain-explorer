//! The anchoring precompile: a caller-partitioned commitment log enshrined at
//! T10, keeping only the head per `(namespace, key)` — so the `Anchored` log is
//! the only record of history.
//!
//! What a payload *means* is deliberately not here: those shapes track a
//! contract in another repo, so reading them belongs to the decoder that
//! versions with it (`nvnmchain-anchoring`). A payload is only meaningful with
//! its namespace beside it — one contract per registry, so the same commitment
//! under two namespaces is two different records.
//!
//! That split is why this ingests the log itself rather than reading it from a
//! general indexer: `metadata` is a dynamic `bytes`, and an indexer decoding it
//! as the head word hands back the ABI offset instead of the payload.
//! `decoder.rs` dereferences it, so `is_self_verifying` below hashes a payload
//! rather than `0x…40`.

use crate::decoder::keccak_hex;

/// Fixed at genesis (`IAnchoring.sol`).
pub const ANCHORING_ADDRESS: &str = "0x0000000000000000000000000000000000000A00";

pub const ANCHORED_SIGNATURE: &str = "Anchored(address,bytes32,bytes32,bytes)";
/// `keccak256(ANCHORED_SIGNATURE)` — asserted in the tests so it cannot drift.
pub const ANCHORED_TOPIC: &str =
    "0x778db4d46fc7a84c4e5105dcb250cb47092b78648868d3efaf18e1205b25801d";

/// Lowercase `0x…` form, so hex from the chain and hex we derive compare equal.
pub fn normalize_hex(value: &str) -> String {
    let value = value.trim();
    format!(
        "0x{}",
        value.strip_prefix("0x").unwrap_or(value).to_lowercase()
    )
}

/// Whether the commitment is `keccak256(metadata)`, as `anchorAndHash` writes
/// it. The precompile's own guarantee, so it holds whatever the payload means.
pub fn is_self_verifying(commitment: &str, metadata: &str) -> bool {
    let Ok(raw) = hex::decode(metadata.strip_prefix("0x").unwrap_or(metadata)) else {
        return false;
    };
    keccak_hex(&raw) == normalize_hex(commitment)
}
