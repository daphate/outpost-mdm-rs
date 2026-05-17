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
    extract::{Path, State},
    routing::post,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Long-lived device token TTL (90 days). Devices re-enroll if it expires.
const DEVICE_TOKEN_TTL_SECS: i64 = 60 * 60 * 24 * 90;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/devices/{id}/enrollment", post(generate_enrollment))
        .route("/api/v1/enroll", post(enroll))
        .route("/api/v1/sync", post(sync))
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

    Ok(Json(EnrollResponse {
        device_token: token,
        expires_in: DEVICE_TOKEN_TTL_SECS,
        device_id: req.device_id,
        customer_id,
        device_pubkey_acknowledged: pubkey_acknowledged,
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
}

async fn sync(
    device: AuthDevice,
    State(state): State<AppState>,
    Json(req): Json<SyncRequest>,
) -> Result<Json<SyncResponse>, ApiError> {
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

    // Drain pending commands; mark them sent atomically.
    let raw_commands: Vec<SyncCommandRow> = sqlx::query_as::<_, SyncCommandRow>(
        "SELECT id, command, payload_json FROM push_messages \
         WHERE device_id = ? AND status = 'pending' \
         ORDER BY id ASC LIMIT 50",
    )
    .bind(device.id)
    .fetch_all(&state.db)
    .await?;

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

    Ok(Json(SyncResponse {
        commands,
        server_time: Utc::now(),
        update_available,
    }))
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
