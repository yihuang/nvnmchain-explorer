//! SQLite storage layer, mirroring `app/database.py` + `app/models.py`.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::models::{Block, TokenMetadata, Transaction, TransferEvent};

pub type Db = Arc<Mutex<Connection>>;

/// Add a column to an existing table when the schema predates it.
fn add_column_if_missing(conn: &Connection, table: &str, column: &str, ddl: &str) -> Result<()> {
    let cols: Vec<String> = {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        rows.filter_map(|c| c.ok()).collect()
    };
    if !cols.iter().any(|c| c == column) {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {ddl};"))?;
    }
    Ok(())
}

pub fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
            hash TEXT NOT NULL UNIQUE,
            parent_hash TEXT NOT NULL,
            timestamp INTEGER NOT NULL,
            gas_used INTEGER NOT NULL DEFAULT 0,
            gas_limit INTEGER NOT NULL DEFAULT 0,
            miner TEXT NOT NULL DEFAULT '',
            tx_count INTEGER NOT NULL DEFAULT 0,
            raw TEXT NOT NULL DEFAULT '{}',
            created_at INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_blocks_hash ON blocks(hash);
        CREATE INDEX IF NOT EXISTS idx_blocks_timestamp ON blocks(timestamp);

        CREATE TABLE IF NOT EXISTS transactions (
            hash TEXT PRIMARY KEY,
            block_number INTEGER NOT NULL,
            block_hash TEXT NOT NULL,
            position INTEGER NOT NULL DEFAULT 0,
            from_addr TEXT NOT NULL,
            to_addr TEXT,
            status INTEGER NOT NULL DEFAULT 1,
            gas_limit INTEGER NOT NULL DEFAULT 0,
            gas_used INTEGER NOT NULL DEFAULT 0,
            gas_price TEXT NOT NULL DEFAULT '0',
            max_fee_per_gas TEXT NOT NULL DEFAULT '0',
            max_priority_fee_per_gas TEXT NOT NULL DEFAULT '0',
            base_fee TEXT NOT NULL DEFAULT '0',
            contract_address TEXT,
            fee_token TEXT,
            fee_amount TEXT NOT NULL DEFAULT '0',
            nonce INTEGER NOT NULL DEFAULT 0,
            nonce_key TEXT,
            value TEXT NOT NULL DEFAULT '0',
            chain_id INTEGER NOT NULL DEFAULT 787222,
            tx_type INTEGER NOT NULL DEFAULT 118,
            input TEXT NOT NULL DEFAULT '0x',
            raw TEXT,
            trace_data TEXT,
            receipt_data TEXT,
            timestamp INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_tx_block_number ON transactions(block_number);
        CREATE INDEX IF NOT EXISTS idx_tx_block_hash ON transactions(block_hash);
        CREATE INDEX IF NOT EXISTS idx_tx_from ON transactions(from_addr);
        CREATE INDEX IF NOT EXISTS idx_tx_to ON transactions(to_addr);
        CREATE INDEX IF NOT EXISTS idx_tx_timestamp ON transactions(timestamp);

        CREATE TABLE IF NOT EXISTS token_metadata (
            address TEXT PRIMARY KEY,
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
            address TEXT PRIMARY KEY,
            name TEXT NOT NULL DEFAULT '',
            abi TEXT NOT NULL DEFAULT '[]',
            is_token INTEGER NOT NULL DEFAULT 0,
            is_precompile INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS transfer_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            tx_hash TEXT NOT NULL,
            block_number INTEGER NOT NULL,
            log_index INTEGER NOT NULL DEFAULT 0,
            token_addr TEXT NOT NULL,
            from_addr TEXT NOT NULL,
            to_addr TEXT NOT NULL,
            amount TEXT NOT NULL,
            timestamp INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_transfer_tx_hash ON transfer_events(tx_hash);
        CREATE INDEX IF NOT EXISTS idx_transfer_block ON transfer_events(block_number);
        CREATE INDEX IF NOT EXISTS idx_transfer_token ON transfer_events(token_addr);
        CREATE INDEX IF NOT EXISTS idx_transfer_from ON transfer_events(from_addr);
        CREATE INDEX IF NOT EXISTS idx_transfer_to ON transfer_events(to_addr);

        CREATE TABLE IF NOT EXISTS kv (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL DEFAULT '',
            updated_at INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS token_balances (
            token_addr TEXT NOT NULL,
            holder_addr TEXT NOT NULL,
            balance TEXT NOT NULL DEFAULT '0',
            updated_at INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (token_addr, holder_addr)
        );
        CREATE INDEX IF NOT EXISTS idx_tb_holder ON token_balances(holder_addr);
        "#,
    )?;
    // Schema migrations for databases created before these columns existed.
    add_column_if_missing(
        &conn,
        "blocks",
        "base_fee",
        "base_fee TEXT NOT NULL DEFAULT '0'",
    )?;
    add_column_if_missing(&conn, "blocks", "size", "size INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(
        &conn,
        "blocks",
        "extra_data",
        "extra_data TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(&conn, "blocks", "epoch", "epoch INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(&conn, "blocks", "view", "view INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(
        &conn,
        "blocks",
        "proposer",
        "proposer TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        &conn,
        "transactions",
        "method_id",
        "method_id TEXT NOT NULL DEFAULT ''",
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
    conn.execute(
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
    "number, hash, parent_hash, timestamp, gas_used, gas_limit, base_fee, size, extra_data, \
     epoch, view, proposer, miner, tx_count, raw, created_at";

fn upsert_block(conn: &Connection, block: &Block) -> Result<()> {
    conn.execute(
        r#"
        INSERT INTO blocks (number, hash, parent_hash, timestamp, gas_used, gas_limit,
                            base_fee, size, extra_data, epoch, view, proposer,
                            miner, tx_count, raw, created_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
        ON CONFLICT(number) DO UPDATE SET
            hash=excluded.hash, parent_hash=excluded.parent_hash, timestamp=excluded.timestamp,
            gas_used=excluded.gas_used, gas_limit=excluded.gas_limit, base_fee=excluded.base_fee,
            size=excluded.size, extra_data=excluded.extra_data, epoch=excluded.epoch,
            view=excluded.view, proposer=excluded.proposer, miner=excluded.miner,
            tx_count=excluded.tx_count, raw=excluded.raw
        "#,
        params![
            block.number,
            block.hash,
            block.parent_hash,
            block.timestamp,
            block.gas_used,
            block.gas_limit,
            block.base_fee,
            block.size,
            block.extra_data,
            block.epoch,
            block.view,
            block.proposer,
            block.miner,
            block.tx_count,
            block.raw,
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
/// transfer events, and any token metadata in a single SQLite transaction.
pub fn save_block_bundle(
    db: &Db,
    block: &Block,
    txs: &[Transaction],
    transfers: &[TransferEvent],
    tokens: &[crate::tokens::TokenMeta],
) -> Result<()> {
    let mut conn = lock(db);
    let txn = conn.transaction().context("begin block transaction")?;
    upsert_block(&txn, block)?;
    for tx in txs {
        upsert_transaction(&txn, tx)?;
    }
    for transfer in transfers {
        insert_transfer(&txn, transfer)?;
    }
    apply_transfer_balances(&txn, transfers)?;
    refresh_holder_counts(&txn, transfers)?;
    for tx in txs {
        if let Some(addr) = &tx.contract_address {
            txn.execute(
                "INSERT OR IGNORE INTO contract_labels (address, name, abi, is_token, is_precompile, created_at)
                 VALUES (?1, '', '[]', 0, 0, ?2)",
                params![addr, now_ts()],
            )?;
        }
    }
    for meta in tokens {
        upsert_token_meta(&txn, meta)?;
    }
    txn.commit().context("commit block transaction")?;
    Ok(())
}

fn row_to_block(row: &rusqlite::Row) -> rusqlite::Result<Block> {
    Ok(Block {
        number: row.get(0)?,
        hash: row.get(1)?,
        parent_hash: row.get(2)?,
        timestamp: row.get(3)?,
        gas_used: row.get(4)?,
        gas_limit: row.get(5)?,
        base_fee: row.get(6)?,
        size: row.get(7)?,
        extra_data: row.get(8)?,
        epoch: row.get(9)?,
        view: row.get(10)?,
        proposer: row.get(11)?,
        miner: row.get(12)?,
        tx_count: row.get(13)?,
        raw: row.get(14)?,
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
        params![hash],
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
    conn.execute(
        r#"
        INSERT INTO transactions (
            hash, block_number, block_hash, position, from_addr, to_addr, status,
            gas_limit, gas_used, gas_price, max_fee_per_gas, max_priority_fee_per_gas,
            base_fee, contract_address, fee_token, fee_amount, nonce, nonce_key,
            value, chain_id, tx_type, method_id, input, raw, trace_data, receipt_data, timestamp, created_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28)
        ON CONFLICT(hash) DO UPDATE SET
            block_number=excluded.block_number, block_hash=excluded.block_hash, position=excluded.position,
            from_addr=excluded.from_addr, to_addr=excluded.to_addr, status=excluded.status,
            gas_limit=excluded.gas_limit, gas_used=excluded.gas_used, gas_price=excluded.gas_price,
            max_fee_per_gas=excluded.max_fee_per_gas, max_priority_fee_per_gas=excluded.max_priority_fee_per_gas,
            base_fee=excluded.base_fee, contract_address=excluded.contract_address, fee_token=excluded.fee_token,
            fee_amount=excluded.fee_amount, nonce=excluded.nonce, nonce_key=excluded.nonce_key,
            value=excluded.value, chain_id=excluded.chain_id, tx_type=excluded.tx_type, method_id=excluded.method_id,
            input=excluded.input, raw=excluded.raw, trace_data=excluded.trace_data,
            receipt_data=excluded.receipt_data, timestamp=excluded.timestamp
        "#,
        params![
            tx.hash,
            tx.block_number,
            tx.block_hash,
            tx.position,
            tx.from_addr,
            tx.to_addr,
            tx.status,
            tx.gas_limit,
            tx.gas_used,
            tx.gas_price,
            tx.max_fee_per_gas,
            tx.max_priority_fee_per_gas,
            tx.base_fee,
            tx.contract_address,
            tx.fee_token,
            tx.fee_amount,
            tx.nonce,
            tx.nonce_key,
            tx.value,
            tx.chain_id,
            tx.tx_type,
            tx.method_id,
            tx.input,
            tx.raw,
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

const TX_COLS: &str = "hash, block_number, block_hash, position, from_addr, to_addr, status, gas_limit, gas_used, gas_price, max_fee_per_gas, max_priority_fee_per_gas, base_fee, contract_address, fee_token, fee_amount, nonce, nonce_key, value, chain_id, tx_type, method_id, input, raw, trace_data, receipt_data, timestamp, created_at";

fn row_to_tx(row: &rusqlite::Row) -> rusqlite::Result<Transaction> {
    Ok(Transaction {
        hash: row.get(0)?,
        block_number: row.get(1)?,
        block_hash: row.get(2)?,
        position: row.get(3)?,
        from_addr: row.get(4)?,
        to_addr: row.get(5)?,
        status: row.get(6)?,
        gas_limit: row.get(7)?,
        gas_used: row.get(8)?,
        gas_price: row.get(9)?,
        max_fee_per_gas: row.get(10)?,
        max_priority_fee_per_gas: row.get(11)?,
        base_fee: row.get(12)?,
        contract_address: row.get(13)?,
        fee_token: row.get(14)?,
        fee_amount: row.get(15)?,
        nonce: row.get(16)?,
        nonce_key: row.get(17)?,
        value: row.get(18)?,
        chain_id: row.get(19)?,
        tx_type: row.get(20)?,
        method_id: row.get(21)?,
        input: row.get(22)?,
        raw: row.get(23)?,
        trace_data: row.get(24)?,
        receipt_data: row.get(25)?,
        timestamp: row.get(26)?,
        created_at: row.get(27)?,
    })
}

pub fn get_transaction(db: &Db, hash: &str) -> Option<Transaction> {
    let conn = lock(db);
    conn.query_row(
        &format!("SELECT {TX_COLS} FROM transactions WHERE hash=?1"),
        params![hash],
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
    let Ok(rows) = stmt.query_map(params![address, per_page as i64, offset], row_to_tx) else {
        return Vec::new();
    };
    rows.filter_map(|r| r.ok()).collect()
}

pub fn get_address_transaction_count(db: &Db, address: &str) -> i64 {
    let conn = lock(db);
    conn.query_row(
        "SELECT COUNT(*) FROM transactions WHERE from_addr=?1 OR to_addr=?1",
        params![address],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Token metadata
// ---------------------------------------------------------------------------

fn upsert_token_meta(conn: &Connection, meta: &crate::tokens::TokenMeta) -> Result<()> {
    let ts = now_ts();
    conn.execute(
        r#"
        INSERT INTO token_metadata (address, name, symbol, decimals, currency, total_supply, logo_uri, holder_count, created_at, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', 0, ?7, ?7)
        ON CONFLICT(address) DO UPDATE SET
            name=excluded.name, symbol=excluded.symbol, decimals=excluded.decimals,
            currency=excluded.currency, total_supply=excluded.total_supply, updated_at=excluded.updated_at
        "#,
        params![meta.address, meta.name, meta.symbol, meta.decimals, meta.currency, meta.total_supply, ts],
    )?;
    Ok(())
}

pub fn save_token_metadata(db: &Db, meta: &crate::tokens::TokenMeta) -> Result<()> {
    let conn = lock(db);
    upsert_token_meta(&conn, meta)
}

fn row_to_token(row: &rusqlite::Row) -> rusqlite::Result<TokenMetadata> {
    Ok(TokenMetadata {
        address: row.get(0)?,
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
        params![address],
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
        "SELECT COUNT(*) FROM transfer_events WHERE lower(token_addr)=lower(?1)",
        params![token_addr],
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

fn insert_transfer(conn: &Connection, transfer: &TransferEvent) -> Result<()> {
    conn.execute(
        "INSERT INTO transfer_events (tx_hash, block_number, log_index, token_addr, from_addr, to_addr, amount, timestamp, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            transfer.tx_hash,
            transfer.block_number,
            transfer.log_index,
            transfer.token_addr,
            transfer.from_addr,
            transfer.to_addr,
            transfer.amount,
            transfer.timestamp,
            transfer.created_at,
        ],
    )?;
    Ok(())
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
        .query_row(
            "SELECT balance FROM token_balances WHERE token_addr=?1 AND holder_addr=?2",
            params![token, holder],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
        .unwrap_or_else(|| "0".into());
    let new = bigint(&current) + delta;
    if new.sign() == num_bigint::Sign::NoSign {
        conn.execute(
            "DELETE FROM token_balances WHERE token_addr=?1 AND holder_addr=?2",
            params![token, holder],
        )?;
    } else {
        conn.execute(
            "INSERT INTO token_balances (token_addr, holder_addr, balance, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(token_addr, holder_addr) DO UPDATE SET
                 balance=excluded.balance, updated_at=excluded.updated_at",
            params![token, holder, new.to_string(), now_ts()],
        )?;
    }
    Ok(())
}

fn apply_transfer_balances(conn: &Connection, transfers: &[TransferEvent]) -> Result<()> {
    for t in transfers {
        let amount = bigint(&t.amount);
        adjust_balance(conn, &t.token_addr, &t.from_addr, &-&amount)?;
        adjust_balance(conn, &t.token_addr, &t.to_addr, &amount)?;
    }
    Ok(())
}

/// Keep `token_metadata.holder_count` in sync for tokens touched by this
/// batch of transfers. Uses the primary-key index so it stays cheap.
fn refresh_holder_counts(conn: &Connection, transfers: &[TransferEvent]) -> Result<()> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for t in transfers {
        if !seen.insert(t.token_addr.clone()) {
            continue;
        }
        let count = conn.query_row(
            "SELECT COUNT(*) FROM token_balances WHERE token_addr=?1",
            params![t.token_addr],
            |r| r.get::<_, i64>(0),
        )?;
        conn.execute(
            "UPDATE token_metadata SET holder_count=?1, updated_at=?2 WHERE address=?3",
            params![count, now_ts(), t.token_addr],
        )?;
    }
    Ok(())
}

/// Rebuild `token_balances` + holder counts from the transfer history.
/// Run once after upgrading a pre-existing database so incremental updates
/// start from a correct state.
pub fn rebuild_token_balances(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM token_balances", [])?;
    let mut stmt = conn.prepare(
        "SELECT token_addr, from_addr, to_addr, amount FROM transfer_events ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
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
            params![count, now_ts(), token],
        )?;
    }
    tracing::info!("rebuilt token balances from {n} transfer events");
    Ok(())
}

pub fn save_transfer(db: &Db, transfer: &TransferEvent) -> Result<()> {
    let conn = lock(db);
    insert_transfer(&conn, transfer)
}

fn row_to_transfer(row: &rusqlite::Row) -> rusqlite::Result<TransferEvent> {
    Ok(TransferEvent {
        id: row.get(0)?,
        tx_hash: row.get(1)?,
        block_number: row.get(2)?,
        log_index: row.get(3)?,
        token_addr: row.get(4)?,
        from_addr: row.get(5)?,
        to_addr: row.get(6)?,
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
             FROM transfer_events WHERE lower(token_addr)=lower(?1)
             ORDER BY block_number DESC, log_index DESC LIMIT ?2 OFFSET ?3",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map(
            params![token_addr, per_page as i64, offset],
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
             FROM transfer_events WHERE from_addr=?1 OR to_addr=?1
             ORDER BY block_number DESC, log_index DESC LIMIT ?2 OFFSET ?3",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map(params![address, per_page as i64, offset], row_to_transfer)
        else {
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
        let params = rusqlite::params_from_iter(chunk.iter().map(|a| a.as_str()));
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
        if let Some(meta) = metas.get(token_addr) {
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
