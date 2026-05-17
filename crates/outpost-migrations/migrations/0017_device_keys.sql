-- v0.14 — Per-device encrypted distribution: §2.6 device_keys table.
--
-- Хранит public ECDH P-256 keys устройств, выданные при `/api/v1/enroll`.
-- Каждое устройство имеет одну активную пару (revoked_at IS NULL); при
-- re-enroll'е с rotation key создаётся новая row, старая может остаться
-- active'ной до явного revoke (так старые encrypted_distributions с прежним
-- key всё ещё расшифровываемы пока устройство не сменит keystore handle).
--
-- key_id = sha256(pubkey_der)[0..8].hex() — детерминированный fingerprint,
-- используется в `encrypted_distributions.recipient_key_id` для денормализации.

CREATE TABLE device_keys (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    device_id   INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    -- Algorithm hint: "ECDH-P256" (текущий дефолт) | "X25519" (если будущий
    -- API позволит — Android Keystore до API 31 не поддерживает X25519, поэтому
    -- v1 only P-256).
    alg         TEXT    NOT NULL,
    -- Raw DER-encoded SubjectPublicKeyInfo. Для P-256 это 91-байт документ
    -- (SPKI с uncompressed point внутри).
    pubkey_der  BLOB    NOT NULL,
    -- Hex fingerprint sha256(pubkey_der)[0..8] = 16 hex chars.
    key_id      TEXT    NOT NULL,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now')),
    revoked_at  TEXT,
    UNIQUE (device_id, key_id)
);

CREATE INDEX idx_device_keys_active_per_device ON device_keys(device_id) WHERE revoked_at IS NULL;
CREATE INDEX idx_device_keys_key_id ON device_keys(key_id);
