# Tempo Explorer (Rust)

Blockchain explorer for [Tempo](https://tempo.xyz) — an EVM-compatible chain
with no native token (gas paid in ERC-20/TIP-20 tokens).

This is a full rewrite of the original Python/FastAPI explorer in Rust:

| Layer | Python (original) | Rust (this repo) |
|-------|-------------------|------------------|
| Web framework | FastAPI + Jinja2 | [axum](https://github.com/tokio-rs/axum) + [Tera](https://tera.netlify.app/) |
| Storage | SQLModel / SQLAlchemy + Alembic | rusqlite (schema created on boot) |
| RPC client | httpx | reqwest (async JSON-RPC) |
| ABI decoding | eth-abi / eth-contract | self-contained ABIv2 decoder + keccak |
| Indexer | asyncio task | tokio task (forward tip + backfill) |

The default RPC is `https://rpc.nvnm.canary.mantrachain.dev` (Mantra EVM,
chain id `0xc0316`) — the chain this codebase is validated against. Point it
anywhere with `TEMPO_RPC`.

## Quick start

```bash
cargo run --release
```

Open http://localhost:8080. On first boot the indexer seeds from the chain
head, indexes new blocks every few seconds, and backfills history downward
(`INDEX_BATCH` blocks per cycle — raise it, e.g. `INDEX_BATCH=2500`, to catch
up faster).

## Configuration (env vars)

| Var | Default | Meaning |
|-----|---------|---------|
| `TEMPO_RPC` | `https://rpc.nvnm.canary.mantrachain.dev` | JSON-RPC endpoint |
| `CHAIN_ID` | `787222` | Chain id shown in the UI |
| `DB_PATH` | `explorer.db` | SQLite database path |
| `HOST` / `PORT` | `0.0.0.0` / `8080` | Bind address |
| `INDEX_POLL_SECONDS` | `3` | Indexer poll interval |
| `INDEX_BATCH` | `5` | Blocks indexed per cycle (forward + backfill) |
| `RUST_LOG` | `tempo_explorer=info` | Log verbosity |

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

## Indexer

The background task polls the chain head every `INDEX_POLL_SECONDS` and indexes
up to `INDEX_BATCH` blocks per cycle in two phases:

1. **Forward** — new blocks at the tip.
2. **Backfill** — older blocks, descending (resumes from the lowest stored
   block after a restart, so an interrupted backfill is not abandoned).

For each block it stores the raw block, every transaction (with receipt and
flattened `debug_traceBlockByNumber` call tree when available), and decodes
TIP-20 `Transfer` / `TransferWithMemo` events into the `transfer_events` table
so address and token transfer tabs have data. Token metadata (name, symbol,
decimals, total supply) is lazy-fetched via `eth_call` whenever a fee token
appears.

## Tests

```bash
# Unit tests (no network)
cargo test --test decoder

# Integration tests against the live chain RPC
cargo test --test live_rpc
```

The integration tests hit the RPC: they assert the chain id, fetch and index
recent blocks into a temp SQLite DB, and boot the HTTP API to verify the JSON
endpoints end to end.

## Layout

```
src/
  main.rs       entry point (server + indexer)
  config.rs     settings
  rpc.rs        async JSON-RPC client
  parse.rs      raw RPC → storage models
  db.rs         SQLite layer
  decoder.rs    ABI decoder, events, traces
  contracts.rs  precompile / token labels
  tokens.rs     token metadata + formatting
  indexer.rs    background indexing
  web.rs        axum routes + template helpers
templates/      Tera templates
tests/          decoder unit tests + live RPC integration tests
```

## License

MIT
