-- Situational platform Ф0: heterogeneous device classes + org-unit hierarchy.
--
-- device_class — extensible discriminator so one MDM registry can carry
--   different device kinds (android_tactical | acoustic_node | stalker_player |
--   wearable | …). Default keeps every existing row = the current Outpost phone
--   fleet, so this is backward-compatible. Enrollment captures the class going
--   forward (see routes/enrollment.rs).
--
-- units — per-customer subdivision tree (self-referencing parent_id). Adds the
--   "подразделение" scoping axis alongside the existing customer(tenant)/role
--   axes; maps and admin views filter by unit. Kept separate from the flat
--   `groups` table, which stays a rollout/assignment label set.
--
-- Both `device.unit_id` and `user.unit_id` are nullable (ON DELETE SET NULL):
-- a device/user may be unassigned, and deleting a unit must not cascade-delete
-- devices/users (only their unit link).

ALTER TABLE devices ADD COLUMN device_class TEXT NOT NULL DEFAULT 'android_tactical';
CREATE INDEX idx_devices_class ON devices(customer_id, device_class);

CREATE TABLE units (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    parent_id   INTEGER          REFERENCES units(id)     ON DELETE CASCADE,
    name        TEXT    NOT NULL,
    description TEXT,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now')),
    updated_at  TEXT    NOT NULL DEFAULT (datetime('now')),
    UNIQUE (customer_id, parent_id, name)
);

CREATE INDEX idx_units_customer ON units(customer_id);
CREATE INDEX idx_units_parent   ON units(parent_id);

-- SQLite ADD COLUMN with a REFERENCES clause requires a NULL default (FKs on),
-- which is exactly what we want here.
ALTER TABLE devices ADD COLUMN unit_id INTEGER REFERENCES units(id) ON DELETE SET NULL;
CREATE INDEX idx_devices_unit ON devices(unit_id);

ALTER TABLE users ADD COLUMN unit_id INTEGER REFERENCES units(id) ON DELETE SET NULL;
CREATE INDEX idx_users_unit ON users(unit_id);
