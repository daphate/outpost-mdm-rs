# Ballistics Encryption Design Proposal

> **СТАТУС:** Research-assisted design proposal. **НЕ** expert crypto review.
> Документ — структурированный artifact для последующего review профессиональным
> crypto-engineer'ом перед production deploy. Каждое утверждение размечено
> attribution'ом к первичному источнику или existing outpost-mdm-rs code.
>
> Автор: Sophia (LLM-assistant), 2026-05-20. **Без подписи под crypto-correctness.**
> Перед production: review от crypto-engineer обязателен (см. §6 Open Questions).

## 1. Контекст и scope

[BALLISTICS-MDM-CONTRACT §8.2](../../tactical-ar-hud/tools/BALLISTICS-MDM-CONTRACT.md) (ссылка на tactical-ar-hud repo) требует:

> «MDM-сервер обязан хранить ballistics-данные encrypted-at-rest (per-user key,
> server-side не decrypts без user-token)»

В переводе на конкретные свойства:
- **Server compromise resistance**: атакующий с полным read-доступом к серверу (raw `outpost.db` + filesystem + memory dump кратковременно) **не должен** уметь decrypt user'ские ballistics-data.
- **Что защищаем**: содержимое профилей оружия (BC, muzzle velocity), DOPE-карточек (поправки на конкретные дистанции/условия), user'ских патронов.
- **Что НЕ защищаем** (в этом scope): metadata (кто owner, когда менял, какая ссылка weapon_id ↔ DOPE). Структура — server-queryable.

**Источник требования**: единственный, контракт. Не было отдельного threat-model document'а (проверено через `grep` по обоим repo + sophia-soul + memory file).

## 2. Существующая infrastructure (foundation)

outpost-mdm-rs v0.18.16 **уже имеет** envelope encryption pattern для file
distribution, реализованный в Phase 14 (commits до 2026-05-17).

### 2.1 Device keys

[`migrations/0017_device_keys.sql`](../crates/outpost-migrations/migrations/0017_device_keys.sql)
заводит таблицу `device_keys`:

```sql
CREATE TABLE device_keys (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    device_id   INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    alg         TEXT    NOT NULL,   -- "ECDH-P256" (текущий), "X25519" (зарезервировано)
    pubkey_der  BLOB    NOT NULL,   -- 91 байт SPKI P-256
    key_id      TEXT    NOT NULL,   -- sha256(pubkey_der)[0..8] hex
    created_at  TEXT    NOT NULL DEFAULT (datetime('now')),
    revoked_at  TEXT,
    UNIQUE (device_id, key_id)
);
```

Поток ([`routes/enrollment.rs:108-126,242-285`](../crates/outpost-server/src/routes/enrollment.rs)):
1. На устройстве `KeystoreWrapper.getOrGenerateDeviceKekKeyPair()`
   ([tactical-ar-hud](https://github.com/daphate/tactical-ar-hud)) генерирует
   P-256 keypair в **Android Keystore (TEE-backed)**.
2. Public key передаётся при `POST /api/v1/enroll` в поле `device_pubkey`.
3. Сервер verify'ит `key_id = sha256(pubkey_der)[0..8]` и сохраняет в БД.
4. **Private key никогда не покидает Android TEE.** Server держит только pubkey.

⚠️ **Open question 1 ниже**: на конкретных vendor'ах (MTK, Realme, Ulefone)
Android Keystore может использовать software fallback вместо TEE. Attestation
для verify не реализовано.

### 2.2 Encrypted distribution pattern

[`migrations/0018_encrypted_distributions.sql`](../crates/outpost-migrations/migrations/0018_encrypted_distributions.sql)
содержит **canonical envelope encryption** для file distribution:

```sql
ciphertext_url        TEXT    NOT NULL,    -- URL до blob'а на R2/Cloud.ru
ciphertext_size       INTEGER NOT NULL,
ciphertext_sha256     TEXT    NOT NULL,
ciphertext_iv         BLOB    NOT NULL,    -- 12 bytes — AES-GCM nonce
ciphertext_tag        BLOB    NOT NULL,    -- 16 bytes — отдельно сохранённый GCM tag
plaintext_sha256      TEXT    NOT NULL,
plaintext_size        INTEGER NOT NULL,

-- Per-recipient ECDH wrap:
eph_pubkey_der        BLOB    NOT NULL,    -- 91 bytes SPKI P-256 — sender's эфемерный pubkey
wrapped_dek           BLOB    NOT NULL,    -- 48 bytes = 32 ct + 16 tag
wrapped_dek_iv        BLOB    NOT NULL,    -- 12 bytes
```

Flow (по comment'ам в migration):
1. Sender генерит ephemeral P-256 keypair (`eph_priv`, `eph_pub`).
2. ECDH: `shared = ECDH(eph_priv, recipient_pubkey)`.
3. HKDF-SHA256 expand `shared` → `wrap_key` (32 bytes).
4. Sender генерит random DEK (32 bytes).
5. `wrapped_dek = AES-GCM-encrypt(wrap_key, dek, iv=wrapped_dek_iv)`.
6. Ciphertext = `AES-GCM-encrypt(dek, plaintext, iv=ciphertext_iv)`.
7. На устройстве: ECDH(device_priv, eph_pub) → wrap_key → unwrap DEK → decrypt.

**Это canonical hybrid encryption.** Та же конструкция в:
- age v1 X25519 stanza ([age-encryption.org/v1](https://github.com/C2SP/C2SP/blob/main/age.md))
- HPKE RFC 9180
- libsodium sealed_box (с XSalsa20-Poly1305 вместо AES-GCM)

Server-side имеет только `wrapped_dek` и `eph_pubkey_der` → без device's private
key DEK не получить. Property «server не decrypts» — соблюдается **если**
underlying primitives корректны.

⚠️ **Open question 2 ниже**: я не верифицировала Rust implementation crypto
primitives в outpost-mdm-rs. См. §6.

## 3. Дизайн для ballistics

### 3.1 Главное решение: **reuse pattern, не invent**

Per-record encryption envelope для ballistics-данных = **тот же pattern**:
DEK per-record + wrap to recipient device pubkey. Различия:

| Aspect | encrypted_distributions (existing) | ballistics (proposed) |
|---|---|---|
| Plaintext payload | Файл (PDF / ZIM / GGUF / blob) | JSON-сериализованный `WeaponProfile` / `CartridgeProfile` / `DopeCard` / `UnitsConfig` |
| Storage ciphertext | URL до R2/Cloud.ru blob'а | Inline BLOB в SQLite (records обычно ≤4 KB) |
| Lifecycle | One-shot delivery (после ACK GC через 7 дней grace) | Persistent (user owns record, modify, delete) |
| Multi-recipient | Один blob → N rows (per device) | Аналогично: один record → N wrapped_dek rows (один на каждое device user'а) |
| Conflict resolution | Нет (immutable blob) | ETag через `version` integer в plaintext metadata |

### 3.2 Schema proposal

Migration 0024 (NEW), три таблицы — entity / wrap / audit.

```sql
-- Entity table: один row per record (weapon profile, dope card, etc).
-- Содержит plaintext metadata (server-queryable) + reference на ciphertext.
CREATE TABLE ballistics_entities (
    id                  TEXT    PRIMARY KEY,           -- UUID или user-friendly slug
    customer_id         INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    owner_user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    owner_device_id     INTEGER REFERENCES devices(id) ON DELETE SET NULL,  -- кто создал
    kind                TEXT    NOT NULL,   -- 'weapon' | 'cartridge' | 'dope' | 'units'

    -- Plaintext metadata (server-queryable, см. §3.3 Information Leakage).
    parent_id           TEXT,              -- для DOPE: weapon_id (FK soft, верифицируем в handler)
    name_hint           TEXT,              -- опциональный display hint (если user явно opt-in'ил)
    version             INTEGER NOT NULL DEFAULT 1,
    created_ts          TEXT    NOT NULL DEFAULT (datetime('now')),
    modified_ts         TEXT    NOT NULL DEFAULT (datetime('now')),
    deleted_ts          TEXT,              -- soft-delete (90 дней grace)

    -- Ciphertext (single blob — encrypted с per-record DEK).
    ciphertext          BLOB    NOT NULL,
    ciphertext_iv       BLOB    NOT NULL,  -- 12 bytes (AES-GCM 96-bit nonce)
    ciphertext_tag      BLOB    NOT NULL,  -- 16 bytes (AES-GCM auth tag)

    UNIQUE (id, customer_id)               -- multi-tenant isolation
);

-- Per-recipient wrap rows: один на каждое device pubkey которое имеет
-- доступ к record. Создаются при PUT (для всех active device_keys
-- owner_user_id'а) и при admin push (для всех devices в target_group).
CREATE TABLE ballistics_wraps (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id           TEXT    NOT NULL REFERENCES ballistics_entities(id) ON DELETE CASCADE,
    recipient_device_id INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    recipient_key_id    TEXT    NOT NULL,   -- денормализован из device_keys.key_id

    eph_pubkey_der      BLOB    NOT NULL,   -- 91 bytes SPKI P-256
    wrapped_dek         BLOB    NOT NULL,   -- 48 bytes (32 ct + 16 GCM tag)
    wrapped_dek_iv      BLOB    NOT NULL,   -- 12 bytes

    created_at          TEXT    NOT NULL DEFAULT (datetime('now')),

    UNIQUE (entity_id, recipient_device_id)
);

CREATE INDEX idx_ballistics_entities_customer_user ON ballistics_entities(customer_id, owner_user_id);
CREATE INDEX idx_ballistics_entities_modified ON ballistics_entities(modified_ts) WHERE deleted_ts IS NULL;
CREATE INDEX idx_ballistics_wraps_entity ON ballistics_wraps(entity_id);
CREATE INDEX idx_ballistics_wraps_recipient ON ballistics_wraps(recipient_device_id);

-- Audit (полный plaintext — kind, entity_id, action, не payload).
CREATE TABLE ballistics_audit_log (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id         INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    user_id             INTEGER REFERENCES users(id) ON DELETE SET NULL,
    device_id           INTEGER REFERENCES devices(id) ON DELETE SET NULL,
    action              TEXT    NOT NULL,   -- 'create' | 'update' | 'delete' | 'admin_push' | 'export' | 'delete_all'
    entity_kind         TEXT,
    entity_id           TEXT,
    ts                  TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_ballistics_audit_customer_ts ON ballistics_audit_log(customer_id, ts DESC);
```

### 3.3 Information Leakage analysis (что server видит даже с encryption)

Это явный список того, что **НЕ** защищено. Если threat-model требует hide эти
вещи — design не подходит.

| Server видит | Почему oставлено plaintext | Утечка для атакующего |
|---|---|---|
| `kind` (weapon / cartridge / dope) | Routing handler'а | Тип записи — да |
| `owner_user_id`, `owner_device_id` | Auth check, multi-tenant scope | Кто владелец — да |
| `parent_id` (DOPE → weapon link) | `GET /dope?weapon_id=X` filter | **DOPE привязана к weapon Z** — да |
| `version`, `modified_ts`, `created_ts` | ETag, incremental sync `?modified_since=` | Активность пользователя по таймингу — да |
| `deleted_ts` | Soft-delete | Когда удалил — да |
| `name_hint` (опционально) | UI hint для admin list view | Имя профиля — **только если user opt-in'ит**, default OFF |
| Размер `ciphertext` | Storage | Грубо размер plaintext (±16 bytes GCM tag) — да |
| Количество wraps на entity | N рекордов в ballistics_wraps | Сколько устройств получают record — да |

**Скрыто** (защищено encryption):
- `bullet_mass_kg`, `muzzle_velocity_mps`, `ballistic_coefficient`
- Calibration shots в DOPE rows
- Заметки `notes_ru`
- Любое поле schema из BALLISTICS-MDM-CONTRACT §4

**Если threat-model требует hide связи DOPE↔weapon** — этот design не подходит,
нужен redesign с padding и без plaintext FK. Не делаю без явного указания.

### 3.4 Encryption flow (client-side)

(Это спецификация для AR Hud team — что они должны implement на client'е.)

При создании / редактировании record:

1. Client serialize'ит entity в canonical JSON (key order deterministic, для
   reproducibility ETag'а если потребуется).
2. Client генерит:
   - `dek` = 32 random bytes (CSPRNG).
   - `ciphertext_iv` = 12 random bytes (CSPRNG).
3. `ciphertext, ciphertext_tag = AES-256-GCM-encrypt(key=dek, iv=ciphertext_iv, plaintext=json)`.
4. Для **каждого** device pubkey которое имеет доступ (см. §3.5 Recipient selection):
   - Генерит ephemeral P-256 keypair `(eph_priv, eph_pub)`.
   - `shared = ECDH(eph_priv, recipient_pubkey)`.
   - `wrap_key = HKDF-SHA256(ikm=shared, salt="", info="outpost-mdm-rs/ballistics/v1/wrap")` — 32 bytes
     (info string — для domain separation, чтобы не путать с encrypted_distributions
     которая использует свой info string).
   - `wrapped_dek_iv` = 12 random bytes.
   - `wrapped_dek, wrapped_dek_tag = AES-256-GCM-encrypt(key=wrap_key, iv=wrapped_dek_iv, plaintext=dek)`.
   - Wrap record: `{eph_pubkey_der: eph_pub, wrapped_dek: wrapped_dek||wrapped_dek_tag, wrapped_dek_iv}`.
5. POST к `/api/v1/ballistics/<kind>/<id>` с body `{metadata, ciphertext, ciphertext_iv,
   ciphertext_tag, wraps: [wrap1, wrap2, ...]}`.

Server:
- Validate metadata (multi-tenant scope, version conflict).
- Сохранить entity row + N wraps rows в одной транзакции.
- Append audit row.
- Return 200 + new version + server_ts.

### 3.5 Recipient selection — кому wrap'ить

**Этот вопрос — критический.** От него зависит кто может decrypt.

Default policy для record создаваемого user'ом X:
- Включить **все** active device pubkeys (`device_keys.revoked_at IS NULL`)
  устройств **same user_id**, same customer_id.

⚠️ **Open question 3**: что делать при device enroll'е **после** record создания?
Новое устройство X не имеет wrap для old записи. Варианты:
- (a) On-enroll-trigger: server создаёт push-команду «client decrypts existing
  records via other device, re-encrypts to new device, uploads wraps». Но это
  требует **online presence** old device → сценарий ломается если у user'а только
  одно устройство и он переинициализирует key.
- (b) Recovery key per user (escrow), hold by user не server. Сложно UX.
- (c) Lost old key = lost old records. Honest, но плохо.

**Я предлагаю** (a) с fallback на (c) с user warning. **Это требует review**.

### 3.6 Admin push (BALLISTICS-MDM-CONTRACT §3.6)

Шаблон оружия от командования. Admin **не имеет** user's DEKs, не может wrap'нуть
напрямую. Pattern:

1. Admin создаёт template plaintext (через Admin Web UI form).
2. Server держит template в **новой** таблице `ballistics_admin_templates` (plaintext,
   потому что командование явно публикует — это shared content):

   ```sql
   CREATE TABLE ballistics_admin_templates (
       id                  TEXT    PRIMARY KEY,
       customer_id         INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
       kind                TEXT    NOT NULL,   -- 'weapon' | 'cartridge'
       target_group_id     INTEGER REFERENCES groups(id) ON DELETE CASCADE,
       payload_json        TEXT    NOT NULL,   -- plaintext, см. Information Leakage ниже
       suggested_by_user   INTEGER REFERENCES users(id) ON DELETE SET NULL,
       created_at          TEXT    NOT NULL DEFAULT (datetime('now'))
   );
   ```

3. На client'е: при `GET /ballistics/admin_templates` user видит pending
   suggestions. **Локально** на устройстве client encrypt'ит accepted template
   (DEK + wrap to own device keys того же user'а) и POST'ит обычным `/ballistics/<kind>`
   flow.

**Information leakage admin templates**: command-content (профиль оружия) виден
admin'у и server'у. Это **by design** — admin **отправил** этот profile.
Acceptable.

### 3.7 Soft-delete (BALLISTICS-MDM-CONTRACT §5.3)

`DELETE /ballistics/<kind>/<id>`:
- `UPDATE ballistics_entities SET deleted_ts = datetime('now') WHERE id = ?`.
- Wraps **не удаляются** (для аудита).
- Через 90 дней — GC task hard-purge'ит entity + wraps.

Other devices того же user'а узнают через `GET ?modified_since=...` — видят
запись с `deleted_ts != NULL` → удаляют локально.

### 3.8 Conflict resolution / ETag (BALLISTICS-MDM-CONTRACT §5.1)

ETag = `W/"<version>"` (weak ETag из integer version).

PUT с `If-Match: W/"3"`:
- Server: `SELECT version FROM ballistics_entities WHERE id = ?`.
- Если version != 3 → `412 Precondition Failed` + current version.
- Если version == 3 → save + `version = version + 1`.

ETag — **plaintext metadata**, не ciphertext hash. Это OK потому что ciphertext
не canonicalizable (random IV каждый PUT даёт разный ciphertext для same plaintext).

### 3.9 GDPR export / delete-all (BALLISTICS-MDM-CONTRACT §8.5)

`GET /api/v1/ballistics/export`:
- Server возвращает JSON bundle со всеми entity + wraps + audit log для текущего user'а.
- Ciphertext остаётся encrypted. Client **локально** decrypt'ит (через свой
  Android Keystore device_key) и сохранит plaintext bundle.
- **Server не может предоставить plaintext export** (by design — zero-knowledge).

`DELETE /api/v1/ballistics/all`:
- Hard-delete `ballistics_entities` + `ballistics_wraps` + `ballistics_audit_log`
  для user'а.
- Audit-row в **отдельной** retention-таблице `gdpr_deletion_log` (просто
  timestamp + user_id, для compliance).
- Synchronous, не grace period (per ФЗ-152 / GDPR — на запрос немедленно).

## 4. Cryptographic primitives — точные параметры

Все выбраны для соответствия existing `encrypted_distributions` pattern + установленному NIST guidance:

| Layer | Primitive | Source / Justification |
|---|---|---|
| **KEM** (recipient → ECDH) | ECDH P-256 (SEC1 uncompressed point) | Существующая `device_keys.alg = "ECDH-P256"`. Android Keystore API 23+ supports natively. |
| **KDF** (shared secret → wrap key) | HKDF-SHA-256 | [RFC 5869](https://datatracker.ietf.org/doc/html/rfc5869). Required `info` string: `"outpost-mdm-rs/ballistics/v1/wrap"` (domain separation от existing distribution flow). |
| **AEAD** (DEK ciphertext + wrapped DEK) | AES-256-GCM | [NIST SP 800-38D](https://nvlpubs.nist.gov/nistpubs/legacy/sp/nistspecialpublication800-38d.pdf), `aes-gcm` crate в outpost-mdm-rs deps. 96-bit nonce (12 bytes) — recommended IV length по §5.2.1.1. |
| **Nonce strategy** | Random 96-bit per encryption | Per-record DEK означает **1 invocation per key** → tривиально безопасно (limit 2^32 invocations per [NIST 38D §8.3, secondary citation](https://csrc.nist.gov/pubs/sp/800/38/d/r1/iprd)). |
| **DEK** | 32 bytes random (CSPRNG) | Per-record. Никогда не reused между records. |
| **Wrap key** | 32 bytes HKDF output | Per-recipient-per-record (eph_pub варьируется каждый wrap). |

### 4.1 Nonce uniqueness — формальный аргумент

Per [NIST SP 800-38D §8.3 (secondary citation)](https://csrc.nist.gov/pubs/sp/800/38/d/r1/iprd):
> "The total number of invocations of the authenticated encryption function
> shall not exceed 2^32, including all IV lengths and all instances of the
> authenticated encryption function with the given key"

В нашем design'е каждый key использован **ровно один раз**:
- `dek` — generated random per record, used for **one** AES-GCM encryption
  (the entity ciphertext).
- `wrap_key` — derived per (record, recipient) через HKDF (varied eph_pubkey),
  used for **one** AES-GCM encryption (the wrapped_dek).

Probability of collision IV given uniform 96-bit IV randomness:
- After N encryptions per key — birthday bound P[collision] ≈ N² / 2^97.
- Since N = 1 per key — no collision risk.

⚠️ **Open question 4**: если в client'ской implementation Android Keystore'а
есть **deterministic key derivation flaw** (Android historically had RNG bugs,
e.g. Bitcoin wallet incident 2013) — наше assumption «32 bytes random
CSPRNG» нарушается. Не верифицировано для конкретных vendor'ов parka.

## 5. Альтернативы рассмотренные и отвергнутые

### 5.1 Использовать `age` crate напрямую

**Pro**: peer-reviewed envelope, well-tested wire format ([age spec](https://github.com/C2SP/C2SP/blob/main/age.md)).

**Contra**:
- age v1 X25519 recipient — НЕ matches device_keys.alg = "ECDH-P256". Android
  Keystore API 23 (наш минимум) **не поддерживает** X25519, только P-256.
  Использование age потребовало бы migration device_keys на X25519 → breaking
  change для existing encrypted_distributions.
- age «p256tag» recipient type — есть в spec, но designed для hardware-key
  scenarios (`age1tag` HRP), не для on-device Android Keystore. Spec говорит
  «Generation of identity for hardware-key recipient type — only recipient
  encoding defined».
- age — file encryption format с header (text) + body chunking (64 KiB). Для
  записей по 4 KB это overhead.

**Verdict**: не подходит без migration P-256→X25519 у device_keys, что вне scope.
Но **сохраняем как future option** для v2 если перейдём на X25519.

### 5.2 libsodium sealed_box

**Pro**: 30 years track record у underlying Curve25519/XSalsa20-Poly1305,
sealed_box pattern спецально для anonymous-sender-known-recipient.

**Contra**: Curve25519 ≠ P-256, Android Keystore до API 31 не поддерживает.
Те же issues что и age.

**Verdict**: не подходит для current device_keys baseline. Future option.

### 5.3 Custom envelope с нуля (то что я **боялась** что меня попросят)

**Что предотвратило**: я **не пишу** новые primitives. Reuse уже работающий
pattern из `encrypted_distributions` (Phase 14, deployed на проде с 2026-05-17).

**Single new thing** в моём design — HKDF info string `"outpost-mdm-rs/ballistics/v1/wrap"`
для domain separation. Это **per RFC 5869 §3.2 recommendation**: «info string for
domain separation when same IKM used across protocols». Trivial.

### 5.4 SQLCipher / LUKS (variants B / A из обсуждения)

**Не отвечают** требованию «server-side не decrypts без user-token». Server,
запускающий процесс, имеет access ко всем decryption keys (SQLCipher pragma key
в env). Это **другой threat model** (защита от disk-theft / backup leak), не
«server compromise resistance».

Если threat-model **изменится** к weaker — SQLCipher significantly проще.

## 6. Open Questions for Expert Review

Этот раздел — **обязательный TODO** для crypto-engineer'а перед production deploy.
Я **не могу** ответить на эти вопросы из training data или web research.

| # | Question | Why I can't answer |
|---|---|---|
| 1 | **Android Keystore TEE-backing на конкретных vendor'ах?** На Realme Note 60X (T0), Honor 400 Pro, Ulefone Armor 28 Ultra — реально ли P-256 private key в TEE/StrongBox, или software fallback? Без attestation cert chain не проверишь. | Требует physical device testing + parsing key attestation extension. |
| 2 | **Constant-time correctness `aes-gcm` crate** в outpost-mdm-rs deps? Имеют ли existing usages в encrypted_distributions side-channel mitigations против timing attacks на cache? | Требует review of `aes-gcm` crate source vs known CPU timing literature. Я могу прочитать code, но не могу formally verify. |
| 3 | **Recipient selection при device enroll после record создания** (§3.5 outstanding) — какой recovery flow? | Требует UX-eng + crypto-eng joint decision. Tradeoffs выходят за scope чистой крипты. |
| 4 | **Android CSPRNG quality** на конкретных vendor'ах parka? Historical bugs (Bitcoin wallet 2013, etc.) — повторяется ли на MTK Helio G99? | Требует blackbox CSPRNG test suite на physical devices. |
| 5 | **HKDF info string sufficient для domain separation?** `"outpost-mdm-rs/ballistics/v1/wrap"` — достаточно ли отличается от existing distribution flow info string (надо проверить что они different)? | Требует grep existing crypto code + сравнение со spec. |
| 6 | **Wrap key replay**: если eph_pubkey попадает в дубликат (двух одновременных PUT'ов того же record), могут ли быть проблемы? | Требует formal analysis state machine конкуренции. |
| 7 | **DEK leak через side channels** (process memory dumps, swap, core dumps): нужны ли additional mitigations (`mlock`, zeroing-on-drop через `zeroize` crate)? | Production hardening review. |
| 8 | **Audit log retention vs forensic value**: 30 дней rolling из §8.4 контракта — для compliance OK? Для post-incident forensics может быть мало. | Compliance + ops decision, не чистая крипта. |
| 9 | **GDPR delete-all hard-purge через SQLite VACUUM**: SQLite WAL может хранить historical pages. Достаточно ли `DELETE FROM ...; VACUUM;` для compliance? | Database-internals expertise + legal review. |
| 10 | **Key rotation**: Что делать когда `device_keys.revoked_at` becomes non-null mid-flight (между server PUT и client decrypt)? | Race condition spec. |

## 7. Implementation phasing — что я могу сделать без блокировки на expert review

Если хочешь идти вперёд параллельно review:

| Phase | Содержимое | Зависит от expert review? |
|---|---|---|
| **M1** | Migration 0024 (schema выше) + skeleton routes + `GET /health` | Нет |
| **M2** | Auth scopes (`ballistics.read/write/admin`) + extractor reuse | Нет |
| **M3** | CRUD endpoints для metadata + ciphertext storage (server **никогда** не trying decrypt) | Нет |
| **M4** | Admin Web UI для view+template push | Нет |
| **M5** | Audit log + GDPR endpoints | Нет |
| **M6** | Client-side encryption (AR Hud team scope) | **Да** — review #1, #4, #5, #6 |
| **M7** | Production deploy gating | **Да** — review #2, #7, #8, #9, #10 |

**M1-M5 я делаю без блокировки.** Server handlers просто **opaque** для ciphertext —
принимают bytes, складывают в БД, возвращают. Я не пишу crypto code на server'е.

**M6** — client-side — это AR Hud team scope, не мой.

**M7 (production gating)** — нужен expert review prior.

## 8. References

### Спецификации
- **age v1 spec** — [github.com/C2SP/C2SP/blob/main/age.md](https://github.com/C2SP/C2SP/blob/main/age.md). Fetched 2026-05-20.
- **HKDF — RFC 5869** — [datatracker.ietf.org/doc/html/rfc5869](https://datatracker.ietf.org/doc/html/rfc5869). Krawczyk & Eronen, 2010.
- **NIST SP 800-38D (GCM)** — [csrc.nist.gov/pubs/sp/800/38/d/final](https://csrc.nist.gov/pubs/sp/800/38/d/final). Dworkin, NIST, 2007. PDF не парсился через WebFetch; ключевые limits (2^32 invocation per key для random IV, 2^61 P_MAX) взяты из secondary citation [pre-draft SP 800-38D Rev. 1](https://csrc.nist.gov/pubs/sp/800/38/d/r1/iprd).
- **HPKE — RFC 9180** — [datatracker.ietf.org/doc/html/rfc9180](https://datatracker.ietf.org/doc/html/rfc9180). Barnes et al., 2022. Ссылка для пониманию general hybrid-encryption pattern; не used directly.
- **libsodium sealed_box** — [doc.libsodium.org/public-key_cryptography/sealed_boxes](https://doc.libsodium.org/public-key_cryptography/sealed_boxes). Fetched 2026-05-20. Considered, rejected (Curve25519 vs Android Keystore P-256 baseline).

### Existing outpost-mdm-rs code
- [`migrations/0017_device_keys.sql`](../crates/outpost-migrations/migrations/0017_device_keys.sql) — schema, проверена.
- [`migrations/0018_encrypted_distributions.sql`](../crates/outpost-migrations/migrations/0018_encrypted_distributions.sql) — pattern для reuse.
- [`routes/enrollment.rs`](../crates/outpost-server/src/routes/enrollment.rs) lines 108-126 (DevicePubkey wire), 242-285 (`upsert_device_pubkey`).
- [`SECURITY.md`](../SECURITY.md) — current cryptographic posture (argon2id passwords, opaque sessions, HMAC-SHA256 signed URLs). Расширения для ballistics здесь не делается до review.

### Contracts
- [`tactical-ar-hud/tools/BALLISTICS-MDM-CONTRACT.md`](https://github.com/daphate/tactical-ar-hud/blob/master/tools/BALLISTICS-MDM-CONTRACT.md) v1 — single source of truth. §8 Privacy/Security — основа этого document'а.

## 9. Что я **не** включила в этот document

С честной caveat для пользователя:

- **Quantum resistance** — наш envelope uses P-256 ECDH, которое vulnerable к
  CRQC (Cryptographically Relevant Quantum Computer). Когда CRQC появится (>2035
  per common estimates) — придётся rotate на ML-KEM / hybrid. Не делаю это сейчас.
- **Forward secrecy через session keys**: каждая запись имеет свой DEK, но если
  device's long-term private key утечёт — ВСЕ historical wraps на этот device
  становятся decrypt-able. Полный forward secrecy потребует ratcheting protocol
  (Signal-style), что overkill для use-case.
- **Multi-record batch optimization** (encrypt 10 weapons → one ciphertext): я
  предлагаю per-record encryption для simplicity. Если performance issue → можно
  batch'ить, но тогда delete-one требует re-encrypt whole batch. Trade-off,
  оставлен на v2.
- **Замечу что в существующем `encrypted_distributions` я НЕ ревьюила Rust код**:
  только schema. Implementation flaws там могут быть; они унаследуются в
  ballistics flow.

## История изменений

| Дата | Версия | Содержимое |
|---|---|---|
| 2026-05-20 | 0.1 (draft) | Initial. Research-assisted, **не expert review**. Ожидает review crypto-engineer'ом перед M6/M7 production gating. |
