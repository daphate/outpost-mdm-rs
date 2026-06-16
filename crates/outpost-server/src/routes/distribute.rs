//! Per-device encrypted file distribution endpoints.
//!
//! See `tactical-ar-hud/tools/MDM-DEVICE-CONTROL-CONTRACT.md` §2.
//!
//! Admin flow:
//!   1. `POST /api/v1/files/{file_id}/distribute` — admin указывает target
//!      (device | group | customer_fleet), kind, filename, optional expires_at.
//!      Server читает uploaded_files.file_path, encrypt'ит для каждого
//!      получателя, кладёт ciphertext в `$APP_FILES_DIR/encrypted/<dist_id>.bin`,
//!      создаёт N push_messages с `command='fetch-encrypted-file'`.
//!
//! Device flow:
//!   1. На /sync устройство получает command с payload (eph_pubkey, wrapped_dek,
//!      ciphertext_url, etc.).
//!   2. Клиент GET /api/v1/encrypted-distributions/{id}/blob с Bearer device_token
//!      — server проверяет что distribution.recipient_device_id == authed device,
//!      отдаёт raw ciphertext bytes.
//!   3. Клиент ECDH+HKDF unwrap DEK, AES-GCM decrypt, install в PdfStorage/ZIM/etc.
//!   4. На следующем /sync шлёт applied_commands[id, status=ok|error].

use crate::auth_extract::{AuthDevice, AuthUser};
use crate::distribution;
use crate::error::ApiError;
use crate::permission::require_permission;
use crate::state::AppState;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const MIN_VERSION_CODE_FOR_DISTRIBUTION: i64 = 178;

/// rc42 b37+ minimum (same as update-config gate). Старые клиенты не имеют
/// `fetch-encrypted-file` handler в SyncCommandDispatcher.
const ALLOWED_KINDS: &[&str] = &[
    "pdf",
    "zim",
    "knowledge_db_chunk",
    "model_gguf",
    "arbitrary_blob",
];

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/files/{file_id}/distribute", post(distribute_file))
        // Device-authenticated endpoint — отдаёт ciphertext blob recipient'у.
        .route("/api/v1/encrypted-distributions/{id}/blob", get(fetch_blob))
}

#[derive(Debug, Deserialize)]
pub struct DistributeRequest {
    pub target: DistributeTarget,
    pub filename: String,
    pub kind: String,
    pub expires_at: Option<String>,
    /// Опциональная заметка для admin audit (не передаётся клиенту).
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DistributeTarget {
    Device { id: i64 },
    Group { id: i64 },
    CustomerFleet,
}

/// Untyped variant — used by web form handler в `routes/web.rs` для
/// translation `serde_json::Value` (built from form data) в typed request.
/// Avoids duplicating logic между JSON и form route'ами.
#[derive(Debug, Deserialize)]
pub struct DistributeRequestRaw {
    pub target: serde_json::Value,
    pub filename: String,
    pub kind: String,
    pub expires_at: Option<String>,
    pub notes: Option<String>,
}

/// Минимальный actor-snapshot для разделяемого core-handler'а.
/// Web (WebUser) и JSON (AuthUser) handler'ы заполняют этот struct и
/// передают в [`do_distribute_file`]. Это позволяет не дублировать 200+
/// строк encrypt-pipeline'а между двумя entry-point'ами.
#[derive(Debug, Clone, Copy)]
pub struct DistributeActor {
    pub user_id: i64,
    pub customer_id: i64,
    pub role_id: i64,
}

impl From<&crate::routes::web::WebUser> for DistributeActor {
    fn from(u: &crate::routes::web::WebUser) -> Self {
        Self {
            user_id: u.id,
            customer_id: u.customer_id,
            role_id: u.role_id,
        }
    }
}

impl From<crate::routes::web::WebUser> for DistributeActor {
    fn from(u: crate::routes::web::WebUser) -> Self {
        Self::from(&u)
    }
}

impl From<&crate::auth_extract::AuthUser> for DistributeActor {
    fn from(u: &crate::auth_extract::AuthUser) -> Self {
        Self {
            user_id: u.id,
            customer_id: u.customer_id,
            role_id: u.role_id,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DistributeResponse {
    pub recipient_count: i64,
    pub eligible_count: i64,
    pub skipped_no_pubkey: i64,
    pub skipped_old_clients: i64,
    pub command_ids: Vec<i64>,
    pub distribution_ids: Vec<i64>,
    pub ciphertext_size: i64,
    pub plaintext_size: i64,
}

/// `POST /api/v1/files/{file_id}/distribute` — encrypt a file for one
/// (device), N (group), or the whole customer-fleet.
///
/// Sync (blocking) implementation for now — для blob ≤ 200 MB × до 200
/// recipient'ов это занимает несколько секунд CPU. Если флот вырастет —
/// перевести в background task с progress reporting (см. MDM-DEVICE-CONTROL-
/// CONTRACT.md §6 Open question 1).
async fn distribute_file(
    user: AuthUser,
    State(state): State<AppState>,
    Path(file_id): Path<i64>,
    Json(req): Json<DistributeRequest>,
) -> Result<(StatusCode, Json<DistributeResponse>), ApiError> {
    let actor: DistributeActor = (&user).into();
    let raw = DistributeRequestRaw {
        target: serde_json::to_value(&req.target).unwrap_or(serde_json::Value::Null),
        filename: req.filename,
        kind: req.kind,
        expires_at: req.expires_at,
        notes: req.notes,
    };
    let resp = do_distribute_file(&state, &actor, file_id, raw).await?;
    Ok((StatusCode::ACCEPTED, Json(resp)))
}

// Re-export DistributeTarget Serialize чтобы to_value работало.
impl serde::Serialize for DistributeTarget {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(Some(2))?;
        match self {
            DistributeTarget::Device { id } => {
                m.serialize_entry("type", "device")?;
                m.serialize_entry("id", id)?;
            }
            DistributeTarget::Group { id } => {
                m.serialize_entry("type", "group")?;
                m.serialize_entry("id", id)?;
            }
            DistributeTarget::CustomerFleet => {
                m.serialize_entry("type", "customer_fleet")?;
            }
        }
        m.end()
    }
}

/// Shared core. Web и JSON handler'ы оба вызывают этот fn — отличие только
/// в источнике user identity (WebUser vs AuthUser) и в shape входа
/// (JSON typed vs form-encoded translated в `serde_json::Value`).
pub async fn do_distribute_file(
    state: &AppState,
    actor: &DistributeActor,
    file_id: i64,
    req: DistributeRequestRaw,
) -> Result<DistributeResponse, ApiError> {
    require_permission(&state.db, actor.role_id, "files.write").await?;

    if !ALLOWED_KINDS.contains(&req.kind.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "unknown kind '{}'; allowed: {}",
            req.kind,
            ALLOWED_KINDS.join(", ")
        )));
    }
    if req.filename.trim().is_empty() {
        return Err(ApiError::BadRequest("filename is required".into()));
    }
    // Распарсить target из generic JSON value.
    let target: DistributeTarget = serde_json::from_value(req.target.clone())
        .map_err(|e| ApiError::BadRequest(format!("invalid target: {e}")))?;

    // 1. Загрузить uploaded_files row + plaintext bytes.
    let file_row: Option<(String, String)> = sqlx::query_as(
        "SELECT file_path, original_name FROM uploaded_files \
         WHERE id = ? AND customer_id = ?",
    )
    .bind(file_id)
    .bind(actor.customer_id)
    .fetch_optional(&state.db)
    .await?;
    let Some((file_path, _orig_name)) = file_row else {
        return Err(ApiError::NotFound);
    };
    let plaintext_path: PathBuf = state.app_files_dir.join(&file_path);
    let plaintext = tokio::fs::read(&plaintext_path).await.map_err(|e| {
        tracing::error!(error = %e, "read uploaded_files plaintext failed");
        ApiError::Internal
    })?;
    if plaintext.is_empty() {
        return Err(ApiError::BadRequest("uploaded file is empty".into()));
    }

    // 2. Resolve recipients (device_id, pubkey_der_bytes, key_id, version_code).
    let recipients = resolve_recipients(state, actor.customer_id, &target).await?;
    let recipient_count = recipients.total as i64;
    if recipient_count == 0 {
        return Err(ApiError::BadRequest("no devices in target".into()));
    }

    // 3. Encrypt blob once (random DEK + IV). Wrap DEK per-recipient.
    let (blob, dek) = distribution::encrypt_blob(&plaintext)
        .map_err(|e| ApiError::BadRequest(format!("encrypt_blob: {e}")))?;

    // 4. Persist blob to local disk under $APP_FILES_DIR/encrypted/<random_id>.bin.
    //    Используем sha256 ciphertext как имя — детерминированно для дедупа.
    let blob_dir = state.app_files_dir.join("encrypted");
    tokio::fs::create_dir_all(&blob_dir).await.map_err(|e| {
        tracing::error!(error = %e, "create encrypted dir failed");
        ApiError::Internal
    })?;
    let blob_filename = format!("{}.bin", blob.ciphertext_sha256_hex);
    let blob_path = blob_dir.join(&blob_filename);
    if !blob_path.exists() {
        tokio::fs::write(&blob_path, &blob.ciphertext)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "write ciphertext blob failed");
                ApiError::Internal
            })?;
    }

    // 5. Per-recipient: encrypt_for_recipient + INSERT row + push_message.
    let mut command_ids = Vec::new();
    let mut distribution_ids = Vec::new();
    let mut skipped_no_pubkey = 0i64;
    let mut skipped_old = 0i64;
    let mut blob_url_template: Option<String> = None;
    for r in &recipients.items {
        if let Some(v) = r.app_version_code {
            if v < MIN_VERSION_CODE_FOR_DISTRIBUTION {
                skipped_old += 1;
                continue;
            }
        } else {
            skipped_old += 1;
            continue;
        }
        let pubkey_bytes = match &r.pubkey_der {
            Some(b) => b,
            None => {
                skipped_no_pubkey += 1;
                continue;
            }
        };
        let payload = distribution::encrypt_for_recipient(&dek, pubkey_bytes, file_id, r.device_id)
            .map_err(|e| ApiError::BadRequest(format!("encrypt_for_recipient: {e}")))?;

        // Транзакция: INSERT encrypted_distributions → INSERT push_messages →
        // обновить distribution.push_message_id.
        let mut tx = state.db.begin().await?;

        // Сначала вставляем distribution с placeholder ciphertext_url; узнаём id.
        let dist_id: i64 = sqlx::query_scalar(
            "INSERT INTO encrypted_distributions \
                (customer_id, file_id, filename, kind, \
                 recipient_device_id, recipient_key_id, \
                 ciphertext_url, ciphertext_size, ciphertext_sha256, \
                 ciphertext_iv, ciphertext_tag, \
                 plaintext_sha256, plaintext_size, \
                 eph_pubkey_der, wrapped_dek, wrapped_dek_iv, \
                 expires_at) \
             VALUES (?, ?, ?, ?, ?, ?, '', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             RETURNING id",
        )
        .bind(actor.customer_id)
        .bind(file_id)
        .bind(&req.filename)
        .bind(&req.kind)
        .bind(r.device_id)
        .bind(&r.key_id)
        .bind(blob.ciphertext.len() as i64)
        .bind(&blob.ciphertext_sha256_hex)
        .bind(blob.iv.to_vec())
        .bind(blob.tag.to_vec())
        .bind(&blob.plaintext_sha256_hex)
        .bind(plaintext.len() as i64)
        .bind(&payload.eph_pubkey_sec1)
        .bind(&payload.wrapped_dek)
        .bind(payload.wrapped_dek_iv.to_vec())
        .bind(req.expires_at.as_deref())
        .fetch_one(&mut *tx)
        .await?;

        // Теперь известен id → строим ciphertext_url.
        let ciphertext_url = format!("/api/v1/encrypted-distributions/{dist_id}/blob");
        sqlx::query("UPDATE encrypted_distributions SET ciphertext_url = ? WHERE id = ?")
            .bind(&ciphertext_url)
            .bind(dist_id)
            .execute(&mut *tx)
            .await?;
        blob_url_template.get_or_insert(ciphertext_url.clone());

        // INSERT push_message с command='fetch-encrypted-file' и полным
        // self-contained payload (client дёргает blob_url отдельно).
        let payload_json = serde_json::json!({
            "distribution_id": dist_id,
            "file_id": file_id,
            "kind": req.kind,
            "filename": req.filename,
            "ciphertext_url": ciphertext_url,
            "ciphertext_size": blob.ciphertext.len(),
            "ciphertext_sha256": blob.ciphertext_sha256_hex,
            "ciphertext_iv": base64::engine::general_purpose::STANDARD.encode(blob.iv),
            "ciphertext_tag": base64::engine::general_purpose::STANDARD.encode(blob.tag),
            "plaintext_sha256": blob.plaintext_sha256_hex,
            "plaintext_size": plaintext.len(),
            "recipient_device_id": r.device_id,
            "recipient_key_id": r.key_id,
            "eph_pubkey_der": base64::engine::general_purpose::STANDARD.encode(&payload.eph_pubkey_sec1),
            "wrapped_dek_b64": base64::engine::general_purpose::STANDARD.encode(&payload.wrapped_dek),
            "wrapped_dek_iv": base64::engine::general_purpose::STANDARD.encode(payload.wrapped_dek_iv),
            "expires_at": req.expires_at,
        })
        .to_string();
        let cmd_id: i64 = sqlx::query_scalar(
            "INSERT INTO push_messages (customer_id, device_id, command, payload_json, status) \
             VALUES (?, ?, 'fetch-encrypted-file', ?, 'pending') RETURNING id",
        )
        .bind(actor.customer_id)
        .bind(r.device_id)
        .bind(&payload_json)
        .fetch_one(&mut *tx)
        .await?;

        // Backfill push_message_id.
        sqlx::query("UPDATE encrypted_distributions SET push_message_id = ? WHERE id = ?")
            .bind(cmd_id)
            .bind(dist_id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;

        command_ids.push(cmd_id);
        distribution_ids.push(dist_id);
    }

    let eligible_count = command_ids.len() as i64;

    tracing::info!(
        actor_user = actor.user_id,
        file_id,
        kind = %req.kind,
        recipient_count,
        eligible_count,
        skipped_no_pubkey,
        skipped_old,
        ciphertext_size = blob.ciphertext.len(),
        "encrypted distribution"
    );

    Ok(DistributeResponse {
        recipient_count,
        eligible_count,
        skipped_no_pubkey,
        skipped_old_clients: skipped_old,
        command_ids,
        distribution_ids,
        ciphertext_size: blob.ciphertext.len() as i64,
        plaintext_size: plaintext.len() as i64,
    })
}

#[derive(Debug)]
struct ResolvedRecipient {
    device_id: i64,
    app_version_code: Option<i64>,
    pubkey_der: Option<Vec<u8>>,
    key_id: String,
}

#[derive(Debug)]
struct ResolvedRecipientList {
    total: usize,
    items: Vec<ResolvedRecipient>,
}

async fn resolve_recipients(
    state: &AppState,
    customer_id: i64,
    target: &DistributeTarget,
) -> Result<ResolvedRecipientList, ApiError> {
    #[allow(clippy::type_complexity)] // local row tuple; a named type alias would not aid clarity
    let raw: Vec<(i64, Option<i64>, Option<Vec<u8>>, Option<String>)> = match target {
        DistributeTarget::Device { id } => {
            sqlx::query_as(
                "SELECT d.id, d.app_version_code, dk.pubkey_der, dk.key_id \
                 FROM devices d \
                 LEFT JOIN device_keys dk ON dk.device_id = d.id AND dk.revoked_at IS NULL \
                 WHERE d.id = ? AND d.customer_id = ?",
            )
            .bind(id)
            .bind(customer_id)
            .fetch_all(&state.db)
            .await?
        }
        DistributeTarget::Group { id } => {
            sqlx::query_as(
                "SELECT d.id, d.app_version_code, dk.pubkey_der, dk.key_id \
                 FROM devices d \
                 JOIN device_groups dg ON dg.device_id = d.id \
                 LEFT JOIN device_keys dk ON dk.device_id = d.id AND dk.revoked_at IS NULL \
                 WHERE dg.group_id = ? AND d.customer_id = ?",
            )
            .bind(id)
            .bind(customer_id)
            .fetch_all(&state.db)
            .await?
        }
        DistributeTarget::CustomerFleet => {
            sqlx::query_as(
                "SELECT d.id, d.app_version_code, dk.pubkey_der, dk.key_id \
                 FROM devices d \
                 LEFT JOIN device_keys dk ON dk.device_id = d.id AND dk.revoked_at IS NULL \
                 WHERE d.customer_id = ?",
            )
            .bind(customer_id)
            .fetch_all(&state.db)
            .await?
        }
    };

    let total = raw.len();
    let items: Vec<ResolvedRecipient> = raw
        .into_iter()
        .map(
            |(device_id, app_version_code, pubkey_der, key_id)| ResolvedRecipient {
                device_id,
                app_version_code,
                pubkey_der,
                key_id: key_id.unwrap_or_default(),
            },
        )
        .collect();
    Ok(ResolvedRecipientList { total, items })
}

/// `GET /api/v1/encrypted-distributions/{id}/blob` — device-authenticated.
/// Авторизованный device может скачать ciphertext только своей строки;
/// чужая строка (даже sibling-device в своём же тенанте) → 404, не 403.
async fn fetch_blob(
    device: AuthDevice,
    State(state): State<AppState>,
    Path(dist_id): Path<i64>,
) -> Result<Response, ApiError> {
    let row: Option<(i64, String, i64)> = sqlx::query_as(
        "SELECT recipient_device_id, ciphertext_sha256, ciphertext_size \
         FROM encrypted_distributions \
         WHERE id = ? AND customer_id = ? AND purged_at IS NULL",
    )
    .bind(dist_id)
    .bind(device.customer_id)
    .fetch_optional(&state.db)
    .await?;
    let Some((recipient_id, sha, size)) = row else {
        return Err(ApiError::NotFound);
    };
    if recipient_id != device.id {
        // v0.18.20 (security review LEAK — продолжение LEAK-2): 404, не 403.
        // dist_id — INTEGER AUTOINCREMENT (dense global space). 403-vs-404
        // для строки, адресованной sibling-device в том же тенанте, —
        // existence-oracle по чужим distribution id. Consistency с
        // load_entity_row / ballistics put/delete: «exists-but-not-yours → 404».
        return Err(ApiError::NotFound);
    }

    let blob_path = state
        .app_files_dir
        .join("encrypted")
        .join(format!("{sha}.bin"));
    let bytes = tokio::fs::read(&blob_path).await.map_err(|e| {
        tracing::error!(error = %e, %sha, "blob file missing on disk");
        ApiError::NotFound
    })?;
    if (bytes.len() as i64) != size {
        tracing::warn!(
            on_disk = bytes.len(),
            recorded = size,
            "blob size mismatch — possible corruption"
        );
    }

    let body = Bytes::from(bytes);
    let mut resp = body.into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    Ok(resp)
}
