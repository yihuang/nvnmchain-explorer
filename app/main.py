"""Tempo Explorer - FastAPI web application."""

from __future__ import annotations

import json
import logging
import math
import sys
import time
from contextlib import asynccontextmanager
from datetime import datetime

from eth_utils import to_checksum_address
from fastapi import FastAPI, Query, Request
from fastapi.responses import HTMLResponse, JSONResponse, RedirectResponse
from fastapi.templating import Jinja2Templates

from .config import settings
from .contracts import identify_address
from .database import (
    get_address_holdings,
    get_address_transaction_count,
    get_address_transactions,
    get_address_transfers,
    get_all_tokens,
    get_block_by_hash,
    get_block_by_number,
    get_block_transactions,
    get_latest_block,
    get_recent_blocks,
    get_recent_transactions,
    get_token_count,
    get_token_metadata,
    get_token_transfers,
    get_transaction,
    init_db,
    save_token_metadata,
)
from .decoder import (
    decode_event,
    decode_function_call,
    extract_balance_changes,
    extract_calls,
)
from .indexer import start as start_indexer
from .tokens import (
    fetch_token_metadata,
    format_token_amount,
    format_token_amount_with_symbol,
)

logger = logging.getLogger("tempo.http")
logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
    stream=sys.stderr,
    force=True,
)

@asynccontextmanager
async def lifespan(app: FastAPI):
    """Startup: migrate schema, init db, start indexer. Shutdown: stop indexer."""
    _migrate_schema()
    init_db()
    indexer_task = start_indexer()

    yield  # server runs here

    # Shutdown: cancel the indexer task
    if indexer_task is not None:
        indexer_task.cancel()


app = FastAPI(title="Tempo Explorer", lifespan=lifespan)


class NotFound(Exception):
    """Raised when a requested resource is not found.

    The exception handler renders the 404 page for HTML requests
    or returns a JSON error for API requests.
    """

    def __init__(self, type: str, id: str, message: str | None = None):
        self.type = type
        self.id = id
        self.message = message


@app.exception_handler(NotFound)
async def not_found_handler(request: Request, exc: NotFound):
    if _wants_json(request):
        return JSONResponse({"error": exc.message or f"{exc.type} not found"}, status_code=404)
    return templates.TemplateResponse(
        request,
        "404.html",
        {"request": request, "type": exc.type, "id": exc.id},
        status_code=404,
    )

@app.exception_handler(Exception)
async def log_exceptions(request: Request, exc: Exception):
    """Log all unhandled exceptions with full traceback and return 500."""
    logger.exception("500 error processing %s %s", request.method, request.url)
    if _wants_json(request):
        return JSONResponse({"error": "Internal server error"}, status_code=500)
    return templates.TemplateResponse(
        request,
        "500.html",
        {"request": request},
        status_code=500,
    )

# Templates
templates = Jinja2Templates(directory="app/templates")
templates.env.filters["to_checksum"] = to_checksum_address


def _bytes_to_hex(val: bytes | int | float | None) -> str:
    """Convert bytes or integer to a 0x-prefixed hex string."""
    if val is None:
        return "0x0"
    if isinstance(val, bytes):
        return "0x" + val.hex() if val else "0x0"
    # Integer/float fallback (pre-migration data)
    if val == 0:
        return "0x0"
    return hex(int(val))


templates.env.filters["bytes_to_hex"] = _bytes_to_hex


def _timestamp_to_date(ts: int) -> str:
    return datetime.fromtimestamp(ts).strftime("%m/%d/%y %H:%M:%S")


templates.env.filters["timestamp_to_date"] = _timestamp_to_date


# ── Helpers ─────────────────────────────────────────────────────────
def _wants_json(request: Request) -> bool:
    """Check if client wants JSON via Accept header or ?format=json query."""
    accept = request.headers.get("accept", "")
    if "application/json" in accept:
        return True
    return request.query_params.get("format") == "json"


def _hex_int(val: str | int | None, default: int = 0) -> int:
    if val is None:
        return default
    if isinstance(val, int):
        return val
    if isinstance(val, str):
        if val.startswith("0x"):
            return int(val, 16)
        return int(val) if val else default


def _format_time_ago(timestamp: int | str) -> str:
    """Format a Unix timestamp as a relative time string."""
    try:
        if isinstance(timestamp, str):
            ts = int(timestamp, 16) if timestamp.startswith("0x") else int(timestamp)
        else:
            ts = timestamp
    except (ValueError, TypeError):
        return "unknown"
    now = time.time()
    diff = now - ts
    if diff < 0:
        return "just now"
    if diff < 60:
        return f"{int(diff)}s ago"
    if diff < 3600:
        return f"{int(diff // 60)}m ago"
    if diff < 86400:
        return f"{int(diff // 3600)}h ago"
    if diff < 2592000:
        return f"{int(diff // 86400)}d ago"
    if diff < 31536000:
        return f"{int(diff // 2592000)}mo ago"
    return f"{int(diff // 31536000)}y ago"


def _truncate_hash(h: str | None, prefix: int = 8, suffix: int = 4) -> str:
    if not h:
        return ""
    if len(h) > prefix + suffix + 3:
        return f"{h[: prefix + 2]}…{h[-suffix:]}"
    return h




def _template_ctx(request: Request, **data) -> dict:
    """Build a template context dict with common helpers merged into *data*."""
    return {
        "request": request,
        **data,
        "format_time_ago": _format_time_ago,
        "truncate_hash": _truncate_hash,
        "get_block_url": get_block_url,
        "get_tx_url": get_tx_url,
        "get_address_url": get_address_url,
        "get_token_url": get_token_url,
        "identify_address": identify_address,
        "format_token_amount": format_token_amount,
        "format_token_amount_with_symbol": format_token_amount_with_symbol,
        "to_checksum": to_checksum_address,
    }


# ── Event handlers ──────────────────────────────────────────────────

def _migrate_schema() -> None:
    """Run Alembic migrations.

    - Fresh DB: tables don't exist → Alembic runs all migrations from scratch.
    - Existing DB (no alembic_version): tables exist but not migration-tracked →
      stamp initial state then apply pending migrations.
    - Migration-tracked DB: alembic_version exists → apply pending migrations only.
    """
    from alembic.command import stamp
    from alembic.command import upgrade as alembic_upgrade
    from alembic.config import Config
    from sqlalchemy import inspect

    cfg = Config("alembic.ini")
    cfg.set_main_option("sqlalchemy.url", f"sqlite:///{settings.db_path}")

    from .database import get_engine

    engine = get_engine()
    try:
        tables = inspect(engine).get_table_names()
    except Exception:
        # Corrupted or unreadable DB — delete and start fresh
        import os

        for suffix in ("", "-wal", "-shm"):
            try:
                os.remove(settings.db_path + suffix)
            except FileNotFoundError:
                pass
        # Reset engine so the new DB is picked up
        import app.database as db_mod

        db_mod._engine = None
        db_mod._engine_path = None
        engine = get_engine()
        tables = []

    if "alembic_version" in tables:
        alembic_upgrade(cfg, "head")
    elif tables:
        stamp(cfg, "fb79e1cdcb0f")
        alembic_upgrade(cfg, "head")
    else:
        alembic_upgrade(cfg, "head")

# ── Routes ──────────────────────────────────────────────────────────


@app.get("/")
async def home(request: Request):
    """Home page with recent blocks and transactions."""
    data = {
        "latest_block": get_latest_block(),
        "recent_blocks": get_recent_blocks(settings.recent_block_count) or [],
        "recent_txs": get_recent_transactions(settings.recent_tx_count) or [],
    }
    if _wants_json(request):
        return data

    return templates.TemplateResponse(request, "home.html", _template_ctx(request, **data))


@app.get("/block/{block_id}")
async def block_page(request: Request, block_id: str):
    """Block detail page."""
    block = None
    if block_id.isdigit():
        block = get_block_by_number(int(block_id))
    else:
        block = get_block_by_hash(block_id)
        if not block:
            try:
                block = get_block_by_number(int(block_id, 16))
            except (ValueError, OverflowError):
                pass

    if not block:
        raise NotFound("Block", block_id)

    data = {
        "block": block,
        "transactions": get_block_transactions(block["number"]),
    }
    if _wants_json(request):
        return data

    return templates.TemplateResponse(request, "block.html", _template_ctx(request, **data))


@app.get("/blocks")
async def blocks_page(
    request: Request,
    from_: int | None = Query(None, alias="from"),
    page: int = Query(1, ge=1),
):
    """Blocks list page."""
    per_page = 25
    latest_block = get_latest_block()
    latest_num = latest_block["number"] if latest_block else 0

    if from_ is not None:
        end = from_
    else:
        end = max(0, latest_num - (page - 1) * per_page)

    blocks = []
    for i in range(end, max(-1, end - per_page), -1):
        b = get_block_by_number(i)
        if b:
            blocks.append(b)

    data = {
        "blocks": blocks,
        "latest_num": latest_num,
        "total_blocks": latest_num + 1,
        "per_page": per_page,
        "page": page,
    }
    if _wants_json(request):
        return data

    return templates.TemplateResponse(request, "blocks.html", _template_ctx(request, **data))


@app.get("/tx/{tx_hash}")
async def tx_page(
    request: Request,
    tx_hash: str,
    tab: str = Query("overview"),
):
    """Transaction detail page with tabs (reads from cache)."""
    tx = get_transaction(tx_hash)
    if not tx:
        raise NotFound("Transaction", tx_hash)

    # Load cached receipt and trace
    receipt = json.loads(tx["receipt_data"]) if tx.get("receipt_data") else None
    trace_raw = json.loads(tx["trace_data"]) if tx.get("trace_data") else None
    trace = trace_raw if trace_raw else None  # already flattened list

    block = get_block_by_number(tx["block_number"])

    # Parse calls from trace or fallback to top-level
    calls = extract_calls(tx, trace)
    if not calls:
        calls = [
            {
                "depth": 0,
                "type": "CALL",
                "to": tx.get("to_addr", ""),
                "from": tx.get("from_addr", ""),
                "value": int(tx.get("value", 0)),
                "data": tx.get("input", "0x"),
                "decoded": decode_function_call(tx.get("input", "0x")),
                "gas": "0",
                "gas_used": "0",
                "children": [],
            }
        ]

    # Decode events from receipt logs
    events = []
    if receipt:
        for log in receipt.get("logs", []):
            decoded = decode_event(log)
            if decoded:
                events.append(decoded)

    # Balance changes from Transfer events
    balance_changes = extract_balance_changes(
        {"logs": receipt.get("logs", []) if receipt else []}, tx
    )

    # Fee info
    fee_token = tx.get("fee_token")
    fee_amount = tx.get("fee_amount", "0")
    gas_price = _hex_int(tx.get("gas_price"))
    gas_used = tx.get("gas_used", 0)
    gas_limit = tx.get("gas_limit", 0)
    max_fee = _hex_int(tx.get("max_fee_per_gas"))
    max_priority = _hex_int(tx.get("max_priority_fee_per_gas"))
    base_fee = _hex_int(tx.get("base_fee"))
    tx_type = tx.get("tx_type", 118)

    # Token info for fee (from cache, no inline RPC)
    fee_token_meta = get_token_metadata(fee_token) if fee_token else None

    data = {
        "tx": tx,
        "block": block,
        "receipt": receipt,
        "trace": trace,
        "calls": calls,
        "events": events,
        "balance_changes": balance_changes,
        "gas_price": gas_price,
        "gas_used": gas_used,
        "gas_limit": gas_limit,
        "max_fee": max_fee,
        "max_priority": max_priority,
        "base_fee": base_fee,
        "tx_type": tx_type,
        "fee_token": fee_token,
        "fee_amount": fee_amount,
        "fee_token_meta": fee_token_meta,
    }
    if _wants_json(request):
        return data

    return templates.TemplateResponse(
        request, "tx.html", _template_ctx(request, active_tab=tab, **data)
    )


@app.get("/receipt/{tx_hash}", response_class=HTMLResponse)
async def receipt_page(request: Request, tx_hash: str):
    """Receipt page - redirects to tx page."""
    return RedirectResponse(url=f"/tx/{tx_hash}")


@app.get("/address/{address}")
async def address_page(
    request: Request,
    address: str,
    tab: str = Query("transactions"),
    page: int = Query(1, ge=1),
):
    """Address detail page."""
    try:
        checksummed = to_checksum_address(address)
    except (ValueError, TypeError):
        if _wants_json(request):
            return JSONResponse({"error": f"Invalid address: {address}"}, status_code=400)
        raise NotFound("Address", address, "Invalid address")

    addr_info = identify_address(checksummed)
    per_page = 25

    txs = []
    tx_count = 0
    total_pages = 0

    if tab == "transactions":
        txs = get_address_transactions(checksummed, page, per_page)
        tx_count = get_address_transaction_count(checksummed)
        total_pages = max(1, math.ceil(tx_count / per_page))
    elif tab == "transfers":
        txs = get_address_transfers(checksummed, page, per_page)

    data = {
        "address": checksummed,
        "addr_info": addr_info,
        "type": addr_info["type"],
        "label": addr_info.get("label"),
        "transactions": txs,
        "holdings": get_address_holdings(checksummed),
        "tx_count": tx_count,
        "page": page,
        "total_pages": total_pages,
        "per_page": per_page,
    }
    if _wants_json(request):
        return data

    return templates.TemplateResponse(
        request, "address.html", _template_ctx(request, active_tab=tab, **data)
    )
@app.get("/token/{address}")
async def token_page(
    request: Request,
    address: str,
    tab: str = Query("transactions"),
    page: int = Query(1, ge=1),
):
    """Token detail page."""
    try:
        checksummed = to_checksum_address(address)
    except (ValueError, TypeError):
        if _wants_json(request):
            return JSONResponse({"error": f"Invalid address: {address}"}, status_code=400)
        raise NotFound("Token", address, "Invalid address")

    meta = get_token_metadata(checksummed)
    if not meta:
        meta = await fetch_token_metadata(checksummed)
        save_token_metadata(meta)

    per_page = 25
    transfers = []
    if tab == "transfers":
        transfers = get_token_transfers(checksummed, page, per_page)

    data = {
        "token": meta,
        "transfers": transfers,
        "page": page,
        "per_page": per_page,
    }
    if _wants_json(request):
        return data

    return templates.TemplateResponse(
        request, "token.html", _template_ctx(request, active_tab=tab, **data)
    )


@app.get("/tokens")
async def tokens_page(
    request: Request,
    page: int = Query(1, ge=1),
):
    """Tokens list page."""
    per_page = 25
    tokens = get_all_tokens(page, per_page)
    total = get_token_count()
    total_pages = max(1, math.ceil(total / per_page))

    data = {
        "tokens": tokens,
        "total": total,
        "page": page,
        "total_pages": total_pages,
        "per_page": per_page,
    }
    if _wants_json(request):
        return data

    return templates.TemplateResponse(request, "tokens.html", _template_ctx(request, **data))


@app.get("/search")
async def search(
    request: Request,
    q: str = Query(""),
):
    """Search for blocks, transactions, addresses, tokens.

    HTML: redirects on match, shows fallback page otherwise.
    JSON: returns match info or empty results.
    """
    if not q or q.strip() == "":
        if _wants_json(request):
            return JSONResponse({"query": q, "match": None})
        return RedirectResponse(url="/")

    q = q.strip()
    match = None

    # Try as block number
    if q.isdigit():
        b = get_block_by_number(int(q))
        if b:
            match = {"type": "block", "id": q, "url": f"/block/{q}"}

    # Try as transaction hash
    if not match:
        tx = get_transaction(q)
        if tx:
            match = {"type": "transaction", "id": q, "url": f"/tx/{q}"}

    # Try as block hash
    if not match:
        b = get_block_by_hash(q)
        if b:
            match = {"type": "block", "id": str(b["number"]), "url": f"/block/{b['number']}"}

    # Try as address
    if not match:
        try:
            checksummed = to_checksum_address(q)
            match = {"type": "address", "id": checksummed, "url": f"/address/{checksummed}"}
        except (ValueError, TypeError):
            pass
        meta = get_token_metadata(q)
        if meta:
            match = {"type": "token", "id": meta["address"], "url": f"/token/{meta['address']}"}

    if _wants_json(request):
        return JSONResponse({"query": q, "match": match})

    if match:
        return RedirectResponse(url=match["url"])

    return templates.TemplateResponse(
        request,
        "search.html",
        {"request": request, "query": q, "results": []},
    )


# ── URL helpers ─────────────────────────────────────────────────────


def get_block_url(block_id: int | str) -> str:
    if isinstance(block_id, int):
        return f"/block/{block_id}"
    # Try number first
    b = get_block_by_hash(str(block_id))
    if b:
        return f"/block/{b['number']}"
    return f"/block/{block_id}"


def get_tx_url(tx_hash: str) -> str:
    return f"/tx/{tx_hash}"


def get_address_url(address: str) -> str:
    try:
        return f"/address/{to_checksum_address(address)}"
    except (ValueError, TypeError):
        return f"/address/{address}"


def get_token_url(address: str) -> str:
    try:
        return f"/token/{to_checksum_address(address)}"
    except (ValueError, TypeError):
        return f"/token/{address}"


def main() -> None:
    """Entry point for direct uvicorn launch."""
    import socket
    import sys

    import uvicorn

    # Check port availability before binding
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.settimeout(1)
        try:
            s.connect((settings.host, settings.port))
            print(
                f"ERROR: Port {settings.port} is already in use on {settings.host}.",
                file=sys.stderr,
            )
            print(
                f"       Stop the other process or change port in .env / settings.",
                file=sys.stderr,
            )
            sys.exit(1)
        except (ConnectionRefusedError, OSError):
            pass  # port is free

    uvicorn.run("app.main:app", host=settings.host, port=settings.port, reload=True)
