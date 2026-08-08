//! Row types shared between SQLite storage and the JSON API.
//!
//! Field names and shapes are the stable wire contract for the JSON API and
//! templates.

use serde::Serialize;

pub type Json = serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct Block {
    pub number: i64,
    pub hash: String,
    pub parent_hash: String,
    pub timestamp: i64,
    pub gas_used: i64,
    pub gas_limit: i64,
    /// Base fee per gas in wei (decimal string, `0` when unavailable).
    pub base_fee: String,
    /// Block size in bytes.
    pub size: i64,
    pub extra_data: String,
    /// Consensus epoch / view from the node's `consensusContext`.
    pub epoch: i64,
    pub view: i64,
    /// Consensus proposer (validator address) from `consensusContext`.
    pub proposer: String,
    pub miner: String,
    pub tx_count: i64,
    pub raw: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Transaction {
    pub hash: String,
    pub block_number: i64,
    pub block_hash: String,
    pub position: i64,
    pub from_addr: String,
    pub to_addr: Option<String>,
    pub status: i64,
    pub gas_limit: i64,
    pub gas_used: i64,
    pub gas_price: String,
    pub max_fee_per_gas: String,
    pub max_priority_fee_per_gas: String,
    pub base_fee: String,
    pub contract_address: Option<String>,
    pub fee_token: Option<String>,
    pub fee_amount: String,
    pub nonce: i64,
    pub nonce_key: Option<String>,
    pub value: String,
    pub chain_id: i64,
    pub tx_type: i64,
    /// First four bytes of the call data (e.g. `0xa9059cbb`).
    pub method_id: String,
    pub input: String,
    pub raw: Option<String>,
    pub trace_data: Option<String>,
    pub receipt_data: Option<String>,
    pub timestamp: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TokenMetadata {
    pub address: String,
    pub name: String,
    pub symbol: String,
    pub decimals: i64,
    pub currency: String,
    pub total_supply: String,
    pub logo_uri: String,
    pub holder_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContractLabel {
    pub address: String,
    pub name: String,
    pub abi: String,
    pub is_token: i64,
    pub is_precompile: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TransferEvent {
    pub id: i64,
    pub tx_hash: String,
    pub block_number: i64,
    pub log_index: i64,
    pub token_addr: String,
    pub from_addr: String,
    pub to_addr: String,
    pub amount: String,
    pub timestamp: i64,
    pub created_at: i64,
}
