-- Situational platform Ф0: permissions for the org-unit (units) CRUD.
--
-- Mirrors the groups grant model — management is admin-level: super-admin (1)
-- and admin (2) get read+write; operator (3) and viewer (4) get read only.

INSERT INTO permissions (name, description) VALUES
    ('units.read',  'Read org units (subdivisions)'),
    ('units.write', 'Create / update / delete org units');

INSERT INTO user_role_permissions (role_id, permission_id)
    SELECT 1, id FROM permissions WHERE name LIKE 'units.%'
    UNION ALL
    SELECT 2, id FROM permissions WHERE name LIKE 'units.%';

INSERT INTO user_role_permissions (role_id, permission_id)
    SELECT 3, id FROM permissions WHERE name = 'units.read'
    UNION ALL
    SELECT 4, id FROM permissions WHERE name = 'units.read';
