"""Test fixtures for Tempo Explorer E2E tests."""

from __future__ import annotations

import os
import tempfile
from collections.abc import AsyncGenerator

import pytest
import pytest_asyncio
from httpx import ASGITransport, AsyncClient

# ── Test configuration ──────────────────────────────────────────────

TEST_DB = os.path.join(tempfile.gettempdir(), "tempo_explorer_test.db")

# Known test data on mainnet
TEST_BLOCK_NUM = 27183315
TEST_TX_HASH = "0x7e639334bb324e53c4a9291e3edd2a48fba8598891c3ac368f0517134e96cc0c"
TEST_ADDRESS = "0x306dcc6b034f4a6c7898ad6b7375bf9f47c0f428"
TEST_TOKEN_ADDRESS = "0x20C0000000000000000000000000000000000000"
TEST_KNOWN_BLOCK_HASH = "0xb5c55aff74703a09ed966bcb970479774543ff2be49b7980e23e2e79a5bb5a49"


# ── Monkey-patch DB path BEFORE any imports touch settings ──────────

# Must happen before `app.main` startup runs init_db()
os.environ.setdefault("EXPLORER_DB_PATH", TEST_DB)

# Clear any stale DB
try:
    os.remove(TEST_DB)
except FileNotFoundError:
    pass
try:
    os.remove(TEST_DB + "-wal")
except FileNotFoundError:
    pass
try:
    os.remove(TEST_DB + "-shm")
except FileNotFoundError:
    pass


# ── Fixtures ────────────────────────────────────────────────────────


@pytest.fixture(scope="session")
def event_loop():
    """Session-scoped event loop for async fixtures."""
    import asyncio

    loop = asyncio.new_event_loop()
    yield loop
    loop.close()


@pytest_asyncio.fixture(scope="session")
async def app():
    """Return the FastAPI app with test DB configured."""
    from app.config import settings

    settings.db_path = TEST_DB
    settings.recent_block_count = 5
    settings.recent_tx_count = 5

    from app.database import init_db
    from app.main import app as fastapi_app

    init_db()
    yield fastapi_app

    # Cleanup
    try:
        os.remove(TEST_DB)
    except FileNotFoundError:
        pass


@pytest_asyncio.fixture(scope="session")
async def client(app) -> AsyncGenerator[AsyncClient]:
    """HTTPX async test client against the FastAPI app."""
    transport = ASGITransport(app=app)
    async with AsyncClient(transport=transport, base_url="http://test") as ac:
        yield ac


@pytest_asyncio.fixture(scope="session")
async def cached_client(client: AsyncClient) -> AsyncClient:
    """Pre-populate cache with known data using the indexer, then return the client.

    This fixture indexes a real block and transaction so subsequent
    tests can assert content without hitting RPC every time.
    """
    from app.indexer import index_block
    from app.tokens import fetch_token_metadata
    from app.database import save_token_metadata

    # Index the known block (fetches block, traces, receipts from live RPC)
    await index_block(TEST_BLOCK_NUM)

    # Fetch and cache token metadata for the fee token
    try:
        meta = await fetch_token_metadata(TEST_TOKEN_ADDRESS)
        save_token_metadata(meta)
    except Exception:
        pass

    return client


# ── Assertion helpers ───────────────────────────────────────────────


def assert_status(resp, expected: int = 200, msg: str | None = None):
    """Assert HTTP status with optional context message."""
    label = msg or f"{resp.url}"
    assert resp.status_code == expected, f"{label}: got {resp.status_code}, expected {expected}"


def assert_contains(html: str, *patterns: str):
    """Assert all patterns appear in HTML."""
    for p in patterns:
        assert p in html, f"Expected '{p}' in response"


def assert_not_contains(html: str, *patterns: str):
    """Assert no pattern appears in HTML."""
    for p in patterns:
        assert p not in html, f"Did not expect '{p}' in response"


# ── JSON API helpers ────────────────────────────────────────────────


def json_get(client, path: str, params: dict | None = None) -> dict:
    """GET a route with ?format=json and return parsed JSON body."""
    import asyncio

    q = params or {}
    q["format"] = "json"
    coro = client.get(path, params=q)
    resp = asyncio.get_event_loop().run_until_complete(coro)
    assert resp.status_code == 200, f"JSON GET {path}: {resp.status_code}"
    try:
        return resp.json()
    except Exception as e:
        raise AssertionError(f"JSON decode failed for {path}: {resp.text[:200]}") from e


def assert_json_field(data: dict, field: str, expected_type=None):
    """Assert a JSON field exists and optionally has the right type."""
    assert field in data, f"Missing JSON field '{field}' in {list(data.keys())}"
    if expected_type:
        assert isinstance(data[field], expected_type), (
            f"Field '{field}' should be {expected_type}, got {type(data[field]).__name__}"
        )


def assert_json_match(data: dict, **checks):
    """Assert JSON dict matches key=value pairs."""
    for key, expected in checks.items():
        assert data.get(key) == expected, (
            f"JSON field '{key}': expected {expected!r}, got {data.get(key)!r}"
        )
