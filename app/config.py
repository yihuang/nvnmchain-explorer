"""App configuration."""

from __future__ import annotations

from dataclasses import dataclass, field

from tempo.constants import CHAIN_ID_MAINNET, RPC_URL_MAINNET


@dataclass
class Settings:
    rpc_url: str = field(default=RPC_URL_MAINNET)
    chain_id: int = field(default=CHAIN_ID_MAINNET)
    port: int = 8080
    host: str = "0.0.0.0"
    db_path: str = "explorer.db"
    # How many blocks to cache
    max_cached_blocks: int = 100_000
    # How many recent blocks to show on home
    recent_block_count: int = 15
    # How many recent txs to show on home
    recent_tx_count: int = 15


settings = Settings()
