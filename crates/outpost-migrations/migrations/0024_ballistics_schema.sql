-- v0.18.17 — Ballistics encryption-at-rest schema.
--
-- Реализация BALLISTICS-MDM-CONTRACT v1 (см. tactical-ar-hud
-- tools/BALLISTICS-MDM-CONTRACT.md) + design proposal docs/BALLISTICS-CRYPTO-DESIGN.md.
--
-- Архитектура: server opaque envelope. Server **никогда** не пытается decrypt
-- ciphertext. Хранит metadata (plaintext: kind/owner/version/timestamps/links)
-- + ciphertext BLOB + per-recipient wrap rows. Decryption — только на client'е
-- через Android Keystore-backed P-256 private key.
--
-- Все endpoint'ы за feature flag BALLISTICS_ENABLED (env var, default false).
-- На production включается ТОЛЬКО после expert crypto review §6 design'а.
--
-- Information Leakage (что server видит даже с encryption):
--   - kind, owner_user_id, owner_device_id  → routing + auth
--   - parent_id (DOPE → weapon)             → required для ?weapon_id= filter
--   - version, modified_ts, deleted_ts      → ETag + incremental sync
--   - ciphertext size                       → ≈plaintext size ±16 bytes GCM tag
--   - wrap count                            → N recipient devices
-- Скрыто (под encryption):
--   - bullet_mass, muzzle_velocity, BC, DOPE rows, notes, name (опционально)

-- =====================================================================
-- 1. Entities — основная таблица записей.
-- =====================================================================
CREATE TABLE ballistics_entities (
    -- Client-generated ID (UUID или user-friendly slug). Для cartridges
    -- ОБЯЗАТЕЛЬНО префикс `user_` (per BALLISTICS-MDM-CONTRACT §3.3).
    id                  TEXT    PRIMARY KEY,
    customer_id         INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    owner_user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- nullable: при admin push с group target создаётся entity без специфического
    -- owner_device_id (entity per recipient device — wraps идут к разным
    -- device pubkey'ам same user_id).
    owner_device_id     INTEGER REFERENCES devices(id) ON DELETE SET NULL,

    -- Категория. Только одно из четырёх — handler validate'ит.
    kind                TEXT    NOT NULL CHECK (kind IN ('weapon', 'cartridge', 'dope', 'units')),

    -- Soft FK для DOPE → weapon link. Server validate'ит существование при
    -- PUT. Реальный FK не объявляем чтобы parent_id мог указывать на entity
    -- разного типа в будущем (e.g. cartridge ↔ weapon).
    parent_id           TEXT,

    -- Опциональный display hint для admin list view. Default NULL — content
    -- не утекает. Client opt-in: передаёт значение только если user явно
    -- включил «показывать имя в admin UI» в Settings.
    name_hint           TEXT,

    -- Monotonic version для ETag/If-Match. Инкрементится server-side при
    -- успешном PUT.
    version             INTEGER NOT NULL DEFAULT 1,

    -- Server-controlled timestamps. modified_ts обновляется на каждом PUT;
    -- created_ts — однократно при первом INSERT.
    created_ts          TEXT    NOT NULL DEFAULT (datetime('now')),
    modified_ts         TEXT    NOT NULL DEFAULT (datetime('now')),
    deleted_ts          TEXT,                          -- soft-delete; 90 дней grace до GC

    -- Ciphertext blob. Client encrypts с per-record DEK через AES-256-GCM.
    -- Server opaque — никогда не decrypt'ит.
    ciphertext          BLOB    NOT NULL,
    ciphertext_iv       BLOB    NOT NULL,              -- 12 bytes (AES-GCM 96-bit nonce)
    ciphertext_tag      BLOB    NOT NULL,              -- 16 bytes (AES-GCM auth tag)

    -- Multi-tenant isolation. customer_id всегда фильтруется в каждом read/write.
    UNIQUE (id, customer_id)
);

CREATE INDEX idx_ballistics_entities_customer_user
    ON ballistics_entities(customer_id, owner_user_id);

CREATE INDEX idx_ballistics_entities_kind
    ON ballistics_entities(customer_id, owner_user_id, kind) WHERE deleted_ts IS NULL;

CREATE INDEX idx_ballistics_entities_modified
    ON ballistics_entities(customer_id, owner_user_id, modified_ts);

CREATE INDEX idx_ballistics_entities_parent
    ON ballistics_entities(customer_id, parent_id) WHERE parent_id IS NOT NULL;

-- GC index: для cleanup task'а который hard-purge'ит soft-deleted после 90 дней.
CREATE INDEX idx_ballistics_entities_gc
    ON ballistics_entities(deleted_ts) WHERE deleted_ts IS NOT NULL;

-- =====================================================================
-- 2. Wraps — per-recipient ECDH+AES-GCM wrapped DEK rows.
-- =====================================================================
-- Один entity → N wraps (один на каждое active device_keys row того же
-- owner_user_id). При admin push к группе — N wraps на N target devices.
--
-- Wrap data (по BALLISTICS-CRYPTO-DESIGN.md §3.2):
--   eph_pubkey_der  — ephemeral P-256 sender pubkey (91 bytes SPKI)
--   wrapped_dek     — AES-256-GCM(wrap_key, DEK) = 48 bytes (32 ct + 16 tag)
--   wrapped_dek_iv  — 12 bytes
--
-- wrap_key derivation (client-side):
--   shared    = ECDH(eph_priv, recipient_device_pubkey)
--   wrap_key  = HKDF-SHA-256(ikm=shared, salt="", info="outpost-mdm-rs/ballistics/v1/wrap")
CREATE TABLE ballistics_wraps (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id           TEXT    NOT NULL,
    customer_id         INTEGER NOT NULL,                  -- denormalized для FK + multi-tenant
    recipient_device_id INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    -- Денормализован из device_keys.key_id; при device key rotation старые
    -- wraps остаются (client может decrypt пока has old privкey в keystore).
    recipient_key_id    TEXT    NOT NULL,

    eph_pubkey_der      BLOB    NOT NULL,                  -- 91 bytes SPKI P-256
    wrapped_dek         BLOB    NOT NULL,                  -- 48 bytes (32 ct + 16 GCM tag)
    wrapped_dek_iv      BLOB    NOT NULL,                  -- 12 bytes

    created_at          TEXT    NOT NULL DEFAULT (datetime('now')),

    FOREIGN KEY (entity_id, customer_id) REFERENCES ballistics_entities(id, customer_id) ON DELETE CASCADE,
    UNIQUE (entity_id, recipient_device_id)
);

CREATE INDEX idx_ballistics_wraps_entity ON ballistics_wraps(entity_id);
CREATE INDEX idx_ballistics_wraps_recipient ON ballistics_wraps(recipient_device_id);

-- =====================================================================
-- 3. Audit log — каждый CRUD logged с metadata (kind, action, who, when).
-- =====================================================================
-- Per BALLISTICS-MDM-CONTRACT §8.4. Plaintext payload НЕ логируется (только
-- metadata).
CREATE TABLE ballistics_audit_log (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id         INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    user_id             INTEGER REFERENCES users(id) ON DELETE SET NULL,
    device_id           INTEGER REFERENCES devices(id) ON DELETE SET NULL,
    action              TEXT    NOT NULL CHECK (action IN ('create', 'update', 'delete', 'admin_push', 'export', 'delete_all')),
    entity_kind         TEXT,
    entity_id           TEXT,
    ts                  TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_ballistics_audit_customer_ts
    ON ballistics_audit_log(customer_id, ts DESC);

CREATE INDEX idx_ballistics_audit_user
    ON ballistics_audit_log(user_id, ts DESC) WHERE user_id IS NOT NULL;

-- =====================================================================
-- 4. Admin templates — plaintext shared content (admin push к группе).
-- =====================================================================
-- Per BALLISTICS-MDM-CONTRACT §3.6. Командование явно публикует template —
-- его содержимое **by design** visible админу и серверу. Personal user'ские
-- данные (encrypted) — отдельная таблица ballistics_entities.
--
-- Client при accept template'а локально encrypt'ит его как обычный record
-- и POST'ит в /ballistics/<kind>.
CREATE TABLE ballistics_admin_templates (
    id                  TEXT    PRIMARY KEY,
    customer_id         INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    kind                TEXT    NOT NULL CHECK (kind IN ('weapon', 'cartridge')),
    -- Target — группа устройств. Если NULL — fleet-wide для customer_id.
    target_group_id     INTEGER REFERENCES groups(id) ON DELETE CASCADE,
    -- Plaintext WeaponProfile/CartridgeProfile JSON, см. BALLISTICS-MDM-CONTRACT §4.
    payload_json        TEXT    NOT NULL,
    suggested_by_user   INTEGER REFERENCES users(id) ON DELETE SET NULL,
    title               TEXT,                              -- human-readable hint для UI ("Soldier rifle profile, batch B")
    created_at          TEXT    NOT NULL DEFAULT (datetime('now')),
    -- Soft retract: admin может отозвать. Client при следующем pull получает
    -- запись с retracted_at != NULL → скрывает из pending suggestions.
    retracted_at        TEXT
);

CREATE INDEX idx_ballistics_admin_templates_customer
    ON ballistics_admin_templates(customer_id, created_at DESC);

CREATE INDEX idx_ballistics_admin_templates_group
    ON ballistics_admin_templates(target_group_id) WHERE retracted_at IS NULL;

-- =====================================================================
-- 5. GDPR deletion log — compliance retention.
-- =====================================================================
-- Per BALLISTICS-MDM-CONTRACT §8.5 + Российский ФЗ-152.
-- DELETE /ballistics/all делает hard-purge всех ballistics_entities +
-- _wraps + _audit_log для user'а; в эту таблицу пишется одна row с
-- ts + user_id + customer_id для post-hoc compliance verification.
CREATE TABLE ballistics_gdpr_deletion_log (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id         INTEGER NOT NULL,           -- НЕ FK: customer может тоже быть deleted
    user_id             INTEGER NOT NULL,           -- НЕ FK по той же причине
    deleted_entity_count INTEGER NOT NULL,          -- сколько было удалено (для verifiability)
    deleted_wrap_count  INTEGER NOT NULL,
    requested_by_user   INTEGER REFERENCES users(id) ON DELETE SET NULL,
    ts                  TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_ballistics_gdpr_deletion_log_user
    ON ballistics_gdpr_deletion_log(customer_id, user_id);
