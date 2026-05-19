-- Server-wide timezone for admin UI rendering.
--
-- All timestamps in the database are stored as UTC (SQLite `datetime('now')`)
-- and shipped as `YYYY-MM-DD HH:MM:SS` strings. Previously the admin Web UI
-- rendered them verbatim, so an admin in Moscow saw 22:30 next to a row that
-- had actually happened at 01:30 local time. Confusing for live ops.
--
-- This setting names the IANA timezone we convert UTC timestamps INTO before
-- showing them on /devices, /telemetry, /devices/{id}/telemetry, /files, etc.
-- Default: Europe/Moscow (MSK, UTC+3 fixed since 2014).
--
-- The value is read once at outpost-server startup into AppState.server_tz
-- (Arc<RwLock<chrono_tz::Tz>>) and hot-reloaded on every /settings save —
-- no restart required after switching the dropdown in admin UI.
--
-- Idempotent — only INSERT if the key is absent (operator may have already
-- set a custom timezone via raw SQL or earlier migration revision).

INSERT INTO settings (key, value_json, description)
SELECT 'server.timezone',
       '"Europe/Moscow"',
       'IANA timezone name for rendering timestamps in admin UI. Default MSK.'
WHERE NOT EXISTS (
    SELECT 1 FROM settings WHERE key = 'server.timezone'
);
