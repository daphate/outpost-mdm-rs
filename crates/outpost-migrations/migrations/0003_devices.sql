-- Outpost MDM schema: devices, groups, device-group membership.
--
-- The `devices` table carries telemetry columns inline (battery_pct,
-- last_lat, last_lon, last_seen_at) folded from the upstream "deviceinfo"
-- plugin — Outpost has no plugin SPI, so the data lives in core.

CREATE TABLE groups (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    name        TEXT    NOT NULL,
    description TEXT,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now')),
    updated_at  TEXT    NOT NULL DEFAULT (datetime('now')),
    UNIQUE (customer_id, name)
);

CREATE INDEX idx_groups_customer ON groups(customer_id);

CREATE TABLE devices (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id        INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    -- Stable device identifier supplied at enrollment (often serial or IMEI).
    serial             TEXT    NOT NULL,
    -- Human-readable label (call-sign / room number / operator name).
    display_name       TEXT,
    -- argon2id-hashed enrollment secret, exchanged for a device JWT after
    -- enrollment. NULL after device has migrated to JWT-only auth.
    enrollment_secret  TEXT,
    -- Long-lived JWT issued at enrollment (rotated on re-enroll).
    device_token_jti   TEXT,
    -- Latest version reported by the device.
    app_version        TEXT,
    os_version         TEXT,
    -- Telemetry (folded from upstream "deviceinfo" plugin)
    battery_pct        INTEGER,           -- 0-100
    last_lat           REAL,
    last_lon           REAL,
    last_seen_at       TEXT,
    is_online          INTEGER NOT NULL DEFAULT 0,
    is_enrolled        INTEGER NOT NULL DEFAULT 0,
    is_active          INTEGER NOT NULL DEFAULT 1,
    -- Free-form per-device metadata for future expansion (Outpost-specific
    -- fields: preferred_llm, preferred_translator_llm, kiosk_pkg, …).
    metadata_json      TEXT    NOT NULL DEFAULT '{}',
    created_at         TEXT    NOT NULL DEFAULT (datetime('now')),
    updated_at         TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE UNIQUE INDEX idx_devices_serial ON devices(customer_id, serial);
CREATE INDEX        idx_devices_last_seen ON devices(last_seen_at);
CREATE INDEX        idx_devices_is_online ON devices(is_online);

CREATE TABLE device_groups (
    device_id INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    group_id  INTEGER NOT NULL REFERENCES groups(id)  ON DELETE CASCADE,
    PRIMARY KEY (device_id, group_id)
);

CREATE INDEX idx_device_groups_group ON device_groups(group_id);
