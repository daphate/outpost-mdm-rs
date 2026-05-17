-- Outpost MDM schema: push messages and scheduled push tasks.
--
-- Folded from upstream's `push` plugin into the core schema (no plugin SPI
-- in Outpost). The scheduler (a tokio task running every 60s) drains
-- `push_schedule` rows whose `due_at` is in the past, fanning out into
-- per-device `push_messages`.

CREATE TABLE push_messages (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id     INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    device_id       INTEGER NOT NULL REFERENCES devices(id)   ON DELETE CASCADE,
    -- Command code: 'reboot', 'install-apk', 'update-config', 'sync-models',
    -- 'sync-knowledge', 'sync-maps', 'remote-wipe', …
    command         TEXT    NOT NULL,
    -- Free-form JSON payload (e.g. {application_version_id: 42}).
    payload_json    TEXT    NOT NULL DEFAULT '{}',
    -- Lifecycle: 'pending' → 'sent' → 'delivered' | 'failed' | 'cancelled'
    status          TEXT    NOT NULL DEFAULT 'pending',
    -- Optional: tie a push back to the scheduled task that produced it.
    schedule_id     INTEGER REFERENCES push_schedule(id) ON DELETE SET NULL,
    created_at      TEXT    NOT NULL DEFAULT (datetime('now')),
    sent_at         TEXT,
    delivered_at    TEXT,
    last_error      TEXT
);

CREATE INDEX idx_push_messages_device  ON push_messages(device_id, status);
CREATE INDEX idx_push_messages_status  ON push_messages(status, created_at);

CREATE TABLE push_schedule (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id       INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    -- Targeting: exactly one of (device_id, group_id, configuration_id, NULL=all)
    device_id         INTEGER REFERENCES devices(id)        ON DELETE CASCADE,
    group_id          INTEGER REFERENCES groups(id)         ON DELETE CASCADE,
    configuration_id  INTEGER REFERENCES configurations(id) ON DELETE CASCADE,
    command           TEXT    NOT NULL,
    payload_json      TEXT    NOT NULL DEFAULT '{}',
    -- One-shot at `due_at` if non-NULL; otherwise `cron_expr` for recurring.
    due_at            TEXT,
    cron_expr         TEXT,
    -- Lifecycle: 'pending' → 'running' → 'done' | 'failed' | 'cancelled'
    status            TEXT    NOT NULL DEFAULT 'pending',
    created_by        INTEGER REFERENCES users(id) ON DELETE SET NULL,
    created_at        TEXT    NOT NULL DEFAULT (datetime('now')),
    last_run_at       TEXT,
    last_error        TEXT
);

CREATE INDEX idx_push_schedule_due    ON push_schedule(due_at, status);
CREATE INDEX idx_push_schedule_status ON push_schedule(status);
