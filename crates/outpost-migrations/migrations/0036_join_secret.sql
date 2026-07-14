-- Режим B привязки STALKER: общий join-секрет игры на профиле заказчика.
-- Заполнен → самрегистрация игроков включена (POST /api/v1/join создаёт
-- устройство stalker_player по секрету); NULL → выключена. join_unit_id —
-- подразделение, куда падают новые игроки (опционально).
ALTER TABLE customer_profiles ADD COLUMN join_secret TEXT;
ALTER TABLE customer_profiles ADD COLUMN join_unit_id INTEGER REFERENCES units(id) ON DELETE SET NULL;
CREATE INDEX idx_customer_profiles_join ON customer_profiles(join_secret);
