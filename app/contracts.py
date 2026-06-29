"""Contract identification and labeling for Tempo chain.

Tempo has many well-known precompiles and system contracts with
predictable addresses. This module labels them for the explorer.
"""

from __future__ import annotations

from eth_utils import to_checksum_address
from tempo.constants import (
    ACCOUNT_KEYCHAIN_ADDRESS,
    ALPHA_USD,
    BETA_USD,
    FEE_MANAGER_ADDRESS,
    NONCE_ADDRESS,
    PATH_USD,
    RECEIVE_POLICY_GUARD_ADDRESS,
    SIGNATURE_VERIFIER_ADDRESS,
    STABLECOIN_DEX_ADDRESS,
    THETA_USD,
    TIP20_FACTORY_ADDRESS,
    TIP20_REWARDS_REGISTRY_ADDRESS,
    TIP403_REGISTRY_ADDRESS,
    VALIDATOR_CONFIG_ADDRESS,
)

# ── Precompile address ranges ──────────────────────────────────────────
# Tempo uses pattern-based addresses for precompiles:
#   0x20C000... → TIP-20 tokens (StdTokens.sol)
#   0xfeEC0000... → FeeManager
#   0xaAAAaaAA... → AccountKeychain
#   etc.
PRECOMPILE_LABELS: dict[str, str] = {
    FEE_MANAGER_ADDRESS: "Fee Manager",
    TIP403_REGISTRY_ADDRESS: "TIP-403 Registry",
    TIP20_FACTORY_ADDRESS: "TIP-20 Factory",
    TIP20_REWARDS_REGISTRY_ADDRESS: "TIP-20 Rewards Registry",
    STABLECOIN_DEX_ADDRESS: "Stablecoin DEX",
    NONCE_ADDRESS: "Nonce Manager",
    VALIDATOR_CONFIG_ADDRESS: "Validator Config",
    ACCOUNT_KEYCHAIN_ADDRESS: "Account Keychain",
    SIGNATURE_VERIFIER_ADDRESS: "Signature Verifier",
    RECEIVE_POLICY_GUARD_ADDRESS: "Receive Policy Guard",
}

# TIP-20 native token prefix: all tokens start with 0x20C000...
TIP20_TOKEN_PREFIX = "0x20c0000000000000000"

# Known TIP-20 token addresses (StdTokens)
KNOWN_TOKENS: dict[str, dict[str, str]] = {
    PATH_USD: {
        "name": "pathUSD",
        "symbol": "pathUSD",
        "currency": "USD",
    },
    ALPHA_USD: {
        "name": "Alpha USD",
        "symbol": "ALPHA",
        "currency": "USD",
    },
    BETA_USD: {
        "name": "Beta USD",
        "symbol": "BETA",
        "currency": "USD",
    },
    THETA_USD: {
        "name": "Theta USD",
        "symbol": "THETA",
        "currency": "USD",
    },
}


def is_precompile_address(addr: str) -> bool:
    """Check if an address is a known precompile."""
    checksummed = to_checksum_address(addr)
    return checksummed in PRECOMPILE_LABELS


def get_precompile_name(addr: str) -> str | None:
    """Get the human-readable name of a precompile address."""
    return PRECOMPILE_LABELS.get(to_checksum_address(addr))


def is_tip20_token(addr: str) -> bool:
    """Check if an address looks like a TIP-20 token.

    TIP-20 native tokens follow the pattern 0x20C000... with
    the last 20 hex digits encoding the token index.
    """
    lower = addr.lower()
    return lower.startswith("0x20c0000000000000000")


def is_contract(addr: str) -> bool:
    """Check if an address is a contract (not EOA) based on known patterns.

    On Tempo, most interesting addresses are precompiles or tokens.
    We also check if the address is in our contract_labels table.
    """
    checksummed = to_checksum_address(addr)
    if checksummed in PRECOMPILE_LABELS:
        return True
    return bool(is_tip20_token(addr))


def get_contract_name(addr: str) -> str | None:
    """Get a friendly name for a contract address."""
    checksummed = to_checksum_address(addr)
    if checksummed in PRECOMPILE_LABELS:
        return PRECOMPILE_LABELS[checksummed]
    if checksummed in KNOWN_TOKENS:
        return KNOWN_TOKENS[checksummed]["name"]
    if is_tip20_token(addr):
        return None  # Let the caller look up from token_metadata
    return None


def is_eoa(addr: str) -> bool:
    """On Tempo, an EOA is any address that is NOT a known contract/precompile."""
    return not is_contract(addr)


def identify_address(addr: str) -> dict:
    """Classify an address into a type with label."""
    checksummed = to_checksum_address(addr)
    if checksummed in PRECOMPILE_LABELS:
        return {"type": "precompile", "label": PRECOMPILE_LABELS[checksummed]}
    if checksummed in KNOWN_TOKENS:
        info = KNOWN_TOKENS[checksummed]
        return {"type": "token", "label": info["name"], "symbol": info["symbol"]}
    if is_tip20_token(addr):
        return {"type": "token", "label": None, "symbol": None}
    return {"type": "eoa", "label": None}
