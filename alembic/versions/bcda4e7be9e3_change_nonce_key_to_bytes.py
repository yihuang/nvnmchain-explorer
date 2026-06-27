"""change nonce_key from integer to bytes (blob)

Revision ID: bcda4e7be9e3
Revises: e9414a2d8914
Create Date: 2026-06-27 18:30:00.000000

"""

from collections.abc import Sequence

import sqlalchemy as sa

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "bcda4e7be9e3"
down_revision: str | Sequence[str] | None = "e9414a2d8914"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def _int_to_bytes(val):
    """Convert an integer to bytes (big-endian). Returns None for zero/null."""
    if val is None or val == 0:
        return b""
    n = int(val)
    return n.to_bytes((n.bit_length() + 7) // 8, "big")


def upgrade() -> None:
    """Upgrade schema."""
    conn = op.get_bind()

    # 1. Add a temporary column
    op.add_column("transactions", sa.Column("nonce_key_blob", sa.LargeBinary(), nullable=True))

    # 2. Convert existing data
    rows = conn.execute(
        sa.text("SELECT hash, nonce_key FROM transactions WHERE nonce_key IS NOT NULL")
    ).fetchall()
    for row in rows:
        hash_val = row[0]
        nk_val = row[1]
        try:
            nk_bytes = _int_to_bytes(nk_val)
        except (ValueError, OverflowError, TypeError):
            nk_bytes = b""
        conn.execute(
            sa.text("UPDATE transactions SET nonce_key_blob = :nk WHERE hash = :hash"),
            {"nk": nk_bytes, "hash": hash_val},
        )

    # 3. Drop old column and rename new one using batch mode
    with op.batch_alter_table("transactions") as batch_op:
        batch_op.drop_column("nonce_key")
        batch_op.alter_column("nonce_key_blob", new_column_name="nonce_key")


def downgrade() -> None:
    """Downgrade schema — convert bytes back to integer."""
    conn = op.get_bind()

    # 1. Add temporary integer column
    op.add_column("transactions", sa.Column("nonce_key_int", sa.Integer(), nullable=True))

    # 2. Convert bytes back to integer (lossy for values > 2^63)
    rows = conn.execute(
        sa.text("SELECT hash, nonce_key FROM transactions WHERE nonce_key IS NOT NULL")
    ).fetchall()
    for row in rows:
        hash_val = row[0]
        nk_blob = row[1]
        nk_int = int.from_bytes(nk_blob, "big") if nk_blob else 0
        conn.execute(
            sa.text("UPDATE transactions SET nonce_key_int = :nk WHERE hash = :hash"),
            {"nk": nk_int, "hash": hash_val},
        )

    # 3. Swap columns
    with op.batch_alter_table("transactions") as batch_op:
        batch_op.drop_column("nonce_key")
        batch_op.alter_column("nonce_key_int", new_column_name="nonce_key")
