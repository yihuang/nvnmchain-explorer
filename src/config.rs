//! Runtime configuration, mirroring `app/config.py`.
//!
//! Defaults target the chain we validate against; override with env vars.

use std::env;

pub const DEFAULT_RPC_URL: &str = "https://rpc.nvnm.canary.mantrachain.dev";
/// Chain id reported by the RPC above (`eth_chainId` → 0xc0316).
pub const DEFAULT_CHAIN_ID: u64 = 787_222;

#[derive(Debug, Clone)]
pub struct Settings {
    pub rpc_url: String,
    pub chain_id: u64,
    pub host: String,
    pub port: u16,
    pub db_path: String,
    pub max_cached_blocks: u64,
    pub recent_block_count: usize,
    pub recent_tx_count: usize,
    /// Seconds between indexer poll cycles.
    pub poll_seconds: f64,
    /// Blocks indexed per poll cycle (forward and backfill each).
    pub batch_size: u64,
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Read `NVNM_RPC`, falling back to the legacy `TEMPO_RPC` variable.
fn rpc_url() -> String {
    env::var("NVNM_RPC")
        .or_else(|_| env::var("TEMPO_RPC"))
        .unwrap_or_else(|_| DEFAULT_RPC_URL.to_string())
}

fn env_u64(key: &str, default: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn env_f64(key: &str, default: f64) -> f64 {
    env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

impl Settings {
    pub fn from_env() -> Self {
        Self {
            rpc_url: rpc_url(),
            chain_id: env_u64("CHAIN_ID", DEFAULT_CHAIN_ID),
            host: env_or("HOST", "0.0.0.0"),
            port: env_u64("PORT", 8080) as u16,
            db_path: env_or("DB_PATH", "explorer.db"),
            max_cached_blocks: env_u64("MAX_CACHED_BLOCKS", 100_000),
            recent_block_count: env_usize("RECENT_BLOCK_COUNT", 15),
            recent_tx_count: env_usize("RECENT_TX_COUNT", 15),
            poll_seconds: env_f64("INDEX_POLL_SECONDS", 3.0),
            batch_size: env_u64("INDEX_BATCH", 5),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self::from_env()
    }
}
