-- Per-device / per-group / per-customer bootstrap bundle assignments.
--
-- Каждый Outpost-Android клиент при fetch'е `bootstrap-manifest.json`
-- видит общий `bundles[]` массив (минимум / рекомендуемый / полный /
-- soldier-v31-medical). MDM может **target'ить** конкретный bundle
-- (e.g. soldier-v31-medical) на:
--   - device (target_type='device', target_id=devices.id)
--   - device group (target_type='group', target_id=groups.id)
--   - customer-wide (target_type='customer', target_id=customers.id)
--
-- Resolution chain при `GET /api/v1/devices/{id}/bundles`:
--   1. customer-wide assignment (lowest specificity, baseline)
--   2. group assignments (where device is member)
--   3. direct device assignment (highest specificity, overrides)
-- Внутри одного specificity-уровня — `priority` DESC.
--
-- Assignment **не означает** что bundle уже скачан — это hint клиенту
-- который BundleDownloader использует для prioritization. Реальная
-- загрузка идёт по obvious manifest URL'ам (Cloud.ru primary, R2 fallback).
--
-- See: `tools/CONTENT-DISTRIBUTION-CONTRACT.md` §«Канал 2: bundles[]» +
--      INSIGHT-054 (soldier-v31 bundle 2026-06-03).

CREATE TABLE bundle_assignments (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id          INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    bundle_id            TEXT    NOT NULL,
    target_type          TEXT    NOT NULL CHECK(target_type IN ('device','group','customer')),
    target_id            INTEGER NOT NULL,
    priority             INTEGER NOT NULL DEFAULT 100,
    assigned_by_user_id  INTEGER          REFERENCES users(id) ON DELETE SET NULL,
    assigned_at          TIMESTAMP NOT NULL DEFAULT (datetime('now')),
    notes                TEXT,
    UNIQUE(customer_id, bundle_id, target_type, target_id)
);

CREATE INDEX idx_bundle_assignments_target ON bundle_assignments(target_type, target_id);
CREATE INDEX idx_bundle_assignments_bundle ON bundle_assignments(bundle_id);
CREATE INDEX idx_bundle_assignments_customer ON bundle_assignments(customer_id);

-- Permissions для admin role'ов. Seed через `crates/outpost-migrations/seed`
-- (отдельный seed migration при следующем bump'е). Пока — manual INSERT'ы
-- через admin UI / SQL console:
--
--   INSERT INTO permissions(name) VALUES ('bundles.read'), ('bundles.write');
--   INSERT INTO role_permissions(role_id, permission_id)
--     SELECT r.id, p.id FROM roles r, permissions p
--     WHERE r.name IN ('admin','operator') AND p.name IN ('bundles.read','bundles.write');
