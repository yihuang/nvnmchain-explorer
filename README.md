# Tempo Explorer

Blockchain explorer for [Tempo](https://tempo.xyz) — an EVM-compatible chain with no native token (gas paid in ERC-20/TIP-20 tokens).

## Quick start

```bash
uv sync
uv run alembic upgrade head
uv run fastapi run app/main.py
```

Open http://localhost:8000.

## Background indexer

The indexer (`app/indexer.py`) runs as an `asyncio.Task` inside the FastAPI
process, started on app startup. It polls the chain head every 3 seconds and
indexes up to 5 new blocks per cycle.

For each block:
1. **Block** — fetched via `eth_getBlockByNumber`, parsed, cached to SQLite.
2. **Traces** — fetched in a single `debug_traceBlockByNumber` call, flattened
   via `flatten_trace` (decodes function calls using eth-contract ABIs).
3. **Receipts & events** — fetched per-tx, decoded via eth-contract event
   parsers, stored with fee metadata. Token metadata is lazy-fetched when
   `feeToken` is present.

The indexer skips already-cached blocks and catches up from the last indexed
height on restart. It logs a warning on RPC errors and retries on the next
poll cycle — never crashes.

## Routes

| Path | Description |
|------|-------------|
| `/` | Dashboard (stats, recent blocks/txs) |
| `/block/{num\|hash}` | Block detail |
| `/blocks` | Block list |
| `/tx/{hash}` | Transaction detail (tabs: Overview/Balances/Calls/Events/Raw) |
| `/address/{addr}` | Address info (transactions, transfers, holdings) |
| `/token/{addr}` | Token metadata + transfers |
| `/tokens` | Token list |
| `/search?q=...` | Smart redirect (block#/tx/address/token auto-detection) |

All data endpoints accept `?format=json` or `Accept: application/json`.

## Stack

- **[FastAPI](https://fastapi.tiangolo.com/)** + **Jinja2** (server-rendered HTML)
- **[SQLModel](https://sqlmodel.tiangolo.com/)** / **SQLAlchemy** + **Alembic** (SQLite cache)
- **[tempo-py](https://github.com/yihuang/tempo-py)** — typed call builders, chain constants, and eth-contract ABIs
- Tests: **pytest** + **httpx** (97 E2E tests)
- Lint/format: **ruff**

## References

| Repo | Description |
|------|-------------|
| [tempo-py](https://github.com/yihuang/tempo-py) | Python SDK — transaction building, signing, ABI decoders, chain constants |
| [Tempo](https://tempo.xyz) | EVM-compatible chain with gas-in-token economics |

## Tests

```bash
uv run pytest tests/ -v
```

## Migrations

```bash
alembic revision --autogenerate -m "description"
alembic upgrade head
```
