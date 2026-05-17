-- Phase 23 — Multi-tenant management for super-admins.
--
-- The customers table existed from day 1 with a single seed row. v0.7
-- starts using it as a real multi-tenant store: super-admins manage
-- additional customers, toggle activation, and (via the Signup flow,
-- phase 23c) accept self-service registrations.
--
-- This migration adds:
--   * customers.is_active flag for soft-disable without dropping data
--   * customers.kind     enum string ("production" | "demo" | "test") —
--                        lets super-admin filter live vs sandbox tenants
--   * customers.* permissions, granted to super-admin role only
--   * users.totp_secret  + totp_enabled (declared here so 23b doesn't
--                        need a separate migration for the column add;
--                        the actual 2FA *flow* lands in the next phase)
--   * totp_recovery_codes table (one-time codes; 10 generated at enroll)
--
-- Single migration consolidates the schema for 23a + 23b. 23c (Signup)
-- needs no schema work.

-- ---- customers --------------------------------------------------------
ALTER TABLE customers ADD COLUMN is_active INTEGER NOT NULL DEFAULT 1;
ALTER TABLE customers ADD COLUMN kind      TEXT    NOT NULL DEFAULT 'production';

CREATE INDEX idx_customers_active ON customers(is_active);

-- ---- customers.* permissions (grant only to super-admin) --------------
INSERT INTO permissions (name, description) VALUES
    ('customers.read',  'List + read all tenants across the deployment'),
    ('customers.write', 'Create / edit / disable tenants');

-- Grant exclusively to super-admin (role_id = 1). Regular admin role
-- (id = 2) is per-tenant and explicitly does NOT get cross-tenant
-- access — multi-tenant requires the super-admin marker.
INSERT INTO user_role_permissions (role_id, permission_id)
    SELECT 1, id FROM permissions WHERE name IN ('customers.read', 'customers.write');

-- ---- TOTP (2FA) — column adds + recovery codes ------------------------
ALTER TABLE users ADD COLUMN totp_secret  TEXT;
ALTER TABLE users ADD COLUMN totp_enabled INTEGER NOT NULL DEFAULT 0;

CREATE TABLE totp_recovery_codes (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- argon2id hash of the plaintext code; we never store the code in
    -- the clear after the one-time display.
    code_hash     TEXT    NOT NULL,
    used_at       TEXT,
    created_at    TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_totp_recovery_user ON totp_recovery_codes(user_id);
