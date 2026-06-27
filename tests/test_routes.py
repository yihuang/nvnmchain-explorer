"""E2E tests: all routes return correct HTTP status codes."""

from __future__ import annotations

import pytest
from httpx import AsyncClient

from tests.conftest import (
    TEST_ADDRESS,
    TEST_BLOCK_NUM,
    TEST_TOKEN_ADDRESS,
    TEST_TX_HASH,
    assert_status,
)


class TestRouteStatus:
    """Every route handler returns the expected HTTP status code."""

    @pytest.mark.asyncio
    async def test_home_page(self, cached_client: AsyncClient):
        resp = await cached_client.get("/")
        assert_status(resp, 200, "Home page")

    @pytest.mark.asyncio
    async def test_blocks_list(self, cached_client: AsyncClient):
        resp = await cached_client.get("/blocks")
        assert_status(resp, 200, "Blocks list")

    @pytest.mark.asyncio
    async def test_tokens_list(self, cached_client: AsyncClient):
        resp = await cached_client.get("/tokens")
        assert_status(resp, 200, "Tokens list")

    @pytest.mark.asyncio
    async def test_block_by_number(self, cached_client: AsyncClient):
        resp = await cached_client.get(f"/block/{TEST_BLOCK_NUM}")
        assert_status(resp, 200, f"Block #{TEST_BLOCK_NUM}")

    @pytest.mark.asyncio
    async def test_block_by_hash(self, cached_client: AsyncClient):
        # First get the actual hash from the block page
        resp = await cached_client.get(f"/block/{TEST_BLOCK_NUM}")
        assert_status(resp, 200)
        html = resp.text
        # Extract hash from block page
        import re

        match = re.search(r'<div class="mono text-xs break-all">(0x[a-f0-9]+)</div>', html)
        if match:
            block_hash = match.group(1)
            resp2 = await cached_client.get(f"/block/{block_hash}")
            assert_status(resp2, 200, f"Block by hash {block_hash[:20]}")

    @pytest.mark.asyncio
    async def test_transaction(self, cached_client: AsyncClient):
        resp = await cached_client.get(f"/tx/{TEST_TX_HASH}")
        assert_status(resp, 200, f"Tx {TEST_TX_HASH[:20]}")

    @pytest.mark.asyncio
    async def test_transaction_tabs(self, cached_client: AsyncClient):
        """All tx tabs render successfully."""
        for tab in ["overview", "balances", "calls", "events", "raw"]:
            resp = await cached_client.get(f"/tx/{TEST_TX_HASH}?tab={tab}")
            assert_status(resp, 200, f"Tx tab={tab}")

    @pytest.mark.asyncio
    async def test_receipt_redirect(self, cached_client: AsyncClient):
        resp = await cached_client.get(f"/receipt/{TEST_TX_HASH}")
        assert_status(resp, 307, "Receipt redirect")

    @pytest.mark.asyncio
    async def test_address(self, cached_client: AsyncClient):
        resp = await cached_client.get(f"/address/{TEST_ADDRESS}")
        assert_status(resp, 200, f"Address {TEST_ADDRESS[:20]}")

    @pytest.mark.asyncio
    async def test_address_tabs(self, cached_client: AsyncClient):
        for tab in ["transactions", "transfers", "holdings"]:
            resp = await cached_client.get(f"/address/{TEST_ADDRESS}?tab={tab}")
            assert_status(resp, 200, f"Address tab={tab}")

    @pytest.mark.asyncio
    async def test_token(self, cached_client: AsyncClient):
        resp = await cached_client.get(f"/token/{TEST_TOKEN_ADDRESS}")
        assert_status(resp, 200, f"Token {TEST_TOKEN_ADDRESS[:20]}")

    @pytest.mark.asyncio
    async def test_token_tabs(self, cached_client: AsyncClient):
        for tab in ["transactions", "transfers"]:
            resp = await cached_client.get(f"/token/{TEST_TOKEN_ADDRESS}?tab={tab}")
            assert_status(resp, 200, f"Token tab={tab}")

    @pytest.mark.asyncio
    async def test_search_fallback(self, cached_client: AsyncClient):
        resp = await cached_client.get("/search", params={"q": "nonexistent123xyz"})
        assert_status(resp, 200, "Search fallback")

    @pytest.mark.asyncio
    async def test_search_empty(self, cached_client: AsyncClient):
        resp = await cached_client.get("/search", params={"q": ""})
        assert_status(resp, 307, "Empty search redirect")


class TestRouteErrors:
    """Error pages for invalid/missing resources."""

    @pytest.mark.asyncio
    async def test_nonexistent_block(self, cached_client: AsyncClient):
        resp = await cached_client.get("/block/9999999999999999999")
        assert_status(resp, 404, "Non-existent block")

    @pytest.mark.asyncio
    async def test_nonexistent_tx(self, cached_client: AsyncClient):
        resp = await cached_client.get(
            "/tx/0x0000000000000000000000000000000000000000000000000000000000000000"
        )
        assert_status(resp, 404, "Non-existent tx")

    @pytest.mark.asyncio
    async def test_nonexistent_address(self, cached_client: AsyncClient):
        resp = await cached_client.get("/address/0x0000000000000000000000000000000000000000")
        assert_status(resp, 200, "Zero address exists (is valid hex)")

    @pytest.mark.asyncio
    async def test_bad_address_format(self, cached_client: AsyncClient):
        resp = await cached_client.get("/address/not-an-address")
        assert_status(resp, 404, "Bad address format")

    @pytest.mark.asyncio
    async def test_nonexistent_route(self, cached_client: AsyncClient):
        resp = await cached_client.get("/nonexistent")
        assert_status(resp, 404, "Non-existent route")
