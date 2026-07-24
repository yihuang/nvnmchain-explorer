"""App configuration.

Defaults target Tempo mainnet; override with TEMPO_RPC / CHAIN_ID / DB_PATH
(and PORT / HOST) to point at a local node. Use a distinct DB_PATH per chain:
a local devnet's block hashes differ from mainnet's, so a shared cache is stale.
"""

from __future__ import annotations

import os
from dataclasses import dataclass, field

from tempo.constants import CHAIN_ID_MAINNET, RPC_URL_MAINNET


@dataclass
class Settings:
    rpc_url: str = field(default_factory=lambda: os.environ.get("TEMPO_RPC", RPC_URL_MAINNET))
    chain_id: int = field(default_factory=lambda: int(os.environ.get("CHAIN_ID", CHAIN_ID_MAINNET)))
    port: int = field(default_factory=lambda: int(os.environ.get("PORT", "8080")))
    host: str = field(default_factory=lambda: os.environ.get("HOST", "0.0.0.0"))
    db_path: str = field(default_factory=lambda: os.environ.get("DB_PATH", "explorer.db"))
    # How many blocks to cache
    max_cached_blocks: int = 100_000
    # How many recent blocks to show on home
    recent_block_count: int = 15
    # How many recent txs to show on home
    recent_tx_count: int = 15


settings = Settings()
