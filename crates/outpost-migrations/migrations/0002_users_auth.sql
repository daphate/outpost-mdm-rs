-- Outpost MDM schema: users, roles, permissions.
--
-- Roles ship as fixed seeds (super-admin, admin, operator, viewer); customer
-- admins cannot create new roles in v1. Permissions are named tokens consumed
-- by handler-level `require_permission(...)` middleware.

CREATE TABLE user_roles (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    name           TEXT    NOT NULL UNIQUE,
    description    TEXT,
    is_super_admin INTEGER NOT NULL DEFAULT 0  -- BOOLEAN
);

INSERT INTO user_roles (id, name, description, is_super_admin) VALUES
    (1, 'super-admin', 'Cross-tenant administrator',                              1),
    (2, 'admin',       'Tenant administrator',                                    0),
    (3, 'operator',    'Day-to-day fleet operations (read all + push commands)', 0),
    (4, 'viewer',      'Read-only access',                                        0);

CREATE TABLE permissions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT    NOT NULL UNIQUE,
    description TEXT
);

INSERT INTO permissions (id, name, description) VALUES
    (1,  'devices.read',         'Read devices'),
    (2,  'devices.write',        'Update / delete devices'),
    (3,  'devices.enroll',       'Issue enrollment QR codes'),
    (4,  'applications.read',    'Read APK catalog'),
    (5,  'applications.write',   'Upload / delete APKs'),
    (6,  'configurations.read',  'Read configurations'),
    (7,  'configurations.write', 'Edit configurations'),
    (8,  'push.send',            'Schedule push commands to devices'),
    (9,  'users.read',           'Read user accounts'),
    (10, 'users.write',          'Manage user accounts'),
    (11, 'groups.read',          'Read device groups'),
    (12, 'groups.write',         'Manage device groups'),
    (13, 'files.read',           'Read uploaded files / signed URLs'),
    (14, 'files.write',          'Upload files');

CREATE TABLE user_role_permissions (
    role_id       INTEGER NOT NULL REFERENCES user_roles(id) ON DELETE CASCADE,
    permission_id INTEGER NOT NULL REFERENCES permissions(id) ON DELETE CASCADE,
    PRIMARY KEY (role_id, permission_id)
);

-- super-admin and admin: all permissions
INSERT INTO user_role_permissions (role_id, permission_id)
    SELECT 1, id FROM permissions
    UNION ALL
    SELECT 2, id FROM permissions;

-- operator: read all + push.send + files
INSERT INTO user_role_permissions (role_id, permission_id)
    SELECT 3, id FROM permissions
    WHERE name LIKE '%.read' OR name IN ('push.send', 'files.write');

-- viewer: read all
INSERT INTO user_role_permissions (role_id, permission_id)
    SELECT 4, id FROM permissions
    WHERE name LIKE '%.read';

CREATE TABLE users (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id          INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    role_id              INTEGER NOT NULL REFERENCES user_roles(id),
    login                TEXT    NOT NULL,
    email                TEXT,
    -- argon2id-encoded password phc string; NULL means "bootstrap required".
    password_hash        TEXT,
    -- Forces a password reset on next login. Set to 1 by the seed.
    must_change_password INTEGER NOT NULL DEFAULT 0,
    is_active            INTEGER NOT NULL DEFAULT 1,
    last_login_at        TEXT,
    created_at           TEXT    NOT NULL DEFAULT (datetime('now')),
    updated_at           TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE UNIQUE INDEX idx_users_login ON users(login);
CREATE INDEX        idx_users_customer ON users(customer_id);
CREATE INDEX        idx_users_role ON users(role_id);
