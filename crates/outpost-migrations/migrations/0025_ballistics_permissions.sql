-- v0.18.17 — Permissions для ballistics endpoint scopes (BALLISTICS-MDM-
-- CONTRACT §2).
--
-- Три scope'а:
--   ballistics.read   — pull weapons / cartridges / DOPE / units / audit-log
--                       / admin templates. Также read GDPR export.
--   ballistics.write  — push (PUT/DELETE) personal записей user'а.
--   ballistics.admin  — push шаблонов (admin templates) target group'е +
--                       admin override flag.
--
-- Role grants (consistent с существующим паттерном):
--   super-admin (1) — все три
--   admin (2)       — все три (per-tenant admin может template push'ить)
--   operator (3)    — read + write (но НЕ admin push)
--   viewer (4)      — только read

INSERT INTO permissions (name, description) VALUES
    ('ballistics.read',  'Read ballistics profiles / DOPE / units / audit log'),
    ('ballistics.write', 'Create / update / delete personal ballistics records'),
    ('ballistics.admin', 'Push ballistics templates to device groups (admin override)');

-- super-admin (1) и admin (2): все 3 ballistics permissions.
INSERT INTO user_role_permissions (role_id, permission_id)
    SELECT 1, id FROM permissions WHERE name LIKE 'ballistics.%'
    UNION ALL
    SELECT 2, id FROM permissions WHERE name LIKE 'ballistics.%';

-- operator (3): read + write (но НЕ admin).
INSERT INTO user_role_permissions (role_id, permission_id)
    SELECT 3, id FROM permissions WHERE name IN ('ballistics.read', 'ballistics.write');

-- viewer (4): только read.
INSERT INTO user_role_permissions (role_id, permission_id)
    SELECT 4, id FROM permissions WHERE name = 'ballistics.read';
