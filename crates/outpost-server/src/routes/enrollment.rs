//! Device-facing enrollment and sync endpoints (long-polling transport).
//!
//! `POST /api/v1/devices/{id}/enrollment` (admin) regenerates a device's
//! enrollment_secret and returns the QR payload an admin must hand to the
//! field unit. `POST /api/v1/enroll` is the device-facing call that
//! exchanges (device_id, enrollment_secret) for a long-lived device JWT.
//!
//! `POST /api/v1/sync` (device JWT) is the per-tick check-in: device
//! sends telemetry + acks, server returns pending commands.

use crate::auth;
use crate::auth_extract::{AuthDevice, AuthUser};
use crate::error::ApiError;
use crate::permission::require_permission;
use crate::session;
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::post,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Long-lived device token TTL (90 days). Devices re-enroll if it expires.
const DEVICE_TOKEN_TTL_SECS: i64 = 60 * 60 * 24 * 90;

/// Long-poll hard upper-bound (v0.17 MDM-DEVICE-CONTROL-CONTRACT §1.5).
/// Если client передаёт `wait_for_command_ms` больше — clamping. 30 секунд —
/// разумный компромисс: достаточно для near-real-time push (admin тыкает
/// «Применить», устройство получает за ≤30s), но не настолько долго чтобы
/// держать TCP-соединение через NAT-таймауты (типично 60s+ NAT TTL).
pub const LONG_POLL_MAX_MS: u64 = 30_000;

/// Опрос pending-команд внутри long-poll loop'а. 2 секунды — достаточно
/// отзывчиво (worst-case +2s после admin POST'а) и не нагружает SQLite
/// сильнее чем background scheduler. Когда захотим sub-секунду — мигрируем
/// на `tokio::sync::Notify` per-device без breaking-changes для wire.
const LONG_POLL_TICK_MS: u64 = 2_000;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/devices/{id}/enrollment", post(generate_enrollment))
        .route("/api/v1/enroll", post(enroll))
        .route("/api/v1/sync", post(sync))
        // v0.18.20 (security review DOS-1 follow-up): per-route body limit.
        // /sync берёт `Json<SyncRequest>`, у которого `current_state` —
        // Option<Value> (unbounded), а acks/applied_commands — Vec<String>.
        // Json-экстрактор уважает DefaultBodyLimit, поэтому слой реально гейтит
        // тело ДО десериализации. Без него /sync наследовал глобальный 200 MiB
        // → один enrolled device мог OOM-killed процесс (MemoryMax=256M);
        // count-cap MT-4 (≤256) проверяется уже ПОСЛЕ буферизации, поэтому
        // body-limit — обязательное дополнение к нему. /enroll + /enrollment —
        // крошечный JSON, 256 KiB всем троим с запасом.
        .layer(axum::extract::DefaultBodyLimit::max(256 * 1024))
}

// ----------------- admin: generate enrollment payload --------------------

#[derive(Debug, Serialize)]
pub struct EnrollmentPayload {
    pub server_url: Option<String>,
    pub customer_id: i64,
    pub device_id: i64,
    pub enrollment_secret: String,
}

async fn generate_enrollment(
    user: AuthUser,
    State(state): State<AppState>,
    Path(device_id): Path<i64>,
) -> Result<Json<EnrollmentPayload>, ApiError> {
    require_permission(&state.db, user.role_id, "devices.enroll").await?;
    let device_row: Option<(i64,)> =
        sqlx::query_as("SELECT id FROM devices WHERE id = ? AND customer_id = ?")
            .bind(device_id)
            .bind(user.customer_id)
            .fetch_optional(&state.db)
            .await?;
    if device_row.is_none() {
        return Err(ApiError::NotFound);
    }
    let secret = auth::generate_password(32);
    sqlx::query(
        "UPDATE devices SET enrollment_secret = ?, is_enrolled = 0, updated_at = datetime('now') \
         WHERE id = ?",
    )
    .bind(&secret)
    .bind(device_id)
    .execute(&state.db)
    .await?;

    // server_url is read from settings; null if unset (admin must configure).
    let server_url: Option<String> = sqlx::query_scalar(
        "SELECT json_extract(value_json, '$') FROM settings WHERE key = 'server.enrollment_base_url'",
    )
    .fetch_optional(&state.db)
    .await?
    .flatten();

    Ok(Json(EnrollmentPayload {
        server_url,
        customer_id: user.customer_id,
        device_id,
        enrollment_secret: secret,
    }))
}

// ----------------- device: enroll ----------------------------------------

#[derive(Debug, Deserialize)]
pub struct EnrollRequest {
    pub device_id: i64,
    pub enrollment_secret: String,
    pub os_version: Option<String>,
    pub app_version: Option<String>,
    /// v0.14 (MDM-DEVICE-CONTROL-CONTRACT §2.4): client'ский ECDH P-256
    /// public key — 65 байт SEC1 uncompressed point, генерится клиентом
    /// в Android Keystore (`KeystoreWrapper.getOrGenerateDeviceKekKeyPair()`).
    /// Server хранит в `device_keys`, использует для per-device encrypt-for-recipient.
    /// Если отсутствует — устройство не сможет получать encrypted-distribution
    /// файлы (но enroll проходит).
    pub device_pubkey: Option<DevicePubkey>,
}

#[derive(Debug, Deserialize)]
pub struct DevicePubkey {
    /// `"ECDH-P256"` для v1; в будущем может быть `"X25519"`.
    pub alg: String,
    /// 65 байт SEC1 uncompressed (`0x04 || X(32) || Y(32)`), base64url-encoded
    /// в JSON wire-format. Парсим decode'ом ниже.
    pub der: String,
    /// `sha256(der)[0..8]` hex — детерминированный fingerprint.
    pub key_id: String,
}

#[derive(Debug, Serialize)]
pub struct EnrollResponse {
    pub device_token: String,
    pub expires_in: i64,
    pub device_id: i64,
    pub customer_id: i64,
    /// v0.14: подтверждаем что pubkey сохранён в device_keys. `false` если
    /// клиент не прислал pubkey (legacy) или прислал invalid bytes.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub device_pubkey_acknowledged: bool,
    /// v0.17 (MDM-DEPLOY-CONTRACT §1.5): опциональный read-only Cloud.ru
    /// service-account, который клиент сохраняет в `ModelPreferences.cloudruCreds`
    /// и применяет через `CloudRuSigner.setOverride()`. Server включает в
    /// response только если CLOUDRU_TENANT_ID/KEY_ID/SECRET env'ы заданы.
    /// Иначе поле отсутствует, и клиент работает на встроенном в APK fallback'е.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloudru_credentials: Option<CloudruCredentials>,
}

/// v0.17 wire-shape для cloudru creds в enroll response. Snake_case на
/// проводе соответствует существующему контракту `ModelPreferences.setCloudruCreds(
/// tenantId, keyId, secret)` в Android-клиенте (`CloudRuSigner.kt:50-55`).
#[derive(Debug, Serialize)]
pub struct CloudruCredentials {
    pub tenant_id: String,
    pub key_id: String,
    pub secret: String,
}

async fn enroll(
    State(state): State<AppState>,
    Json(req): Json<EnrollRequest>,
) -> Result<Json<EnrollResponse>, ApiError> {
    let row: Option<(i64, String, Option<String>)> =
        sqlx::query_as("SELECT customer_id, serial, enrollment_secret FROM devices WHERE id = ?")
            .bind(req.device_id)
            .fetch_optional(&state.db)
            .await?;
    let (customer_id, serial, stored_secret) = row.ok_or(ApiError::Unauthorized)?;
    let stored = stored_secret.ok_or(ApiError::Unauthorized)?;
    if stored != req.enrollment_secret {
        // Avoid leaking timing — but this is internal stub, plain compare.
        return Err(ApiError::Unauthorized);
    }
    // Clear secret (single use), mark enrolled, capture initial versions.
    sqlx::query(
        "UPDATE devices SET \
            enrollment_secret = NULL, \
            is_enrolled = 1, \
            os_version = COALESCE(?, os_version), \
            app_version = COALESCE(?, app_version), \
            last_seen_at = datetime('now'), \
            is_online = 1, \
            updated_at = datetime('now') \
         WHERE id = ?",
    )
    .bind(&req.os_version)
    .bind(&req.app_version)
    .bind(req.device_id)
    .execute(&state.db)
    .await?;
    let token = session::create_device_session(
        &state.db,
        req.device_id,
        customer_id,
        &serial,
        DEVICE_TOKEN_TTL_SECS,
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    // v0.14: store device_pubkey for per-device encrypted distribution.
    // Server-side validation: decode base64, check length, compute and
    // verify key_id (if client прислал wrong key_id — игнорируем pubkey,
    // не считаем enroll'ом провалом).
    let pubkey_acknowledged = if let Some(pk) = req.device_pubkey {
        match store_device_pubkey(&state.db, req.device_id, &pk).await {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!(
                    device_id = req.device_id,
                    alg = %pk.alg,
                    error = %e,
                    "device_pubkey reject — enroll still succeeds"
                );
                false
            }
        }
    } else {
        false
    };

    // v0.17: пробрасываем server-side Cloud.ru read-only creds в response,
    // если они сконфигурированы. Клиент сохранит их через MDM override flow
    // и будет использовать для скачивания моделей/документов через signer.
    // Per-device персонализированные creds — следующая итерация (см. roadmap).
    let cloudru_credentials = state.cloudru_signer.as_ref().map(|signer| {
        CloudruCredentials {
            tenant_id: signer.tenant_id().to_string(),
            key_id: signer.key_id().to_string(),
            secret: signer.secret().to_string(),
        }
    });

    Ok(Json(EnrollResponse {
        device_token: token,
        expires_in: DEVICE_TOKEN_TTL_SECS,
        device_id: req.device_id,
        customer_id,
        device_pubkey_acknowledged: pubkey_acknowledged,
        cloudru_credentials,
    }))
}

/// Декодирует и сохраняет client ECDH pubkey в `device_keys`. Возвращает
/// `Ok(())` если запись успешно вставлена или уже существовала (по UNIQUE
/// constraint). Возвращает `Err` если: алгоритм неизвестен, base64 invalid,
/// длина не SEC1-uncompressed, или key_id mismatch'ит computed sha256.
async fn store_device_pubkey(
    pool: &sqlx::SqlitePool,
    device_id: i64,
    pk: &DevicePubkey,
) -> anyhow::Result<()> {
    use base64::Engine;
    if pk.alg != "ECDH-P256" {
        return Err(anyhow::anyhow!("unknown alg: {}", pk.alg));
    }
    let der_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&pk.der)
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(&pk.der))
        .map_err(|e| anyhow::anyhow!("base64 decode: {e}"))?;
    if der_bytes.len() != crate::distribution::SEC1_UNCOMPRESSED_LEN {
        return Err(anyhow::anyhow!(
            "pubkey not {}-byte SEC1 uncompressed (got {})",
            crate::distribution::SEC1_UNCOMPRESSED_LEN,
            der_bytes.len(),
        ));
    }
    // Smoke-check: pubkey должен быть валидной точкой на P-256 curve.
    let _ = p256::PublicKey::from_sec1_bytes(&der_bytes)
        .map_err(|e| anyhow::anyhow!("invalid P-256 point: {e}"))?;
    // Verify key_id matches.
    use sha2::Digest;
    let digest = sha2::Sha256::digest(&der_bytes);
    let expected_id: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    if expected_id != pk.key_id.to_ascii_lowercase() {
        return Err(anyhow::anyhow!(
            "key_id mismatch: server-computed {expected_id}, client sent {}",
            pk.key_id
        ));
    }
    // UPSERT — на повторный enroll device'а с тем же pubkey просто игнорим
    // повторный INSERT через ON CONFLICT.
    sqlx::query(
        "INSERT INTO device_keys (device_id, alg, pubkey_der, key_id) \
         VALUES (?, ?, ?, ?) \
         ON CONFLICT(device_id, key_id) DO NOTHING",
    )
    .bind(device_id)
    .bind(&pk.alg)
    .bind(&der_bytes)
    .bind(&expected_id)
    .execute(pool)
    .await?;
    Ok(())
}

// ----------------- device: sync ------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SyncRequest {
    pub battery_pct: Option<i64>,
    pub last_lat: Option<f64>,
    pub last_lon: Option<f64>,
    pub os_version: Option<String>,
    pub app_version: Option<String>,
    /// rc42 b37+: integer Android versionCode (см. `BuildConfig.VERSION_CODE`).
    /// Используется в Tier-2 rollout-policy для сравнения «у устройства версия
    /// ниже target → отдать update_available». Если устройство шлёт только
    /// `app_version` (string), policy будет no-op для него — мы не парсим
    /// строки в код.
    pub app_version_code: Option<i64>,
    /// rc42 b37+: monotonic integer, увеличивается на каждый set*() в
    /// ModelPreferences. Server сравнивает с `devices.current_state_version`
    /// и обновляет snapshot если client прислал свежее.
    pub state_version: Option<i64>,
    /// rc42 b37+: snapshot всех видимых admin'у ModelPreferences-настроек.
    /// См. MDM-DEVICE-CONTROL-CONTRACT.md §1.3 за полным списком ключей.
    /// Не содержит secrets (есть только `*_has_token` bool-маркеры).
    pub current_state: Option<serde_json::Value>,
    /// rc42 b37+: outcomes исполнения push-команд из prior sync'ов.
    /// Каждая запись содержит `id` (UUID или int-as-string) и `status`
    /// ("ok" | "error") + опциональный `message`. Server обновляет
    /// `push_messages.status` соответственно.
    #[serde(default)]
    pub applied_commands: Vec<AppliedCommand>,
    /// IDs команд которые device считает delivered (idempotent ack).
    /// Может быть пустым; в случае конфликта с applied_commands побеждает
    /// applied_commands.status. v0.13: тип сменён с Vec<i64> на Vec<String>
    /// — id может быть UUID (b37+) или integer-as-string (legacy ≤ b36).
    /// Server парсит как i64 fallback'ом, иначе ignore-with-warn.
    #[serde(default)]
    pub acks: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct AppliedCommand {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub message: Option<String>,
}

/// Wire shape команды в /api/v1/sync response. v0.13: id отправляется как
/// String для forward-compat с UUID-based client'ами (rc42 b37+
/// ModelPreferences treats command_id как opaque string). Internally в БД
/// id остаётся INTEGER PRIMARY KEY AUTOINCREMENT.
#[derive(Debug, Serialize)]
pub struct SyncCommand {
    pub id: String,
    pub command: String,
    pub payload_json: String,
}

#[derive(Debug, sqlx::FromRow)]
struct SyncCommandRow {
    id: i64,
    command: String,
    payload_json: String,
}

/// Прочитать pending push-команды для конкретного device, не меняя их state.
/// Reused в обычном drain'е и в long-poll tick'е (без UPDATE — обновление
/// `status='sent'` происходит **один раз** после того как long-poll либо
/// собрал команды, либо deadline истёк).
async fn fetch_pending_for_device(
    pool: &sqlx::SqlitePool,
    device_id: i64,
) -> Result<Vec<SyncCommandRow>, ApiError> {
    let rows = sqlx::query_as::<_, SyncCommandRow>(
        "SELECT id, command, payload_json FROM push_messages \
         WHERE device_id = ? AND status = 'pending' \
         ORDER BY id ASC LIMIT 50",
    )
    .bind(device_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// v0.12 (Tier-2): описание APK-обновления, которое клиент должен скачать
/// и установить через `PackageInstaller`. Включается в response **только**
/// если выбранный target.version_code > device.app_version_code. Если
/// устройство уже на target или выше — поле отсутствует.
#[derive(Debug, Serialize)]
pub struct SyncUpdateAvailable {
    pub version_code: i64,
    pub version_name: String,
    pub sha256: String,
    pub size_bytes: i64,
    /// Где скачивать APK. Если в БД `application_versions.source_url`
    /// заполнен (watcher-discovered row) — отдаём его (R2 anonymous URL).
    /// Если row uploaded локально в MDM — будет signed-URL на /files/...
    /// (TODO в Tier-2.5 когда выложу `application_versions/{id}/download`).
    pub url: String,
    /// Причина почему обновление прилетело — для UI и audit log на устройстве.
    /// `"pinned"` | `"canary"` | `"fleet"` — соответствует source policy.
    pub source: &'static str,
}

#[derive(Debug, Serialize)]
pub struct SyncResponse {
    pub commands: Vec<SyncCommand>,
    pub server_time: chrono::DateTime<chrono::Utc>,
    /// v0.12 Tier-2. `None` если устройство on-target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_available: Option<SyncUpdateAvailable>,
    /// v0.18.19 (per INSIGHT-055 §7.1.4): ETag-style hint для bundle
    /// assignments. SHA-256 hex от canonical concat всех effective
    /// bundles этого device'а (см. `compute_bundles_etag`).
    ///
    /// **Client usage**: запомнить etag из last sync; на новом sync —
    /// сравнить server's `bundles_etag` с локальным. Если equal — skip
    /// `GET /api/v1/device/bundles` (assignments не изменились). Если
    /// different — fetch + update local state + persist new etag.
    ///
    /// `None` если device без enrollment'а ИЛИ ошибка вычисления (client
    /// должен трактовать None как «не знаю, fetch на всякий случай»).
    /// `"sha256:<hex>"` префикс для будущей миграции на другие hash'и.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundles_etag: Option<String>,
}

/// Query-string параметры для `/api/v1/sync`. v0.17: добавлен
/// `wait_for_command_ms` для long-polling режима. Клиент опционально
/// передаёт `?wait_for_command_ms=30000` — если по результату обычного
/// drain'а нет pending commands, server держит соединение до этого
/// timeout'а либо до появления push'а. Default = 0 (старое immediate-return
/// поведение для legacy клиентов).
#[derive(Debug, Deserialize, Default)]
pub struct SyncQuery {
    #[serde(default)]
    pub wait_for_command_ms: Option<u64>,
}

/// Sliding refresh threshold: bump expiry если remaining < 50% of full TTL.
/// При 90-дневном TTL это означает что каждый /sync который происходит во
/// второй половине лimerock'и сессии продлевает её на ещё 90 дней.
const SESSION_REFRESH_THRESHOLD_PCT: i64 = 50;

async fn sync(
    device: AuthDevice,
    State(state): State<AppState>,
    Query(q): Query<SyncQuery>,
    Json(req): Json<SyncRequest>,
) -> Result<Json<SyncResponse>, ApiError> {
    // v0.18.20 (security review MT-4): cap applied_commands / acks lengths.
    // Каждый элемент → один UPDATE push_messages; без лимита device мог
    // прислать десятки тысяч элементов (body до 200MB) → write-амплификация
    // на single-writer SQLite. Реальный pending-drain capped LIMIT 50, так что
    // 256 — щедрый потолок для legitimate ack batch.
    const MAX_SYNC_BATCH: usize = 256;
    if req.applied_commands.len() > MAX_SYNC_BATCH || req.acks.len() > MAX_SYNC_BATCH {
        return Err(ApiError::BadRequest(format!(
            "applied_commands/acks too large (max {MAX_SYNC_BATCH} each)"
        )));
    }

    // v0.17 sliding refresh: продлеваем активную device-session чтобы
    // месяц+ offline scenarios «just work». См. KDoc refresh_if_aging_for_subject.
    // Result игнорируем — false (нет refresh'а) это норма для свежих сессий.
    if let Err(e) = session::refresh_if_aging_for_subject(
        &state.db,
        crate::session::KIND_DEVICE,
        device.id,
        DEVICE_TOKEN_TTL_SECS,
        SESSION_REFRESH_THRESHOLD_PCT,
    )
    .await
    {
        tracing::warn!(device_id = device.id, error = %e, "session sliding refresh failed (non-fatal)");
    }

    sqlx::query(
        "UPDATE devices SET \
            battery_pct      = COALESCE(?, battery_pct), \
            last_lat         = COALESCE(?, last_lat), \
            last_lon         = COALESCE(?, last_lon), \
            os_version       = COALESCE(?, os_version), \
            app_version      = COALESCE(?, app_version), \
            app_version_code = COALESCE(?, app_version_code), \
            last_seen_at     = datetime('now'), \
            is_online        = 1, \
            updated_at       = datetime('now') \
         WHERE id = ?",
    )
    .bind(req.battery_pct)
    .bind(req.last_lat)
    .bind(req.last_lon)
    .bind(&req.os_version)
    .bind(&req.app_version)
    .bind(req.app_version_code)
    .bind(device.id)
    .execute(&state.db)
    .await?;

    // v0.13: store ModelPreferences snapshot if client sent fresh state.
    if let (Some(version), Some(state_json)) = (req.state_version, req.current_state.as_ref()) {
        let json_str = serde_json::to_string(state_json).unwrap_or_else(|_| "{}".to_string());
        sqlx::query(
            "UPDATE devices SET \
                current_state_json    = ?, \
                current_state_version = ?, \
                current_state_seen_at = datetime('now') \
             WHERE id = ? AND ? > current_state_version",
        )
        .bind(json_str)
        .bind(version)
        .bind(device.id)
        .bind(version)
        .execute(&state.db)
        .await?;
    }

    // v0.13: process applied_commands outcomes (rc42 b37+ clients send these).
    for ac in &req.applied_commands {
        // Client может слать id как UUID-строку или legacy integer-as-string.
        // Поддерживаем integer parse fallback'ом; UUID игнорируется т.к. в текущей
        // схеме push_messages.id INTEGER. Когда добавим uuid column — расширим.
        let Ok(ack_id) = ac.id.parse::<i64>() else {
            tracing::warn!(
                device_id = device.id,
                cmd_id = %ac.id,
                "skip applied_command — non-integer id (UUID schema TBD)"
            );
            continue;
        };
        let new_status = if ac.status == "ok" {
            "delivered"
        } else {
            "failed"
        };
        sqlx::query(
            "UPDATE push_messages \
             SET status = ?, last_error = ?, delivered_at = datetime('now') \
             WHERE id = ? AND device_id = ? AND status IN ('pending','sent')",
        )
        .bind(new_status)
        .bind(ac.message.clone().unwrap_or_default())
        .bind(ack_id)
        .bind(device.id)
        .execute(&state.db)
        .await?;
    }

    // Mark acked commands as delivered (scoped to this device).
    // v0.13: acks теперь Vec<String>; parse через i64::from_str (legacy
    // integer ids). UUID-only acks ignore-with-warn — uuid column в roadmap.
    for ack_id_str in &req.acks {
        let Ok(ack_id) = ack_id_str.parse::<i64>() else {
            tracing::warn!(
                device_id = device.id,
                cmd_id = %ack_id_str,
                "skip ack — non-integer id"
            );
            continue;
        };
        sqlx::query(
            "UPDATE push_messages \
             SET status = 'delivered', delivered_at = datetime('now') \
             WHERE id = ? AND device_id = ? AND status IN ('pending','sent')",
        )
        .bind(ack_id)
        .bind(device.id)
        .execute(&state.db)
        .await?;
    }

    // Drain pending commands.
    let mut raw_commands: Vec<SyncCommandRow> =
        fetch_pending_for_device(&state.db, device.id).await?;

    // v0.17 long-polling: если client попросил подождать и pending пуст,
    // poll'им каждые 2 сек до wait_ms (cap 30s) либо до появления push'а.
    // Это даёт sub-30s latency для admin push'ей без перехода на FCM/WebSocket.
    let wait_ms = q
        .wait_for_command_ms
        .unwrap_or(0)
        .min(LONG_POLL_MAX_MS);
    if raw_commands.is_empty() && wait_ms > 0 {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms);
        loop {
            // Sleep либо до tick, либо до deadline — что наступит раньше.
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            let remaining = deadline.duration_since(now);
            let tick = Duration::from_millis(LONG_POLL_TICK_MS).min(remaining);
            tokio::time::sleep(tick).await;
            raw_commands = fetch_pending_for_device(&state.db, device.id).await?;
            if !raw_commands.is_empty() {
                break;
            }
        }
    }

    for c in &raw_commands {
        sqlx::query(
            "UPDATE push_messages SET status = 'sent', sent_at = datetime('now') WHERE id = ?",
        )
        .bind(c.id)
        .execute(&state.db)
        .await?;
    }
    let commands: Vec<SyncCommand> = raw_commands
        .into_iter()
        .map(|r| SyncCommand {
            id: r.id.to_string(),
            command: r.command,
            payload_json: r.payload_json,
        })
        .collect();

    // ---------- v0.12 Tier-2: APK rollout policy resolution ----------------
    // Решаем какую версию устройство должно держать:
    //   1. devices.pinned_version_id (per-device pin admin'ом) → ровно её.
    //   2. иначе latest application_rollouts с phase='fleet' для applications
    //      этого customer_id → fleet-wide раскатка.
    //   3. иначе самый свежий application_rollouts с phase='canary' где
    //      устройство в group_id.
    //
    // Если получившийся target.version_code > device.app_version_code (известный
    // нам после /v1/sync с rc42 b37+) — отдаём update_available.
    let update_available =
        resolve_update_for_device(&state.db, device.id, device.customer_id).await;

    // ---------- v0.18.19: bundles ETag hint (INSIGHT-055 §7.1.4) -----------
    // Cheap sha256 hash от canonical concat effective bundles этого device'а.
    // Client сравнивает с локально-сохранённым; equal → skip GET /device/bundles.
    let bundles_etag = compute_bundles_etag(&state.db, device.id, device.customer_id)
        .await
        .ok();

    Ok(Json(SyncResponse {
        commands,
        server_time: Utc::now(),
        update_available,
        bundles_etag,
    }))
}

/// v0.18.19 (per INSIGHT-055 §7.1.4): compute ETag-style hint от effective
/// bundles этого device'а. SHA-256 hex от canonical, length-prefixed
/// сериализации отсортированного set'а.
///
/// v0.18.20 (security review CRYPTO-1): сериализация теперь length-prefixed,
/// а не concat через in-band разделители `:`/`\n`. `bundle_id` не запрещает
/// эти байты, поэтому raw-concat был НЕ инъективен (две разных конфигурации
/// → один hash → device пропустил бы реальный update). Для каждого entry,
/// в порядке сортировки по `(bundle_id, source)` (`text` — НЕ doctest):
///
/// ```text
/// u32_le(len bundle_id)   ‖ bundle_id_bytes
/// u32_le(len source)      ‖ source_bytes
/// i64_le(priority)
/// u32_le(len assigned_at) ‖ assigned_at_bytes
/// ```
///
/// Сортировка по `(bundle_id, source)` гарантирует одинаковый hash для
/// одинакового set'а независимо от порядка query'я. Пустой set → SHA-256 от
/// пустого входа.
///
/// Returns `"sha256:<hex>"` (с префиксом — для будущей миграции на
/// другие hash'и). При DB error возвращает `Err` (caller treats as `None`).
async fn compute_bundles_etag(
    pool: &sqlx::SqlitePool,
    device_id: i64,
    customer_id: i64,
) -> Result<String, sqlx::Error> {
    use sha2::{Digest, Sha256};

    // Union: device + groups + customer. Same logic как
    // `routes::bundles::resolve_effective_bundles`, но возвращаем только
    // (bundle_id, source, priority, assigned_at) — без full EffectiveBundle DTO.
    let device_rows: Vec<(String, i64, String)> = sqlx::query_as(
        "SELECT bundle_id, priority, assigned_at FROM bundle_assignments \
         WHERE customer_id = ? AND target_type = 'device' AND target_id = ?",
    )
    .bind(customer_id)
    .bind(device_id)
    .fetch_all(pool)
    .await?;

    let group_rows: Vec<(String, i64, String)> = sqlx::query_as(
        "SELECT ba.bundle_id, ba.priority, ba.assigned_at \
         FROM bundle_assignments ba \
         WHERE ba.customer_id = ? \
           AND ba.target_type = 'group' \
           AND ba.target_id IN (SELECT group_id FROM device_groups WHERE device_id = ?)",
    )
    .bind(customer_id)
    .bind(device_id)
    .fetch_all(pool)
    .await?;

    let customer_rows: Vec<(String, i64, String)> = sqlx::query_as(
        "SELECT bundle_id, priority, assigned_at FROM bundle_assignments \
         WHERE customer_id = ? AND target_type = 'customer' AND target_id = ?",
    )
    .bind(customer_id)
    .bind(customer_id)
    .fetch_all(pool)
    .await?;

    // Merge with same specificity rules: device > group > customer per bundle_id.
    use std::collections::HashMap;
    let mut effective: HashMap<String, (String, i64, String)> = HashMap::new();
    for (bid, prio, ts) in device_rows {
        effective
            .entry(bid.clone())
            .or_insert((String::from("device"), prio, ts));
    }
    for (bid, prio, ts) in group_rows {
        effective
            .entry(bid.clone())
            .or_insert((String::from("group"), prio, ts));
    }
    for (bid, prio, ts) in customer_rows {
        effective
            .entry(bid.clone())
            .or_insert((String::from("customer"), prio, ts));
    }

    // Canonical sort: by bundle_id ascending (lexicographic).
    let mut entries: Vec<(String, String, i64, String)> = effective
        .into_iter()
        .map(|(bid, (src, prio, ts))| (bid, src, prio, ts))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    let mut hasher = Sha256::new();
    for (bid, src, prio, ts) in &entries {
        // v0.18.20 (security review CRYPTO-1): length-prefix каждое
        // variable-length поле вместо in-band разделителей `:`/`\n`. bundle_id
        // не запрещает `:` и `\n`, поэтому raw-concat был НЕ инъективен —
        // два разных набора могли дать одинаковый hash → device пропустил бы
        // реальный update. Length-prefix (u32 LE) делает сериализацию
        // bijective. prio — fixed-width i64 LE.
        let bid_b = bid.as_bytes();
        let src_b = src.as_bytes();
        let ts_b = ts.as_bytes();
        hasher.update((bid_b.len() as u32).to_le_bytes());
        hasher.update(bid_b);
        hasher.update((src_b.len() as u32).to_le_bytes());
        hasher.update(src_b);
        hasher.update(prio.to_le_bytes());
        hasher.update((ts_b.len() as u32).to_le_bytes());
        hasher.update(ts_b);
    }
    let digest = hasher.finalize();
    Ok(format!("sha256:{:x}", digest))
}

#[derive(Debug, sqlx::FromRow)]
struct TargetRow {
    version_id: i64,
    version_code: i64,
    version_name: String,
    sha256: String,
    file_size_bytes: i64,
    source_url: Option<String>,
    source: String, // "pinned" | "fleet" | "canary"
}

/// Возвращает `Some(SyncUpdateAvailable)` если устройство должно обновиться,
/// `None` если оно on-target или target_version не определён.
///
/// Прозрачен к ошибкам БД: при любом упавшем lookup'е логгируем warn и
/// возвращаем `None` — sync-loop не должен ломаться из-за rollout-policy.
async fn resolve_update_for_device(
    pool: &sqlx::SqlitePool,
    device_id: i64,
    customer_id: i64,
) -> Option<SyncUpdateAvailable> {
    let target = pick_target_version(pool, device_id, customer_id).await.ok()??;
    // Узнаём текущую версию устройства из БД (свежий UPDATE на pre-step).
    let cur_code: Option<i64> =
        sqlx::query_scalar("SELECT app_version_code FROM devices WHERE id = ?")
            .bind(device_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    let device_code = cur_code.unwrap_or(0);
    if target.version_code <= device_code {
        return None;
    }
    // Без source_url мы не знаем, откуда устройству качать. Tier-2.5 добавит
    // MDM-local signed URL; пока выдаём только для watcher-discovered rows.
    let url = target.source_url.clone()?;
    Some(SyncUpdateAvailable {
        version_code: target.version_code,
        version_name: target.version_name,
        sha256: target.sha256,
        size_bytes: target.file_size_bytes,
        url,
        source: match target.source.as_str() {
            "pinned" => "pinned",
            "fleet" => "fleet",
            _ => "canary",
        },
    })
}

/// Шаги lookup'а в порядке приоритета. Возвращает None если ни одна policy
/// не применима к этому устройству.
async fn pick_target_version(
    pool: &sqlx::SqlitePool,
    device_id: i64,
    customer_id: i64,
) -> Result<Option<TargetRow>, sqlx::Error> {
    // 1. per-device pin: devices.pinned_version_id
    let pinned: Option<TargetRow> = sqlx::query_as(
        "SELECT av.id AS version_id, av.version_code, av.version_name, av.sha256, \
                av.file_size_bytes, av.source_url, 'pinned' AS source \
         FROM devices d \
         JOIN application_versions av ON av.id = d.pinned_version_id \
         WHERE d.id = ?",
    )
    .bind(device_id)
    .fetch_optional(pool)
    .await?;
    if pinned.is_some() {
        return Ok(pinned);
    }
    // 2. fleet rollout — латест по created_at (для одного application
    //    одновременно валидной должна быть одна fleet-роллаут).
    let fleet: Option<TargetRow> = sqlx::query_as(
        "SELECT av.id AS version_id, av.version_code, av.version_name, av.sha256, \
                av.file_size_bytes, av.source_url, 'fleet' AS source \
         FROM application_rollouts r \
         JOIN application_versions av ON av.id = r.target_version_id \
         JOIN applications a ON a.id = r.application_id \
         WHERE r.phase = 'fleet' AND a.customer_id = ? \
         ORDER BY r.created_at DESC LIMIT 1",
    )
    .bind(customer_id)
    .fetch_optional(pool)
    .await?;
    if fleet.is_some() {
        return Ok(fleet);
    }
    // 3. canary rollout — устройство в group_id.
    let canary: Option<TargetRow> = sqlx::query_as(
        "SELECT av.id AS version_id, av.version_code, av.version_name, av.sha256, \
                av.file_size_bytes, av.source_url, 'canary' AS source \
         FROM application_rollouts r \
         JOIN application_versions av ON av.id = r.target_version_id \
         JOIN applications a ON a.id = r.application_id \
         JOIN device_groups dg ON dg.group_id = r.group_id \
         WHERE r.phase = 'canary' AND dg.device_id = ? AND a.customer_id = ? \
         ORDER BY r.created_at DESC LIMIT 1",
    )
    .bind(device_id)
    .bind(customer_id)
    .fetch_optional(pool)
    .await?;
    Ok(canary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enroll_response_omits_cloudru_when_disabled() {
        let resp = EnrollResponse {
            device_token: "TKN".into(),
            expires_in: DEVICE_TOKEN_TTL_SECS,
            device_id: 1,
            customer_id: 1,
            device_pubkey_acknowledged: false,
            cloudru_credentials: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        // skip_serializing_if = Option::is_none → ключа в JSON быть не должно.
        assert!(
            !json.contains("cloudru_credentials"),
            "cloudru_credentials must be omitted when None, got: {json}"
        );
        // device_pubkey_acknowledged тоже skip'нется так как Not::not.
        assert!(!json.contains("device_pubkey_acknowledged"), "got: {json}");
    }

    #[test]
    fn enroll_response_includes_cloudru_when_enabled() {
        let resp = EnrollResponse {
            device_token: "TKN".into(),
            expires_in: DEVICE_TOKEN_TTL_SECS,
            device_id: 1,
            customer_id: 1,
            device_pubkey_acknowledged: true,
            cloudru_credentials: Some(CloudruCredentials {
                tenant_id: "tenant-uuid".into(),
                key_id: "akid".into(),
                secret: "secret-bytes".into(),
            }),
        };
        let v: serde_json::Value = serde_json::to_value(&resp).unwrap();
        let creds = &v["cloudru_credentials"];
        assert_eq!(creds["tenant_id"], "tenant-uuid");
        assert_eq!(creds["key_id"], "akid");
        assert_eq!(creds["secret"], "secret-bytes");
        assert_eq!(v["device_pubkey_acknowledged"], true);
    }

    // v0.18.19 — bundles_etag hint в SyncResponse (per INSIGHT-055 §7.1.4).

    #[tokio::test]
    async fn bundles_etag_empty_when_no_assignments() {
        let pool = crate::db::open_pool(":memory:").await.unwrap();
        // Seed minimal customer + device. customer_id=1 уже есть seed'нут в migrations.
        sqlx::query(
            "INSERT INTO devices (customer_id, serial, is_enrolled) VALUES (1, 'TEST', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let device_id: i64 = sqlx::query_scalar("SELECT id FROM devices WHERE serial='TEST'")
            .fetch_one(&pool)
            .await
            .unwrap();

        let etag = compute_bundles_etag(&pool, device_id, 1).await.unwrap();
        // Empty effective set → sha256 от пустой строки (e3b0c4...).
        assert_eq!(
            etag,
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[tokio::test]
    async fn bundles_etag_stable_for_same_assignments() {
        let pool = crate::db::open_pool(":memory:").await.unwrap();
        sqlx::query(
            "INSERT INTO devices (customer_id, serial, is_enrolled) VALUES (1, 'TEST', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let device_id: i64 = sqlx::query_scalar("SELECT id FROM devices WHERE serial='TEST'")
            .fetch_one(&pool)
            .await
            .unwrap();

        sqlx::query(
            "INSERT INTO bundle_assignments(customer_id, bundle_id, target_type, target_id, \
                                            priority, assigned_at) \
             VALUES (1, 'soldier-v31', 'device', ?, 100, '2026-06-04T10:00:00Z')",
        )
        .bind(device_id)
        .execute(&pool)
        .await
        .unwrap();

        let etag1 = compute_bundles_etag(&pool, device_id, 1).await.unwrap();
        let etag2 = compute_bundles_etag(&pool, device_id, 1).await.unwrap();
        assert_eq!(etag1, etag2, "etag must be stable for unchanged state");
        assert!(etag1.starts_with("sha256:"));
    }

    #[tokio::test]
    async fn bundles_etag_changes_on_new_assignment() {
        let pool = crate::db::open_pool(":memory:").await.unwrap();
        sqlx::query(
            "INSERT INTO devices (customer_id, serial, is_enrolled) VALUES (1, 'TEST', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let device_id: i64 = sqlx::query_scalar("SELECT id FROM devices WHERE serial='TEST'")
            .fetch_one(&pool)
            .await
            .unwrap();

        let etag_before = compute_bundles_etag(&pool, device_id, 1).await.unwrap();
        sqlx::query(
            "INSERT INTO bundle_assignments(customer_id, bundle_id, target_type, target_id, \
                                            priority, assigned_at) \
             VALUES (1, 'minimum', 'device', ?, 50, '2026-06-04T10:00:00Z')",
        )
        .bind(device_id)
        .execute(&pool)
        .await
        .unwrap();
        let etag_after = compute_bundles_etag(&pool, device_id, 1).await.unwrap();
        assert_ne!(
            etag_before, etag_after,
            "etag must change after new assignment"
        );
    }

    #[tokio::test]
    async fn bundles_etag_changes_on_delete() {
        let pool = crate::db::open_pool(":memory:").await.unwrap();
        sqlx::query(
            "INSERT INTO devices (customer_id, serial, is_enrolled) VALUES (1, 'TEST', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let device_id: i64 = sqlx::query_scalar("SELECT id FROM devices WHERE serial='TEST'")
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO bundle_assignments(customer_id, bundle_id, target_type, target_id, \
                                            priority, assigned_at) \
             VALUES (1, 'soldier-v31', 'device', ?, 100, '2026-06-04T10:00:00Z')",
        )
        .bind(device_id)
        .execute(&pool)
        .await
        .unwrap();

        let etag_with = compute_bundles_etag(&pool, device_id, 1).await.unwrap();
        sqlx::query("DELETE FROM bundle_assignments WHERE bundle_id = 'soldier-v31'")
            .execute(&pool)
            .await
            .unwrap();
        let etag_without = compute_bundles_etag(&pool, device_id, 1).await.unwrap();
        assert_ne!(
            etag_with, etag_without,
            "etag must change after assignment delete"
        );
    }

    #[tokio::test]
    async fn bundles_etag_changes_on_priority_update() {
        let pool = crate::db::open_pool(":memory:").await.unwrap();
        sqlx::query(
            "INSERT INTO devices (customer_id, serial, is_enrolled) VALUES (1, 'TEST', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let device_id: i64 = sqlx::query_scalar("SELECT id FROM devices WHERE serial='TEST'")
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO bundle_assignments(customer_id, bundle_id, target_type, target_id, \
                                            priority, assigned_at) \
             VALUES (1, 'soldier-v31', 'device', ?, 100, '2026-06-04T10:00:00Z')",
        )
        .bind(device_id)
        .execute(&pool)
        .await
        .unwrap();

        let etag_low = compute_bundles_etag(&pool, device_id, 1).await.unwrap();
        sqlx::query(
            "UPDATE bundle_assignments SET priority = 200 WHERE bundle_id = 'soldier-v31'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let etag_high = compute_bundles_etag(&pool, device_id, 1).await.unwrap();
        assert_ne!(
            etag_low, etag_high,
            "etag must change after priority update (affects tie-break ordering)"
        );
    }
}
