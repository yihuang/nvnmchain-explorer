"""SQLModel ORM models for the Tempo Explorer database."""

from __future__ import annotations

from datetime import UTC, datetime

from pydantic import BaseModel
from sqlmodel import Field, SQLModel


def _utc_ts() -> int:
    """Current UTC Unix timestamp."""
    return int(datetime.now(UTC).timestamp())


class Block(SQLModel, table=True):
    __tablename__ = "blocks"

    number: int = Field(primary_key=True)
    hash: str = Field(unique=True, nullable=False, index=True)
    parent_hash: str = Field(nullable=False)
    timestamp: int = Field(nullable=False, index=True)
    gas_used: int = Field(default=0)
    gas_limit: int = Field(default=0)
    miner: str = Field(default="")
    tx_count: int = Field(default=0)
    raw: str = Field(default="{}")
    created_at: int = Field(default_factory=_utc_ts)


class Transaction(SQLModel, table=True):
    __tablename__ = "transactions"

    hash: str = Field(primary_key=True)
    block_number: int = Field(nullable=False, index=True, foreign_key="blocks.number")
    block_hash: str = Field(nullable=False)
    position: int = Field(default=0)
    from_addr: str = Field(nullable=False, index=True)
    to_addr: str | None = Field(default=None, index=True)
    status: int = Field(default=1)
    gas_limit: int = Field(default=0)
    gas_used: int = Field(default=0)
    gas_price: str = Field(default="0")
    max_fee_per_gas: str = Field(default="0")
    max_priority_fee_per_gas: str = Field(default="0")
    base_fee: str = Field(default="0")
    contract_address: str | None = Field(default=None)
    fee_token: str | None = Field(default=None)
    fee_amount: str = Field(default="0")
    nonce: int = Field(default=0)
    nonce_key: str | None = Field(default="0x")
    value: str = Field(default="0")
    chain_id: int = Field(default=4217)
    tx_type: int = Field(default=118)
    input: str = Field(default="0x")
    raw: str | None = Field(default=None)
    trace_data: str | None = Field(default=None)
    receipt_data: str | None = Field(default=None)
    timestamp: int = Field(default=0, index=True)
    created_at: int = Field(default_factory=_utc_ts)


class TokenMetadata(SQLModel, table=True):
    __tablename__ = "token_metadata"

    address: str = Field(primary_key=True)
    name: str = Field(default="", index=True)
    symbol: str = Field(default="")
    decimals: int = Field(default=18)
    currency: str = Field(default="")
    total_supply: str = Field(default="0")
    logo_uri: str = Field(default="")
    holder_count: int = Field(default=0)
    created_at: int = Field(default_factory=_utc_ts)
    updated_at: int = Field(default_factory=_utc_ts)


class ContractLabel(SQLModel, table=True):
    __tablename__ = "contract_labels"

    address: str = Field(primary_key=True)
    name: str = Field(default="")
    abi: str = Field(default="[]")
    is_token: int = Field(default=0)
    is_precompile: int = Field(default=0)
    created_at: int = Field(default_factory=_utc_ts)


class TransferEvent(SQLModel, table=True):
    __tablename__ = "transfer_events"

    id: int | None = Field(default=None, primary_key=True)
    tx_hash: str = Field(nullable=False, index=True)
    block_number: int = Field(nullable=False, index=True)
    log_index: int = Field(default=0)
    token_addr: str = Field(nullable=False, index=True)
    from_addr: str = Field(nullable=False, index=True)
    to_addr: str = Field(nullable=False, index=True)
    amount: str = Field(nullable=False)
    timestamp: int = Field(default=0)
    created_at: int = Field(default_factory=_utc_ts)


# ── Structured return types for decoder ─────────────────────────────


class DecodedParam(BaseModel):
    """A single decoded ABI parameter."""

    type: str
    name: str
    value: str
    indexed: bool = False


class DecodedCall(BaseModel):
    """A decoded function call (TIP20, known ABI, or unknown)."""

    name: str | None = None
    signature: str | None = None
    params: list[DecodedParam] = []
    selector: str = ""
    raw_args: str = ""


class DecodedEvent(BaseModel):
    """A decoded event log (TIP20 or unknown)."""

    name: str | None = None
    signature: str | None = None
    contract: str = ""
    params: list[DecodedParam] = []
    topic0: str = ""
    log_index: str | None = None
    transaction_hash: str | None = None
