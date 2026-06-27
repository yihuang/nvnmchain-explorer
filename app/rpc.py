"""Async RPC client for Tempo blockchain, wrapping tempo-py's web3 client."""

from __future__ import annotations

import json
from typing import Any

import httpx
from tempo.types import as_bytes

from .config import settings


class RPCError(Exception):
    def __init__(self, code: int, message: str, data: Any = None):
        self.code = code
        self.message = message
        self.data = data
        super().__init__(f"RPC error {code}: {message}")


class TempoRPC:
    """Async JSON-RPC client for Tempo blockchain."""

    def __init__(self, rpc_url: str | None = None):
        self.rpc_url = rpc_url or settings.rpc_url
        self._req_id = 0

    def _next_id(self) -> int:
        self._req_id += 1
        return self._req_id

    async def _call(self, method: str, params: list | None = None) -> Any:
        payload = {
            "jsonrpc": "2.0",
            "id": self._next_id(),
            "method": method,
            "params": params or [],
        }
        async with httpx.AsyncClient(timeout=30.0) as client:
            resp = await client.post(
                self.rpc_url,
                content=json.dumps(payload),
                headers={"Content-Type": "application/json"},
            )
            data = resp.json()
            if "error" in data and data["error"] is not None:
                err = data["error"]
                raise RPCError(
                    code=err.get("code", 0),
                    message=err.get("message", "unknown"),
                    data=err.get("data"),
                )
            return data.get("result")

    async def eth_block_number(self) -> int:
        result = await self._call("eth_blockNumber")
        return int(result, 16) if result else 0

    async def eth_get_block_by_number(self, block_num: int, full: bool = True) -> dict | None:
        hex_num = hex(block_num)
        return await self._call("eth_getBlockByNumber", [hex_num, full])

    async def eth_get_block_by_hash(self, block_hash: str, full: bool = True) -> dict | None:
        return await self._call("eth_getBlockByHash", [block_hash, full])

    async def eth_get_transaction_receipt(self, tx_hash: str) -> dict | None:
        return await self._call("eth_getTransactionReceipt", [tx_hash])

    async def eth_get_transaction_by_hash(self, tx_hash: str) -> dict | None:
        return await self._call("eth_getTransactionByHash", [tx_hash])

    async def eth_get_balance(self, address: str, block: str = "latest") -> int:
        result = await self._call("eth_getBalance", [address, block])
        return int(result, 16) if result else 0

    async def eth_get_code(self, address: str, block: str = "latest") -> str:
        return await self._call("eth_getCode", [address, block]) or "0x"

    async def eth_call(self, to: str, data: str, block: str = "latest") -> str:
        return await self._call("eth_call", [{"to": to, "data": data}, block]) or "0x"

    async def eth_get_logs(self, filter_obj: dict) -> list[dict]:
        return await self._call("eth_getLogs", [filter_obj]) or []

    async def eth_chain_id(self) -> int:
        result = await self._call("eth_chainId")
        return int(result, 16) if result else 0

    async def eth_gas_price(self) -> int:
        result = await self._call("eth_gasPrice")
        return int(result, 16) if result else 0

    async def eth_get_transaction_count(self, address: str, block: str = "latest") -> int:
        result = await self._call("eth_getTransactionCount", [address, block])
        return int(result, 16) if result else 0

    async def eth_fee_history(
        self, block_count: int, newest_block: str, reward_percentiles: list | None = None
    ) -> dict | None:
        params = [hex(block_count), newest_block, reward_percentiles or []]
        return await self._call("eth_feeHistory", params)

    async def eth_get_storage_at(self, address: str, slot: str, block: str = "latest") -> str:
        return await self._call("eth_getStorageAt", [address, slot, block]) or "0x"

    async def debug_trace_transaction(
        self, tx_hash: str, tracer: str = "callTracer", timeout: int = 60
    ) -> dict | None:
        """Trace a transaction execution using the callTracer.

        Returns nested call tree::

            {"type":"CALL","from":"0x...","to":"0x...","input":"0x...",
             "calls":[{"type":"CALL","from":"0x...",...},...]}

        Returns None if tracing is unavailable or times out.
        """
        try:
            return await self._call("debug_traceTransaction", [tx_hash, {"tracer": tracer}])
        except (RPCError, httpx.TimeoutException, httpx.HTTPError):
            return None

    async def debug_trace_block(
        self, block_num: int, tracer: str = "callTracer"
    ) -> list[dict] | None:
        """Trace all transactions in a block using callTracer.

        Returns a list of trace results, one per transaction, in order::

            [{"txHash":"0x...","result":{"type":"CALL",...}}, ...]

        Returns None if tracing is unavailable.
        """
        try:
            return await self._call(
                "debug_traceBlockByNumber", [hex(block_num), {"tracer": tracer}]
            )
        except (RPCError, httpx.TimeoutException, httpx.HTTPError):
            return None


# Shared instance
rpc = TempoRPC()


# NOTE: database imports are inside functions to avoid circular imports.
# database.py → tokens.py → rpc.py → database.py would be the cycle.


async def fetch_and_cache_block(block_num: int) -> dict | None:
    """Fetch block + all its txs, cache to DB, return parsed block."""
    from .database import get_block_by_number, save_block

    # Check cache
    cached = get_block_by_number(block_num)
    if cached and cached.get("raw") and cached["raw"] != "{}":
        # Block is cached. If it has txs but none in DB, re-fetch.
        if cached["tx_count"] > 0:
            from .database import get_block_transactions

            existing = get_block_transactions(block_num)
            if existing:
                return cached
        else:
            return cached

    raw_block = await rpc.eth_get_block_by_number(block_num, full=True)
    if not raw_block:
        return None

    parsed = parse_block(raw_block)
    save_block(parsed)

    # Cache transactions
    from .database import save_transaction

    txs = raw_block.get("transactions", [])
    for tx_data in txs:
        tx_parsed = parse_transaction(tx_data, parsed)
        save_transaction(tx_parsed)

    return parsed


def parse_block(raw: dict) -> dict:
    return {
        "number": int(raw.get("number", "0x0"), 16),
        "hash": raw.get("hash", ""),
        "parent_hash": raw.get("parentHash", ""),
        "timestamp": int(raw.get("timestamp", "0x0"), 16),
        "gas_used": int(raw.get("gasUsed", "0x0"), 16),
        "gas_limit": int(raw.get("gasLimit", "0x0"), 16),
        "miner": raw.get("miner", ""),
        "tx_count": len(raw.get("transactions", [])),
        "raw": json.dumps(raw),
    }


def parse_transaction(tx: dict, block: dict | None = None) -> dict:
    """Parse a raw RPC tx dict into our normalized format."""
    # Tempo tx type 0x76 uses 'calls' instead of 'to'/'data'
    to_addr = tx.get("to") or None

    # Parse gas fields as decimals if strings, hex if hex
    def _parse_int(val: Any) -> int:
        if isinstance(val, str):
            return int(val, 16) if val.startswith("0x") else int(val) if val else 0
        return int(val) if val else 0

    gas_price_str = tx.get("gasPrice", "0x0")
    max_fee_str = tx.get("maxFeePerGas", gas_price_str)
    max_priority_str = tx.get("maxPriorityFeePerGas", "0x0")

    return {
        "hash": tx.get("hash", ""),
        "block_number": _parse_int(tx.get("blockNumber", 0)),
        "block_hash": tx.get("blockHash", block["hash"] if block else ""),
        "position": _parse_int(tx.get("transactionIndex", 0)),
        "from_addr": tx.get("from", ""),
        "to_addr": to_addr,
        "status": 1,  # Will be updated by receipt
        "gas_limit": _parse_int(tx.get("gas", 0)),
        "gas_used": 0,  # From receipt
        "gas_price": gas_price_str if isinstance(gas_price_str, str) else hex(gas_price_str),
        "max_fee_per_gas": max_fee_str if isinstance(max_fee_str, str) else hex(max_fee_str),
        "max_priority_fee_per_gas": max_priority_str
        if isinstance(max_priority_str, str)
        else hex(max_priority_str),
        "base_fee": "0x0",
        "fee_token": None,
        "fee_amount": "0",
        "nonce_key": tx.get("nonceKey") or "0x",
        "value": str(_parse_int(tx.get("value", 0))),
        "chain_id": _parse_int(tx.get("chainId", 4217)),
        "tx_type": _parse_int(tx.get("type", 0x76)),
        "input": tx.get("input", tx.get("data", "0x")),
        "timestamp": int(block["timestamp"], 16)
        if block and isinstance(block.get("timestamp"), str)
        else (block["timestamp"] if block else 0),
    }


async def fetch_transaction_with_receipt(tx_hash: str) -> dict | None:
    """Fetch tx + receipt, cache, return combined dict."""
    from .database import get_transaction, save_transaction

    cached = get_transaction(tx_hash)
    if cached and cached.get("raw"):
        return cached

    tx = await rpc.eth_get_transaction_by_hash(tx_hash)
    if not tx:
        return None

    receipt = await rpc.eth_get_transaction_receipt(tx_hash)

    # Get block for timestamp
    block: dict | None = None
    bn = tx.get("blockNumber")
    if bn:
        block = await rpc.eth_get_block_by_number(int(bn, 16), full=False)

    parsed = parse_transaction(tx, block)

    if receipt:
        parsed["status"] = int(receipt.get("status", "0x1"), 16)
        parsed["gas_used"] = int(receipt.get("gasUsed", "0x0"), 16)
        # Parse Tempo-specific receipt fields
        if "feeToken" in receipt:
            parsed["fee_token"] = receipt["feeToken"]
        if "feeAmount" in receipt:
            parsed["fee_amount"] = (
                str(int(receipt["feeAmount"], 16))
                if isinstance(receipt["feeAmount"], str) and receipt["feeAmount"].startswith("0x")
                else str(receipt["feeAmount"])
            )
        # Base fee
        if "effectiveGasPrice" in receipt:
            egp = receipt["effectiveGasPrice"]
            parsed["base_fee"] = egp if isinstance(egp, str) else hex(int(egp))

    save_transaction(parsed)
    return parsed
