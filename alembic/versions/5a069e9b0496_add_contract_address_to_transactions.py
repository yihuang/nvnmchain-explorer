"""add contract_address to transactions

Revision ID: 5a069e9b0496
Revises: bcda4e7be9e3
Create Date: 2026-06-29 15:06:08.864214

"""

from typing import Sequence, Union

from alembic import op
import sqlalchemy as sa
import sqlmodel


# revision identifiers, used by Alembic.
revision: str = "5a069e9b0496"
down_revision: Union[str, Sequence[str], None] = "bcda4e7be9e3"
branch_labels: Union[str, Sequence[str], None] = None
depends_on: Union[str, Sequence[str], None] = None


def upgrade() -> None:
    op.add_column(
        "transactions",
        sa.Column("contract_address", sqlmodel.sql.sqltypes.AutoString(), nullable=True),
    )


def downgrade() -> None:
    op.drop_column("transactions", "contract_address")
