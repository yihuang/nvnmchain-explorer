"""Database layer using SQLModel ORM with Alembic migrations."""

from __future__ import annotations

from contextlib import contextmanager
from datetime import UTC, datetime

from sqlalchemy import event
from sqlalchemy import text as sa_text
from sqlmodel import Session, SQLModel, create_engine, func, select

from .config import settings
from .models import (
    Block,
    ContractLabel,
    TokenMetadata,
    Transaction,
    TransferEvent,
)
from .tokens import format_token_amount

# ── Engine ───────────────────────────────────────────────────────────

_engine: object | None = None
_engine_path: str | None = None


def get_engine():
    global _engine, _engine_path
    if _engine is None or _engine_path != settings.db_path:
        _engine = create_engine(
            f"sqlite:///{settings.db_path}",
            connect_args={"check_same_thread": False},
            echo=False,
        )

        @event.listens_for(_engine, "connect")
        def _wal(dbapi_conn, _record):
            """Serve reads while the indexer writes; the default journal locks them out."""
            dbapi_conn.execute("PRAGMA journal_mode=WAL")

        _engine_path = settings.db_path
    return _engine


def create_tables():
    """Create all tables from SQLModel metadata (dev/init only)."""
    SQLModel.metadata.create_all(get_engine())


init_db = create_tables  # backward-compat alias


@contextmanager
def get_session():
    """Context manager yielding a SQLModel Session with auto-commit."""
    session = Session(get_engine())
    try:
        yield session
        session.commit()
    except Exception:
        session.rollback()
        raise
    finally:
        session.close()


# ── Blocks ──────────────────────────────────────────────────────────


def save_block(data: dict) -> None:
    """Insert or replace a block."""
    with get_session() as session:
        stmt = select(Block).where(Block.number == data["number"])
        existing = session.exec(stmt).first()
        if existing:
            for key, val in data.items():
                setattr(existing, key, val)
        else:
            session.add(Block(**data))


def get_block_by_number(num: int) -> dict | None:
    try:
        with get_session() as session:
            row = session.exec(select(Block).where(Block.number == num)).first()
            return _row_to_dict(row) if row else None
    except (OverflowError, Exception):
        return None


def get_block_by_hash(hash_: str) -> dict | None:
    with get_session() as session:
        row = session.exec(select(Block).where(Block.hash == hash_)).first()
        return _row_to_dict(row) if row else None


def get_latest_block() -> dict | None:
    with get_session() as session:
        row = session.exec(select(Block).order_by(Block.number.desc()).limit(1)).first()
        return _row_to_dict(row) if row else None


def get_recent_blocks(limit: int = 15) -> list[dict]:
    with get_session() as session:
        rows = session.exec(select(Block).order_by(Block.number.desc()).limit(limit)).all()
        return [_row_to_dict(r) for r in rows]


# ── Transactions ────────────────────────────────────────────────────


def save_transaction(data: dict) -> None:
    """Insert or replace a transaction."""
    with get_session() as session:
        stmt = select(Transaction).where(Transaction.hash == data["hash"])
        existing = session.exec(stmt).first()
        if existing:
            for key, val in data.items():
                setattr(existing, key, val)
        else:
            session.add(Transaction(**data))


def get_transaction(hash_: str) -> dict | None:
    with get_session() as session:
        row = session.exec(select(Transaction).where(Transaction.hash == hash_)).first()
        return _row_to_dict(row) if row else None


def get_block_transactions(block_number: int) -> list[dict]:
    with get_session() as session:
        rows = session.exec(
            select(Transaction)
            .where(Transaction.block_number == block_number)
            .order_by(Transaction.position)
        ).all()
        return [_row_to_dict(r) for r in rows]


def get_recent_transactions(limit: int = 15) -> list[dict]:
    with get_session() as session:
        rows = session.exec(
            select(Transaction).order_by(Transaction.timestamp.desc()).limit(limit)
        ).all()
        return [_row_to_dict(r) for r in rows]


def get_address_transactions(address: str, page: int = 1, per_page: int = 25) -> list[dict]:
    offset = (page - 1) * per_page
    with get_session() as session:
        stmt = (
            select(Transaction)
            .where((Transaction.from_addr == address) | (Transaction.to_addr == address))
            .order_by(Transaction.timestamp.desc())
            .offset(offset)
            .limit(per_page)
        )
        rows = session.exec(stmt).all()
        return [_row_to_dict(r) for r in rows]


def get_address_transaction_count(address: str) -> int:
    with get_session() as session:
        stmt = select(func.count(Transaction.hash)).where(
            (Transaction.from_addr == address) | (Transaction.to_addr == address)
        )
        result = session.exec(stmt).one()
        return result or 0


# ── Token Metadata ──────────────────────────────────────────────────


def save_token_metadata(data: dict) -> None:
    """Insert or update token metadata."""

    with get_session() as session:
        stmt = select(TokenMetadata).where(TokenMetadata.address == data["address"])
        existing = session.exec(stmt).first()
        if existing:
            data["updated_at"] = int(datetime.now(UTC).timestamp())
            for key, val in data.items():
                setattr(existing, key, val)
        else:
            session.add(TokenMetadata(**data))


def get_token_metadata(address: str) -> dict | None:
    with get_session() as session:
        row = session.exec(select(TokenMetadata).where(TokenMetadata.address == address)).first()
        return _row_to_dict(row) if row else None


def get_all_tokens(page: int = 1, per_page: int = 25) -> list[dict]:
    offset = (page - 1) * per_page
    with get_session() as session:
        rows = session.exec(
            select(TokenMetadata)
            .order_by(TokenMetadata.holder_count.desc())
            .offset(offset)
            .limit(per_page)
        ).all()
        return [_row_to_dict(r) for r in rows]


def get_token_count() -> int:
    with get_session() as session:
        result = session.exec(select(func.count(TokenMetadata.address))).one()
        return result or 0


# ── Contract Labels ─────────────────────────────────────────────────


def save_contract_label(data: dict) -> None:
    with get_session() as session:
        stmt = select(ContractLabel).where(ContractLabel.address == data["address"])
        existing = session.exec(stmt).first()
        if existing:
            for key, val in data.items():
                setattr(existing, key, val)
        else:
            session.add(ContractLabel(**data))


def get_contract_label(address: str) -> dict | None:
    with get_session() as session:
        row = session.exec(select(ContractLabel).where(ContractLabel.address == address)).first()
        return _row_to_dict(row) if row else None


# ── Transfer Events ─────────────────────────────────────────────────


def save_transfer(data: dict) -> None:
    with get_session() as session:
        session.add(TransferEvent(**data))


def get_token_transfers(token_addr: str, page: int = 1, per_page: int = 25) -> list[dict]:
    offset = (page - 1) * per_page
    with get_session() as session:
        # Join with transactions for from/to addresses
        rows = session.exec(
            select(
                TransferEvent,
                Transaction.from_addr.label("tx_from"),
                Transaction.to_addr.label("tx_to"),
                Transaction.timestamp.label("tx_timestamp"),
                Transaction.status.label("tx_status"),
            )
            .join(Transaction, Transaction.hash == TransferEvent.tx_hash)
            .where(TransferEvent.token_addr == token_addr)
            .order_by(TransferEvent.block_number.desc(), TransferEvent.log_index.desc())
            .offset(offset)
            .limit(per_page)
        ).all()
        result = []
        for row in rows:
            ev = _row_to_dict(row[0])
            ev["tx_from"] = getattr(row[1], "tx_from", None) if hasattr(row, "__len__") else None
            ev["tx_to"] = getattr(row[2], "tx_to", None) if hasattr(row, "__len__") else None
            ev["tx_timestamp"] = (
                getattr(row[3], "tx_timestamp", None) if hasattr(row, "__len__") else None
            )
            ev["tx_status"] = (
                getattr(row[4], "tx_status", None) if hasattr(row, "__len__") else None
            )
            if not ev.get("tx_from"):
                ev["tx_from"] = (
                    get_transaction(ev["tx_hash"]).get("from_addr", "")
                    if get_transaction(ev["tx_hash"])
                    else ""
                )
            result.append(ev)
        return result


def get_address_transfers(address: str, page: int = 1, per_page: int = 25) -> list[dict]:
    offset = (page - 1) * per_page
    with get_session() as session:
        rows = session.exec(
            select(TransferEvent)
            .where((TransferEvent.from_addr == address) | (TransferEvent.to_addr == address))
            .order_by(TransferEvent.block_number.desc(), TransferEvent.log_index.desc())
            .offset(offset)
            .limit(per_page)
        ).all()
        result = []
        for r in rows:
            d = _row_to_dict(r)
            tx = get_transaction(d["tx_hash"])
            if tx:
                d["tx_from"] = tx.get("from_addr")
                d["tx_to"] = tx.get("to_addr")
                d["tx_timestamp"] = tx.get("timestamp")
                d["tx_status"] = tx.get("status")
            result.append(d)
        return result


# ── Holdings helper ─────────────────────────────────────────────────


def get_address_holdings(address: str) -> list[dict]:
    """Get token holdings for an address from transfer event aggregation."""
    with get_session() as session:
        stmt = sa_text("""
            SELECT token_addr,
                   SUM(CASE WHEN to_addr = :addr THEN CAST(amount AS INTEGER)
                            WHEN from_addr = :addr THEN -CAST(amount AS INTEGER)
                       END) as balance
            FROM transfer_events
            WHERE from_addr = :addr OR to_addr = :addr
            GROUP BY token_addr
            HAVING balance != 0
        """)
        result = session.execute(stmt, {"addr": address}).all()
        holdings = []
        for row in result:
            meta = get_token_metadata(row[0])
            if meta:
                holdings.append(
                    {
                        "token": meta["address"],
                        "name": meta["name"],
                        "symbol": meta["symbol"],
                        "decimals": meta["decimals"],
                        "balance": int(row[1]) if row[1] else 0,
                        "formatted": format_token_amount(
                            int(row[1]) if row[1] else 0, meta["decimals"]
                        ),
                    }
                )
        return holdings


# ── Helper ──────────────────────────────────────────────────────────


def _row_to_dict(row) -> dict:
    """Convert a SQLModel row to a dict, excluding SQLAlchemy internals."""
    if row is None:
        return {}
    d = {}
    for column in row.__table__.columns:
        val = getattr(row, column.name)
        d[column.name] = val
    return d
