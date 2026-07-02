-- Admin-списки push-сообщений фильтруют по customer_id и сортируют по времени
-- (страница /push, per-device история). Существующие индексы покрывают
-- (device_id, status) и (status, created_at), но не tenant-scoped выборку —
-- она сканировала растущую таблицу целиком. Добавляем составной индекс.
CREATE INDEX IF NOT EXISTS idx_push_messages_customer
    ON push_messages(customer_id, created_at);
