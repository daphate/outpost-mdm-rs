-- Outpost MDM schema: bootstrap admin user.
--
-- Inserts a row with NULL `password_hash` and `must_change_password = 1`.
-- The server detects this on first boot, generates a random initial
-- password, hashes it with argon2id, updates the row, and logs the
-- password to stderr exactly once (see P3 bootstrap logic).
--
-- The login is fixed to 'admin' — operators can rename it after first
-- login if desired.

INSERT INTO users (
    customer_id,
    role_id,
    login,
    email,
    password_hash,
    must_change_password,
    is_active
) VALUES (
    1,                  -- customer_id (default tenant)
    1,                  -- role_id (super-admin)
    'admin',
    NULL,
    NULL,               -- bootstrap: server generates + logs on first start
    1,
    1
);
