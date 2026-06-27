"""Token metadata fetching from Tempo chain.

Fetches name, symbol, decimals, totalSupply from TIP-20 tokens.
"""

from __future__ import annotations

from eth_abi import decode as abi_decode
from eth_utils import to_bytes, to_checksum_address

from .contracts import KNOWN_TOKENS
from .rpc import rpc


def _name_call_data() -> str:
    return "0x06fdde03"  # name() selector


def _symbol_call_data() -> str:
    return "0x95d89b41"  # symbol() selector


def _decimals_call_data() -> str:
    return "0x313ce567"  # decimals() selector


def _total_supply_call_data() -> str:
    return "0x18160ddd"  # totalSupply() selector


def _decode_string_result(raw: str) -> str:
    """Decode a string result from an eth_call."""
    if not raw or raw == "0x":
        return ""
    try:
        raw_bytes = to_bytes(hexstr=raw)
        if not raw_bytes:
            return ""
        (result,) = abi_decode(["string"], raw_bytes)
        return result
    except Exception:
        return ""


def _decode_uint8_result(raw: str) -> int:
    if not raw or raw == "0x":
        return 18
    try:
        raw_bytes = to_bytes(hexstr=raw)
        if not raw_bytes:
            return 18
        (result,) = abi_decode(["uint8"], raw_bytes)
        return result
    except Exception:
        return 18


def _decode_uint256_result(raw: str) -> int:
    if not raw or raw == "0x":
        return 0
    try:
        raw_bytes = to_bytes(hexstr=raw)
        if not raw_bytes:
            return 0
        (result,) = abi_decode(["uint256"], raw_bytes)
        return result
    except Exception:
        return 0


async def fetch_token_metadata(address: str) -> dict:
    """Fetch TIP-20 token metadata from the chain."""
    checksummed = to_checksum_address(address)
    known = KNOWN_TOKENS.get(checksummed)

    name = known["name"] if known else await _fetch_name(address)
    symbol = known["symbol"] if known else await _fetch_symbol(address)
    decimals = await _fetch_decimals(address)
    total_supply = await _fetch_total_supply(address)

    # Determine currency from symbol
    if symbol and not (known and known.get("currency")):
        # Try to infer currency
        upper = symbol.upper()
        if upper.endswith("USD") or upper in ("USDC", "USDT", "DAI", "FRAX"):
            currency = "USD"
        elif upper.endswith("EUR"):
            currency = "EUR"
        else:
            currency = symbol
    else:
        currency = known["currency"] if known else (symbol or "")

    return {
        "address": checksummed,
        "name": name,
        "symbol": symbol,
        "decimals": decimals,
        "currency": currency,
        "total_supply": total_supply,
    }


async def _fetch_name(address: str) -> str:
    try:
        result = await rpc.eth_call(address, _name_call_data())
        return _decode_string_result(result)
    except Exception:
        return ""


async def _fetch_symbol(address: str) -> str:
    try:
        result = await rpc.eth_call(address, _symbol_call_data())
        return _decode_string_result(result)
    except Exception:
        return ""


async def _fetch_decimals(address: str) -> int:
    try:
        result = await rpc.eth_call(address, _decimals_call_data())
        return _decode_uint8_result(result)
    except Exception:
        return 18


async def _fetch_total_supply(address: str) -> int:
    try:
        result = await rpc.eth_call(address, _total_supply_call_data())
        return _decode_uint256_result(result)
    except Exception:
        return 0


def format_token_amount(amount: int | str, decimals: int = 18) -> str:
    """Format a token amount to a human-readable string."""
    if isinstance(amount, str):
        try:
            amount = int(amount)
        except (ValueError, TypeError):
            return "0"
    if amount == 0:
        return "0"
    divisor = 10**decimals
    integer_part = amount // divisor
    fractional_part = amount % divisor
    if fractional_part == 0:
        return str(integer_part)
    # Show up to 6 significant decimals
    frac_str = str(fractional_part).zfill(decimals)
    # Trim trailing zeros
    frac_str = frac_str.rstrip("0")
    if not frac_str:
        return str(integer_part)
    # Keep at most 6 digits
    if len(frac_str) > 6:
        frac_str = frac_str[:6]
    return f"{integer_part}.{frac_str}"


def format_token_amount_with_symbol(amount: int | str, decimals: int = 18, symbol: str = "") -> str:
    """Format amount and append symbol."""
    formatted = format_token_amount(amount, decimals)
    if symbol:
        return f"{formatted} {symbol}"
    return formatted
