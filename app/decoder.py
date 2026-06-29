"""ABI decoder for Tempo transactions, events, and calls.

Decodes function selectors, event topics, and logs using the
TIP-20 contract ABI and known function signatures.
"""

from __future__ import annotations

import json
from .models import DecodedCall, DecodedEvent, DecodedParam
from typing import Any

from eth_abi import decode as abi_decode
from eth_utils import keccak, to_bytes, to_checksum_address, to_hex
from tempo.constants import FEE_MANAGER_ADDRESS
from tempo.contracts import TIP20

# ── Known function selectors ─────────────────────────────────────────

# Map 4-byte selector to TIP20 function name for eth-contract decoding
TIP20_SELECTORS: dict[bytes, str] = {
    bytes(TIP20.fns.transfer.selector): "transfer",
    bytes(TIP20.fns.approve.selector): "approve",
    bytes(TIP20.fns.transferFrom.selector): "transferFrom",
    bytes(TIP20.fns.transferWithMemo.selector): "transferWithMemo",
    bytes(TIP20.fns.mint.selector): "mint",
    bytes(TIP20.fns.burn.selector): "burn",
}

# Event topic hashes (used for matching in decode_event)
TRANSFER_TOPIC = TIP20.events.Transfer.topic
TRANSFER_WITH_MEMO_TOPIC = TIP20.events.TransferWithMemo.topic
APPROVAL_TOPIC = TIP20.events.Approval.topic

EVENT_PARSERS: dict[bytes, Any] = {
    TRANSFER_TOPIC: TIP20.events.Transfer,
    TRANSFER_WITH_MEMO_TOPIC: TIP20.events.TransferWithMemo,
    APPROVAL_TOPIC: TIP20.events.Approval,
}

# Fee Manager selectors
FEE_MANAGER_SELECTORS: dict[bytes, str] = {}

# Additional known function signatures (4-byte selectors)
ADDITIONAL_SIGS: dict[bytes, str] = {
    keccak(b"transfer(address,uint256)")[:4]: "transfer(address to, uint256 amount)",
    keccak(b"approve(address,uint256)")[:4]: "approve(address spender, uint256 amount)",
    keccak(b"transferFrom(address,address,uint256)")[
        :4
    ]: "transferFrom(address sender, address to, uint256 amount)",
    keccak(b"balanceOf(address)")[:4]: "balanceOf(address account)",
    keccak(b"totalSupply()")[:4]: "totalSupply()",
    keccak(b"name()")[:4]: "name()",
    keccak(b"symbol()")[:4]: "symbol()",
    keccak(b"decimals()")[:4]: "decimals()",
    keccak(b"allowance(address,address)")[:4]: "allowance(address owner, address spender)",
    keccak(b"mint(address,uint256)")[:4]: "mint(address to, uint256 amount)",
    keccak(b"burn(uint256)")[:4]: "burn(uint256 amount)",
    # AccountKeychain
    keccak(
        b"authorizeKey(address,uint8,(uint64,bool,(address,uint256,uint64)[],bool,(address,(bytes4,address[])[])[]))"
    )[:4]: "authorizeKey(address,uint8,tuple)",
    keccak(b"revokeKey(address)")[:4]: "revokeKey(address keyId)",
}


def decode_function_call(data: str) -> DecodedCall | None:
    """Decode a 0x-prefixed calldata into function name and parameters.

    Returns dict with 'name', 'signature', 'params' or None if unknown.
    """
    if not data or data in ("0x", "0x0"):
        return None

    raw = to_bytes(hexstr=data)
    if len(raw) < 4:
        return None

    selector = raw[:4]

    # Check TIP-20 first
    fn_name = TIP20_SELECTORS.get(selector)
    if fn_name:
        return _decode_tip20_call(raw, fn_name)

    sig = ADDITIONAL_SIGS.get(selector)
    if sig:
        return DecodedCall(
            name=sig.split("(")[0],
            signature=sig,
            params=_decode_params(sig, raw[4:]),
            raw_args=to_hex(raw[4:]),
        )
    return DecodedCall(
        selector=to_hex(selector),
        raw_args=to_hex(raw[4:]),
    )

    return {
        "name": None,
        "signature": None,
        "selector": to_hex(selector),
        "params": [],
        "raw_args": to_hex(raw[4:]),
    }


def _decode_params(signature: str, data: bytes) -> list[DecodedParam]:
    """Try to decode parameters based on known signature types."""
    # For simple types, decode with eth_abi
    try:
        sig_types_str = signature[signature.index("(") + 1 : signature.rindex(")")]
        if not sig_types_str:
            return []
        types = [t.strip() for t in sig_types_str.split(",")]
        decoded = abi_decode(types, data)
        param_names = _extract_param_names(signature)
        params = []
        for i, (t, v) in enumerate(zip(types, decoded, strict=True)):
            pname = param_names[i] if i < len(param_names) else f"arg{i}"
            params.append(DecodedParam(type=t, name=pname, value=_format_value(t, v)))
        return params
    except Exception:
        return []


def _extract_param_names(sig: str) -> list[str]:
    """Extract parameter names from a signature like 'transfer(address to, uint256 amount)'."""
    try:
        inner = sig[sig.index("(") + 1 : sig.rindex(")")]
        if not inner:
            return []
        parts = inner.split(",")
        names = []
        for p in parts:
            p = p.strip()
            # type name
            if " " in p:
                names.append(p.split(" ")[-1].strip())
            else:
                names.append("")
        return names
    except Exception:
        return []


def _format_value(typ: str, value: Any) -> str:
    """Format a decoded ABI value for display."""
    if isinstance(value, bytes):
        if len(value) <= 20:
            return to_checksum_address(value) if len(value) == 20 else to_hex(value)
        return to_hex(value)
    if isinstance(value, bool):
        return str(value).lower()
    if isinstance(value, int):
        return str(value)
    return str(value)


def _format_abi_value(typ: str, value: Any) -> str:
    """Format a decoded eth-contract ABI value for display.

    eth-contract returns native Python types: address→str, uint→int, bytes→bytes.
    """
    if isinstance(value, bytes):
        return to_hex(value)
    if isinstance(value, int):
        return str(value)
    return str(value)


def _build_abi_params(
    inputs: list[dict], values: dict | tuple | Any, *, include_indexed: bool = False
) -> list[DecodedParam]:
    """Build params list from ABI input definitions and decoded values.

    ``values`` is a tuple (from ``decode_input``) or a dict (from ``parse_log``).
    ``include_indexed`` adds an ``indexed`` field to each param for event logs.
    """
    params = []
    for i, inp in enumerate(inputs):
        if isinstance(values, dict):
            value = values[inp["name"]]
        elif isinstance(values, (tuple, list)):
            value = values[i]
        else:
            value = values
        param = DecodedParam(
            type=inp["type"],
            name=inp["name"],
            value=_format_abi_value(inp["type"], value),
            indexed=inp.get("indexed", False) if include_indexed else False,
        )
        params.append(param)
    return params


def _decode_tip20_call(raw: bytes, fn_name: str) -> DecodedCall:
    """Decode a TIP20 function call using eth-contract's decode_input."""
    fn = getattr(TIP20.fns, fn_name)
    decoded = fn.decode_input(raw)
    return DecodedCall(
        name=fn.abi["name"],
        signature=fn.signature,
        params=_build_abi_params(fn.abi["inputs"], decoded),
        raw_args=to_hex(raw[4:]),
    )


# ── Event log decoding ──────────────────────────────────────────────


def _parse_event_log(event: Any, log: dict) -> DecodedEvent | None:
    """Parse a known event log using eth-contract's parse_log.

    Handles missing log fields (test fixtures, partial data) by filling defaults.
    Returns None if the log doesn't match the event ABI (wrong topics, etc).
    """
    # parse_log requires logIndex, transactionIndex, transactionHash, blockHash, blockNumber
    full_log = {
        "logIndex": "0x0",
        "transactionIndex": "0x0",
        "transactionHash": "0x" + "00" * 32,
        "blockHash": "0x" + "00" * 32,
        "blockNumber": "0x0",
        **log,
    }
    try:
        parsed = event.parse_log(full_log)
    except Exception:
        # Log may have a matching topic0 but different indexed params or
        # malformed data — common on a real chain. Gracefully degrade.
        return None
    args = parsed["args"] if isinstance(parsed, dict) else parsed.args
    return DecodedEvent(
        name=event.abi["name"],
        signature=event.signature,
        contract=log.get("address", ""),
        params=_build_abi_params(event.abi["inputs"], args, include_indexed=True),
        log_index=log.get("logIndex"),
        transaction_hash=log.get("transactionHash"),
    )


def decode_event(log: dict) -> DecodedEvent | None:
    """Decode a single event log.

    Returns dict with event name, params, and contract info.
    """
    topics = log.get("topics", [])
    if not topics:
        return None

    topic0 = topics[0] if topics else ""

    for topic_bytes, event in EVENT_PARSERS.items():
        if topic0 == "0x" + topic_bytes.hex():
            parsed = _parse_event_log(event, log)
            if parsed is not None:
                return parsed
            break  # topic matched but log didn't parse — treat as unknown

    # Unknown event
    data = log.get("data", "0x")
    return DecodedEvent(
        topic0=topic0,
        contract=log.get("address", ""),
        params=[
            DecodedParam(
                type="bytes", name="data", value=data[:200] + "..." if len(data) > 200 else data
            )
        ],
    )


# ── Receipt parsing ──────────────────────────────────────────────


def extract_balance_changes(receipt: dict, tx: dict) -> list[dict]:
    """Extract token balance changes from receipt logs.

    Returns list of {address, token, before, after, change, is_fee}
    """
    changes: list[dict] = []
    logs = receipt.get("logs", [])
    fee_token = tx.get("fee_token")
    fee_amount = tx.get("fee_amount", "0")

    for log in logs:
        decoded = decode_event(log)
        if decoded and decoded.name == "Transfer":
            token = log.get("address", "")
            params = {p.name: p.value for p in decoded.params}
            changes.append(
                {
                    "address": params.get("from", ""),
                    "token": token,
                    "change": "-" + params.get("amount", "0"),
                    "is_fee": False,
                }
            )
            changes.append(
                {
                    "address": params.get("to", ""),
                    "token": token,
                    "change": "+" + params.get("amount", "0"),
                    "is_fee": False,
                }
            )

    # Mark fee payment if fee_token matches
    if fee_token and int(fee_amount) > 0:
        for c in changes:
            addr_lower = c["address"].lower()
            if (
                addr_lower == FEE_MANAGER_ADDRESS.lower()
                and c["token"].lower() == fee_token.lower()
            ):
                c["is_fee"] = True
                c["change_type"] = "fee"

    return changes


def extract_state_changes(receipt: dict) -> list[dict]:
    """Extract raw state changes from receipt (if available)."""
    if "stateDiff" in receipt:
        return [{"contract": k, "slots": v} for k, v in receipt["stateDiff"].items()]
    return []


def flatten_trace(trace: dict | None, depth: int = 0) -> list[dict]:
    """Flatten a nested callTracer tree into a flat list with depth info.

    Each trace node becomes::

        {"depth": int, "type": str, "from": str, "to": str,
         "data": str, "output": str, "value": str,
         "gas": str, "gas_used": str,
         "decoded": dict|None, "children": [sub-calls]}

    The flat list is in DFS order, suitable for rendering as a tree.
    """
    if not trace:
        return []

    result: list[dict] = []

    def _walk(node: dict, d: int) -> None:
        decoded = decode_function_call(node.get("input", "0x"))
        flat = {
            "depth": d,
            "type": node.get("type", "CALL"),
            "from": node.get("from", ""),
            "to": node.get("to", ""),
            "data": node.get("input", "0x"),
            "output": node.get("output", ""),
            "value": str(int(node.get("value", "0x0"), 16)) if node.get("value") else "0",
            "gas": str(int(node.get("gas", "0x0"), 16)) if node.get("gas") else "0",
            "gas_used": str(int(node.get("gasUsed", "0x0"), 16)) if node.get("gasUsed") else "0",
            "decoded": decoded.model_dump() if decoded else None,
            "children": [],
        }
        result.append(flat)
        child_calls = node.get("calls") or node.get("children") or []
        for child in list(child_calls) if isinstance(child_calls, (list, tuple)) else []:
            flat["children"].append(len(result) + len(flat.get("children", [])))
            _walk(child, d + 1)

    if isinstance(trace, list):
        for node in trace:
            _walk(node, depth)
    else:
        _walk(trace, depth)

    return result


def extract_calls(tx: dict, trace: list[dict] | None = None) -> list[dict]:
    """Extract internal calls from a transaction.

    When *trace* (from ``debug_traceTransaction``) is available, returns
    the flattened call tree with depth, gas, and full execution data.
    Otherwise falls back to the top-level ``calls`` field.
    """
    if trace:
        # Normalize: flatten_trace historically used "input" but template expects "data"
        for t in trace:
            if "input" in t and "data" not in t:
                t["data"] = t["input"]
        return trace

    raw_tx: dict = json.loads(tx.get("raw") or "{}")
    calls = raw_tx.get("calls", [])

    extracted: list[dict] = []
    for _i, call in enumerate(calls if isinstance(calls, list) else []):
        to_addr = call.get("to", "")
        value = call.get("value", "0")
        data = call.get("data", "0x")
        decoded = decode_function_call(data)
        extracted.append(
            {
                "depth": 0,
                "type": "CALL",
                "to": to_addr,
                "from": tx.get("from_addr", ""),
                "value": int(value, 16)
                if isinstance(value, str) and value.startswith("0x")
                else int(value)
                if value
                else 0,
                "data": data,
                "decoded": decoded.model_dump() if decoded else None,
                "gas": "0",
                "gas_used": "0",
                "children": [],
            }
        )

    return extracted
