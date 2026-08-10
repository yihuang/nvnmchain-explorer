//! SQLite storage layer, mirroring `app/database.py` + `app/models.py`.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::types::ValueRef;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::models::{AnchoredEvent, Block, TokenMetadata, Transaction, TransferEvent};

pub type Db = Arc<Mutex<Connection>>;

/// Decode a `0x`-prefixed hex string into raw bytes for BLOB storage (hashes
/// and addresses are stored binary — half the TEXT size, and the hash/address
/// indexes get the same reduction). Falls back to the raw string bytes for
/// non-hex values (these columns always hold hashes/addresses, so this is
/// purely defensive).
fn hex_blob(s: &str) -> Vec<u8> {
    let hexed = s.strip_prefix("0x").unwrap_or(s);
    match hex::decode(hexed) {
        Ok(b) => b,
        Err(_) => s.as_bytes().to_vec(),
    }
}

/// Encode stored blob bytes back into a `0x`-prefixed hex string.
fn blob_hex(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

pub fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A per-row write through the connection's statement cache. The block path
/// runs these once per row inside one transaction, where `Connection::execute`
/// would re-prepare the same SQL every time.
fn exec_cached(conn: &Connection, sql: &str, params: impl rusqlite::Params) -> Result<usize> {
    Ok(conn.prepare_cached(sql)?.execute(params)?)
}

pub fn init_db(path: &str) -> Result<Connection> {
    let conn = Connection::open(path).with_context(|| format!("open db {path}"))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS blocks (
            number INTEGER PRIMARY KEY,
            hash BLOB NOT NULL UNIQUE,
            parent_hash BLOB NOT NULL,
            timestamp INTEGER NOT NULL,
            timestamp_ms INTEGER NOT NULL DEFAULT 0,
            gas_used INTEGER NOT NULL DEFAULT 0,
            gas_limit INTEGER NOT NULL DEFAULT 0,
            miner BLOB NOT NULL DEFAULT X'',
            tx_count INTEGER NOT NULL DEFAULT 0,
            base_fee TEXT NOT NULL DEFAULT '0',
            size INTEGER NOT NULL DEFAULT 0,
            extra_data TEXT NOT NULL DEFAULT '',
            epoch INTEGER NOT NULL DEFAULT 0,
            view INTEGER NOT NULL DEFAULT 0,
            proposer BLOB NOT NULL DEFAULT X'',
            created_at INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_blocks_timestamp ON blocks(timestamp);

        CREATE TABLE IF NOT EXISTS transactions (
            hash BLOB PRIMARY KEY,
            block_number INTEGER NOT NULL,
            position INTEGER NOT NULL DEFAULT 0,
            from_addr BLOB NOT NULL,
            to_addr BLOB,
            status INTEGER NOT NULL DEFAULT 1,
            gas_used INTEGER NOT NULL DEFAULT 0,
            base_fee TEXT NOT NULL DEFAULT '0',
            contract_address BLOB,
            fee_token BLOB,
            fee_amount TEXT NOT NULL DEFAULT '0',
            input TEXT NOT NULL DEFAULT '0x',
            raw BLOB,
            trace_data TEXT,
            receipt_data TEXT,
            timestamp INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_tx_block_number ON transactions(block_number);
        CREATE INDEX IF NOT EXISTS idx_tx_from ON transactions(from_addr);
        CREATE INDEX IF NOT EXISTS idx_tx_to ON transactions(to_addr);
        CREATE INDEX IF NOT EXISTS idx_tx_timestamp ON transactions(timestamp);

        CREATE TABLE IF NOT EXISTS token_metadata (
            address BLOB PRIMARY KEY,
            name TEXT NOT NULL DEFAULT '',
            symbol TEXT NOT NULL DEFAULT '',
            decimals INTEGER NOT NULL DEFAULT 18,
            currency TEXT NOT NULL DEFAULT '',
            total_supply TEXT NOT NULL DEFAULT '0',
            logo_uri TEXT NOT NULL DEFAULT '',
            holder_count INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_token_name ON token_metadata(name);

        CREATE TABLE IF NOT EXISTS contract_labels (
            address BLOB PRIMARY KEY,
            name TEXT NOT NULL DEFAULT '',
            abi TEXT NOT NULL DEFAULT '[]',
            is_token INTEGER NOT NULL DEFAULT 0,
            is_precompile INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS transfer_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            tx_hash BLOB NOT NULL,
            block_number INTEGER NOT NULL,
            log_index INTEGER NOT NULL DEFAULT 0,
            token_addr BLOB NOT NULL,
            from_addr BLOB NOT NULL,
            to_addr BLOB NOT NULL,
            amount TEXT NOT NULL,
            timestamp INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL DEFAULT 0,
            UNIQUE (block_number, log_index)
        );
        CREATE INDEX IF NOT EXISTS idx_transfer_tx_hash ON transfer_events(tx_hash);
        CREATE INDEX IF NOT EXISTS idx_transfer_block ON transfer_events(block_number);
        CREATE INDEX IF NOT EXISTS idx_transfer_token ON transfer_events(token_addr);
        CREATE INDEX IF NOT EXISTS idx_transfer_from ON transfer_events(from_addr);
        CREATE INDEX IF NOT EXISTS idx_transfer_to ON transfer_events(to_addr);

        CREATE TABLE IF NOT EXISTS anchored_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            tx_hash BLOB NOT NULL,
            block_number INTEGER NOT NULL,
            log_index INTEGER NOT NULL DEFAULT 0,
            namespace BLOB NOT NULL,
            key BLOB NOT NULL,
            commitment BLOB NOT NULL,
            metadata BLOB NOT NULL DEFAULT X'',
            timestamp INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL DEFAULT 0,
            -- Re-indexing a block must not duplicate its logs.
            UNIQUE (block_number, log_index)
        );
        CREATE INDEX IF NOT EXISTS idx_anchored_tx_hash ON anchored_events(tx_hash);
        -- The head lookup and the per-key history both walk this.
        CREATE INDEX IF NOT EXISTS idx_anchored_ns_key
            ON anchored_events(namespace, key, block_number, log_index);

        -- Per-namespace stats for /anchoring, like token_metadata.holder_count:
        -- maintained with each inserted anchor and recomputed at startup, so no
        -- page view aggregates over every anchor ever written.
        CREATE TABLE IF NOT EXISTS anchored_namespaces (
            namespace BLOB PRIMARY KEY,
            anchor_count INTEGER NOT NULL DEFAULT 0,
            key_count INTEGER NOT NULL DEFAULT 0,
            last_block INTEGER NOT NULL DEFAULT 0,
            last_timestamp INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS kv (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL DEFAULT '',
            updated_at INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS token_balances (
            token_addr BLOB NOT NULL,
            holder_addr BLOB NOT NULL,
            balance TEXT NOT NULL DEFAULT '0',
            updated_at INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (token_addr, holder_addr)
        );
        CREATE INDEX IF NOT EXISTS idx_tb_holder ON token_balances(holder_addr);
        "#,
    )?;
    Ok(conn)
}

pub fn lock<'a>(db: &'a Db) -> MutexGuard<'a, Connection> {
    db.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// Key/value store (precomputed stats, etc.)
// ---------------------------------------------------------------------------

pub fn set_kv(conn: &Connection, key: &str, value: &str) -> Result<()> {
    exec_cached(
        conn,
        "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
        params![key, value, now_ts()],
    )?;
    Ok(())
}

pub fn get_kv(db: &Db, key: &str) -> Option<String> {
    let conn = lock(db);
    conn.query_row("SELECT value FROM kv WHERE key=?1", params![key], |r| {
        r.get::<_, String>(0)
    })
    .optional()
    .ok()
    .flatten()
}

// ---------------------------------------------------------------------------
// Blocks
// ---------------------------------------------------------------------------

const BLOCK_COLS: &str =
    "number, hash, parent_hash, timestamp, timestamp_ms, gas_used, gas_limit, base_fee, size, \
     extra_data, epoch, view, proposer, miner, tx_count, created_at";

fn upsert_block(conn: &Connection, block: &Block) -> Result<()> {
    exec_cached(
        conn,
        r#"
        INSERT INTO blocks (number, hash, parent_hash, timestamp, timestamp_ms, gas_used, gas_limit,
                            base_fee, size, extra_data, epoch, view, proposer,
                            miner, tx_count, created_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
        ON CONFLICT(number) DO UPDATE SET
            hash=excluded.hash, parent_hash=excluded.parent_hash, timestamp=excluded.timestamp,
            timestamp_ms=excluded.timestamp_ms,
            gas_used=excluded.gas_used, gas_limit=excluded.gas_limit, base_fee=excluded.base_fee,
            size=excluded.size, extra_data=excluded.extra_data, epoch=excluded.epoch,
            view=excluded.view, proposer=excluded.proposer, miner=excluded.miner,
            tx_count=excluded.tx_count
        "#,
        params![
            block.number,
            hex_blob(&block.hash),
            hex_blob(&block.parent_hash),
            block.timestamp,
            block.timestamp_ms,
            block.gas_used,
            block.gas_limit,
            block.base_fee,
            block.size,
            block.extra_data,
            block.epoch,
            block.view,
            hex_blob(&block.proposer),
            hex_blob(&block.miner),
            block.tx_count,
            block.created_at,
        ],
    )?;
    Ok(())
}

pub fn save_block(db: &Db, block: &Block) -> Result<()> {
    let conn = lock(db);
    upsert_block(&conn, block)
}

/// Persist one indexed block atomically: the block, its transactions,
/// transfer and anchored events, and any token metadata in a single SQLite
/// transaction.
pub fn save_block_bundle(
    db: &Db,
    block: &Block,
    txs: &[Transaction],
    transfers: &[TransferEvent],
    anchored: &[AnchoredEvent],
    tokens: &[crate::tokens::TokenMeta],
) -> Result<()> {
    let mut conn = lock(db);
    let txn = conn.transaction().context("begin block transaction")?;
    upsert_block(&txn, block)?;
    for tx in txs {
        upsert_transaction(&txn, tx)?;
    }
    // Transfers are keyed by (block_number, log_index): re-writing an
    // already-indexed block inserts nothing the second time, and only
    // freshly-inserted transfers move balances / holder counts.
    let mut inserted: Vec<&TransferEvent> = Vec::with_capacity(transfers.len());
    for transfer in transfers {
        if insert_transfer(&txn, transfer)? {
            inserted.push(transfer);
        }
    }
    // Upsert token metadata first so `refresh_holder_counts` below can
    // UPDATE the holder_count of freshly-seen tokens instead of seeding
    // them with the INSERT's literal 0.
    for meta in tokens {
        upsert_token_meta(&txn, meta)?;
    }
    apply_transfer_balances(&txn, &inserted)?;
    refresh_holder_counts(&txn, &inserted)?;
    for event in anchored {
        if insert_anchored(&txn, event)? {
            refresh_anchored_namespace(&txn, event)?;
        }
    }
    for tx in txs {
        if let Some(addr) = &tx.contract_address {
            exec_cached(
                &txn,
                "INSERT OR IGNORE INTO contract_labels (address, name, abi, is_token, is_precompile, created_at)
                 VALUES (?1, '', '[]', 0, 0, ?2)",
                params![addr, now_ts()],
            )?;
        }
    }
    txn.commit().context("commit block transaction")?;
    Ok(())
}

fn row_to_block(row: &rusqlite::Row) -> rusqlite::Result<Block> {
    Ok(Block {
        number: row.get(0)?,
        hash: blob_hex(&row.get::<_, Vec<u8>>(1)?),
        parent_hash: blob_hex(&row.get::<_, Vec<u8>>(2)?),
        timestamp: row.get(3)?,
        timestamp_ms: row.get(4)?,
        gas_used: row.get(5)?,
        gas_limit: row.get(6)?,
        base_fee: row.get(7)?,
        size: row.get(8)?,
        extra_data: row.get(9)?,
        epoch: row.get(10)?,
        view: row.get(11)?,
        proposer: blob_hex(&row.get::<_, Vec<u8>>(12)?),
        miner: blob_hex(&row.get::<_, Vec<u8>>(13)?),
        tx_count: row.get(14)?,
        created_at: row.get(15)?,
    })
}

pub fn get_block_by_number(db: &Db, number: i64) -> Option<Block> {
    let conn = lock(db);
    conn.query_row(
        &format!("SELECT {BLOCK_COLS} FROM blocks WHERE number=?1"),
        params![number],
        row_to_block,
    )
    .optional()
    .ok()
    .flatten()
}

pub fn get_block_by_hash(db: &Db, hash: &str) -> Option<Block> {
    let conn = lock(db);
    conn.query_row(
        &format!("SELECT {BLOCK_COLS} FROM blocks WHERE hash=?1"),
        params![hex_blob(hash)],
        row_to_block,
    )
    .optional()
    .ok()
    .flatten()
}

pub fn get_latest_block(db: &Db) -> Option<Block> {
    let conn = lock(db);
    conn.query_row(
        &format!("SELECT {BLOCK_COLS} FROM blocks ORDER BY number DESC LIMIT 1"),
        [],
        row_to_block,
    )
    .optional()
    .ok()
    .flatten()
}

/// Number of indexed block rows (the history-completion metric).
pub fn get_block_count(db: &Db) -> i64 {
    let conn = lock(db);
    conn.query_row("SELECT COUNT(*) FROM blocks", [], |r| r.get::<_, i64>(0))
        .unwrap_or(0)
}

/// Record the chain head observed by the indexer (for index-progress display).
pub fn set_chain_head(db: &Db, head: i64) {
    let conn = lock(db);
    let _ = set_kv(&conn, "chain_head", &head.to_string());
}

/// The last chain head observed by the indexer; `0` when unknown.
pub fn get_chain_head(db: &Db) -> i64 {
    get_kv(db, "chain_head")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Lowest stored block height; `None` when the table is empty.
pub fn get_min_block_number(db: &Db) -> Option<i64> {
    let conn = lock(db);
    conn.query_row("SELECT MIN(number) FROM blocks", [], |r| r.get::<_, i64>(0))
        .ok()
        .filter(|n| *n != 0)
}

pub fn get_recent_blocks(db: &Db, limit: usize) -> Vec<Block> {
    let conn = lock(db);
    let Ok(mut stmt) = conn.prepare(&format!(
        "SELECT {BLOCK_COLS} FROM blocks ORDER BY number DESC LIMIT ?1"
    )) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(params![limit as i64], row_to_block) else {
        return Vec::new();
    };
    rows.filter_map(|r| r.ok()).collect()
}

// ---------------------------------------------------------------------------
// Transactions
// ---------------------------------------------------------------------------

fn upsert_transaction(conn: &Connection, tx: &Transaction) -> Result<()> {
    exec_cached(
        conn,
        r#"
        INSERT INTO transactions (
            hash, block_number, position, from_addr, to_addr, status,
            gas_used, base_fee, contract_address, fee_token, fee_amount,
            input, raw, trace_data, receipt_data, timestamp, created_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
        ON CONFLICT(hash) DO UPDATE SET
            block_number=excluded.block_number, position=excluded.position,
            from_addr=excluded.from_addr, to_addr=excluded.to_addr, status=excluded.status,
            gas_used=excluded.gas_used, base_fee=excluded.base_fee,
            contract_address=excluded.contract_address, fee_token=excluded.fee_token,
            fee_amount=excluded.fee_amount,
            input=excluded.input, raw=excluded.raw,
            trace_data=excluded.trace_data,
            receipt_data=excluded.receipt_data, timestamp=excluded.timestamp
        "#,
        params![
            hex_blob(&tx.hash),
            tx.block_number,
            tx.position,
            hex_blob(&tx.from_addr),
            tx.to_addr.as_deref().map(hex_blob),
            tx.status,
            tx.gas_used,
            tx.base_fee,
            tx.contract_address.as_deref().map(hex_blob),
            tx.fee_token.as_deref().map(hex_blob),
            tx.fee_amount,
            tx.input,
            tx.raw.as_deref().map(hex_blob),
            tx.trace_data,
            tx.receipt_data,
            tx.timestamp,
            tx.created_at,
        ],
    )?;
    Ok(())
}

pub fn save_transaction(db: &Db, tx: &Transaction) -> Result<()> {
    let conn = lock(db);
    upsert_transaction(&conn, tx)
}

const TX_COLS: &str = "hash, block_number, position, from_addr, to_addr, status, gas_used, base_fee, contract_address, fee_token, fee_amount, input, raw, trace_data, receipt_data, timestamp, created_at";

fn row_to_tx(row: &rusqlite::Row) -> rusqlite::Result<Transaction> {
    Ok(Transaction {
        hash: blob_hex(&row.get::<_, Vec<u8>>(0)?),
        block_number: row.get(1)?,
        position: row.get(2)?,
        from_addr: blob_hex(&row.get::<_, Vec<u8>>(3)?),
        to_addr: row.get::<_, Option<Vec<u8>>>(4)?.map(|b| blob_hex(&b)),
        status: row.get(5)?,
        gas_used: row.get(6)?,
        base_fee: row.get(7)?,
        contract_address: row.get::<_, Option<Vec<u8>>>(8)?.map(|b| blob_hex(&b)),
        fee_token: row.get::<_, Option<Vec<u8>>>(9)?.map(|b| blob_hex(&b)),
        fee_amount: row.get(10)?,
        input: row.get(11)?,
        raw: row
            .get::<_, Option<Vec<u8>>>(12)?
            .map(|b| format!("0x{}", hex::encode(b))),
        trace_data: row.get(13)?,
        receipt_data: row.get(14)?,
        timestamp: row.get(15)?,
        created_at: row.get(16)?,
    })
}

pub fn get_transaction(db: &Db, hash: &str) -> Option<Transaction> {
    let conn = lock(db);
    conn.query_row(
        &format!("SELECT {TX_COLS} FROM transactions WHERE hash=?1"),
        params![hex_blob(hash)],
        row_to_tx,
    )
    .optional()
    .ok()
    .flatten()
}

pub fn get_block_transactions(db: &Db, block_number: i64) -> Vec<Transaction> {
    let conn = lock(db);
    let Ok(mut stmt) = conn.prepare(&format!(
        "SELECT {TX_COLS} FROM transactions WHERE block_number=?1 ORDER BY position"
    )) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(params![block_number], row_to_tx) else {
        return Vec::new();
    };
    rows.filter_map(|r| r.ok()).collect()
}

pub fn get_recent_transactions(db: &Db, limit: usize) -> Vec<Transaction> {
    let conn = lock(db);
    let Ok(mut stmt) = conn.prepare(&format!(
        "SELECT {TX_COLS} FROM transactions ORDER BY timestamp DESC LIMIT ?1"
    )) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(params![limit as i64], row_to_tx) else {
        return Vec::new();
    };
    rows.filter_map(|r| r.ok()).collect()
}

pub fn get_address_transactions(
    db: &Db,
    address: &str,
    page: u32,
    per_page: u32,
) -> Vec<Transaction> {
    let conn = lock(db);
    let offset = (page.saturating_sub(1) * per_page) as i64;
    let Ok(mut stmt) = conn.prepare(&format!(
        "SELECT {TX_COLS} FROM transactions WHERE from_addr=?1 OR to_addr=?1 ORDER BY timestamp DESC LIMIT ?2 OFFSET ?3"
    )) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(
        params![hex_blob(address), per_page as i64, offset],
        row_to_tx,
    ) else {
        return Vec::new();
    };
    rows.filter_map(|r| r.ok()).collect()
}

pub fn get_address_transaction_count(db: &Db, address: &str) -> i64 {
    let conn = lock(db);
    conn.query_row(
        "SELECT COUNT(*) FROM transactions WHERE from_addr=?1 OR to_addr=?1",
        params![hex_blob(address)],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Token metadata
// ---------------------------------------------------------------------------

fn upsert_token_meta(conn: &Connection, meta: &crate::tokens::TokenMeta) -> Result<()> {
    let ts = now_ts();
    exec_cached(
        conn,
        r#"
        INSERT INTO token_metadata (address, name, symbol, decimals, currency, total_supply, logo_uri, holder_count, created_at, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', 0, ?7, ?7)
        ON CONFLICT(address) DO UPDATE SET
            name=excluded.name, symbol=excluded.symbol, decimals=excluded.decimals,
            currency=excluded.currency, total_supply=excluded.total_supply, updated_at=excluded.updated_at
        "#,
        params![
            hex_blob(&meta.address),
            meta.name,
            meta.symbol,
            meta.decimals,
            meta.currency,
            meta.total_supply,
            ts,
        ],
    )?;
    Ok(())
}

pub fn save_token_metadata(db: &Db, meta: &crate::tokens::TokenMeta) -> Result<()> {
    let conn = lock(db);
    upsert_token_meta(&conn, meta)
}

fn row_to_token(row: &rusqlite::Row) -> rusqlite::Result<TokenMetadata> {
    Ok(TokenMetadata {
        address: blob_hex(&row.get::<_, Vec<u8>>(0)?),
        name: row.get(1)?,
        symbol: row.get(2)?,
        decimals: row.get(3)?,
        currency: row.get(4)?,
        total_supply: row.get(5)?,
        logo_uri: row.get(6)?,
        holder_count: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

pub fn get_token_metadata(db: &Db, address: &str) -> Option<TokenMetadata> {
    let conn = lock(db);
    conn.query_row(
        "SELECT address, name, symbol, decimals, currency, total_supply, logo_uri, holder_count, created_at, updated_at FROM token_metadata WHERE address=?1",
        params![hex_blob(address)],
        row_to_token,
    )
    .optional()
    .ok()
    .flatten()
}

pub fn get_all_tokens(db: &Db, page: u32, per_page: u32) -> Vec<TokenMetadata> {
    let conn = lock(db);
    let offset = (page.saturating_sub(1) * per_page) as i64;
    let Ok(mut stmt) = conn.prepare(
        "SELECT address, name, symbol, decimals, currency, total_supply, logo_uri, holder_count, created_at, updated_at FROM token_metadata ORDER BY holder_count DESC LIMIT ?1 OFFSET ?2",
    ) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(params![per_page as i64, offset], row_to_token) else {
        return Vec::new();
    };
    rows.filter_map(|r| r.ok()).collect()
}

pub fn get_token_count(db: &Db) -> i64 {
    let conn = lock(db);
    conn.query_row("SELECT COUNT(*) FROM token_metadata", [], |r| {
        r.get::<_, i64>(0)
    })
    .unwrap_or(0)
}

pub fn get_token_transfer_count(db: &Db, token_addr: &str) -> i64 {
    let conn = lock(db);
    conn.query_row(
        "SELECT COUNT(*) FROM transfer_events WHERE token_addr=?1",
        params![hex_blob(token_addr)],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
}

/// Find a token by exact symbol/name (case-insensitive), used by search.
pub fn get_token_by_symbol_or_name(db: &Db, q: &str) -> Option<TokenMetadata> {
    let conn = lock(db);
    conn.query_row(
        "SELECT address, name, symbol, decimals, currency, total_supply, logo_uri, \
         holder_count, created_at, updated_at FROM token_metadata \
         WHERE lower(symbol)=lower(?1) OR lower(name)=lower(?1) LIMIT 1",
        params![q],
        row_to_token,
    )
    .optional()
    .ok()
    .flatten()
}

// ---------------------------------------------------------------------------
// Transfer events
// ---------------------------------------------------------------------------

/// Insert one transfer event. Returns `true` when the row was newly inserted
/// and `false` when it duplicated an existing (block_number, log_index) —
/// the unique key that makes re-writing an already-indexed block idempotent.
fn insert_transfer(conn: &Connection, transfer: &TransferEvent) -> Result<bool> {
    let inserted = exec_cached(
        conn,
        "INSERT OR IGNORE INTO transfer_events (tx_hash, block_number, log_index, token_addr, from_addr, to_addr, amount, timestamp, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            hex_blob(&transfer.tx_hash),
            transfer.block_number,
            transfer.log_index,
            hex_blob(&transfer.token_addr),
            hex_blob(&transfer.from_addr),
            hex_blob(&transfer.to_addr),
            transfer.amount,
            transfer.timestamp,
            transfer.created_at,
        ],
    )?;
    Ok(inserted != 0)
}

fn bigint(s: &str) -> num_bigint::BigInt {
    num_bigint::BigInt::parse_bytes(s.as_bytes(), 10).unwrap_or_else(|| num_bigint::BigInt::from(0))
}

/// Apply a signed amount delta to one (token, holder) balance, removing the
/// row when the balance hits zero so `holder_count` stays exact and the table
/// stays small.
fn adjust_balance(
    conn: &Connection,
    token: &str,
    holder: &str,
    delta: &num_bigint::BigInt,
) -> Result<()> {
    if delta.sign() == num_bigint::Sign::NoSign {
        return Ok(());
    }
    let current = conn
        .prepare_cached(
            "SELECT balance FROM token_balances WHERE token_addr=?1 AND holder_addr=?2",
        )?
        .query_row(params![token, holder], |r| r.get::<_, String>(0))
        .optional()
        .ok()
        .flatten()
        .unwrap_or_else(|| "0".into());
    let new = bigint(&current) + delta;
    if new.sign() == num_bigint::Sign::NoSign {
        exec_cached(
            conn,
            "DELETE FROM token_balances WHERE token_addr=?1 AND holder_addr=?2",
            params![token, holder],
        )?;
    } else {
        exec_cached(
            conn,
            "INSERT INTO token_balances (token_addr, holder_addr, balance, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(token_addr, holder_addr) DO UPDATE SET
                 balance=excluded.balance, updated_at=excluded.updated_at",
            params![token, holder, new.to_string(), now_ts()],
        )?;
    }
    Ok(())
}

fn apply_transfer_balances(conn: &Connection, transfers: &[&TransferEvent]) -> Result<()> {
    for t in transfers {
        let amount = bigint(&t.amount);
        adjust_balance(conn, &t.token_addr, &t.from_addr, &-&amount)?;
        adjust_balance(conn, &t.token_addr, &t.to_addr, &amount)?;
    }
    Ok(())
}

/// Keep `token_metadata.holder_count` in sync for tokens touched by this
/// batch of transfers. Uses the primary-key index so it stays cheap.
fn refresh_holder_counts(conn: &Connection, transfers: &[&TransferEvent]) -> Result<()> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for t in transfers {
        if !seen.insert(t.token_addr.clone()) {
            continue;
        }
        let count = conn
            .prepare_cached("SELECT COUNT(*) FROM token_balances WHERE token_addr=?1")?
            .query_row(params![t.token_addr], |r| r.get::<_, i64>(0))?;
        exec_cached(
            conn,
            "UPDATE token_metadata SET holder_count=?1, updated_at=?2 WHERE address=?3",
            params![count, now_ts(), hex_blob(&t.token_addr)],
        )?;
    }
    Ok(())
}

/// Rebuild `token_balances` + holder counts from the transfer history.
/// Run once after upgrading a pre-existing database (or after the
/// transfer_events dedup migration) so incremental updates start from a
/// correct state. Address columns may be stored as BLOBs (current format) or
/// TEXT (pre-upgrade format), so both are decoded and normalized to EIP-55
/// checksummed hex, matching the keys the incremental balance path stores.
pub fn rebuild_token_balances(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM token_balances", [])?;
    let mut stmt = conn.prepare(
        "SELECT token_addr, from_addr, to_addr, amount FROM transfer_events ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            addr_from_value(r.get_ref(0)?),
            addr_from_value(r.get_ref(1)?),
            addr_from_value(r.get_ref(2)?),
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut touched: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut n = 0usize;
    for row in rows {
        let (token, from, to, amount) = row?;
        let amount = bigint(&amount);
        adjust_balance(conn, &token, &from, &-&amount)?;
        adjust_balance(conn, &token, &to, &amount)?;
        touched.insert(token);
        n += 1;
    }
    for token in touched {
        let count = conn.query_row(
            "SELECT COUNT(*) FROM token_balances WHERE token_addr=?1",
            params![token],
            |r| r.get::<_, i64>(0),
        )?;
        conn.execute(
            "UPDATE token_metadata SET holder_count=?1, updated_at=?2 WHERE address=?3",
            params![count, now_ts(), hex_blob(&token)],
        )?;
    }
    tracing::info!("rebuilt token balances from {n} transfer events");
    Ok(())
}

/// Decode one address column of `transfer_events` into EIP-55 checksummed
/// hex. The column is a BLOB (raw bytes, current storage) or TEXT (legacy
/// storage); either way we end with the canonical form so `token_balances`
/// keys stay comparable to the incremental path's writes.
fn addr_from_value(v: ValueRef<'_>) -> String {
    let hex = match v {
        ValueRef::Blob(b) => format!("0x{}", hex::encode(b)),
        ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
        _ => String::new(),
    };
    crate::decoder::checksum_address(&hex)
}

pub fn save_transfer(db: &Db, transfer: &TransferEvent) -> Result<()> {
    let conn = lock(db);
    insert_transfer(&conn, transfer)?;
    Ok(())
}

fn row_to_transfer(row: &rusqlite::Row) -> rusqlite::Result<TransferEvent> {
    Ok(TransferEvent {
        id: row.get(0)?,
        tx_hash: blob_hex(&row.get::<_, Vec<u8>>(1)?),
        block_number: row.get(2)?,
        log_index: row.get(3)?,
        token_addr: blob_hex(&row.get::<_, Vec<u8>>(4)?),
        from_addr: blob_hex(&row.get::<_, Vec<u8>>(5)?),
        to_addr: blob_hex(&row.get::<_, Vec<u8>>(6)?),
        amount: row.get(7)?,
        timestamp: row.get(8)?,
        created_at: row.get(9)?,
    })
}

fn transfer_to_json(transfer: &TransferEvent, tx: Option<&Transaction>) -> Value {
    let tx_from = tx
        .map(|t| t.from_addr.clone())
        .unwrap_or_else(|| transfer.from_addr.clone());
    let tx_to = tx
        .map(|t| t.to_addr.clone())
        .unwrap_or_else(|| Some(transfer.to_addr.clone()));
    let tx_timestamp = tx.map(|t| t.timestamp).unwrap_or(transfer.timestamp);
    let tx_status = tx.map(|t| t.status).unwrap_or(1);
    json!({
        "id": transfer.id,
        "tx_hash": transfer.tx_hash,
        "block_number": transfer.block_number,
        "log_index": transfer.log_index,
        "token_addr": transfer.token_addr,
        "from_addr": transfer.from_addr,
        "to_addr": transfer.to_addr,
        "amount": transfer.amount,
        "timestamp": transfer.timestamp,
        "created_at": transfer.created_at,
        "tx_from": tx_from,
        "tx_to": tx_to,
        "tx_timestamp": tx_timestamp,
        "tx_status": tx_status,
    })
}

pub fn get_token_transfers(db: &Db, token_addr: &str, page: u32, per_page: u32) -> Vec<Value> {
    let transfers: Vec<TransferEvent> = {
        let conn = lock(db);
        let offset = (page.saturating_sub(1) * per_page) as i64;
        let Ok(mut stmt) = conn.prepare(
            "SELECT id, tx_hash, block_number, log_index, token_addr, from_addr, to_addr, amount, timestamp, created_at
             FROM transfer_events WHERE token_addr=?1
             ORDER BY block_number DESC, log_index DESC LIMIT ?2 OFFSET ?3",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map(
            params![hex_blob(token_addr), per_page as i64, offset],
            row_to_transfer,
        ) else {
            return Vec::new();
        };
        rows.filter_map(|r| r.ok()).collect()
    };
    let mut out = Vec::with_capacity(transfers.len());
    for t in &transfers {
        out.push(transfer_to_json(
            t,
            get_transaction(db, &t.tx_hash).as_ref(),
        ));
    }
    out
}

pub fn get_address_transfers(db: &Db, address: &str, page: u32, per_page: u32) -> Vec<Value> {
    let transfers: Vec<TransferEvent> = {
        let conn = lock(db);
        let offset = (page.saturating_sub(1) * per_page) as i64;
        let Ok(mut stmt) = conn.prepare(
            "SELECT id, tx_hash, block_number, log_index, token_addr, from_addr, to_addr, amount, timestamp, created_at
             FROM transfer_events WHERE from_addr=?1 OR to_addr=?2
             ORDER BY block_number DESC, log_index DESC LIMIT ?3 OFFSET ?4",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map(
            params![
                hex_blob(address),
                hex_blob(address),
                per_page as i64,
                offset
            ],
            row_to_transfer,
        ) else {
            return Vec::new();
        };
        rows.filter_map(|r| r.ok()).collect()
    };
    let mut out = Vec::with_capacity(transfers.len());
    for t in &transfers {
        out.push(transfer_to_json(
            t,
            get_transaction(db, &t.tx_hash).as_ref(),
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Anchored events
// ---------------------------------------------------------------------------

const ANCHORED_COLS: &str = "tx_hash, block_number, log_index, namespace, key, commitment, \
                             metadata, timestamp, created_at";

/// Insert one `Anchored` log. Returns `true` when the row was newly inserted
/// and `false` when it duplicated an existing (block_number, log_index) — a
/// re-indexed block, whose events must not move the namespace stats again.
fn insert_anchored(conn: &Connection, event: &AnchoredEvent) -> Result<bool> {
    let inserted = exec_cached(
        conn,
        "INSERT OR IGNORE INTO anchored_events (tx_hash, block_number, log_index, namespace, key,
                                                commitment, metadata, timestamp, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            hex_blob(&event.tx_hash),
            event.block_number,
            event.log_index,
            hex_blob(&event.namespace),
            hex_blob(&event.key),
            hex_blob(&event.commitment),
            hex_blob(&event.metadata),
            event.timestamp,
            event.created_at,
        ],
    )?;
    Ok(inserted != 0)
}

/// Fold one freshly-inserted anchor into its namespace's summary row. Runs in
/// the same transaction as the insert, so the stats never drift from the log.
fn refresh_anchored_namespace(conn: &Connection, event: &AnchoredEvent) -> Result<()> {
    // The event's own row is already in, so a second row under the pair means
    // the key existed before this anchor. One probe on idx_anchored_ns_key,
    // stopping at the second entry.
    let key_existed =
        conn.prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM anchored_events
                           WHERE namespace=?1 AND key=?2 LIMIT 1 OFFSET 1)",
        )?
        .query_row(
            params![hex_blob(&event.namespace), hex_blob(&event.key)],
            |r| r.get::<_, i64>(0),
        )? != 0;
    exec_cached(
        conn,
        "INSERT INTO anchored_namespaces (namespace, anchor_count, key_count, last_block, last_timestamp)
         VALUES (?1, 1, ?2, ?3, ?4)
         ON CONFLICT(namespace) DO UPDATE SET
             anchor_count = anchor_count + 1,
             key_count = key_count + excluded.key_count,
             last_block = MAX(last_block, excluded.last_block),
             last_timestamp = MAX(last_timestamp, excluded.last_timestamp)",
        params![
            hex_blob(&event.namespace),
            i64::from(!key_existed),
            event.block_number,
            event.timestamp,
        ],
    )?;
    Ok(())
}

/// Recompute every namespace's summary row from the log — idempotent, run once
/// at startup for databases that predate the table; after that each block
/// maintains it incrementally.
pub fn sync_anchored_namespaces(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM anchored_namespaces", [])?;
    conn.execute(
        "INSERT INTO anchored_namespaces (namespace, anchor_count, key_count, last_block, last_timestamp)
         SELECT namespace, COUNT(*), COUNT(DISTINCT key), MAX(block_number), MAX(timestamp)
         FROM anchored_events GROUP BY namespace",
        [],
    )?;
    Ok(())
}

fn row_to_anchored(row: &rusqlite::Row) -> rusqlite::Result<AnchoredEvent> {
    Ok(AnchoredEvent {
        tx_hash: blob_hex(&row.get::<_, Vec<u8>>(0)?),
        block_number: row.get(1)?,
        log_index: row.get(2)?,
        namespace: crate::decoder::checksum_address(&blob_hex(&row.get::<_, Vec<u8>>(3)?)),
        key: blob_hex(&row.get::<_, Vec<u8>>(4)?),
        commitment: blob_hex(&row.get::<_, Vec<u8>>(5)?),
        metadata: blob_hex(&row.get::<_, Vec<u8>>(6)?),
        timestamp: row.get(7)?,
        created_at: row.get(8)?,
    })
}

/// Namespaces that have anchored anything, busiest first.
pub fn get_anchored_namespaces(db: &Db, page: u32, per_page: u32) -> Vec<Value> {
    let conn = lock(db);
    let offset = (page.saturating_sub(1) * per_page) as i64;
    let Ok(mut stmt) = conn.prepare(
        "SELECT namespace, anchor_count, key_count, last_block, last_timestamp
         FROM anchored_namespaces
         ORDER BY anchor_count DESC, last_block DESC
         LIMIT ?1 OFFSET ?2",
    ) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(params![per_page as i64, offset], |r| {
        Ok(json!({
            "namespace": crate::decoder::checksum_address(&blob_hex(&r.get::<_, Vec<u8>>(0)?)),
            "anchor_count": r.get::<_, i64>(1)?,
            "key_count": r.get::<_, i64>(2)?,
            "last_block": r.get::<_, i64>(3)?,
            "last_timestamp": r.get::<_, i64>(4)?,
        }))
    }) else {
        return Vec::new();
    };
    rows.filter_map(|r| r.ok()).collect()
}

/// Keys a namespace has anchored, each at its head row — what `latest` returns.
pub fn get_namespace_keys(db: &Db, namespace: &str, page: u32, per_page: u32) -> Vec<Value> {
    let conn = lock(db);
    let offset = (page.saturating_sub(1) * per_page) as i64;
    let Ok(mut stmt) = conn.prepare(
        "SELECT key, commitment, metadata, block_number, timestamp, tx_hash, revisions
         FROM (
             SELECT *,
                    COUNT(*) OVER (PARTITION BY key) AS revisions,
                    ROW_NUMBER() OVER (
                        PARTITION BY key ORDER BY block_number DESC, log_index DESC
                    ) AS rn
             FROM anchored_events
             WHERE namespace = ?1
         )
         WHERE rn = 1
         ORDER BY block_number DESC, log_index DESC
         LIMIT ?2 OFFSET ?3",
    ) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(params![hex_blob(namespace), per_page as i64, offset], |r| {
        Ok(json!({
            "key": blob_hex(&r.get::<_, Vec<u8>>(0)?),
            "commitment": blob_hex(&r.get::<_, Vec<u8>>(1)?),
            "metadata": blob_hex(&r.get::<_, Vec<u8>>(2)?),
            "block_number": r.get::<_, i64>(3)?,
            "timestamp": r.get::<_, i64>(4)?,
            "tx_hash": blob_hex(&r.get::<_, Vec<u8>>(5)?),
            "revisions": r.get::<_, i64>(6)?,
        }))
    }) else {
        return Vec::new();
    };
    rows.filter_map(|r| r.ok()).collect()
}

/// Every revision of one `(namespace, key)`, newest first — the head is row zero.
pub fn get_key_history(db: &Db, namespace: &str, key: &str) -> Vec<AnchoredEvent> {
    let conn = lock(db);
    let Ok(mut stmt) = conn.prepare(&format!(
        "SELECT {ANCHORED_COLS} FROM anchored_events WHERE namespace=?1 AND key=?2
         ORDER BY block_number DESC, log_index DESC"
    )) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(params![hex_blob(namespace), hex_blob(key)], row_to_anchored)
    else {
        return Vec::new();
    };
    rows.filter_map(|r| r.ok()).collect()
}

pub fn get_recent_anchored(db: &Db, limit: usize) -> Vec<AnchoredEvent> {
    let conn = lock(db);
    let Ok(mut stmt) = conn.prepare(&format!(
        "SELECT {ANCHORED_COLS} FROM anchored_events
         ORDER BY block_number DESC, log_index DESC LIMIT ?1"
    )) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(params![limit as i64], row_to_anchored) else {
        return Vec::new();
    };
    rows.filter_map(|r| r.ok()).collect()
}

pub fn count_anchored(db: &Db) -> i64 {
    let conn = lock(db);
    conn.query_row("SELECT COUNT(*) FROM anchored_events", [], |r| {
        r.get::<_, i64>(0)
    })
    .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Holdings
// ---------------------------------------------------------------------------

/// Batch token metadata lookup; one query per ~100 addresses.
pub fn get_tokens_metadata(
    db: &Db,
    addresses: &[String],
) -> std::collections::HashMap<String, TokenMetadata> {
    let mut out = std::collections::HashMap::new();
    if addresses.is_empty() {
        return out;
    }
    let conn = lock(db);
    for chunk in addresses.chunks(100) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT address, name, symbol, decimals, currency, total_supply, logo_uri, \
             holder_count, created_at, updated_at FROM token_metadata WHERE address IN ({placeholders})"
        );
        let Ok(mut stmt) = conn.prepare(&sql) else {
            continue;
        };
        let params = rusqlite::params_from_iter(chunk.iter().map(|a| hex_blob(a)));
        let Ok(rows) = stmt.query_map(params, row_to_token) else {
            continue;
        };
        for row in rows.flatten() {
            out.insert(row.address.clone(), row);
        }
    }
    out
}

/// All token addresses known to the indexer (for cache seeding).
pub fn get_all_token_addresses(db: &Db) -> Vec<String> {
    let conn = lock(db);
    let Ok(mut stmt) = conn.prepare("SELECT address FROM token_metadata") else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
        return Vec::new();
    };
    rows.filter_map(|r| r.ok()).collect()
}

pub fn get_token_holder_count(db: &Db, token_addr: &str) -> i64 {
    let conn = lock(db);
    conn.query_row(
        "SELECT COUNT(*) FROM token_balances WHERE token_addr=?1",
        params![token_addr],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
}

/// Recompute `token_metadata.holder_count` for every token that has balance
/// rows. Cheap (uses the token_balances primary-key index) and idempotent;
/// run at startup to backfill stale counts written before the BLOB-key fix.
pub fn sync_holder_counts(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("SELECT DISTINCT token_addr FROM token_balances")?;
    let tokens: Vec<String> = stmt
        .query_map([], |r| Ok(addr_from_value(r.get_ref(0)?)))
        .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())?;
    for token in tokens {
        let count = conn.query_row(
            "SELECT COUNT(*) FROM token_balances WHERE token_addr=?1",
            params![token],
            |r| r.get::<_, i64>(0),
        )?;
        conn.execute(
            "UPDATE token_metadata SET holder_count=?1, updated_at=?2 WHERE address=?3",
            params![count, now_ts(), hex_blob(&token)],
        )?;
    }
    Ok(())
}

/// Contract-label lookup (populated at index time for created contracts).
pub fn get_contract_label(db: &Db, addr: &str) -> Option<String> {
    let conn = lock(db);
    conn.query_row(
        "SELECT name FROM contract_labels WHERE address=?1",
        params![addr],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
}

pub fn get_address_holdings(db: &Db, address: &str) -> Vec<Value> {
    let balances: Vec<(String, String)> = {
        let conn = lock(db);
        let Ok(mut stmt) =
            conn.prepare("SELECT token_addr, balance FROM token_balances WHERE holder_addr=?1")
        else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map(params![address], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        }) else {
            return Vec::new();
        };
        rows.filter_map(|r| r.ok()).collect()
    };
    let mut holdings = Vec::new();
    let token_addrs: Vec<String> = balances.iter().map(|(a, _)| a.clone()).collect();
    let metas = get_tokens_metadata(db, &token_addrs);
    for (token_addr, balance) in &balances {
        // `get_tokens_metadata` keys are lowercase hex (blob_hex), while
        // `token_balances.token_addr` stores EIP-55 checksummed text; match
        // case-insensitively so the joined metadata is found.
        if let Some(meta) = metas.get(&token_addr.to_lowercase()) {
            let formatted = crate::tokens::format_token_amount(balance, meta.decimals);
            holdings.push(json!({
                "token": meta.address,
                "name": meta.name,
                "symbol": meta.symbol,
                "decimals": meta.decimals,
                "balance": balance,
                "formatted": formatted,
            }));
        }
    }
    holdings
}
