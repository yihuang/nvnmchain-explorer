"""Regression: old DB schema must survive schema migration without crash.

Tests _migrate_schema directly and queries the DB — no HTTP layer involved.
Each test saves/restores global state to avoid polluting other tests.
"""

from __future__ import annotations

import os
import sqlite3
import tempfile

import pytest


@pytest.fixture
def old_db():
    """Set up an old-schema DB (no trace_data, no receipt_data, nonce_key as INTEGER)."""
    db_path = os.path.join(tempfile.gettempdir(), "tempo_old_schema_regression.db")
    for suffix in ("", "-wal", "-shm"):
        try:
            os.remove(db_path + suffix)
        except FileNotFoundError:
            pass

    conn = sqlite3.connect(db_path)
    conn.executescript("""
        CREATE TABLE blocks (number INTEGER PRIMARY KEY, hash TEXT, parent_hash TEXT,
            timestamp INTEGER, gas_used INTEGER, gas_limit INTEGER, miner TEXT,
            tx_count INTEGER, raw TEXT, created_at INTEGER);
        CREATE TABLE transactions (hash TEXT PRIMARY KEY, block_number INTEGER,
            block_hash TEXT, position INTEGER, from_addr TEXT, to_addr TEXT,
            status INTEGER, gas_limit INTEGER, gas_used INTEGER, gas_price TEXT,
            max_fee_per_gas TEXT, max_priority_fee_per_gas TEXT, base_fee TEXT,
            fee_token TEXT, fee_amount TEXT, nonce INTEGER, nonce_key INTEGER,
            value TEXT, chain_id INTEGER, tx_type INTEGER, input TEXT, raw TEXT,
            timestamp INTEGER, created_at INTEGER);
        INSERT INTO blocks (number, hash, parent_hash, timestamp,
            gas_used, gas_limit, miner, tx_count, raw, created_at)
            VALUES (27183315, '0xabc', '0xdef', 1782525872, 0, 0, '', 1, '{}', 0);
        INSERT INTO transactions (hash, block_number, block_hash, position,
            from_addr, to_addr, status, gas_limit, gas_used, gas_price,
            max_fee_per_gas, max_priority_fee_per_gas, base_fee, fee_token,
            fee_amount, nonce, nonce_key, value, chain_id, tx_type, input,
            raw, timestamp, created_at)
            VALUES ('0x7e639334bb324e53c4a9291e3edd2a48fba8598891c3ac368f0517134e96cc0c',
            27183315, '0xabc', 0,
            '0xf080385c4d3e08859cfb26b379d3752b7e51395a',
            '0x2ae2182f745b10ab9c11ddaace4028930cc63e93',
            1, 532166, 320827, '0x4a817c800', '0x4a817c800', '0x0',
            '0x4a817c800', NULL, '0', 102, 0, '0', 4217, 118,
            '0x', NULL, 1782525872, 0);
    """)
    conn.close()
    yield db_path
    for suffix in ("", "-wal", "-shm"):
        try:
            os.remove(db_path + suffix)
        except FileNotFoundError:
            pass


def _with_db(old_db: str, fn):
    """Run fn with app DB configured to old_db, then restore."""
    from app.config import settings
    import app.database as db_mod

    saved_path = settings.db_path
    settings.db_path = old_db
    db_mod._engine = None
    db_mod._engine_path = None

    try:
        fn()
    finally:
        settings.db_path = saved_path
        db_mod._engine = None
        db_mod._engine_path = None


def test_migrate_schema_adds_columns(old_db):
    def check():
        from app.main import _migrate_schema

        _migrate_schema()

        conn = sqlite3.connect(old_db)
        cursor = conn.execute("PRAGMA table_info(transactions)")
        cols = {row[1]: row[2] for row in cursor.fetchall()}
        conn.close()
        assert "trace_data" in cols
        assert "receipt_data" in cols

    _with_db(old_db, check)


def test_query_after_migration_succeeds(old_db):
    def check():
        from app.main import _migrate_schema

        _migrate_schema()

        from app.database import get_transaction

        tx = get_transaction("0x7e639334bb324e53c4a9291e3edd2a48fba8598891c3ac368f0517134e96cc0c")
        assert tx is not None
        assert tx["trace_data"] is None
        assert tx["receipt_data"] is None

    _with_db(old_db, check)


def test_block_query_after_migration(old_db):
    def check():
        from app.main import _migrate_schema

        _migrate_schema()

        from app.database import get_block_by_number

        block = get_block_by_number(27183315)
        assert block is not None
        assert block["tx_count"] == 1

    _with_db(old_db, check)


def test_extract_calls_with_null_raw(old_db):
    def check():
        from app.main import _migrate_schema

        _migrate_schema()

        from app.database import get_transaction
        from app.decoder import extract_calls

        tx = get_transaction("0x7e639334bb324e53c4a9291e3edd2a48fba8598891c3ac368f0517134e96cc0c")
        assert tx is not None
        calls = extract_calls(tx, None)
        assert isinstance(calls, list)

    _with_db(old_db, check)
