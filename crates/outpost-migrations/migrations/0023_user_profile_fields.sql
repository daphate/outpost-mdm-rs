-- v0.18.16: добавить поля профиля пользователя.
--
-- Изначально таблица users имела только login + email + password_hash +
-- role_id + must_change_password + 2fa fields. Для удобного fleet
-- management команде нужны:
--   - display_name    — человекочитаемое отображаемое имя в UI и логах
--   - comment         — admin-комментарий («ст. лейтенант С., штаб 12»)
--   - phone           — рабочий телефон
--   - tg              — Telegram username (без `@`) для contact'а
--
-- SQLite ALTER TABLE ADD COLUMN — без default'а / NOT NULL, безопасно
-- для существующих row'ов (получат NULL).
-- Идемпотентно: повторное выполнение не вызовет проблем, sqlx::migrate
-- ведёт счётчик применённых миграций.

ALTER TABLE users ADD COLUMN display_name TEXT;
ALTER TABLE users ADD COLUMN comment      TEXT;
ALTER TABLE users ADD COLUMN phone        TEXT;
ALTER TABLE users ADD COLUMN tg           TEXT;
