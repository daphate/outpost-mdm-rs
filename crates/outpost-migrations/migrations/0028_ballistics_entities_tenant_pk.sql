-- v0.18.20 — ballistics_entities: tenant-scoped primary key.
--
-- Root fix for the cross-tenant existence oracle that the response-level
-- 403→404 fixes (LEAK-2 / MT-3) could not close.
--
-- Problem: 0024 declared `ballistics_entities.id TEXT PRIMARY KEY` — a GLOBAL
-- unique constraint on the *client-supplied* id. So a given id could exist in
-- only ONE tenant fleet-wide. The create path (PUT put_entity) maps a PK
-- collision to 409 Conflict (v0.18.20 MT-3); but with a GLOBAL pk that
-- collision fires when the id is already taken in ANOTHER tenant, so 409-vs-201
-- leaked whether an id exists somewhere in the fleet — an existence oracle over
-- the global id space. No response-level change can close this: the leak IS the
-- create-collision.
--
-- Fix: drop the standalone `id PRIMARY KEY`; make the primary key the composite
-- `(id, customer_id)`. id is now unique only WITHIN a tenant, so the same
-- logical id can coexist across tenants and a cross-tenant create-collision is
-- impossible — the 409 path can only fire for a genuine SAME-tenant duplicate
-- (which the caller is entitled to observe within its own tenant).
--
-- PK column order is (id, customer_id), NOT (customer_id, id): the
-- ballistics_wraps FK references ballistics_entities(id, customer_id), and
-- SQLite resolves a foreign key against a unique index on exactly those columns
-- in that order. Keeping (id, customer_id) reuses the column order the existing
-- UNIQUE + FK already rely on; per-tenant uniqueness is identical either way.
--
-- Rebuild mechanics: SQLite cannot ALTER a primary key, so the table is rebuilt.
-- foreign_keys is connection-level (db.rs `.foreign_keys(true)` in production;
-- OFF in the migration tests' raw pool) and is a no-op to toggle inside a
-- migration transaction, so we CANNOT disable it here. With FK ON a naive
-- `DROP TABLE ballistics_entities` would implicit-DELETE its rows and CASCADE
-- into ballistics_wraps (ON DELETE CASCADE) — data loss. We instead: back the
-- wraps up to a constraint-free table, drop the wraps child (removing the FK),
-- rebuild entities, recreate the wraps table, restore the wrap rows (FK now
-- validates against the rebuilt entities), and rebuild every index. This order
-- never deletes a referenced parent row, so it is safe whether FK is ON or OFF.
-- The whole migration runs in sqlx's single transaction (atomic; any error
-- rolls the entire rebuild back).

-- 1. Back wraps up to a constraint-free table. CREATE TABLE AS SELECT copies
--    data only — no PK/FK/CASCADE — so the next DROP cannot cascade into it.
CREATE TABLE _ballistics_wraps_backup AS SELECT * FROM ballistics_wraps;

-- 2. Drop the child first. Nothing references ballistics_wraps, so this is a
--    clean drop that also removes the FK pointing at ballistics_entities.
DROP TABLE ballistics_wraps;

-- 3. Rebuild entities with the composite primary key (only change vs 0024:
--    `id TEXT PRIMARY KEY` + `UNIQUE (id, customer_id)` → `PRIMARY KEY
--    (id, customer_id)`; id stays NOT NULL via the composite PK).
CREATE TABLE ballistics_entities_new (
    id                  TEXT    NOT NULL,
    customer_id         INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    owner_user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    owner_device_id     INTEGER REFERENCES devices(id) ON DELETE SET NULL,
    kind                TEXT    NOT NULL CHECK (kind IN ('weapon', 'cartridge', 'dope', 'units')),
    parent_id           TEXT,
    name_hint           TEXT,
    version             INTEGER NOT NULL DEFAULT 1,
    created_ts          TEXT    NOT NULL DEFAULT (datetime('now')),
    modified_ts         TEXT    NOT NULL DEFAULT (datetime('now')),
    deleted_ts          TEXT,
    ciphertext          BLOB    NOT NULL,
    ciphertext_iv       BLOB    NOT NULL,
    ciphertext_tag      BLOB    NOT NULL,
    PRIMARY KEY (id, customer_id)
);

INSERT INTO ballistics_entities_new
    (id, customer_id, owner_user_id, owner_device_id, kind, parent_id, name_hint,
     version, created_ts, modified_ts, deleted_ts, ciphertext, ciphertext_iv, ciphertext_tag)
SELECT
    id, customer_id, owner_user_id, owner_device_id, kind, parent_id, name_hint,
    version, created_ts, modified_ts, deleted_ts, ciphertext, ciphertext_iv, ciphertext_tag
FROM ballistics_entities;

-- 4. Drop the old parent (no children now → no cascade) and rename the new one
--    into its place.
DROP TABLE ballistics_entities;
ALTER TABLE ballistics_entities_new RENAME TO ballistics_entities;

-- 5. Recreate entities indexes (identical to 0024).
CREATE INDEX idx_ballistics_entities_customer_user
    ON ballistics_entities(customer_id, owner_user_id);
CREATE INDEX idx_ballistics_entities_kind
    ON ballistics_entities(customer_id, owner_user_id, kind) WHERE deleted_ts IS NULL;
CREATE INDEX idx_ballistics_entities_modified
    ON ballistics_entities(customer_id, owner_user_id, modified_ts);
CREATE INDEX idx_ballistics_entities_parent
    ON ballistics_entities(customer_id, parent_id) WHERE parent_id IS NOT NULL;
CREATE INDEX idx_ballistics_entities_gc
    ON ballistics_entities(deleted_ts) WHERE deleted_ts IS NOT NULL;

-- 6. Recreate the wraps table (identical schema to 0024; the FK now resolves to
--    the rebuilt ballistics_entities by name).
CREATE TABLE ballistics_wraps (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id           TEXT    NOT NULL,
    customer_id         INTEGER NOT NULL,
    recipient_device_id INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    recipient_key_id    TEXT    NOT NULL,
    eph_pubkey_der      BLOB    NOT NULL,
    wrapped_dek         BLOB    NOT NULL,
    wrapped_dek_iv      BLOB    NOT NULL,
    created_at          TEXT    NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (entity_id, customer_id) REFERENCES ballistics_entities(id, customer_id) ON DELETE CASCADE,
    UNIQUE (entity_id, recipient_device_id)
);

-- 7. Restore wrap rows (explicit columns; preserves original autoincrement ids;
--    FK validates against the rebuilt entities when foreign_keys is ON).
INSERT INTO ballistics_wraps
    (id, entity_id, customer_id, recipient_device_id, recipient_key_id,
     eph_pubkey_der, wrapped_dek, wrapped_dek_iv, created_at)
SELECT
    id, entity_id, customer_id, recipient_device_id, recipient_key_id,
    eph_pubkey_der, wrapped_dek, wrapped_dek_iv, created_at
FROM _ballistics_wraps_backup;

-- 8. Recreate wraps indexes (identical to 0024).
CREATE INDEX idx_ballistics_wraps_entity ON ballistics_wraps(entity_id);
CREATE INDEX idx_ballistics_wraps_recipient ON ballistics_wraps(recipient_device_id);

-- 9. Drop the transient backup.
DROP TABLE _ballistics_wraps_backup;
