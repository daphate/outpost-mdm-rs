-- Ф1 map framework: position history/tracks + fleet-wide tactical markers.

-- Latest-fix extras on devices (moving-device fields the phones report on /sync,
-- alongside the existing last_lat/last_lon). All nullable — old rows / non-moving
-- classes (e.g. acoustic_node) simply leave them empty.
ALTER TABLE devices ADD COLUMN last_alt      REAL;
ALTER TABLE devices ADD COLUMN last_bearing  REAL;
ALTER TABLE devices ADD COLUMN last_speed    REAL;
ALTER TABLE devices ADD COLUMN last_accuracy REAL;
-- Time of the last row appended to device_positions (for the append throttle).
ALTER TABLE devices ADD COLUMN last_track_at TEXT;

-- Position history for tracks/breadcrumbs. Appended by /sync under a
-- distance-or-time throttle (~10 m / ~30 s) to cap write volume; pruned by the
-- scheduler on settings.positions.retention_days. Current-fleet map reads the
-- latest fix off `devices`, NOT this table, so only two indexes are needed.
CREATE TABLE device_positions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    device_id   INTEGER NOT NULL REFERENCES devices(id)   ON DELETE CASCADE,
    ts          TEXT    NOT NULL DEFAULT (datetime('now')),  -- server receive time, UTC
    lat         REAL    NOT NULL,
    lon         REAL    NOT NULL,
    alt         REAL,
    bearing     REAL,
    speed       REAL,
    accuracy    REAL
);
CREATE INDEX idx_device_positions_device_ts ON device_positions(device_id, ts);
CREATE INDEX idx_device_positions_ts        ON device_positions(ts);

-- Fleet-wide tactical markers (enemy / vehicle / poi / sos) reported by devices
-- and rendered on the situational maps. `id` is the client UUID so re-reports of
-- the same marker upsert idempotently (ON CONFLICT(id) DO UPDATE). `expires_at`
-- (created_at + ttl for enemy/sos, NULL for poi/vehicle) drives client fade +
-- scheduler hard-delete; `is_active` is the soft-retract flag.
CREATE TABLE tactical_markers (
    id                 TEXT    PRIMARY KEY,
    customer_id        INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    kind               TEXT    NOT NULL CHECK (kind IN ('enemy','vehicle','poi','sos')),
    subtype            TEXT,
    lat                REAL    NOT NULL,
    lon                REAL    NOT NULL,
    alt                REAL,
    confidence         TEXT,
    label              TEXT,
    notes              TEXT,
    reporter_device_id INTEGER REFERENCES devices(id) ON DELETE SET NULL,
    reporter_callsign  TEXT,
    created_at         TEXT    NOT NULL DEFAULT (datetime('now')),
    updated_at         TEXT    NOT NULL DEFAULT (datetime('now')),
    expires_at         TEXT,
    is_active          INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX idx_tactical_markers_customer_active ON tactical_markers(customer_id, is_active, expires_at);
CREATE INDEX idx_tactical_markers_reporter        ON tactical_markers(reporter_device_id);
