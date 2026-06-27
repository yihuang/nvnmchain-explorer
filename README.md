# Tempo Explorer

Blockchain explorer for [Tempo](https://tempo.xyz) — an EVM-compatible chain with no native token (gas paid in ERC-20/TIP-20 tokens).

## Quick start

```bash
uv sync
uv run alembic upgrade head
uv run fastapi run app/main.py
```

Open http://localhost:8000. The indexer auto-starts and caches blocks from the Tempo RPC in the background.

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

- **FastAPI** + **Jinja2** (server-rendered HTML)
- **SQLModel** / **SQLAlchemy** + **Alembic** (SQLite cache)
- **tempo-py** + **web3.py** (RPC client)
- Tests: **pytest** + **httpx** (97 E2E tests)
- Lint/format: **ruff**

## Tests

```bash
uv run pytest tests/ -v
```

## Migrations

```bash
alembic revision --autogenerate -m "description"
alembic upgrade head
```
