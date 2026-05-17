-- Outpost MDM schema: system settings key-value store.
--
-- Holds installation-wide settings (server display name, default
-- enrollment QR URL, default config IDs, etc.). Values are stored as JSON
-- text so callers can persist scalars, arrays, or objects via the same
-- table.

CREATE TABLE settings (
    key         TEXT    PRIMARY KEY,
    value_json  TEXT    NOT NULL,
    description TEXT,
    updated_at  TEXT    NOT NULL DEFAULT (datetime('now'))
);

INSERT INTO settings (key, value_json, description) VALUES
    ('server.name',                '"Outpost MDM"',  'Display name shown in the admin UI header'),
    ('server.enrollment_base_url', 'null',           'Public base URL for enrollment QR codes; auto-detected if null'),
    ('push.scheduler_tick_secs',   '60',             'Push scheduler tick interval (seconds)'),
    ('files.signed_url_ttl_secs',  '300',            'Lifetime of signed download URLs (seconds)'),
    ('auth.jwt_ttl_secs',          '86400',          'Session JWT lifetime (seconds) — 24h default');
