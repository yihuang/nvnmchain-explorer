//! The anchoring precompile: one Merkle Mountain Range per caller, enshrined at
//! T10. It keeps the leaf count and the peaks, so its two append events are the
//! only record of which leaves arrived and what they carried.
//!
//! What a payload *means* is not read here: those shapes track a contract in
//! another repo, and reading them belongs to the decoder that versions with it
//! (`nvnmchain-anchoring`). `decode_envelope` only names the fields of the two
//! envelopes a registry commits to, so a page shows them rather than one run of
//! hex. A payload is only meaningful with its namespace beside it — one contract
//! per registry, so the same commitment under two namespaces is two different
//! records.
//!
//! That split is why the log is ingested here rather than read back from a
//! general indexer: `metadata` is a dynamic `bytes`, so one decoding it as the
//! head word hands back the ABI offset instead of the payload — and
//! `is_self_verifying` would hash the wrong thing.

use crate::decoder::{decode_abi_args, keccak256, keccak_hex, normalize_hex};

/// The envelopes a registry commits to: the `bytes32` tag each leads with, and the
/// fields behind it, named as `nvnmchain-anchoring` names them. Only the layout: what a
/// field means, and how versions fold into a record, stays with that decoder. A payload
/// leading with a tag not listed here falls through to its raw bytes.
const ENVELOPES: &[(&str, &[(&str, &str)])] = &[
    (
        "record",
        &[
            ("kind", "bytes32"),
            ("checksum_hash", "bytes32"),
            ("index", "uint256"),
            ("uri", "string"),
            ("checksum", "string"),
            ("checksum_algo", "string"),
            ("metadata", "string"),
            ("category", "uint8"),
            ("data_pointer", "string"),
            ("author", "address"),
            ("timestamp", "uint256"),
        ],
    ),
    (
        "status",
        &[
            ("kind", "bytes32"),
            ("checksum_hash", "bytes32"),
            ("index", "uint256"),
            ("status", "string"),
            ("author", "address"),
            ("seq", "uint256"),
        ],
    ),
];

/// The tag `raw` leads with, if it is one of the envelopes': the name, then zeroes to
/// the end of the word.
fn tag_of(raw: &[u8]) -> Option<&'static str> {
    let word = raw.get(..32)?;
    ENVELOPES.iter().map(|(name, _)| *name).find(|name| {
        word.starts_with(name.as_bytes()) && word[name.len()..].iter().all(|b| *b == 0)
    })
}

/// The envelope tag `metadata` leads with, if any. Reads one word, so a listing labels
/// every row without decoding a page of payloads.
pub fn envelope_kind(metadata: &str) -> Option<&'static str> {
    let hexed = metadata.strip_prefix("0x").unwrap_or(metadata);
    tag_of(&hex::decode(hexed.get(..64)?).ok()?)
}

/// An envelope's fields after its tag, named, or `None` for a payload that leads with
/// no tag the table knows: a batch's, an empty one, a shape added since.
pub fn decode_envelope(metadata: &str) -> Option<(&'static str, Vec<(&'static str, String)>)> {
    let raw = hex::decode(metadata.strip_prefix("0x").unwrap_or(metadata)).ok()?;
    let tag = tag_of(&raw)?;
    let fields = ENVELOPES.iter().find(|(name, _)| *name == tag)?.1;
    let types: Vec<&str> = fields.iter().map(|(_, ty)| *ty).collect();
    let values = decode_abi_args(&types, &raw);
    if values.len() != fields.len() {
        return None; // the bytes do not fit the layout
    }
    let named = fields
        .iter()
        .map(|(name, _)| *name)
        .zip(values)
        .skip(1)
        .collect();
    Some((tag, named))
}

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
