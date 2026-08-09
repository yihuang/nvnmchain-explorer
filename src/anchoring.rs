//! The anchoring precompile: address, event topic, and the `AnchoringRegistry`
//! envelopes anchored through it.
//!
//! A caller-partitioned commitment log enshrined at T10, keeping only the head
//! per `(namespace, key)` — so the `Anchored` log is the only record of history.
//!
//! Here rather than in the generic decoder: anchoring is this chain's
//! precompile, not part of upstream Tempo.

use num_bigint::{BigInt, Sign};
use serde::Serialize;

use crate::decoder::{checksum_address, keccak_hex};

/// Fixed at genesis (`IAnchoring.sol`).
pub const ANCHORING_ADDRESS: &str = "0x0000000000000000000000000000000000000A00";

pub const ANCHORED_SIGNATURE: &str = "Anchored(address,bytes32,bytes32,bytes)";
/// `keccak256(ANCHORED_SIGNATURE)` — asserted in the tests so it cannot drift.
pub const ANCHORED_TOPIC: &str =
    "0x778db4d46fc7a84c4e5105dcb250cb47092b78648868d3efaf18e1205b25801d";

// ---------------------------------------------------------------------------
// Metadata envelopes
// ---------------------------------------------------------------------------
//
// Metadata is opaque bytes to the precompile, so what a payload means is the
// writer's business. `AnchoringRegistry` anchors three shapes, each a plain
// `abi.encode` of its fields with nothing to identify it by — but the *key* it
// is anchored under is `keccak256(abi.encode(kind, ids…))` over ids that the
// payload itself carries. Re-deriving that key from the decoded payload both
// identifies the envelope and proves the reading is right: a wrong shape
// yields a different hash. Anything else anchored under the precompile decodes
// to nothing, which is the ordinary outcome, not an error.

#[derive(Debug, Clone, Copy, PartialEq)]
enum Ty {
    Uint,
    Address,
    Str,
}

struct Schema {
    kind: &'static str,
    fields: &'static [(&'static str, Ty)],
    /// Head-word positions of the ids that make up the anchored key, in the
    /// order `AnchoringRegistry` hashes them.
    key_ids: &'static [usize],
}

/// Each schema is one `abi.encode` call in `AnchoringRegistry.sol`, paired with
/// the `*Key()` helper naming the slot it is anchored at. ACL changes are
/// deliberately absent: role history lives in the registry's own events, not in
/// the anchoring log.
const SCHEMAS: &[Schema] = &[
    Schema {
        // addRegistry → registryKey(id)
        kind: "registry",
        fields: &[
            ("id", Ty::Uint),
            ("name", Ty::Str),
            ("description", Ty::Str),
            ("metadata", Ty::Str),
            ("creator", Ty::Address),
            ("timestamp", Ty::Uint),
        ],
        key_ids: &[0],
    },
    Schema {
        // addRecord → recordKey(registry_id, record_id)
        kind: "record",
        fields: &[
            ("registry_id", Ty::Uint),
            ("record_id", Ty::Uint),
            ("index", Ty::Uint),
            ("uri", Ty::Str),
            ("checksum", Ty::Str),
            ("checksum_algo", Ty::Str),
            ("metadata", Ty::Str),
            ("timestamp", Ty::Uint),
        ],
        key_ids: &[0, 1],
    },
    Schema {
        // updateRecordStatus → statusKey(registry_id, record_id, index)
        kind: "status",
        fields: &[
            ("registry_id", Ty::Uint),
            ("record_id", Ty::Uint),
            ("index", Ty::Uint),
            ("status", Ty::Str),
            ("seq", Ty::Uint),
        ],
        key_ids: &[0, 1, 2],
    },
];

#[derive(Debug, Clone, Serialize)]
pub struct EnvelopeField {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Envelope {
    /// `registry`, `record` or `status`.
    pub kind: String,
    /// One-line description, for listings.
    pub summary: String,
    pub fields: Vec<EnvelopeField>,
}

impl Envelope {
    fn field(&self, name: &str) -> &str {
        self.fields
            .iter()
            .find(|f| f.name == name)
            .map(|f| f.value.as_str())
            .unwrap_or("")
    }
}

/// Decode the `AnchoringRegistry` envelope anchored at `key`, or `None` when
/// `metadata` is not one (anything may be anchored).
pub fn decode_envelope(key: &str, metadata: &str) -> Option<Envelope> {
    let raw = hex::decode(metadata.strip_prefix("0x").unwrap_or(metadata)).ok()?;
    let key = normalize_hex(key);
    for schema in SCHEMAS {
        let Some((values, words)) = decode_strict(schema.fields, &raw) else {
            continue;
        };
        let ids: Vec<[u8; 32]> = schema.key_ids.iter().map(|i| words[*i]).collect();
        if derive_key(schema.kind, &ids) != key {
            continue;
        }
        let fields = schema
            .fields
            .iter()
            .zip(&values)
            .map(|((name, _), value)| EnvelopeField {
                name: (*name).to_string(),
                value: value.clone(),
            })
            .collect();
        let mut envelope = Envelope {
            kind: schema.kind.to_string(),
            summary: String::new(),
            fields,
        };
        envelope.summary = summarize(&envelope);
        return Some(envelope);
    }
    None
}

fn summarize(envelope: &Envelope) -> String {
    match envelope.kind.as_str() {
        "registry" => format!(
            "Registry #{} — {}",
            envelope.field("id"),
            envelope.field("name")
        ),
        "record" => format!(
            "Record #{} v{} — {}",
            envelope.field("record_id"),
            envelope.field("index"),
            envelope.field("checksum")
        ),
        "status" => format!(
            "Status of record #{} v{} — {}",
            envelope.field("record_id"),
            envelope.field("index"),
            envelope.field("status")
        ),
        other => other.to_string(),
    }
}

/// `keccak256(abi.encode(kind, ids…))` — the key `AnchoringRegistry` anchors
/// this envelope under. The ids are reused as their raw words, so no re-encoding
/// can misrepresent them.
fn derive_key(kind: &str, ids: &[[u8; 32]]) -> String {
    let mut encoded = Vec::with_capacity(32 * (ids.len() + 3));
    // Head: the offset of the dynamic `kind` string, then the static ids.
    encoded.extend_from_slice(&usize_word(32 * (1 + ids.len())));
    for id in ids {
        encoded.extend_from_slice(id);
    }
    // Tail: the string's length and its right-padded bytes (all kinds are short).
    encoded.extend_from_slice(&usize_word(kind.len()));
    let mut padded = [0u8; 32];
    padded[..kind.len()].copy_from_slice(kind.as_bytes());
    encoded.extend_from_slice(&padded);
    keccak_hex(&encoded)
}

/// Decode a fixed field list out of `data`, accepting only the exact layout
/// `abi.encode` produces for it: head words in order, each dynamic tail packed
/// immediately after the last, and nothing left over. That tightness is what
/// keeps a foreign payload from being read as an envelope — the general decoder
/// in `decoder.rs` is deliberately lenient instead.
fn decode_strict(fields: &[(&str, Ty)], data: &[u8]) -> Option<(Vec<String>, Vec<[u8; 32]>)> {
    if !data.len().is_multiple_of(32) {
        return None;
    }
    let head_len = fields.len() * 32;
    if data.len() < head_len {
        return None;
    }
    let mut values = Vec::with_capacity(fields.len());
    let mut words = Vec::with_capacity(fields.len());
    // Where the next dynamic value has to start; tails follow the head in field
    // order, so this is the only offset a canonical encoding can name.
    let mut tail = head_len;
    for (i, (_, ty)) in fields.iter().enumerate() {
        let word: [u8; 32] = data[i * 32..(i + 1) * 32].try_into().ok()?;
        words.push(word);
        match ty {
            Ty::Uint => values.push(BigInt::from_bytes_be(Sign::Plus, &word).to_string()),
            Ty::Address => {
                // The 12 high bytes of an ABI-encoded address are zero.
                if word[..12].iter().any(|b| *b != 0) {
                    return None;
                }
                values.push(checksum_address(&hex::encode(&word[12..])));
            }
            Ty::Str => {
                if word_to_usize(&word)? != tail {
                    return None;
                }
                let len = word_to_usize(data.get(tail..tail + 32)?.try_into().ok()?)?;
                let start = tail + 32;
                let end = start.checked_add(len)?;
                if end > data.len() {
                    return None;
                }
                values.push(String::from_utf8_lossy(&data[start..end]).into_owned());
                tail = start.checked_add(len.next_multiple_of(32))?;
            }
        }
    }
    // Slack after the last tail means these are not the fields that were encoded.
    if tail != data.len() {
        return None;
    }
    Some((values, words))
}

/// A 32-byte word as a length/offset, or `None` when it is too large to be one.
fn word_to_usize(word: &[u8; 32]) -> Option<usize> {
    if word[..24].iter().any(|b| *b != 0) {
        return None;
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&word[24..]);
    usize::try_from(u64::from_be_bytes(buf)).ok()
}

fn usize_word(value: usize) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[24..].copy_from_slice(&(value as u64).to_be_bytes());
    word
}

/// Lowercase `0x…` form, so hex from the chain and hex we derive compare equal.
pub fn normalize_hex(value: &str) -> String {
    format!(
        "0x{}",
        value
            .trim()
            .strip_prefix("0x")
            .unwrap_or(value)
            .to_lowercase()
    )
}

/// Whether the commitment is `keccak256(metadata)` — what `anchorAndHash`
/// writes, making the logged payload self-verifying.
pub fn is_self_verifying(commitment: &str, metadata: &str) -> bool {
    let Ok(raw) = hex::decode(metadata.strip_prefix("0x").unwrap_or(metadata)) else {
        return false;
    };
    keccak_hex(&raw) == normalize_hex(commitment)
}
