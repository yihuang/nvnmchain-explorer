"""Background indexer: polls chain, batches block + trace + receipt via debug_traceBlockByNumber."""

from __future__ import annotations

import asyncio
import json
import logging

from .database import get_latest_block, save_block, save_token_metadata, save_transaction
from .decoder import flatten_trace
from .rpc import parse_block, parse_transaction, rpc
from .tokens import fetch_token_metadata

logger = logging.getLogger("tempo.indexer")

POLL_INTERVAL = 3  # seconds between block-number polls
MAX_BLOCKS_PER_POLL = 5  # catch up at most this many per cycle


async def _fetch_token_if_needed(address: str) -> None:
    """Lazy-fetch token metadata and cache it."""
    from .database import get_token_metadata

    if address and not get_token_metadata(address):
        meta = await fetch_token_metadata(address)
        save_token_metadata(meta)


async def index_block(block_num: int) -> None:
    """Fetch, trace, and store one block (+ all txs + receipts) atomically."""
    # 1. Fetch block with full tx objects
    raw_block = await rpc.eth_get_block_by_number(block_num, full=True)
    if not raw_block:
        logger.warning("block %s not found", block_num)
        return

    block = parse_block(raw_block)
    save_block(block)

    # 2. Fetch all traces in one batch call
    traces = await rpc.debug_trace_block(block_num)
    trace_map: dict[str, list[dict]] = {}
    if traces:
        for entry in traces:
            tx_hash = entry.get("txHash") or entry.get("transactionHash", "")
            result = entry.get("result", entry)
            if tx_hash:
                trace_map[tx_hash] = flatten_trace(result)

    # 3. Fetch receipts (still one-per-tx) and save everything
    timestamp = block["timestamp"]
    for tx_data in raw_block.get("transactions", []):
        tx_hash = tx_data.get("hash", "")
        tx_parsed = parse_transaction(tx_data, block)
        tx_parsed["timestamp"] = timestamp

        # Attach trace
        flat_trace = trace_map.get(tx_hash)
        if flat_trace:
            tx_parsed["trace_data"] = json.dumps(flat_trace)

        # Fetch and attach receipt
        receipt = await rpc.eth_get_transaction_receipt(tx_hash)
        if receipt:
            tx_parsed["receipt_data"] = json.dumps(receipt)
            tx_parsed["status"] = int(receipt.get("status", "0x1"), 16)
            tx_parsed["gas_used"] = int(receipt.get("gasUsed", "0x0"), 16)
            if "feeToken" in receipt:
                tx_parsed["fee_token"] = receipt["feeToken"]
                await _fetch_token_if_needed(receipt["feeToken"])
            if "feeAmount" in receipt:
                raw_fee = receipt["feeAmount"]
                tx_parsed["fee_amount"] = (
                    str(int(raw_fee, 16))
                    if isinstance(raw_fee, str) and raw_fee.startswith("0x")
                    else str(raw_fee)
                )
            if "effectiveGasPrice" in receipt:
                tx_parsed["base_fee"] = receipt["effectiveGasPrice"]

        save_transaction(tx_parsed)


async def run_forever() -> None:
    """Main loop: index new blocks at the tip, backfill old blocks.

    Each poll cycle:
    1. Index up to ``MAX_BLOCKS_PER_POLL`` new blocks at the chain tip.
    2. Backfill up to ``MAX_BLOCKS_PER_POLL`` older blocks going backwards.
    """
    highest = 0
    latest = get_latest_block()
    if latest:
        highest = latest["number"]
        logger.info("resuming from block %s", highest)

    # Backfill from the current chain head down to block 0.
    backfill_target = 0

    # Track totals for progress reporting
    total_indexed = 0
    last_log_ts = 0.0

    logger.info("indexer started, polling every %ss", POLL_INTERVAL)

    while True:
        try:
            head = await rpc.eth_block_number()

            # Initial jump to tip on fresh DB — skip forward crawl
            if highest == 0:
                highest = head
                backfill_target = head
                logger.info("initialised: indexing from block %s down to 0", head)

            # Phase 1 — index up to MAX_BLOCKS_PER_POLL new blocks at tip
            if head > highest:
                end = min(highest + MAX_BLOCKS_PER_POLL, head)
                n = end - highest
                logger.info("new blocks: %s -> %s (+%s)", highest, end, n)
                for num in range(highest + 1, end + 1):
                    await index_block(num)
                highest = end
                backfill_target = max(backfill_target, head)
                total_indexed += n

            # Phase 2 — backfill MAX_BLOCKS_PER_POLL older blocks from tip down
            if backfill_target > 0:
                start = max(1, backfill_target - MAX_BLOCKS_PER_POLL)
                n = backfill_target - start
                remaining = backfill_target - 1
                logger.info(
                    "backfill: blocks %s-%s (%s behind tip)",
                    start, backfill_target - 1, remaining,
                )
                for num in range(backfill_target - 1, start - 1, -1):
                    await index_block(num)
                backfill_target = start - 1
                total_indexed += n

            # Periodic progress summary
            now = asyncio.get_running_loop().time()
            if now - last_log_ts >= 30 or total_indexed == 0:
                tipped = highest >= head
                logger.info(
                    "progress: %s blocks indexed, tip=%s, backfill_remaining=%s%s",
                    total_indexed,
                    head,
                    max(0, backfill_target),
                    "" if tipped else " (catching up to tip)",
                )
                last_log_ts = now
        except Exception:
            await asyncio.sleep(POLL_INTERVAL)
            continue

        await asyncio.sleep(POLL_INTERVAL)



def start() -> asyncio.Task | None:
    """Launch the indexer as a background asyncio task (call from startup).

    Returns the task so the caller can cancel it on shutdown.
    """
    import asyncio

    try:
        loop = asyncio.get_running_loop()
    except RuntimeError:
        loop = asyncio.get_event_loop()
    task = loop.create_task(run_forever())
    return task
