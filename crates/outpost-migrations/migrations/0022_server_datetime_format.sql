-- v0.18.16: seed `settings.server.datetime_format = 'ru'`.
--
-- Аналогично migration 0020 (server.timezone) — settings-key с дефолтным
-- значением. На /settings будет dropdown с 4 вариантами (ru / iso / eu / us);
-- AppState.server_dt_format подтягивается на startup'е (load_server_dt_format)
-- и hot-reloadable через settings_save handler.
--
-- WHERE-guard на отсутствие ключа делает миграцию идемпотентной — если admin
-- успел поставить кастомное значение через UI до этой миграции, не перетираем.
--
-- Schema settings: (key TEXT PRIMARY KEY, value_json TEXT, updated_at TEXT).
-- Колонки `updated_by` НЕТ — settings_save апдейтит через upsert без неё.

INSERT INTO settings (key, value_json, updated_at)
SELECT 'server.datetime_format', '"ru"', datetime('now')
WHERE NOT EXISTS (
  SELECT 1 FROM settings WHERE key = 'server.datetime_format'
);
