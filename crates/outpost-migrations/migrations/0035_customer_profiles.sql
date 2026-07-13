-- Ф6: профиль заказчика — структурированное расширение per-tenant namespace.
-- Единый источник и для мультиарендного рантайма (по customer_id), и как вход
-- для per-customer on-prem сборки (см. docs/ON-PREM-BUILD.md). Данные, не форк
-- кода: включённые классы устройств, фиче-флаги, домен, брендирование,
-- параметры enrollment/TLS.
CREATE TABLE customer_profiles (
    customer_id     INTEGER PRIMARY KEY REFERENCES customers(id) ON DELETE CASCADE,
    single_tenant   INTEGER NOT NULL DEFAULT 0,  -- 1 = целевой on-прем single-tenant
    enabled_classes TEXT,      -- JSON-массив разрешённых device_class
    feature_flags   TEXT,      -- JSON-объект фиче-флагов (ballistics/bearing/players/…)
    domain          TEXT,      -- домен on-прем инстанса
    branding_json   TEXT,      -- брендирование (имя/цвета/лого-ref), опционально
    enrollment_json TEXT,      -- параметры enrollment/TLS
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
);
