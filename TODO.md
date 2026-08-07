# TODO

Tracked alongside the Rust rewrite of this explorer.

## Done

- [x] Fully rewrite the Python explorer as a Rust crate (axum + rusqlite + Tera)
- [x] Verify the Rust rewrite against the live chain RPC (`https://rpc.nvnm.canary.mantrachain.dev`, chain id `0xc0316`)
- [x] Port the indexer (forward tip + backfill), ABI decoder, token metadata, and all routes
- [x] Fill the `transfer_events` table during indexing (the Python app's schema had it, but never wrote to it)
- [x] End-to-end tests that boot the server against the chain RPC and assert the JSON API
- [x] First UI polish pass (gradient background, accent stats, footer)

## In progress

- [ ] Make the UI prettier (better typography, responsive tables, nicer badges/empty states)
- [ ] Reconcile/remove the legacy Python sources now that the Rust app is the primary implementation

## Backlog

- [ ] Persist the backfill frontier in the DB so very long backfills survive restarts without re-scanning
- [ ] Concurrency: replace the single SQLite mutex with a pool once traffic grows
