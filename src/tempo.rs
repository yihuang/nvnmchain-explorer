//! Re-exports of the tempo alloy-fork types used across the app, so the git
//! dependency is referenced from one place.
//!
//! `tempo-primitives` is the official Tempo SDK (an alloy fork) — it knows
//! how to RLP-encode/decode the typed 0x76 transaction that this chain uses.

pub use tempo_primitives::AASigned;
pub use tempo_primitives::TempoTransaction;
pub use tempo_primitives::TempoTxEnvelope;
