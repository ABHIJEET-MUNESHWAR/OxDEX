-- Initial OxDEX schema.
-- One table holds all signed intents and their lifecycle. The signed
-- payload is stored as JSONB so we can evolve the wire format without
-- migrations, while the indexed columns (mints, valid_to, status, owner)
-- power the hot-path queries.

CREATE TABLE IF NOT EXISTS orders (
    id           BYTEA       PRIMARY KEY,         -- 32-byte sha256
    owner        BYTEA       NOT NULL,            -- 32-byte ed25519 pubkey
    sell_mint    BYTEA       NOT NULL,            -- 32-byte mint
    buy_mint     BYTEA       NOT NULL,            -- 32-byte mint
    valid_to     BIGINT      NOT NULL,            -- unix seconds
    status       TEXT        NOT NULL,            -- enum: open|auctioned|filled|partially_filled|cancelled|expired
    filled_sell  BIGINT      NOT NULL DEFAULT 0,
    filled_buy   BIGINT      NOT NULL DEFAULT 0,
    signed       JSONB       NOT NULL,            -- canonical SignedOrder JSON
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Hot path: solver scans open orders for a given pair.
CREATE INDEX IF NOT EXISTS orders_open_by_pair
    ON orders (sell_mint, buy_mint)
    WHERE status = 'open';

-- Sweep job: find expired open orders.
CREATE INDEX IF NOT EXISTS orders_open_by_valid_to
    ON orders (valid_to)
    WHERE status = 'open';

-- Owner-scoped lookups.
CREATE INDEX IF NOT EXISTS orders_by_owner
    ON orders (owner);

