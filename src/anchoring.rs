//! The anchoring precompile: one Merkle Mountain Range per caller, enshrined at
//! T10. It keeps the leaf count and the peaks, so its two append events are the
//! only record of which leaves arrived and what they carried.
//!
//! What a payload *means* is deliberately not here: those shapes track a
//! contract in another repo, so reading them belongs to the decoder that
//! versions with it (`nvnmchain-anchoring`). `decode_envelope` names the fields
//! of the two envelopes a registry commits to and stops there -- a convenience
//! over reading them out of the hex, not a second decoder. A payload is only meaningful with
//! its namespace beside it — one contract per registry, so the same commitment
//! under two namespaces is two different records.
//!
//! That split is why the log is ingested here rather than read back from a
//! general indexer: `metadata` is a dynamic `bytes`, so one decoding it as the
//! head word hands back the ABI offset instead of the payload — and
//! `is_self_verifying` would hash the wrong thing.

use crate::decoder::{decode_abi_args, keccak256, keccak_hex, normalize_hex};

/// The fields of the envelopes a registry commits to, under the `bytes32` tag each
/// leads with.
///
/// The module note above says what a payload *means* is not read here, and it still is
/// not: this only names fields already on the page as one run of hex. What bounds it is
/// the tag -- wire format an indexer matches on, so a registry that grows a new envelope
/// falls through to the raw bytes rather than being decoded as the wrong shape. Which
/// version is current, what a status implies, how versions fold into a record: still the
/// decoder that versions with the contract.
const ENVELOPES: &[(&str, &[(&str, &str)])] = &[
    (
        "record",
        &[
            ("kind", "bytes32"),
            ("checksum_hash", "bytes32"),
            ("index", "uint256"),
            ("uri", "string"),
            ("checksum", "string"),
            ("algo", "string"),
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

/// The tag a payload leads with, as the ASCII an indexer matches on.
fn kind_tag(raw: &[u8]) -> Option<String> {
    let word = raw.get(..32)?;
    let text: Vec<u8> = word.iter().copied().take_while(|b| *b != 0).collect();
    let tag = String::from_utf8(text).ok()?;
    // The rest of the word must be padding, or this is not a tag at all.
    if tag.is_empty() || !word[tag.len()..].iter().all(|b| *b == 0) {
        return None;
    }
    Some(tag)
}

/// The envelope tag a payload leads with, if it is one we declare. Cheap enough for a
/// listing, where naming every field of every row would decode a page of payloads to
/// show one word each.
pub fn envelope_kind(metadata: &str) -> Option<String> {
    let raw = hex::decode(metadata.strip_prefix("0x").unwrap_or(metadata)).ok()?;
    let tag = kind_tag(&raw)?;
    ENVELOPES.iter().find(|(name, _)| *name == tag)?;
    Some(tag)
}

/// One envelope's fields as `(name, value)`, or `None` when the payload does not lead
/// with a tag this knows -- a batch's payload, an empty one, or a shape added since.
pub fn decode_envelope(metadata: &str) -> Option<(String, Vec<(String, String)>)> {
    let raw = hex::decode(metadata.strip_prefix("0x").unwrap_or(metadata)).ok()?;
    let tag = kind_tag(&raw)?;
    let fields = ENVELOPES.iter().find(|(name, _)| *name == tag)?.1;
    let types: Vec<&str> = fields.iter().map(|(_, ty)| *ty).collect();
    let values = decode_abi_args(&types, &raw);
    // `decode_abi_args` hands back nothing at all when the bytes do not fit the types.
    if values.len() != fields.len() {
        return None;
    }
    let named = fields
        .iter()
        .map(|(name, _)| (*name).to_string())
        .zip(values)
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
