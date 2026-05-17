-- Outpost MDM schema: opaque session tokens stored server-side.
--
-- Replaces the JWT-based auth from v0.1.0. Each successful login (user
-- or device enrollment) inserts a row whose PRIMARY KEY is the sha256
-- hex digest of the ORIGINAL random token returned to the client. The
-- client carries the original 64-char hex; the server only ever stores
-- its hash. A DB-file leak therefore does not expose live sessions
-- (the original tokens are needed and never persisted).
--
-- Lookup path:  hash(presented_token)  →  WHERE id_hash = ? AND
--                                          revoked_at IS NULL AND
--                                          expires_at > datetime('now')

CREATE TABLE sessions (
    id_hash       TEXT    PRIMARY KEY,           -- sha256(opaque token), 64-char hex
    kind          TEXT    NOT NULL,              -- 'user' | 'device'
    subject_id    INTEGER NOT NULL,              -- users.id or devices.id (no FK because the kind discriminates)
    customer_id   INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    -- Cached at issuance for fast extractor lookup. Role/login changes
    -- on the underlying user/device take effect on the next login.
    role_id       INTEGER NOT NULL DEFAULT 0,    -- 0 for device sessions
    login         TEXT    NOT NULL,              -- user.login or device.serial
    issued_at     TEXT    NOT NULL DEFAULT (datetime('now')),
    expires_at    TEXT    NOT NULL,
    revoked_at    TEXT
);

CREATE INDEX idx_sessions_subject ON sessions(kind, subject_id);
CREATE INDEX idx_sessions_expires ON sessions(expires_at);
CREATE INDEX idx_sessions_revoked ON sessions(revoked_at);
