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


async def _catch_up_to(target: int, last_indexed: int) -> int:
    """Index blocks from last_indexed+1 up to target."""
    start = max(last_indexed + 1, target - MAX_BLOCKS_PER_POLL + 1)
    for num in range(start, target + 1):
        await index_block(num)
    return target


async def run_forever() -> None:
    """Main loop: poll chain head and index new blocks."""
    last_indexed = 0

    # Resume from the latest cached block
    latest = get_latest_block()
    if latest:
        last_indexed = latest["number"]
        logger.info("resuming from block %s", last_indexed)

    logger.info("indexer started, polling every %ss", POLL_INTERVAL)

    while True:
        try:
            head = await rpc.eth_block_number()
            if head > last_indexed:
                logger.info("new blocks: %s -> %s", last_indexed, head)
                last_indexed = await _catch_up_to(head, last_indexed)
            elif head < last_indexed:
                # Chain reorg or reset — re-index from head
                logger.warning("chain rolled back: %s -> %s, re-indexing", last_indexed, head)
                last_indexed = await _catch_up_to(head, head - 1)

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
