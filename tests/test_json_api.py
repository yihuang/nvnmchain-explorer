"""E2E tests for JSON API endpoints.

Every data endpoint supports ?format=json and Accept: application/json.
Tests verify the JSON schema: field presence, types, and semantic correctness.
"""

from __future__ import annotations

import pytest
from httpx import AsyncClient

from tests.conftest import (
    TEST_ADDRESS,
    TEST_BLOCK_NUM,
    TEST_TOKEN_ADDRESS,
    TEST_TX_HASH,
    assert_json_field,
    assert_json_match,
    assert_status,
)


class TestJSONHome:
    """GET /?format=json returns chain overview."""

    @pytest.mark.asyncio
    async def test_home_json_structure(self, cached_client: AsyncClient):
        resp = await cached_client.get("/", params={"format": "json"})
        assert_status(resp, 200)
        data = resp.json()
        assert_json_field(data, "latest_block", dict)
        assert_json_field(data, "recent_blocks", list)
        assert_json_field(data, "recent_txs", list)

    @pytest.mark.asyncio
    async def test_home_latest_block_fields(self, cached_client: AsyncClient):
        resp = await cached_client.get("/", params={"format": "json"})
        data = resp.json()
        lb = data["latest_block"]
        for field in ("number", "hash", "timestamp", "tx_count", "gas_used", "gas_limit"):
            assert field in lb, f"Missing '{field}' in latest_block"

    @pytest.mark.asyncio
    async def test_home_accept_header(self, cached_client: AsyncClient):
        resp = await cached_client.get("/", headers={"Accept": "application/json"})
        assert_status(resp, 200)
        data = resp.json()
        assert_json_field(data, "latest_block", dict)

    @pytest.mark.asyncio
    async def test_home_block_numbers(self, cached_client: AsyncClient):
        resp = await cached_client.get("/", params={"format": "json"})
        data = resp.json()
        blocks = data.get("recent_blocks", [])
        assert len(blocks) > 0, "Expected recent blocks"
        # Verify blocks are in descending order
        for i in range(len(blocks) - 1):
            assert blocks[i]["number"] >= blocks[i + 1]["number"], (
                f"Block order wrong at index {i}: {blocks[i]['number']} < {blocks[i + 1]['number']}"
            )


class TestJSONBlock:
    """GET /block/{id}?format=json returns block details."""

    @pytest.mark.asyncio
    async def test_block_by_number(self, cached_client: AsyncClient):
        resp = await cached_client.get(f"/block/{TEST_BLOCK_NUM}", params={"format": "json"})
        assert_status(resp, 200)
        data = resp.json()
        assert_json_field(data, "block", dict)
        assert_json_field(data, "transactions", list)
        assert_json_match(data["block"], number=TEST_BLOCK_NUM)

    @pytest.mark.asyncio
    async def test_block_fields(self, cached_client: AsyncClient):
        resp = await cached_client.get(f"/block/{TEST_BLOCK_NUM}", params={"format": "json"})
        data = resp.json()["block"]
        for field in (
            "number",
            "hash",
            "parent_hash",
            "timestamp",
            "miner",
            "gas_used",
            "gas_limit",
            "tx_count",
        ):
            assert field in data, f"Missing '{field}' in block"

    @pytest.mark.asyncio
    async def test_block_transactions(self, cached_client: AsyncClient):
        resp = await cached_client.get(f"/block/{TEST_BLOCK_NUM}", params={"format": "json"})
        data = resp.json()
        txs = data["transactions"]
        assert len(txs) == data["block"]["tx_count"], (
            f"tx_count mismatch: {len(txs)} vs {data['block']['tx_count']}"
        )
        for tx in txs:
            assert tx["block_number"] == TEST_BLOCK_NUM
            assert_json_field(tx, "hash", str)
            assert_json_field(tx, "from_addr", str)

    @pytest.mark.asyncio
    async def test_block_not_found(self, cached_client: AsyncClient):
        resp = await cached_client.get("/block/9999999999999999999", params={"format": "json"})
        assert_status(resp, 404)
        data = resp.json()
        assert "error" in data


class TestJSONTransaction:
    """GET /tx/{hash}?format=json returns full tx details."""

    @pytest.mark.asyncio
    async def test_tx_structure(self, cached_client: AsyncClient):
        resp = await cached_client.get(f"/tx/{TEST_TX_HASH}", params={"format": "json"})
        assert_status(resp, 200)
        data = resp.json()
        assert_json_field(data, "tx", dict)
        assert_json_field(data, "calls", list)
        assert_json_field(data, "events", list)
        assert_json_field(data, "balance_changes", list)
        assert_json_field(data, "gas_used", int)
        assert_json_field(data, "gas_limit", int)

    @pytest.mark.asyncio
    async def test_tx_core_fields(self, cached_client: AsyncClient):
        resp = await cached_client.get(f"/tx/{TEST_TX_HASH}", params={"format": "json"})
        data = resp.json()["tx"]
        for field in (
            "hash",
            "block_number",
            "from_addr",
            "to_addr",
            "gas_limit",
            "gas_used",
            "nonce",
            "tx_type",
        ):
            assert field in data, f"Missing '{field}' in transaction"
        assert data["hash"] == TEST_TX_HASH

    @pytest.mark.asyncio
    async def test_tx_status_int(self, cached_client: AsyncClient):
        resp = await cached_client.get(f"/tx/{TEST_TX_HASH}", params={"format": "json"})
        status = resp.json()["tx"].get("status")
        assert status in (0, 1), f"Invalid tx status: {status}"

    @pytest.mark.asyncio
    async def test_tx_decoded_calls(self, cached_client: AsyncClient):
        resp = await cached_client.get(f"/tx/{TEST_TX_HASH}", params={"format": "json"})
        calls = resp.json()["calls"]
        assert len(calls) > 0
        for c in calls:
            assert_json_field(c, "to", str)
            # Trace calls use "input" (full calldata); fallback calls use "data"
            assert "input" in c or "data" in c, f"Call missing 'input' or 'data': {list(c.keys())}"

    @pytest.mark.asyncio
    async def test_tx_events(self, cached_client: AsyncClient):
        resp = await cached_client.get(f"/tx/{TEST_TX_HASH}", params={"format": "json"})
        events = resp.json()["events"]
        for e in events:
            assert "name" in e or "topic0" in e, f"Event missing name: {e.keys()}"
            assert_json_field(e, "contract", str)

    @pytest.mark.asyncio
    async def test_tx_gas_values(self, cached_client: AsyncClient):
        resp = await cached_client.get(f"/tx/{TEST_TX_HASH}", params={"format": "json"})
        data = resp.json()
        assert data["gas_limit"] > 0, "gas_limit should be positive"
        assert data["gas_used"] >= 0, "gas_used should be >= 0"
        assert data["gas_price"] >= 0, "gas_price should be >= 0"

    @pytest.mark.asyncio
    async def test_tx_fee_token(self, cached_client: AsyncClient):
        resp = await cached_client.get(f"/tx/{TEST_TX_HASH}", params={"format": "json"})
        data = resp.json()
        # Tempo has no native token — fee_token is an ERC20 address or None
        fee = data.get("fee_token")
        assert fee is None or (isinstance(fee, str) and fee.startswith("0x")), (
            f"Unexpected fee_token: {fee!r}"
        )

    @pytest.mark.asyncio
    async def test_tx_not_found(self, cached_client: AsyncClient):
        bad_hash = "0x" + "00" * 32
        resp = await cached_client.get(f"/tx/{bad_hash}", params={"format": "json"})
        assert_status(resp, 404)
        assert "error" in resp.json()

    @pytest.mark.asyncio
    async def test_tx_trace_field(self, cached_client: AsyncClient):
        """JSON response includes 'trace' field (callTracer result)."""
        resp = await cached_client.get(f"/tx/{TEST_TX_HASH}", params={"format": "json"})
        data = resp.json()
        assert_json_field(data, "trace", (type(None), list))
        if data["trace"] is not None:
            # When trace is available, it mirrors calls with depth info
            assert len(data["trace"]) > 0

    @pytest.mark.asyncio
    async def test_tx_trace_call_structure(self, cached_client: AsyncClient):
        """Each trace entry has depth, type, from/to, gas fields."""
        resp = await cached_client.get(f"/tx/{TEST_TX_HASH}", params={"format": "json"})
        trace = resp.json().get("trace") or resp.json().get("calls")
        assert trace is not None and len(trace) > 0
        for entry in trace:
            assert_json_field(entry, "depth", int)
            assert_json_field(entry, "type", str)
            assert_json_field(entry, "to", str)
            assert "input" in entry or "data" in entry
            # gas fields are strings (hex from RPC)
            assert_json_field(entry, "gas", str)
            assert_json_field(entry, "gas_used", str)

    @pytest.mark.asyncio
    async def test_tx_trace_depth_ordering(self, cached_client: AsyncClient):
        """Trace entries are in DFS order: parent before children."""
        resp = await cached_client.get(f"/tx/{TEST_TX_HASH}", params={"format": "json"})
        trace = resp.json().get("trace") or resp.json().get("calls")
        if trace and len(trace) > 1:
            # Depth should never jump by more than 1 going forward
            for i in range(len(trace) - 1):
                assert trace[i + 1]["depth"] <= trace[i]["depth"] + 1, (
                    f"Depth jump at index {i}: {trace[i]['depth']} -> {trace[i + 1]['depth']}"
                )

    @pytest.mark.asyncio
    async def test_tx_balance_changes_structure(self, cached_client: AsyncClient):
        """Each balance change has address, token, change, is_fee fields."""
        resp = await cached_client.get(f"/tx/{TEST_TX_HASH}", params={"format": "json"})
        changes = resp.json()["balance_changes"]
        for c in changes:
            assert_json_field(c, "address", str)
            assert_json_field(c, "token", str)
            assert_json_field(c, "change", str)
            assert_json_field(c, "is_fee", bool)
            # change should start with + or -
            assert c["change"][0] in ("+", "-"), f"Change should start with +/-: {c['change']}"

    @pytest.mark.asyncio
    async def test_tx_balance_changes_sum_zero(self, cached_client: AsyncClient):
        """Sum of all balance changes for each token should be approx zero (except mint/burn)."""
        resp = await cached_client.get(f"/tx/{TEST_TX_HASH}", params={"format": "json"})
        changes = resp.json()["balance_changes"]
        by_token: dict[str, int] = {}
        for c in changes:
            token = c["token"].lower()
            delta = int(c["change"])
            by_token[token] = by_token.get(token, 0) + delta
        # For most txns, each token's net change is 0 (simple transfer)
        for token, net in by_token.items():
            assert net == 0, (
                f"Token {token} has non-zero net change: {net}. "
                "If the tx mints/burns, adjust this test."
            )

    @pytest.mark.asyncio
    async def test_tx_state_changes_field(self, cached_client: AsyncClient):
        """JSON response may include state_changes (empty if stateDiff not available)."""
        # Verify extract_state_changes works via the decoder
        from app.decoder import extract_state_changes

        result = extract_state_changes({})
        assert result == []
        result = extract_state_changes({"stateDiff": {"0xabc": {"slot": {}}}})
        assert len(result) == 1


class TestJSONAddress:
    """GET /address/{addr}?format=json returns address info."""

    @pytest.mark.asyncio
    async def test_address_structure(self, cached_client: AsyncClient):
        resp = await cached_client.get(f"/address/{TEST_ADDRESS}", params={"format": "json"})
        assert_status(resp, 200)
        data = resp.json()
        assert_json_field(data, "address", str)
        assert_json_field(data, "type", str)
        assert_json_field(data, "transactions", list)

    @pytest.mark.asyncio
    async def test_address_type_classified(self, cached_client: AsyncClient):
        resp = await cached_client.get(f"/address/{TEST_ADDRESS}", params={"format": "json"})
        data = resp.json()
        assert data["type"] in ("eoa", "precompile", "token"), f"Unknown type: {data['type']}"

    @pytest.mark.asyncio
    async def test_address_checksummed(self, cached_client: AsyncClient):
        resp = await cached_client.get(f"/address/{TEST_ADDRESS}", params={"format": "json"})
        data = resp.json()
        from eth_utils import to_checksum_address

        assert data["address"] == to_checksum_address(TEST_ADDRESS), "Address should be checksummed"

    @pytest.mark.asyncio
    async def test_address_invalid(self, cached_client: AsyncClient):
        resp = await cached_client.get("/address/not-an-address", params={"format": "json"})
        assert_status(resp, 400)
        assert "error" in resp.json()


class TestJSONToken:
    """GET /token/{addr}?format=json returns token metadata."""

    @pytest.mark.asyncio
    async def test_token_structure(self, cached_client: AsyncClient):
        resp = await cached_client.get(f"/token/{TEST_TOKEN_ADDRESS}", params={"format": "json"})
        assert_status(resp, 200)
        data = resp.json()
        assert_json_field(data, "token", dict)
        assert_json_field(data, "transfers", list)

    @pytest.mark.asyncio
    async def test_token_meta_fields(self, cached_client: AsyncClient):
        resp = await cached_client.get(f"/token/{TEST_TOKEN_ADDRESS}", params={"format": "json"})
        meta = resp.json()["token"]
        for field in ("address", "name", "symbol", "decimals", "total_supply"):
            assert field in meta, f"Missing '{field}' in token metadata"

    @pytest.mark.asyncio
    async def test_pathusd_known(self, cached_client: AsyncClient):
        resp = await cached_client.get(f"/token/{TEST_TOKEN_ADDRESS}", params={"format": "json"})
        meta = resp.json()["token"]
        assert meta["name"] == "pathUSD"
        assert meta["symbol"] == "pathUSD"
        # pathUSD uses 6 decimals (like USDC), not the default 18
        assert meta["decimals"] == 6, f"pathUSD decimals: expected 6, got {meta['decimals']}"

    @pytest.mark.asyncio
    async def test_token_invalid(self, cached_client: AsyncClient):
        resp = await cached_client.get("/token/0xinvalid", params={"format": "json"})
        assert_status(resp, 400)
        assert "error" in resp.json()


class TestJSONTokensList:
    """GET /tokens?format=json returns the token index."""

    @pytest.mark.asyncio
    async def test_tokens_list_structure(self, cached_client: AsyncClient):
        resp = await cached_client.get("/tokens", params={"format": "json"})
        assert_status(resp, 200)
        data = resp.json()
        assert_json_field(data, "tokens", list)
        assert_json_field(data, "total", int)

    @pytest.mark.asyncio
    async def test_tokens_contains_pathusd(self, cached_client: AsyncClient):
        resp = await cached_client.get("/tokens", params={"format": "json"})
        tokens = resp.json()["tokens"]
        pathusd = [t for t in tokens if t.get("name") == "pathUSD"]
        assert len(pathusd) > 0, "pathUSD should be in token list"
        assert pathusd[0]["symbol"] == "pathUSD"


class TestJSONSearch:
    """GET /search?q=...&format=json returns match info."""

    @pytest.mark.asyncio
    async def test_search_block(self, cached_client: AsyncClient):
        resp = await cached_client.get(
            "/search", params={"q": str(TEST_BLOCK_NUM), "format": "json"}
        )
        assert_status(resp, 200)
        data = resp.json()
        assert_json_field(data, "match", dict)
        assert data["match"]["type"] == "block"
        assert data["match"]["id"] == str(TEST_BLOCK_NUM)

    @pytest.mark.asyncio
    async def test_search_address(self, cached_client: AsyncClient):
        resp = await cached_client.get("/search", params={"q": TEST_ADDRESS, "format": "json"})
        assert_status(resp, 200)
        data = resp.json()
        assert data["match"]["type"] == "address"

    @pytest.mark.asyncio
    async def test_search_tx(self, cached_client: AsyncClient):
        resp = await cached_client.get("/search", params={"q": TEST_TX_HASH, "format": "json"})
        assert_status(resp, 200)
        data = resp.json()
        assert data["match"]["type"] == "transaction"

    @pytest.mark.asyncio
    async def test_search_token(self, cached_client: AsyncClient):
        resp = await cached_client.get(
            "/search", params={"q": TEST_TOKEN_ADDRESS, "format": "json"}
        )
        assert_status(resp, 200)
        data = resp.json()
        assert data["match"]["type"] in ("address", "token"), (
            f"Got type {data['match']['type']}, expected address or token"
        )

    @pytest.mark.asyncio
    async def test_search_no_match(self, cached_client: AsyncClient):
        resp = await cached_client.get(
            "/search", params={"q": "zzz_nonexistent_12345", "format": "json"}
        )
        assert_status(resp, 200)
        data = resp.json()
        assert data["match"] is None

    @pytest.mark.asyncio
    async def test_search_empty(self, cached_client: AsyncClient):
        resp = await cached_client.get("/search", params={"q": "", "format": "json"})
        assert_status(resp, 200)
        data = resp.json()
        assert data["match"] is None


class TestJSONBlocksList:
    """GET /blocks?format=json returns the block index."""

    @pytest.mark.asyncio
    async def test_blocks_list_structure(self, cached_client: AsyncClient):
        resp = await cached_client.get("/blocks", params={"format": "json"})
        assert_status(resp, 200)
        data = resp.json()
        assert_json_field(data, "blocks", list)
        assert_json_field(data, "total_blocks", int)

    @pytest.mark.asyncio
    async def test_block_fields_in_list(self, cached_client: AsyncClient):
        resp = await cached_client.get("/blocks", params={"format": "json"})
        blocks = resp.json()["blocks"]
        assert len(blocks) > 0, "Expected non-empty blocks list"
        b = blocks[0]
        for field in ("number", "hash", "timestamp", "tx_count"):
            assert field in b, f"Missing '{field}' in block list entry"
