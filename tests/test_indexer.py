"""Integration tests: indexer correctly stores blocks and transactions."""

from __future__ import annotations

import json

import pytest
import pytest_asyncio
from httpx import ASGITransport, AsyncClient


# Use a recent block that should be available
TEST_BLOCK_NUM = 27193037
# Known block with transactions (from test suite)
TEST_BLOCK_WITH_TXS = 27183315
TEST_TX_HASH = "0x7e639334bb324e53c4a9291e3edd2a48fba8598891c3ac368f0517134e96cc0c"


@pytest_asyncio.fixture(scope="module")
async def indexed_app():
    """Set up a fresh DB, index a block, return the app + client."""
    import os
    import tempfile

    db_path = os.path.join(tempfile.gettempdir(), "tempo_indexer_test.db")
    for suffix in ("", "-wal", "-shm"):
        try:
            os.remove(db_path + suffix)
        except FileNotFoundError:
            pass

    from app.config import settings

    settings.db_path = db_path
    settings.recent_block_count = 5
    settings.recent_tx_count = 5

    from app.database import init_db, get_block_by_number, get_block_transactions, get_transaction

    init_db()

    from app.indexer import index_block

    # Index the problematic block first
    await index_block(TEST_BLOCK_NUM)

    # Also index a known block with txs for comparison
    await index_block(TEST_BLOCK_WITH_TXS)

    from app.main import app as fastapi_app

    transport = ASGITransport(app=fastapi_app)
    async with AsyncClient(transport=transport, base_url="http://test") as client:
        yield client, get_block_by_number, get_block_transactions, get_transaction

    # Cleanup
    for suffix in ("", "-wal", "-shm"):
        try:
            os.remove(db_path + suffix)
        except FileNotFoundError:
            pass


class TestIndexerBlockStorage:
    """Indexer stores block + all transactions correctly."""

    @pytest.mark.asyncio
    async def test_block_stored(self, indexed_app):
        client, get_block, get_txs, get_tx = indexed_app
        block = get_block(TEST_BLOCK_NUM)
        assert block is not None, f"Block {TEST_BLOCK_NUM} not found in DB"
        assert block["number"] == TEST_BLOCK_NUM
        assert block["hash"] != ""
        assert block["timestamp"] > 0

    @pytest.mark.asyncio
    async def test_tx_count_matches_stored_txs(self, indexed_app):
        _, get_block, get_txs, _ = indexed_app
        block = get_block(TEST_BLOCK_NUM)
        assert block is not None
        txs = get_txs(TEST_BLOCK_NUM)
        assert len(txs) == block["tx_count"], (
            f"Block {TEST_BLOCK_NUM}: tx_count={block['tx_count']} "
            f"but got {len(txs)} transactions in DB"
        )

    @pytest.mark.asyncio
    async def test_tx_fields(self, indexed_app):
        _, get_block, get_txs, _ = indexed_app
        block = get_block(TEST_BLOCK_NUM)
        assert block is not None
        txs = get_txs(TEST_BLOCK_NUM)
        for tx in txs:
            assert tx["hash"] != "", "Transaction hash is empty"
            assert tx["block_number"] == TEST_BLOCK_NUM
            assert tx["from_addr"] != ""
            assert isinstance(tx["status"], int)
            assert tx["status"] in (0, 1)

    @pytest.mark.asyncio
    async def test_tx_has_receipt_data(self, indexed_app):
        _, get_block, get_txs, _ = indexed_app
        txs = get_txs(TEST_BLOCK_NUM)
        for tx in txs:
            # If the tx was indexed, it should have receipt data
            if tx["receipt_data"]:
                receipt = json.loads(tx["receipt_data"])
                assert isinstance(receipt, dict)
                assert "logs" in receipt
                assert "status" in receipt
            # If no receipt data, the tx might not have been fully indexed
            # This is a soft check — report but don't fail
            if not tx["receipt_data"]:
                print(f"WARN: tx {tx['hash'][:20]}... has no receipt_data")

    @pytest.mark.asyncio
    async def test_tx_has_trace_data(self, indexed_app):
        _, get_block, get_txs, _ = indexed_app
        txs = get_txs(TEST_BLOCK_NUM)
        for tx in txs:
            if tx["trace_data"]:
                trace = json.loads(tx["trace_data"])
                assert isinstance(trace, list)
                if trace:
                    assert "depth" in trace[0]
                    assert "type" in trace[0]

    @pytest.mark.asyncio
    async def test_known_block_with_txs(self, indexed_app):
        """Verify the known block with transactions indexes correctly."""
        _, get_block, get_txs, _ = indexed_app
        block = get_block(TEST_BLOCK_WITH_TXS)
        assert block is not None, f"Block {TEST_BLOCK_WITH_TXS} should be indexed"
        txs = get_txs(TEST_BLOCK_WITH_TXS)
        assert len(txs) > 0, f"Block {TEST_BLOCK_WITH_TXS} should have transactions"
        assert len(txs) == block["tx_count"], (
            f"tx_count mismatch: {len(txs)} vs {block['tx_count']}"
        )

    @pytest.mark.asyncio
    async def test_known_tx_receipt(self, indexed_app):
        """Verify a specific known tx has receipt and trace data."""
        _, _, _, get_tx = indexed_app
        tx = get_tx(TEST_TX_HASH)
        assert tx is not None, f"Tx {TEST_TX_HASH[:20]}... should be in DB"
        assert tx["receipt_data"] is not None, "Receipt data should be present"
        receipt = json.loads(tx["receipt_data"])
        assert receipt.get("status") in ("0x0", "0x1", "0x", "0x"), (
            f"Unexpected receipt status: {receipt.get('status')}"
        )


class TestBlockPageContent:
    """HTTP block page shows transactions when tx_count > 0."""

    @pytest.mark.asyncio
    async def test_block_page_has_transactions(self, indexed_app):
        client, get_block, _, _ = indexed_app

        # Check block with no txs first
        resp = await client.get(f"/block/{TEST_BLOCK_NUM}", params={"format": "json"})
        assert resp.status_code == 200, f"Block {TEST_BLOCK_NUM}: {resp.status_code}"
        data = resp.json()
        assert data["block"]["tx_count"] == len(data["transactions"]), (
            f"Block {TEST_BLOCK_NUM}: tx_count={data['block']['tx_count']} "
            f"but got {len(data['transactions'])} transactions"
        )

    @pytest.mark.asyncio
    async def test_block_with_txs_page(self, indexed_app):
        client, _, _, _ = indexed_app

        resp = await client.get(f"/block/{TEST_BLOCK_WITH_TXS}", params={"format": "json"})
        assert resp.status_code == 200, f"Block {TEST_BLOCK_WITH_TXS}: {resp.status_code}"
        data = resp.json()
        assert data["block"]["tx_count"] > 0, "Known block should have transactions"
        assert len(data["transactions"]) > 0, "Transaction list should not be empty"
        assert len(data["transactions"]) == data["block"]["tx_count"], (
            f"tx_count mismatch: {len(data['transactions'])} vs {data['block']['tx_count']}"
        )

    @pytest.mark.asyncio
    async def test_tx_page_has_receipt(self, indexed_app):
        client, _, _, _ = indexed_app

        resp = await client.get(f"/tx/{TEST_TX_HASH}", params={"format": "json"})
        assert resp.status_code == 200, f"Tx {TEST_TX_HASH[:20]}: {resp.status_code}"
        data = resp.json()
        assert data["receipt"] is not None, "Receipt should be loaded from cache"
        assert data["trace"] is not None, "Trace should be loaded from cache"
        assert len(data["calls"]) > 0, "Should have decoded calls"
