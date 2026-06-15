# Ballistics Encryption — Implementation Spec for AR Hud team

> **СТАТУС:** Open work-package, **timeline ASAP в течение недели** per coordination
> 2026-05-21. Server-side M1-M5 (skeleton + endpoints + admin UI) реализованы
> в v0.18.17 [commit 265563f](https://github.com/daphate/outpost-mdm-rs/commit/265563f).
> Этот документ — ТЗ для AR Hud team на M6 (client-side encryption) + M7
> (server-side crypto review) с совместным sign-off на production rollout.
>
> Адресовано: AR Hud team (включая crypto-engineer'а).
> Author: Sophia (LLM-assistant), 2026-05-21.

---

## 1. Context + scope

### 1.1 Зачем эта работа

[BALLISTICS-MDM-CONTRACT v1](https://github.com/daphate/tactical-ar-hud/blob/master/tools/BALLISTICS-MDM-CONTRACT.md)
описал sync ballistics-профилей через MDM. §8.2 требует
**encryption-at-rest, server не decrypts без user-token**.

Server-side implementation за feature flag готов. Что осталось до
production rollout `BALLISTICS_ENABLED=true`:

- **M6** — Client-side encryption (Kotlin, AR Hud).
- **M7** — Crypto review server-side (peer review командой AR Hud crypto-eng).

### 1.2 Что уже сделано (server-side, не трогать без обсуждения)

v0.18.17 содержит:

- Migrations [0024_ballistics_schema.sql](../crates/outpost-migrations/migrations/0024_ballistics_schema.sql)
  + [0025_ballistics_permissions.sql](../crates/outpost-migrations/migrations/0025_ballistics_permissions.sql).
  5 таблиц (entities/wraps/audit_log/admin_templates/gdpr_deletion_log) +
  3 permissions (`ballistics.read/write/admin`).
- [`routes/ballistics.rs`](../crates/outpost-server/src/routes/ballistics.rs) ~900 строк.
  Все endpoints из CONTRACT §3: CRUD, list с incremental sync,
  audit log, GDPR export/delete-all, admin push templates.
- Admin Web UI: `/ballistics/templates` (list + create + retract).
- 8 unit-tests интегрированы в общий test suite (110/110 passing).
- **Feature flag `BALLISTICS_ENABLED=false`** (env var). При OFF все data
  endpoints возвращают 503, `/health` отвечает `enabled=false`.

**Сервер opaque**: handlers принимают ciphertext-bytes, кладут в БД,
возвращают. Никаких crypto operations на server-side. Vendor pattern —
существующий [`encrypted_distributions`](../crates/outpost-migrations/migrations/0018_encrypted_distributions.sql)
flow для file distribution (Phase 14, deployed 2026-05-17).

### 1.3 Censor граница

Per [BALLISTICS-CRYPTO-DESIGN.md §6](BALLISTICS-CRYPTO-DESIGN.md):
production-deploy `BALLISTICS_ENABLED=true` **не одобрен** без M6 + M7.
Я (LLM-assistant) — не crypto-expert, **не подписываю**
«production-ready» verdict. Verdict подписывает AR Hud crypto-engineer.

---

## 2. Cross-references

| Document | Path | Purpose |
|---|---|---|
| BALLISTICS-MDM-CONTRACT | [`tactical-ar-hud/tools/BALLISTICS-MDM-CONTRACT.md`](https://github.com/daphate/tactical-ar-hud/blob/master/tools/BALLISTICS-MDM-CONTRACT.md) | Canonical wire contract |
| BALLISTICS-CRYPTO-DESIGN | [`outpost-mdm-rs/docs/BALLISTICS-CRYPTO-DESIGN.md`](BALLISTICS-CRYPTO-DESIGN.md) | Design proposal + 10 Open Questions |
| Server ballistics module | [`outpost-mdm-rs/crates/outpost-server/src/routes/ballistics.rs`](../crates/outpost-server/src/routes/ballistics.rs) | Server-side opaque implementation |
| Existing envelope pattern | [`outpost-mdm-rs/crates/outpost-migrations/migrations/0018_encrypted_distributions.sql`](../crates/outpost-migrations/migrations/0018_encrypted_distributions.sql) | Proven ECDH+HKDF+AES-GCM pattern на проде с 2026-05-17 |
| Device keys schema | [`outpost-mdm-rs/crates/outpost-migrations/migrations/0017_device_keys.sql`](../crates/outpost-migrations/migrations/0017_device_keys.sql) | P-256 device keypair storage |
| Enrollment flow | [`outpost-mdm-rs/crates/outpost-server/src/routes/enrollment.rs`](../crates/outpost-server/src/routes/enrollment.rs) (lines 108-126, 242-285) | DevicePubkey wire + upsert |
| Security policy | [`outpost-mdm-rs/SECURITY.md`](../SECURITY.md) | Current crypto posture |

---

## 3. M6 — Client-side encryption (Kotlin)

### 3.1 Crypto primitives wiring

**Required primitives** (точно matches server schema migration 0024 + crypto-design §4):

| Layer | Primitive | Параметры |
|---|---|---|
| Recipient KEM | ECDH P-256 | Reuse `KeystoreWrapper.getOrGenerateDeviceKekKeyPair()`. Private key TEE-bound, public key 91-byte SPKI. |
| KDF | HKDF-SHA-256 | `info = "outpost-mdm-rs/ballistics/v1/wrap"`, `salt = ""`, output 32 bytes. **Domain separation от existing `encrypted_distributions` info string обязателен** (per RFC 5869 §3.2). |
| Content AEAD | AES-256-GCM | 12-byte random nonce per encryption (`SecureRandom`), 16-byte auth tag stored separately. Per-record DEK (32 bytes CSPRNG). |
| Wrap AEAD | AES-256-GCM | Same primitive. Per-(record, recipient) wrap_key. |

**Reuse существующие компоненты** (не дублировать!):

- `KeystoreWrapper` — Android Keystore P-256 keypair management. Уже
  используется в RBAC enrollment scaffold и encrypted_distributions
  client decryption.
- Existing `EncryptedDistribution` Kotlin код (если присутствует в
  AR Hud repo — AR Hud team проверяет) — pattern для ECDH+HKDF+AES-GCM
  decryption.
- `BallisticsRepository` / `WeaponProfileStore` / `DopeStore` — local
  persistent storage. Encrypt **на пути в server**, НЕ at-rest локально
  (Android filesystem encryption handles local on-disk).

### 3.2 Sync client implementation

**Файлы создать** (suggested paths, AR Hud team может отрегулировать):

#### 3.2.1 `app/src/main/java/ru/tacticalar/outpost/ballistics/mdm/BallisticsMdmClient.kt`

HTTP client для `/api/v1/ballistics/*`:

- Header `X-MDM-Token: <bearer>` (reuse existing MDM token из `ModelPreferences.telemetryToken`).
- Functions:
  - `suspend fun pullEntities(kind: BallisticsKind, modifiedSince: Instant?): List<EncryptedEntity>`
  - `suspend fun pushEntity(kind: BallisticsKind, entity: EncryptedEntity, recipients: List<RecipientWrap>): PutResult`
  - `suspend fun deleteEntity(kind: BallisticsKind, id: String): DeleteResult`
  - `suspend fun pullAdminTemplates(): List<AdminTemplate>`
  - `suspend fun pullAuditLog(): List<AuditEntry>`
  - `suspend fun exportAll(): ExportBundle`
  - `suspend fun deleteAll(): DeleteAllResult`
- Sealed-class errors: `Unauthorized` (401), `Forbidden` (403),
  `NotFound` (404), `Conflict(serverVersion)` (412), `Disabled` (503),
  `Network`, `Parse`, `Validation(errors)`.

#### 3.2.2 `app/src/main/java/ru/tacticalar/outpost/ballistics/mdm/BallisticsCrypto.kt`

```kotlin
class BallisticsCrypto(private val keystore: KeystoreWrapper) {
    /**
     * Encrypt plaintext entity payload for multiple recipients.
     * Generates per-record DEK (32 bytes), encrypts plaintext with AES-256-GCM,
     * then per-recipient wraps DEK via ECDH(eph_priv, recipient_pub) + HKDF +
     * AES-256-GCM.
     */
    fun encryptForRecipients(
        plaintext: ByteArray,
        recipients: List<RecipientPubkey>,
    ): EncryptedBundle

    /**
     * Decrypt entity ciphertext using this device's private key.
     * 1. ECDH(this_device_priv, wrap.eph_pubkey) → shared
     * 2. HKDF-SHA-256(shared, salt="", info="outpost-mdm-rs/ballistics/v1/wrap") → wrap_key
     * 3. AES-256-GCM-decrypt(wrap_key, wrap.wrapped_dek, wrap.wrapped_dek_iv) → DEK
     * 4. AES-256-GCM-decrypt(DEK, bundle.ciphertext, bundle.iv) → plaintext
     */
    fun decryptForThisDevice(
        bundle: EncryptedBundle,
        wrap: WrapForThisDevice,
    ): ByteArray
}

data class EncryptedBundle(
    val ciphertext: ByteArray,   // any size, max 64 KB per server validation
    val ciphertextIv: ByteArray, // 12 bytes
    val ciphertextTag: ByteArray, // 16 bytes
    val wraps: List<WrapForRecipient>,
)

data class WrapForRecipient(
    val recipientDeviceId: Long,
    val recipientKeyId: String,    // sha256(pubkey)[0..8] hex
    val ephPubkeyDer: ByteArray,    // 91 bytes SPKI P-256
    val wrappedDek: ByteArray,      // 48 bytes = 32 ct + 16 tag
    val wrappedDekIv: ByteArray,    // 12 bytes
)
```

#### 3.2.3 `app/src/main/java/ru/tacticalar/outpost/ballistics/mdm/BallisticsSyncWorker.kt`

- Periodic incremental pull (per CONTRACT §6.2). Default interval — 30 min,
  configurable через `ModelPreferences.ballisticsSyncIntervalMinutes`.
- Pending sync queue (per §6.3). Persisted DataStore. При offline —
  накапливает push'ы, при reconnect — drain'ит с exp backoff (1m, 2m, 5m, 15m, 1h).
- Conflict resolution через 412 → UI prompt: «Обнаружена конкурирующая
  правка [name], версия сервера N, ваша M. Перезаписать ваши изменения /
  отменить свои изменения?» (per §5.1).

#### 3.2.4 Settings integration

- `ModelPreferences.ballisticsSyncEnabled: Flow<Boolean>` — opt-in toggle
  (default OFF per CONTRACT §8.1, **обязательно**).
- Settings UI: новый switch «Sync ballistics-профилей через MDM (бета)»
  с inline disclaimer «Профили оружия и патронов передаются на сервер
  в зашифрованном виде. Расшифровать может только это устройство.»

#### 3.2.5 Admin template flow (per CONTRACT §3.6)

- При accept template'а — client **локально** encrypt с user's recipient
  list (own device + other devices same user_id, если такие есть) и
  POST как personal record в `/api/v1/ballistics/<kind>/<id>`.
- UI badge: «Командование предложило профиль: [title]» с кнопками
  «Применить / Отклонить».

### 3.3 Acceptance criteria M6

Все ниже — automated test (MockWebServer ИЛИ live local
`outpost-server` v0.18.17+ с `BALLISTICS_ENABLED=true`):

- [ ] **Roundtrip:** PUT WeaponProfile → server opaque → GET back →
  decrypt → **бит-в-бит совпадение** plaintext payload (Kotlin
  `assertContentEquals`).
- [ ] **Multi-recipient:** create entity с 2 wraps (own device + ещё
  один device того же user'а) → второе устройство decrypts OK (если
  в команде есть две test-машины — иначе через MockWebServer).
- [ ] **Conflict 412:** simulate concurrent edit'ы (2 PUT с одинаковым
  `expected_version`) → second получает 412 PreconditionFailed → UI
  shows resolution prompt.
- [ ] **Soft-delete sync:** DELETE on device A → GET `?modified_since=`
  on device B (или MockWebServer-replay) → видит `deleted_ts != null` →
  локально удаляет.
- [ ] **Admin template accept:** pull `/admin/templates` → user accepts
  → client encrypts + POST → entity видна в
  `/api/v1/ballistics/weapon/<id>` GET.
- [ ] **Feature flag respect:** при server-side `BALLISTICS_ENABLED=false`
  → клиент получает 503 на data endpoints → UI shows «sync disabled by
  admin» (NOT retry loop, NOT crash, NOT user-visible error).
- [ ] **Offline survival:** sync disabled OR network down → local CRUD
  продолжает работать (per CONTRACT §9.5 Open Q5 ответ).
- [ ] **CSPRNG quality:** statistical test (NIST SP 800-22 minimum
  bands — Frequency, Block Frequency, Runs) на 10 000 DEK генераций от
  `SecureRandom` на target tier devices (T0/T1/T2 vendors из списка
  ниже).
- [ ] **Zero-copy secrets:** logcat-grep test — после 50 encrypt/decrypt
  циклов в logcat **нет** упоминаний DEK байтов / plaintext payload
  content. (Use `tools/logcat_secret_audit.sh` если уже есть в repo,
  или create.)
- [ ] **Default OFF compliance:** свежеустановленный APK + fresh
  enrollment → `ballisticsSyncEnabled = false`. Sync **не происходит**
  до явного user-toggle.

**Target tier devices для testing** (canonical из CONTRACT § / device
catalog):
- T0: Realme Note 60X (MTK Helio G99)
- T1: Honor 400 Pro
- T2: Ulefone Armor 28 Ultra

### 3.4 Что НЕ делать в M6 (out of scope)

- Re-encryption при device key rotation — defer to v2.
- Backup/restore encrypted records через external channel (e.g. Telegram).
- Sharing records между **разными** user'ами того же customer'а —
  только через admin push v1.
- Post-quantum primitives — v2 hybrid с ML-KEM.

---

## 4. M7 — Server-side crypto review

### 4.1 Files to review

Canonical list — review должен покрывать **все**:

| Path | Lines | Что в нём |
|---|---|---|
| [`docs/BALLISTICS-CRYPTO-DESIGN.md`](BALLISTICS-CRYPTO-DESIGN.md) | 489 | Design proposal, threat model, primitive choices, alternatives rejected |
| [`crates/outpost-server/src/routes/ballistics.rs`](../crates/outpost-server/src/routes/ballistics.rs) | ~900 | All handlers, validation, multi-tenant boundary checks |
| [`crates/outpost-migrations/migrations/0024_ballistics_schema.sql`](../crates/outpost-migrations/migrations/0024_ballistics_schema.sql) | ~150 | Schema (5 tables) с CHECK constraints, indexes, FK |
| [`crates/outpost-migrations/migrations/0025_ballistics_permissions.sql`](../crates/outpost-migrations/migrations/0025_ballistics_permissions.sql) | ~30 | Permissions + role grants |
| [`crates/outpost-migrations/migrations/0017_device_keys.sql`](../crates/outpost-migrations/migrations/0017_device_keys.sql) | 31 | Existing P-256 device keypair storage |
| [`crates/outpost-migrations/migrations/0018_encrypted_distributions.sql`](../crates/outpost-migrations/migrations/0018_encrypted_distributions.sql) | 65 | Existing ECDH+AES-GCM envelope pattern (foundation для ballistics) |
| [`crates/outpost-server/src/routes/enrollment.rs`](../crates/outpost-server/src/routes/enrollment.rs) (lines 108-126, 242-285) | — | DevicePubkey wire + upsert flow |
| [`SECURITY.md`](../SECURITY.md) | 91 | Current cryptographic posture |

### 4.2 10 Open Questions verification

Per [BALLISTICS-CRYPTO-DESIGN §6](BALLISTICS-CRYPTO-DESIGN.md#6-open-questions-for-expert-review),
findings document должен содержать formal answer на **каждый** OQ в
следующем формате:

```
OQ #N: [полный текст вопроса из crypto-design]
Verdict: resolved | mitigated | accepted-with-risk | blocking
Verifier: [имя + credentials, e.g. "Иванов И.И., M.Sc. Cryptography, MSU 2018"]
Date: YYYY-MM-DD
Evidence: [конкретные refs — file paths, test results, citation URLs]
Recommendation: [если blocking — что fix'ить; если accepted-with-risk —
                 что мониторить в production]
```

Полный список 10 OQ (canonical в crypto-design §6, рекап ниже):

| # | Question | Hint для verifier'а |
|---|---|---|
| 1 | Android Keystore TEE-backing на vendor'ах parka | Parse attestation extension через `KeyInfo` API, compare с known TEE/StrongBox markers. Realme/Honor/Ulefone tested matrix. |
| 2 | Constant-time correctness `aes-gcm` crate | Review crate source vs known CPU cache-timing literature. Note: server-side use opaque — мог не уметь side-channel issue из-за no plaintext on server. |
| 3 | Recipient selection при device enroll после record creation | Decision document: какой recovery flow. Trigger-push? Manual export-import? Lost = accepted? |
| 4 | Android CSPRNG quality на MTK Helio G99 | NIST SP 800-22 test on physical device. Document any FIPS or vendor-specific RNG claims. |
| 5 | HKDF info string sufficient для domain separation | Grep всех HKDF info strings в obech репо (outpost-mdm-rs + tactical-ar-hud). Verify no collision. |
| 6 | Wrap key replay attack window при concurrent PUTs | Race condition analysis: что если 2 PUT'а pre-empt друг друга. Существующая server-side transaction в `routes/ballistics.rs::put_entity` использует `tx.commit()` — поведение под concurrent load? |
| 7 | DEK leak через side channels | Add `zeroize::Zeroize` derive к secret-holding structs? `mlock` для process memory? Process-dump audit. |
| 8 | Audit log 30-day retention vs forensic value | Compliance + ops decision. Может быть legal-eng input. |
| 9 | SQLite VACUUM для GDPR hard-purge | Verify experimentally: WAL pages могут содержать old data после DELETE. `PRAGMA secure_delete=on`? `VACUUM INTO encrypted_disk`? |
| 10 | Key rotation race condition (`device_keys.revoked_at` mid-flight) | State machine spec. Что если client revoke'нул key между server PUT и client decrypt? |

### 4.3 Findings format + sign-off

Создаётся AR Hud team: **`outpost-mdm-rs/docs/BALLISTICS-REVIEW-FINDINGS.md`**

Структура:

```markdown
# BALLISTICS-REVIEW-FINDINGS

Date: YYYY-MM-DD
Reviewer: [Name, role, credentials]
Scope: routes/ballistics.rs + BALLISTICS-CRYPTO-DESIGN.md + migrations 0017/0018/0024/0025

## §1. Open Questions verdicts

### OQ #1: [текст]
Verdict: ...
Evidence: ...
Recommendation: ...

[...повторить для OQ #2..#10]

## §2. Discovered issues (за пределами 10 OQ)

### D-1: [найденная issue]
Severity: critical | high | medium | low | informational
Location: [file:line]
Description: ...
Recommendation: ...

## §3. Sign-off

**Reviewer:** [Имя]
**Role:** [Crypto-engineer / Security architect / etc.]
**Credentials:** [CV link, IACR membership, published papers, etc.]
**Date:** YYYY-MM-DD
**Verdict:** production-ready | production-ready-with-mitigations | requires-fixes

### Blocking issues (если any)
- [list with explicit references to OQ # or D-#]

### Mitigations to apply BEFORE Фаза 2 full rollout
- [list]

### Mitigations to monitor IN production
- [list]

**Approval workflow:**
1. AR Hud crypto-engineer signs §3 (commits findings PR).
2. Николай (repo owner) reviews + approves PR в outpost-mdm-rs.
3. Merge → Phase 2 full rollout одобрен (per §6).
```

---

## 5. Integration test plan

End-to-end сценарии — выполнить после M6 implementation + M7 review.
Run-environment: live `mdm.secondf8n.tech` с `BALLISTICS_ENABLED=true`
(per chosen strategy «production-as-staging», см. §6).

### 5.1 Happy path

| ID | Scenario | Expected |
|---|---|---|
| E1 | Create WeaponProfile (PUT) → GET | bit-exact plaintext roundtrip |
| E2 | Update existing с правильным `If-Match` | 200 + `version + 1` в response |
| E3 | List with `?modified_since=<old_ts>` | только entities с `modified_ts > old_ts` |
| E4 | DOPE filtered by `?weapon_id=X` | только записи с `parent_id = X` |
| E5 | Admin push template → device pulls → user accepts | entity создаётся локально + появляется в GET |
| E6 | Multi-device sync (2 enrolled devices same user_id) | create на A → видно на B через ≤1 sync cycle |

### 5.2 Failure scenarios

| ID | Scenario | Expected |
|---|---|---|
| F1 | Concurrent edit → 412 | UI prompt resolution |
| F2 | Feature flag OFF → data endpoints | 503; `/health` отвечает `enabled=false`; client gracefully degrades |
| F3 | Expired device token → 401 | client refresh через MDM channel → retry |
| F4 | Wrap recipient ≠ same customer | 400 BadRequest (data exfiltration guard) |
| F5 | Oversized ciphertext (>64 KB) | 400 |
| F6 | Invalid kind / wrong id prefix | 400 |
| F7 | Sequence: DELETE → CREATE с тем же id | succeeds (un-soft-delete via PUT) |
| F8 | Same-tenant create на уже занятый id (гонка) | 409 Conflict (не opaque 500) |
| F9 | Update без `owner_user_id` | 200; owner сохраняется (immutable — не 500) |

### 5.3 Privacy invariants

| ID | Scenario | Expected |
|---|---|---|
| P1 | Cross-tenant access attempt | **404** (NOT 403 — не leak'аем existence) |
| P2 | GET wrap для чужого device | wrap не возвращается в `wrap_for_this_device` |
| P3 | GDPR export | server returns ciphertext-bundle; client decrypts → plaintext bundle для save |
| P4 | GDPR delete-all → последующий GET | 404 (hard purge); compliance row в `ballistics_gdpr_deletion_log` |
| P5 | Server logs grep на `bullet_mass`/`muzzle_velocity`/`ballistic_coefficient` | **0 hits** (plaintext content никогда не leak'нется через server logs) |
| P6 | Server logs grep на base64-encoded DEK или wrap_key | 0 hits |
| P7 | Один и тот же id в двух разных тенантах | оба create succeed — id уникален per-tenant (composite PK `(id, customer_id)`, миграция 0028); cross-tenant create НЕ коллизит → нет existence-oracle |
| P8 | Update с другим `owner_user_id` | значение игнорируется — owner immutable после create (нет silent reassignment) |
| P9 | GDPR export превышает per-export LIMIT (10000/5000) | `truncated=true` + `entities_total`/`audit_total` (`schema_version` 2) — обрезка не молчит (Art.15/20 completeness) |

### 5.4 Stress / chaos

| ID | Scenario | Expected |
|---|---|---|
| S1 | 100 concurrent PUTs от одного device | no panics, no 5xx, all eventually persisted |
| S2 | Client с corrupted local pending queue | recovery — drop corrupt, log warning, продолжить |
| S3 | Network drop посередине PUT | client retry с exp backoff, server idempotent через ETag |
| S4 | Server restart посередине long-poll | client retry на reconnect |

---

## 6. Production gating criteria (две фазы)

Per coordination 2026-05-21, deploy идёт в **2 фазы**.

### 6.1 Фаза 1 — Production-as-staging deploy (ASAP, в течение недели)

`BALLISTICS_ENABLED=true` на mdm.secondf8n.tech при выполнении ВСЕХ:

- [ ] M6 implementation минимум: `BallisticsMdmClient` + `BallisticsCrypto` + opt-in toggle (default OFF).
- [ ] Client-side default OFF подтверждён в код-ревью M6 PR.
- [ ] Canary list тестеров согласован (≤5 devices от AR Hud team).
- [ ] Acceptance criteria E1, E2, F2, F3, P1 — pass (минимальный happy path + privacy invariants).
- [ ] Monitoring setup: daily journalctl grep на panics/5xx в `/api/v1/ballistics/*`.
- [ ] Rollback procedure documented: одной командой `BALLISTICS_ENABLED=false` + restart (≤30 sec downtime).
- [ ] Internal-only feature flag — **не** advertised к full fleet через release notes / changelog.

Это deploy **с production risk** для тестерского subset'а. M7 review
продолжается параллельно. Если в течение Phase 1 review находит
blocking issue — см. §8 failure-mode policy.

### 6.2 Фаза 2 — Full production rollout (после M7 sign-off)

Default OFF снимается с client-side (или повышается до «recommended ON»),
feature advertised в release notes:

- [ ] M6 acceptance criteria §3.3 — **все** pass (включая multi-recipient + admin templates + offline + CSPRNG + zero-copy).
- [ ] M7 findings — no `blocking` verdicts; все `mitigated`/`accepted-with-risk` explicit signed-off от AR Hud crypto-engineer.
- [ ] Integration test plan §5 — все happy/failure/privacy scenarios pass в Phase 1 staging period.
- [ ] Update `BALLISTICS-CRYPTO-DESIGN.md` — Open Questions §6 заменены на cross-link к findings document.
- [ ] Update `CHANGELOG.md` обоих репо (`outpost-mdm-rs` + `tactical-ar-hud`) — feature enabled, advertised.
- [ ] Sign-off от AR Hud crypto-engineer в `BALLISTICS-REVIEW-FINDINGS.md §3`.
- [ ] Approval Николая на merge findings PR (как repo owner).
- [ ] Zero panics/5xx в production logs за Phase 1 period (минимум 7 дней наблюдения, или быстрее если crypto-engineer accepts shortened observation window).

---

## 7. Out of scope этого work-package'а

Сознательно НЕ требуем в этом round'е:

- **Post-quantum migration** (P-256 → ML-KEM hybrid). Track как v2 contract.
- **Forward secrecy через ratcheting** (Signal-style). Overkill для use-case.
- **Multi-record batch encryption** (current design = per-record DEK).
- **Direct cross-user sharing** (помимо admin push v1).
- **GC task для hard-purge soft-deleted >90 дней** (TODO server-side, не блокер для review).
- **`parent_bundled_id` для CartridgeProfile** (v2 schema change).

---

## 8. Coordination — резюме ответов и risks

### 8.1 Ответы (зафиксированы 2026-05-21)

| Question | Answer |
|---|---|
| Deadline | ASAP в течение недели |
| Server-side sign-off | AR Hud team (cross-team peer review без отдельного server-side sign-off от Николая, но при approval'е PR'а repo owner'ом) |
| Findings document location | PR в `outpost-mdm-rs/docs/BALLISTICS-REVIEW-FINDINGS.md` |
| Staging strategy | **Production с `BALLISTICS_ENABLED=true` раньше полного sign-off** (production-as-staging) |

### 8.2 ⚠ Risks выбранной staging стратегии

Production-as-staging deploy без полного review — это inhent risk
для tactical-software domain. Mitigations (must в M6/M7):

1. **Client-side default OFF.** Per CONTRACT §8.1: ballistics-sync —
   opt-in toggle, default OFF. Это **обязательно** в M6 — иначе
   server-flag-ON = data-flowing-в-production независимо от sign-off
   готовности.
2. **Canary через ограниченный set devices.** Server-flag ON, но
   реально sync делают **только** explicitly opted-in test devices
   (≤5 от AR Hud team). Прод-устройства остаются OFF.
3. **Server logs grep monitoring.** Daily check на panics/5xx. AR Hud
   подписывается на alerts. При первом panic — `BALLISTICS_ENABLED=false`
   немедленно.
4. **No advertising в product.** Feature **не появляется** в release
   notes / changelog / UX promo до Phase 2 sign-off. Internal testing
   only.
5. **Rollback plan.** Blocking finding → `BALLISTICS_ENABLED=false`
   env override + restart (≤30 sec downtime, no data loss). Existing
   данные останутся в БД (encrypted), sync прекратится. Re-enable
   тривиален.
6. **Censor граница (Sophia / LLM).** Я **не подписываю** «production-ready»
   verdict в findings document. Verdict — целиком ответственность
   AR Hud crypto-engineer'а. Я могу answer вопросы по реализации, но
   не подписываюсь под crypto-correctness.

⚠ **Compressed timeline risk.** ASAP deadline увеличивает риск
rubber-stamping review. Mitigation: фокусировать review на «no blocking
findings» bar (не «zero questions»). Non-blocking concerns explicit
defer'ить на v2 contract revision с явным TODO.

### 8.3 Failure-mode для blocking findings (default policy)

Если M7 review находит blocking issue в течение Phase 1
production-as-staging period:

- **≤2 часа**: `BALLISTICS_ENABLED=false` env override + restart.
- **≤24 часа**: Николай + AR Hud crypto-eng обсуждают: hotfix vs
  revert vs design v2.
- **Hotfix path**: новая migration + server patch + AR Hud client
  update — coordinated release.
- **Revert path**: оставить server code dormant (можно re-enable позже),
  notify тестеров что sync приостановлен.
- **Design-level blocking**: bump к v2 contract с deprecation period
  per BALLISTICS-MDM-CONTRACT §1 versioning policy.

---

## 9. Critical files reference

### Создаётся в рамках этого work-package'а

- [`docs/BALLISTICS-IMPLEMENTATION-SPEC.md`](BALLISTICS-IMPLEMENTATION-SPEC.md) — этот документ.
- `docs/BALLISTICS-REVIEW-FINDINGS.md` — AR Hud crypto-engineer M7 deliverable.
- Client-side Kotlin файлы в `tactical-ar-hud/prototypes/outpost-android/app/src/main/java/ru/tacticalar/outpost/ballistics/mdm/` (точные paths — AR Hud team).

### Read-only references (НЕ менять без отдельного обсуждения)

- [`outpost-mdm-rs/docs/BALLISTICS-CRYPTO-DESIGN.md`](BALLISTICS-CRYPTO-DESIGN.md) — server design proposal.
- [`outpost-mdm-rs/crates/outpost-server/src/routes/ballistics.rs`](../crates/outpost-server/src/routes/ballistics.rs) — server implementation.
- [`tactical-ar-hud/tools/BALLISTICS-MDM-CONTRACT.md`](https://github.com/daphate/tactical-ar-hud/blob/master/tools/BALLISTICS-MDM-CONTRACT.md) — canonical wire contract.

### Update workflow

Если AR Hud team находит несоответствие между этим ТЗ и реальностью
(server schema changed, contract evolved):

1. PR против этого файла со change log в bottom.
2. Sophia (server-side maintainer) review'ит + merges.
3. Обновляется version stamp + cross-references.

---

## 10. История изменений

| Дата | Версия | Изменения |
|---|---|---|
| 2026-05-21 | 0.1 (initial) | Создан по plan'у [`reactive-herding-hamster.md`](https://github.com/daphate/outpost-mdm-rs) после coordination ответов от пользователя. Адресовано AR Hud team. |
