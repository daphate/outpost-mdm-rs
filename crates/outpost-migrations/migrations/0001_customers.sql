-- Outpost MDM schema: customers (tenancy root).
--
-- The schema retains a Customer concept even though the initial deployment
-- is single-tenant. All foreign keys cascade from this table so a future
-- multi-tenant deployment requires no schema surgery.

CREATE TABLE customers (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    name          TEXT    NOT NULL,
    description   TEXT,
    -- Per-tenant configuration namespace (JSON-encoded, validated app-side).
    metadata_json TEXT    NOT NULL DEFAULT '{}',
    created_at    TEXT    NOT NULL DEFAULT (datetime('now')),
    updated_at    TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE UNIQUE INDEX idx_customers_name ON customers(name);

-- The single tenant for an Outpost deployment. Future multi-tenant work
-- inserts additional rows here.
INSERT INTO customers (id, name, description)
VALUES (1, 'default', 'Default tenant');
