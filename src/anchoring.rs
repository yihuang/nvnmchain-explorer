//! The anchoring precompile: one Merkle Mountain Range per caller, enshrined at
//! T10. It keeps the leaf count and the peaks, so its two append events are the
//! only record of which leaves arrived and what they carried.
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

use crate::decoder::{keccak256, keccak_hex, normalize_hex};

/// Fixed at genesis (`IAnchoring.sol`).
pub const ANCHORING_ADDRESS: &str = "0x0000000000000000000000000000000000000A00";

/// Whether the commitment is `keccak256(metadata)`, as a registry's record
/// leaves are: the envelope rides along as the leaf's payload, so the log
/// carries the preimage of what the leaf committed to.
///
/// Only a single leaf can be checked this way. A batch reaches the chain as the
/// roots of subtrees, and no row of it is logged on its own.
pub fn is_self_verifying(commitment: &str, metadata: &str) -> bool {
    let Ok(raw) = hex::decode(metadata.strip_prefix("0x").unwrap_or(metadata)) else {
        return false;
    };
    keccak_hex(&raw) == normalize_hex(commitment)
}

/// The root: the peaks bagged highest first, `keccak256("bag" ‖ acc ‖ peak)`,
/// as the precompile derives it. An append event carries the peaks and not the
/// root, which is this one fold away; no peaks is the empty tree's zero root.
pub fn bag(peaks: &[[u8; 32]]) -> String {
    let Some((first, rest)) = peaks.split_first() else {
        return format!("0x{}", "00".repeat(32));
    };
    let root = rest.iter().fold(*first, |acc, peak| {
        let mut preimage = Vec::with_capacity(3 + 64);
        preimage.extend_from_slice(b"bag");
        preimage.extend_from_slice(&acc);
        preimage.extend_from_slice(peak);
        keccak256(&preimage)
    });
    format!("0x{}", hex::encode(root))
}
