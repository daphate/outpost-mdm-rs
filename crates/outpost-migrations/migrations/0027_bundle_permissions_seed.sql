-- v0.18.20 — Fix BUNDLES-1 (security/code review 2026-06-04): seed the
-- bundles.read / bundles.write permissions that migration 0026 left ONLY
-- inside a SQL comment (lines 41-48). Without this, require_permission()
-- can never match (it does a literal role_id+name lookup with no super-admin
-- bypass), so ALL four admin bundle endpoints — create_assignment,
-- list_assignments, delete_assignment, list_effective_for_device — returned
-- HTTP 403 for every role including super-admin. The admin half of the
-- bundle-assignment feature was dead-on-arrival.
--
-- The 0026 comment also referenced non-existent tables (`role_permissions`,
-- `roles`); the real tables are `user_role_permissions` and `user_roles`.
--
-- Grants mirror 0025 (ballistics) intent:
--   super-admin (1) + admin (2) → read + write
--   operator (3)                → read + write (day-to-day fleet ops)
--   viewer (4)                  → read only
--
-- Idempotent: WHERE NOT EXISTS guards так как admin мог вручную добавить
-- perms через SQL console (per 0026 comment suggestion).

INSERT INTO permissions (name, description)
SELECT 'bundles.read', 'Read bundle assignments'
WHERE NOT EXISTS (SELECT 1 FROM permissions WHERE name = 'bundles.read');

INSERT INTO permissions (name, description)
SELECT 'bundles.write', 'Create / delete bundle assignments'
WHERE NOT EXISTS (SELECT 1 FROM permissions WHERE name = 'bundles.write');

-- super-admin (1) + admin (2) + operator (3): read + write.
INSERT INTO user_role_permissions (role_id, permission_id)
SELECT r.role_id, p.id
FROM (SELECT 1 AS role_id UNION ALL SELECT 2 UNION ALL SELECT 3) r
CROSS JOIN permissions p
WHERE p.name IN ('bundles.read', 'bundles.write')
  AND NOT EXISTS (
    SELECT 1 FROM user_role_permissions urp
    WHERE urp.role_id = r.role_id AND urp.permission_id = p.id
  );

-- viewer (4): read only.
INSERT INTO user_role_permissions (role_id, permission_id)
SELECT 4, p.id
FROM permissions p
WHERE p.name = 'bundles.read'
  AND NOT EXISTS (
    SELECT 1 FROM user_role_permissions urp
    WHERE urp.role_id = 4 AND urp.permission_id = p.id
  );
