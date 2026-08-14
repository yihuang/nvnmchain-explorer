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
//! That split is why the log is ingested here rather than read back from a
//! general indexer: `metadata` is a dynamic `bytes`, so one decoding it as the
//! head word hands back the ABI offset instead of the payload — and
//! `is_self_verifying` would hash the wrong thing.

use crate::decoder::{keccak_hex, normalize_hex};

/// Fixed at genesis (`IAnchoring.sol`).
pub const ANCHORING_ADDRESS: &str = "0x0000000000000000000000000000000000000A00";

/// Whether the commitment is `keccak256(metadata)`, as `anchorAndHash` writes
/// it. The precompile's own guarantee, so it holds whatever the payload means.
pub fn is_self_verifying(commitment: &str, metadata: &str) -> bool {
    let Ok(raw) = hex::decode(metadata.strip_prefix("0x").unwrap_or(metadata)) else {
        return false;
    };
    keccak_hex(&raw) == normalize_hex(commitment)
}
