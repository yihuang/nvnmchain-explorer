# nvnmchain Explorer

Blockchain explorer for the nvnm chain — the Mantra canary EVM network
(chain id `0xc0316`) — with no native token (gas paid in ERC-20/TIP-20
tokens).

The UI is a dark, Blockscout-style dashboard: network stats with activity
sparklines, gas-utilization bars, method badges on transactions, token
holdings and holder counts, and decoded call/event views on transaction
pages.

Written in Rust with [axum](https://github.com/tokio-rs/axum) + [Tera](https://tera.netlify.app/),
rusqlite (schema created on boot), an async reqwest JSON-RPC client, a
self-contained ABIv2 decoder with keccak, and a tokio indexer (forward tip +
backfill).

The default RPC is `https://rpc.nvnm.canary.mantrachain.dev` — the chain this
codebase is validated against. Point it anywhere with `NVNM_RPC` (the legacy
`TEMPO_RPC` variable is still accepted).

## Quick start

```bash
cargo run --release
```

Open http://localhost:8080. On first boot the indexer seeds from the chain
head, tracks new blocks continuously, and backfills history downward.

The indexer is built for a sub-second chain:

- **Instant heads** — subscribes to `eth_subscribe("newHeads")` over WebSocket
  (`wss://ws.nvnm.canary.mantrachain.dev`); polling (`INDEX_POLL_SECONDS`)
  keeps the feed alive while the socket reconnects, so head detection never
  stalls. If the socket is unreachable (it currently does not answer from
  most networks), the indexer warns a few times, then retries silently on a
  long backoff while polling continues uninterrupted.
- **One RPC call per block** — receipts via `eth_getBlockReceipts` (with a
  per-transaction batch fallback), traces via `debug_traceBlockByNumber`, and
  blocks fetched concurrently (`INDEX_CONCURRENCY` in flight).
- **Serialized SQLite writes** — every block (block row + txs + transfers +
  balances + token metadata) is persisted in one transaction by a single
  writer task, measured at ~200 blocks/s while backfilling.
- **Cheap pages** — network stats (block time, TPS, gas utilization, 24h
  counts) are recomputed in the background into a `kv` row, and token
  balances/holder counts are maintained incrementally per block, so no page
  scans history at request time.

## Configuration (env vars)

| Var | Default | Meaning |
|-----|---------|---------|
| `NVNM_RPC` | `https://rpc.nvnm.canary.mantrachain.dev` | JSON-RPC endpoint (legacy `TEMPO_RPC` also accepted) |
| `WS_URL` | `wss://ws.nvnm.canary.mantrachain.dev` | WebSocket endpoint for `newHeads` |
| `INDEX_WS` | `1` | Set `0` to disable the WebSocket feed (pure polling) |
| `CHAIN_ID` | `787222` | Chain id shown in the UI |
| `DB_PATH` | `explorer.db` | SQLite database path |
| `HOST` / `PORT` | `0.0.0.0` / `8080` | Bind address |
| `INDEX_POLL_SECONDS` | `1` | Poll interval when the WebSocket feed is unavailable |
| `INDEX_BATCH` | `5` | Blocks indexed per cycle (forward + backfill) |
| `INDEX_CONCURRENCY` | `32` | Blocks fetched in parallel |
| `NATIVE_SYMBOL` | `OM` | Symbol shown for native (burnt/gas) amounts |
| `STATS_INTERVAL_SECONDS` | `5` | How often the dashboard stats are recomputed |
| `RUST_LOG` | `nvnmchain_explorer=info` | Log verbosity |

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

Two background loops share a single SQLite writer task:

1. **Forward** — new blocks at the tip, driven by the WebSocket head feed (or
   polling fallback), indexed as soon as they appear.
2. **Backfill** — older blocks, descending (resumes from the lowest stored
   block after a restart, so an interrupted backfill is not abandoned).

For each block it stores the raw block plus indexed fields (base fee, size,
extra data, consensus epoch/view, proposer), every transaction (with receipt,
flattened call tree when tracing is available, and a method-id badge derived
from the call data), and decodes TIP-20 `Transfer` / `TransferWithMemo` events
into the `transfer_events` table so address and token transfer tabs have data.
Fees are derived from the Fee Manager transfer when the receipt omits
`feeAmount`. Token metadata (name, symbol, decimals, total supply) is fetched
via `eth_call` for fee tokens and transfer tokens alike (deduplicated, so each
token is fetched once), and token balances are applied incrementally so
holder counts and address holdings stay exact without rescanning history.

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
  config.rs     settings (RPC, WS, DB, indexer)
  rpc.rs        async JSON-RPC client
  ws.rs         WebSocket newHeads feed + polling fallback
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
