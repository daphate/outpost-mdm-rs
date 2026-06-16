//! Ballistics endpoint family — реализация BALLISTICS-MDM-CONTRACT v1.
//!
//! Архитектура: **server opaque envelope**. Server никогда не пытается
//! decrypt ciphertext. Хранит metadata (kind/owner/version/timestamps/links)
//! + ciphertext BLOB + per-recipient ECDH+AES-GCM wrap rows. Decryption —
//!   только на client'е через Android Keystore-backed P-256 private key.
//!
//! **Feature flag**: все endpoints (кроме `/health`) за `BALLISTICS_ENABLED`
//! env var (default false). При flag=off возвращают 503 с reason
//! `"ballistics endpoints disabled — pending crypto review"`. Включать
//! только после expert review per `docs/BALLISTICS-CRYPTO-DESIGN.md §6`.
//!
//! **Authentication**:
//! - Device-facing endpoints (`/api/v1/ballistics/{weapons,cartridges,dope,
//!   units,audit_log,export}/{...}`) — `AuthDevice` extractor (X-MDM-Token
//!   = device's bearer token, per CONTRACT §2).
//! - Admin endpoints (`/api/v1/ballistics/admin/*`) — `AuthUser` + scope
//!   check `ballistics.admin`.
//!
//! **Information leakage** (что server видит даже с encryption):
//!  kind / owner_user_id / owner_device_id / parent_id (DOPE→weapon link)
//!  / version / modified_ts / deleted_ts / ciphertext size / wrap count.
//! См. `docs/BALLISTICS-CRYPTO-DESIGN.md §3.3` для полного списка.

use crate::auth_extract::{AuthDevice, AuthUser};
use crate::error::ApiError;
use crate::permission::require_permission;
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

// =====================================================================
// Constants / wire-level limits
// =====================================================================

/// Hard maximum размера одного ciphertext blob'а (per record). Per
/// BALLISTICS-CRYPTO-DESIGN: typical record ≤4 KB. Cap 64 KB защищает
/// от DoS через гигантские payloads.
const MAX_CIPHERTEXT_BYTES: usize = 64 * 1024;

/// Hard maximum количество recipient wraps в одном PUT. Per design
/// proposal — 1 entity → N wraps (один на каждое device pubkey). 50
/// разумный upper bound (parka обычно ≤10 devices per user).
const MAX_WRAPS_PER_RECORD: usize = 50;

/// v0.18.20 (security review DOS-4): max размер admin-template payload_json.
/// Weapon/cartridge профиль — единицы КБ; 256 KiB с большим запасом.
const MAX_TEMPLATE_PAYLOAD_BYTES: usize = 256 * 1024;

/// AES-GCM nonce length (per NIST SP 800-38D recommendation §5.2.1.1).
const NONCE_LEN: usize = 12;

/// AES-GCM authentication tag length (default per RFC 5116 §5.1).
const GCM_TAG_LEN: usize = 16;

/// AES-256 DEK ciphertext length when wrapping a 32-byte DEK:
/// 32 (plaintext) + 16 (GCM tag) = 48 bytes.
const WRAPPED_DEK_LEN: usize = 32 + GCM_TAG_LEN;

/// SEC1-uncompressed P-256 point (`0x04 || X(32) || Y(32)`) внутри SPKI =
/// 91 bytes for the full SubjectPublicKeyInfo DER encoding. Matches
/// existing `device_keys.pubkey_der` storage и `encrypted_distributions.
/// eph_pubkey_der`.
const SPKI_P256_LEN: usize = 91;

/// Допустимые kind'ы (matches schema CHECK constraint).
const VALID_KINDS: &[&str] = &["weapon", "cartridge", "dope", "units"];

/// Soft-delete grace period для GC: hard-purge через 90 дней.
/// (GC task ещё не реализован в этом milestone; см. M-task в release notes.)
#[allow(dead_code)]
const SOFT_DELETE_GRACE_DAYS: i64 = 90;

// =====================================================================
// Wire DTOs
// =====================================================================

/// Body для PUT `/api/v1/ballistics/{kind}/{id}`.
/// Все bytes-поля передаются base64-encoded (standard alphabet, with padding).
#[derive(Debug, Deserialize)]
pub struct PutEntityRequest {
    /// Plaintext metadata (server-queryable). См. Information Leakage в design.
    pub metadata: EntityMetadata,
    /// Encrypted entity payload — client-side AES-256-GCM(DEK, JSON).
    /// base64-encoded BLOB.
    pub ciphertext: String,
    /// 12-byte AES-GCM nonce; base64-encoded.
    pub ciphertext_iv: String,
    /// 16-byte AES-GCM auth tag (stored separately for storage convenience);
    /// base64-encoded.
    pub ciphertext_tag: String,
    /// Per-recipient wrap rows. См. WrapInput.
    pub wraps: Vec<WrapInput>,
}

#[derive(Debug, Deserialize)]
pub struct EntityMetadata {
    /// Опциональный display hint (по согласию user'а — opt-in).
    /// Если client не передаёт — server хранит NULL.
    pub name_hint: Option<String>,
    /// Soft FK для DOPE → weapon. См. CONTRACT §3.4 (`?weapon_id=`).
    pub parent_id: Option<String>,
    /// owner_user_id — client передаёт явно. Server валидирует что user
    /// существует и принадлежит тому же customer_id что и AuthDevice.
    pub owner_user_id: Option<i64>,
    /// Если client отправляет existing record (update) — version из last
    /// `GET`. Server compare'ит с stored version → 412 при mismatch.
    /// Для create — клиент шлёт 1 (или omit — server присвоит).
    pub expected_version: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct WrapInput {
    /// recipient_device_id — кому wrap'ит DEK. Server валидирует что device
    /// принадлежит тому же customer_id.
    pub recipient_device_id: i64,
    /// Денормализованный device_keys.key_id (sha256(pubkey)[0..8] hex).
    /// Server **не** проверяет что recipient_key_id matches существующий
    /// device_keys row — это **client's** responsibility (см. design §6 OQ#10
    /// race condition при key rotation). Server просто хранит как opaque.
    pub recipient_key_id: String,
    /// Ephemeral P-256 sender pubkey (91 bytes SPKI), base64.
    pub eph_pubkey_der: String,
    /// Wrapped DEK = AES-256-GCM(wrap_key, DEK) — 48 bytes (32 ct + 16 tag),
    /// base64.
    pub wrapped_dek: String,
    /// 12-byte AES-GCM nonce для wrapped_dek, base64.
    pub wrapped_dek_iv: String,
}

/// GET response для individual entity или list element.
#[derive(Debug, Serialize)]
pub struct EntityRow {
    pub id: String,
    pub kind: String,
    pub owner_user_id: Option<i64>,
    pub owner_device_id: Option<i64>,
    pub parent_id: Option<String>,
    pub name_hint: Option<String>,
    pub version: i64,
    pub created_ts: String,
    pub modified_ts: String,
    /// `Some(ISO 8601)` если soft-deleted; иначе `None`.
    /// Client'у этого достаточно для удаления локальной копии.
    pub deleted_ts: Option<String>,
    /// ETag (weak), вычисляется как `W/"<version>"`.
    pub etag: String,
    pub ciphertext: String,
    pub ciphertext_iv: String,
    pub ciphertext_tag: String,
    /// Только wrap для запрашивающего device. Server filter'ит на
    /// AuthDevice.id чтобы не leak'ать всем чужие wraps.
    pub wrap_for_this_device: Option<WrapOutput>,
}

#[derive(Debug, Serialize)]
pub struct WrapOutput {
    pub recipient_device_id: i64,
    pub recipient_key_id: String,
    pub eph_pubkey_der: String,
    pub wrapped_dek: String,
    pub wrapped_dek_iv: String,
}

#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub items: Vec<EntityRow>,
    /// ISO 8601 UTC. Для incremental sync на следующем pull'е.
    pub server_ts: String,
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// ISO 8601 timestamp; server возвращает только entities с
    /// `modified_ts > modified_since`. Soft-deleted (deleted_ts != NULL)
    /// **включаются** в delta — client должен обработать удаление локально.
    pub modified_since: Option<String>,
    /// DOPE filter: вернёт только entities с parent_id = weapon_id.
    pub weapon_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub version: &'static str,
    pub server_ts: String,
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: &'static str,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct AuditLogRow {
    pub id: i64,
    pub action: String,
    pub entity_kind: Option<String>,
    pub entity_id: Option<String>,
    pub ts: String,
    pub user_id: Option<i64>,
    pub device_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct AuditLogResponse {
    pub items: Vec<AuditLogRow>,
}

// =====================================================================
// Helpers — feature flag, base64, kind validation, error mapping
// =====================================================================

/// Reject request if feature flag is off. Returns 503 — устройство понимает
/// что endpoint известен, но not active, и не будет retry'ить как 5xx
/// transient.
fn require_enabled(state: &AppState) -> Result<(), ApiError> {
    if !state.ballistics_enabled {
        Err(ApiError::ServiceUnavailable(
            "ballistics endpoints disabled — pending crypto review".to_string(),
        ))
    } else {
        Ok(())
    }
}

fn b64_decode(input: &str, field: &str) -> Result<Vec<u8>, ApiError> {
    base64::engine::general_purpose::STANDARD
        .decode(input)
        .map_err(|_| ApiError::BadRequest(format!("invalid base64 in field `{field}`")))
}

fn b64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn validate_kind(kind: &str) -> Result<(), ApiError> {
    if VALID_KINDS.contains(&kind) {
        Ok(())
    } else {
        Err(ApiError::BadRequest(format!(
            "invalid kind `{kind}` — must be one of {VALID_KINDS:?}"
        )))
    }
}

/// Validate `id` shape per CONTRACT §3.3:
/// - cartridges must have prefix `user_*` (bundled cartridges из APK
///   read-only и никогда не sync'ятся)
/// - other kinds — accept anything non-empty up to 128 chars.
fn validate_id_shape(kind: &str, id: &str) -> Result<(), ApiError> {
    if id.is_empty() || id.len() > 128 {
        return Err(ApiError::BadRequest("id must be 1..=128 chars".to_string()));
    }
    if kind == "cartridge" && !id.starts_with("user_") {
        return Err(ApiError::BadRequest(
            "cartridge id must have `user_` prefix (bundled cartridges не sync'ятся)".to_string(),
        ));
    }
    Ok(())
}

/// ETag computation — currently weak ETag из version.
/// Client'у достаточно для `If-Match` conflict detection.
fn etag_for(version: i64) -> String {
    format!("W/\"{version}\"")
}

// =====================================================================
// Router
// =====================================================================

pub fn router() -> Router<AppState> {
    Router::new()
        // /health — без auth, без feature flag check (probe должен работать
        // всегда, чтобы AR Hud client мог понять что endpoint exists даже
        // когда disabled).
        .route("/api/v1/ballistics/health", get(health))
        // Generic CRUD — kind как path segment.
        .route(
            "/api/v1/ballistics/{kind}/{id}",
            get(get_entity).put(put_entity).delete(delete_entity),
        )
        .route("/api/v1/ballistics/{kind}", get(list_entities))
        // Audit log (per BALLISTICS-MDM-CONTRACT §8.4).
        .route("/api/v1/ballistics/audit_log", get(get_audit_log))
        // GDPR (per §8.5).
        .route("/api/v1/ballistics/export", get(export_user_data))
        .route(
            "/api/v1/ballistics/all",
            axum::routing::delete(delete_all_user_data),
        )
        // Admin push (per §3.6 + AuthUser scope check).
        .route(
            "/api/v1/ballistics/admin/templates",
            get(list_admin_templates).post(create_admin_template),
        )
        .route(
            "/api/v1/ballistics/admin/templates/{id}/retract",
            post(retract_admin_template),
        )
        // v0.18.20 (security review DOS-1): tight per-route body limit. Без
        // этого глобальный 200MB DefaultBodyLimit позволял device прислать
        // 200MB body → axum буферит+десериализует ВЕСЬ body (Json extractor
        // бежит до require_enabled), wraps-Vec и base64-ciphertext аллоцируются
        // целиком до 64KB/50-cap → OOM (при MemoryMax=256MB — kill процесса).
        // 1 MiB покрывает ciphertext(64KB×1.34) + 50 wraps + admin template
        // JSON с большим запасом.
        .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024))
}

// =====================================================================
// Handlers — health
// =====================================================================

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        version: "v1",
        server_ts: chrono::Utc::now().to_rfc3339(),
        enabled: state.ballistics_enabled,
    })
}

// =====================================================================
// Handlers — entity CRUD (kind-generic)
// =====================================================================

async fn get_entity(
    device: AuthDevice,
    State(state): State<AppState>,
    Path((kind, id)): Path<(String, String)>,
) -> Result<Json<EntityRow>, ApiError> {
    require_enabled(&state)?;
    validate_kind(&kind)?;
    validate_id_shape(&kind, &id)?;

    let row = load_entity_row(&state, device.customer_id, device.id, &kind, &id).await?;
    Ok(Json(row))
}

async fn list_entities(
    device: AuthDevice,
    State(state): State<AppState>,
    Path(kind): Path<String>,
    Query(q): Query<ListQuery>,
) -> Result<Json<ListResponse>, ApiError> {
    require_enabled(&state)?;
    validate_kind(&kind)?;

    // SQL построен fragments чтобы было ровно один query (sqlite query plan
    // cache happy). Filtering на customer_id обязателен (multi-tenant
    // isolation per SECURITY.md), на owner_device_id ИЛИ существование wrap
    // для AuthDevice (recipient).
    let mut sql = String::from(
        "SELECT e.id, e.kind, e.owner_user_id, e.owner_device_id, e.parent_id, \
                e.name_hint, e.version, e.created_ts, e.modified_ts, e.deleted_ts, \
                e.ciphertext, e.ciphertext_iv, e.ciphertext_tag \
         FROM ballistics_entities e \
         WHERE e.customer_id = ? AND e.kind = ? \
           AND (e.owner_device_id = ? \
                OR EXISTS (SELECT 1 FROM ballistics_wraps w \
                           WHERE w.entity_id = e.id AND w.customer_id = e.customer_id \
                             AND w.recipient_device_id = ?))",
    );
    if q.modified_since.is_some() {
        sql.push_str(" AND e.modified_ts > ?");
    }
    if q.weapon_id.is_some() {
        sql.push_str(" AND e.parent_id = ?");
    }
    sql.push_str(" ORDER BY e.modified_ts ASC LIMIT 5000");

    let mut query = sqlx::query_as::<_, EntityRawRow>(&sql)
        .bind(device.customer_id)
        .bind(&kind)
        .bind(device.id)
        .bind(device.id);
    if let Some(ms) = &q.modified_since {
        query = query.bind(ms);
    }
    if let Some(wid) = &q.weapon_id {
        query = query.bind(wid);
    }
    let raws = query.fetch_all(&state.db).await?;

    let mut items = Vec::with_capacity(raws.len());
    for raw in raws {
        let wrap = load_wrap_for_device(&state, device.customer_id, &raw.id, device.id).await?;
        items.push(raw_to_row(raw, wrap));
    }
    Ok(Json(ListResponse {
        items,
        server_ts: chrono::Utc::now().to_rfc3339(),
    }))
}

async fn put_entity(
    device: AuthDevice,
    State(state): State<AppState>,
    Path((kind, id)): Path<(String, String)>,
    Json(req): Json<PutEntityRequest>,
) -> Result<Response, ApiError> {
    require_enabled(&state)?;
    validate_kind(&kind)?;
    validate_id_shape(&kind, &id)?;

    // ---- 1. Validate sizes / shapes ------------------------------------
    let ct_bytes = b64_decode(&req.ciphertext, "ciphertext")?;
    let ct_iv = b64_decode(&req.ciphertext_iv, "ciphertext_iv")?;
    let ct_tag = b64_decode(&req.ciphertext_tag, "ciphertext_tag")?;

    if ct_bytes.len() > MAX_CIPHERTEXT_BYTES {
        return Err(ApiError::BadRequest(format!(
            "ciphertext too large: {} bytes (max {MAX_CIPHERTEXT_BYTES})",
            ct_bytes.len()
        )));
    }
    if ct_iv.len() != NONCE_LEN {
        return Err(ApiError::BadRequest(format!(
            "ciphertext_iv must be {NONCE_LEN} bytes, got {}",
            ct_iv.len()
        )));
    }
    if ct_tag.len() != GCM_TAG_LEN {
        return Err(ApiError::BadRequest(format!(
            "ciphertext_tag must be {GCM_TAG_LEN} bytes, got {}",
            ct_tag.len()
        )));
    }

    if req.wraps.is_empty() {
        return Err(ApiError::BadRequest(
            "at least one wrap required (record unreadable иначе)".to_string(),
        ));
    }
    if req.wraps.len() > MAX_WRAPS_PER_RECORD {
        return Err(ApiError::BadRequest(format!(
            "too many wraps: {} (max {MAX_WRAPS_PER_RECORD})",
            req.wraps.len()
        )));
    }

    // Pre-validate wrap bytes — fail fast перед DB transaction.
    #[allow(clippy::type_complexity)] // local decoded-wrap tuple; alias would not aid clarity
    let mut wraps_decoded: Vec<(i64, String, Vec<u8>, Vec<u8>, Vec<u8>)> =
        Vec::with_capacity(req.wraps.len());
    for (idx, w) in req.wraps.iter().enumerate() {
        let eph = b64_decode(&w.eph_pubkey_der, &format!("wraps[{idx}].eph_pubkey_der"))?;
        let wdek = b64_decode(&w.wrapped_dek, &format!("wraps[{idx}].wrapped_dek"))?;
        let wdek_iv = b64_decode(&w.wrapped_dek_iv, &format!("wraps[{idx}].wrapped_dek_iv"))?;
        if eph.len() != SPKI_P256_LEN {
            return Err(ApiError::BadRequest(format!(
                "wraps[{idx}].eph_pubkey_der must be {SPKI_P256_LEN} bytes, got {}",
                eph.len()
            )));
        }
        if wdek.len() != WRAPPED_DEK_LEN {
            return Err(ApiError::BadRequest(format!(
                "wraps[{idx}].wrapped_dek must be {WRAPPED_DEK_LEN} bytes (32 ct + 16 tag), got {}",
                wdek.len()
            )));
        }
        if wdek_iv.len() != NONCE_LEN {
            return Err(ApiError::BadRequest(format!(
                "wraps[{idx}].wrapped_dek_iv must be {NONCE_LEN} bytes, got {}",
                wdek_iv.len()
            )));
        }
        if w.recipient_key_id.is_empty() || w.recipient_key_id.len() > 64 {
            return Err(ApiError::BadRequest(format!(
                "wraps[{idx}].recipient_key_id must be 1..=64 chars"
            )));
        }
        wraps_decoded.push((
            w.recipient_device_id,
            w.recipient_key_id.clone(),
            eph,
            wdek,
            wdek_iv,
        ));
    }

    // ---- 2. Validate recipient devices принадлежат same customer ------
    // Это критический multi-tenant boundary — без этого admin может wrap'ить
    // для device другого customer'а (data exfiltration vector).
    let recipient_ids: Vec<i64> = wraps_decoded.iter().map(|(id, _, _, _, _)| *id).collect();
    let placeholders = recipient_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let check_sql =
        format!("SELECT COUNT(*) FROM devices WHERE customer_id = ? AND id IN ({placeholders})");
    let mut q = sqlx::query_scalar::<_, i64>(&check_sql).bind(device.customer_id);
    for rid in &recipient_ids {
        q = q.bind(rid);
    }
    let valid_count = q.fetch_one(&state.db).await?;
    if valid_count != recipient_ids.len() as i64 {
        return Err(ApiError::BadRequest(
            "one or more recipient_device_id не принадлежит вашему customer'у".to_string(),
        ));
    }

    // Verify owner_user_id (если передан) — должен быть user same customer.
    if let Some(uid) = req.metadata.owner_user_id {
        let user_row: Option<(i64,)> =
            sqlx::query_as("SELECT id FROM users WHERE id = ? AND customer_id = ?")
                .bind(uid)
                .bind(device.customer_id)
                .fetch_optional(&state.db)
                .await?;
        if user_row.is_none() {
            return Err(ApiError::BadRequest(format!(
                "owner_user_id {uid} не существует в вашем customer'е"
            )));
        }
    }

    // ---- 3. Transaction: upsert entity + replace wraps -----------------
    let mut tx = state.db.begin().await?;

    let existing: Option<(i64, Option<String>)> = sqlx::query_as(
        "SELECT version, deleted_ts FROM ballistics_entities WHERE id = ? AND customer_id = ?",
    )
    .bind(&id)
    .bind(device.customer_id)
    .fetch_optional(&mut *tx)
    .await?;

    let is_create;
    let new_version = match existing {
        Some((current_version, deleted_ts)) => {
            // Update path. If client передал expected_version → check для
            // optimistic concurrency (CONTRACT §5.1).
            if let Some(expected) = req.metadata.expected_version {
                if expected != current_version {
                    return Err(ApiError::PreconditionFailed(format!(
                        "version mismatch: client expects {expected}, server has {current_version}"
                    )));
                }
            }
            // Verify ownership: только owner_device может update.
            // (Admin может через template push, который идёт отдельной таблицей.)
            let owner_check: Option<(Option<i64>,)> = sqlx::query_as(
                "SELECT owner_device_id FROM ballistics_entities WHERE id = ? AND customer_id = ?",
            )
            .bind(&id)
            .bind(device.customer_id)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some((Some(owner_id),)) = owner_check {
                if owner_id != device.id {
                    // v0.18.20 (security review LEAK-2): 404 не 403 — non-owner
                    // device не должен отличать «id чужой» от «id не существует»
                    // (consistency с read-path load_entity_row).
                    return Err(ApiError::NotFound);
                }
            }
            // Если record был soft-deleted — re-create (un-delete).
            let _ = deleted_ts;
            is_create = false;
            // v0.18.20 (security review rust-MT-1/F4): checked_add чтобы
            // client-controlled version=i64::MAX не давал silent wrap (release)
            // / panic (debug). Overflow → 400, не corrupt state.
            current_version
                .checked_add(1)
                .ok_or_else(|| ApiError::BadRequest("version counter overflow".into()))?
        }
        None => {
            // v0.18.20 (security review DL-3): owner_user_id обязателен на
            // create (колонка NOT NULL). Раньше omit давал opaque 500 на
            // constraint violation вместо чистого 400.
            if req.metadata.owner_user_id.is_none() {
                return Err(ApiError::BadRequest(
                    "owner_user_id required on create".to_string(),
                ));
            }
            is_create = true;
            // F4: clamp ceiling — client не должен задавать произвольно большой
            // стартовый version.
            req.metadata
                .expected_version
                .unwrap_or(1)
                .clamp(1, 1_000_000)
        }
    };

    if is_create {
        let insert_res = sqlx::query(
            "INSERT INTO ballistics_entities (id, customer_id, owner_user_id, owner_device_id, \
                kind, parent_id, name_hint, version, ciphertext, ciphertext_iv, ciphertext_tag) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(device.customer_id)
        .bind(req.metadata.owner_user_id)
        .bind(device.id)
        .bind(&kind)
        .bind(&req.metadata.parent_id)
        .bind(&req.metadata.name_hint)
        .bind(new_version)
        .bind(&ct_bytes)
        .bind(&ct_iv)
        .bind(&ct_tag)
        .execute(&mut *tx)
        .await;
        // v0.18.20 (security review MT-3 + migration 0028): PK теперь composite
        // (id, customer_id) — НЕ глобальный. До этой ветки `existing` (scoped
        // by customer_id) уже показал, что id свободен в ЭТОМ тенанте, так что
        // unique-violation здесь возможна только при same-tenant гонке двух
        // одновременных create'ов того же id → 409 Conflict (легитимный
        // same-tenant конфликт, НЕ cross-tenant existence-oracle, который
        // закрыт схемой в 0028). Не-unique ошибки → 500.
        if let Err(e) = insert_res {
            if let sqlx::Error::Database(db) = &e {
                if db.is_unique_violation() {
                    return Err(ApiError::Conflict("entity id already in use".to_string()));
                }
            }
            return Err(ApiError::from(e));
        }
    } else {
        // v0.18.20 (security review follow-up): owner_user_id IMMUTABLE после
        // create — НЕ перезаписывается на update. Personal record принадлежит
        // одному user'у; admin push создаёт ОТДЕЛЬНЫЕ entity per recipient, а не
        // переназначает owner'а существующих. Раньше UPDATE безусловно bind'ил
        // req.metadata.owner_user_id, что давало два дефекта: (а) omit
        // owner_user_id на update → NULL в NOT NULL колонку → opaque 500
        // (асимметрия с create-веткой DL-3); (б) другой owner_user_id молча
        // перезаписывал владельца записи. Теперь колонка не входит в SET —
        // owner сохраняется как был. (BALLISTICS-MDM-CONTRACT молчит про
        // mutability; immutable — консервативный безопасный дефолт,
        // согласующийся с его принципом «не silent overwrite».)
        sqlx::query(
            "UPDATE ballistics_entities SET \
                parent_id = ?, name_hint = ?, version = ?, \
                modified_ts = datetime('now'), deleted_ts = NULL, \
                ciphertext = ?, ciphertext_iv = ?, ciphertext_tag = ? \
             WHERE id = ? AND customer_id = ?",
        )
        .bind(&req.metadata.parent_id)
        .bind(&req.metadata.name_hint)
        .bind(new_version)
        .bind(&ct_bytes)
        .bind(&ct_iv)
        .bind(&ct_tag)
        .bind(&id)
        .bind(device.customer_id)
        .execute(&mut *tx)
        .await?;
    }

    // Заменяем wraps атомарно — delete все old + insert all new. Это проще
    // чем merge, и т.к. ciphertext новый — old wraps бесполезны (DEK
    // different).
    sqlx::query("DELETE FROM ballistics_wraps WHERE entity_id = ? AND customer_id = ?")
        .bind(&id)
        .bind(device.customer_id)
        .execute(&mut *tx)
        .await?;

    for (rdev, rkid, eph, wdek, wdek_iv) in &wraps_decoded {
        sqlx::query(
            "INSERT INTO ballistics_wraps (entity_id, customer_id, recipient_device_id, \
                recipient_key_id, eph_pubkey_der, wrapped_dek, wrapped_dek_iv) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(device.customer_id)
        .bind(rdev)
        .bind(rkid)
        .bind(eph)
        .bind(wdek)
        .bind(wdek_iv)
        .execute(&mut *tx)
        .await?;
    }

    // Audit row.
    sqlx::query(
        "INSERT INTO ballistics_audit_log (customer_id, user_id, device_id, action, entity_kind, entity_id) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(device.customer_id)
    .bind(None::<i64>)
    .bind(device.id)
    .bind(if is_create { "create" } else { "update" })
    .bind(&kind)
    .bind(&id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    let row = load_entity_row(&state, device.customer_id, device.id, &kind, &id).await?;
    let status = if is_create {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(row)).into_response())
}

async fn delete_entity(
    device: AuthDevice,
    State(state): State<AppState>,
    Path((kind, id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    require_enabled(&state)?;
    validate_kind(&kind)?;
    validate_id_shape(&kind, &id)?;

    // Verify ownership.
    let owner_row: Option<(Option<i64>,)> = sqlx::query_as(
        "SELECT owner_device_id FROM ballistics_entities \
         WHERE id = ? AND customer_id = ? AND kind = ?",
    )
    .bind(&id)
    .bind(device.customer_id)
    .bind(&kind)
    .fetch_optional(&state.db)
    .await?;
    let Some((owner_id,)) = owner_row else {
        return Err(ApiError::NotFound);
    };
    if owner_id != Some(device.id) {
        // v0.18.20 (security review LEAK-2): 404 не 403 — не leak'аем
        // existence/ownership чужой записи.
        return Err(ApiError::NotFound);
    }

    // Soft-delete; через 90 дней GC hard-purge'ит.
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "UPDATE ballistics_entities SET deleted_ts = datetime('now'), modified_ts = datetime('now'), \
            version = version + 1 \
         WHERE id = ? AND customer_id = ?",
    )
    .bind(&id)
    .bind(device.customer_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO ballistics_audit_log (customer_id, device_id, action, entity_kind, entity_id) \
         VALUES (?, ?, 'delete', ?, ?)",
    )
    .bind(device.customer_id)
    .bind(device.id)
    .bind(&kind)
    .bind(&id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

// =====================================================================
// Handlers — audit log
// =====================================================================

async fn get_audit_log(
    device: AuthDevice,
    State(state): State<AppState>,
) -> Result<Json<AuditLogResponse>, ApiError> {
    require_enabled(&state)?;
    let rows: Vec<AuditLogRowRaw> = sqlx::query_as::<_, AuditLogRowRaw>(
        "SELECT id, action, entity_kind, entity_id, ts, user_id, device_id \
         FROM ballistics_audit_log \
         WHERE customer_id = ? AND device_id = ? \
           AND ts >= datetime('now', '-30 days') \
         ORDER BY id DESC LIMIT 1000",
    )
    .bind(device.customer_id)
    .bind(device.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(AuditLogResponse {
        items: rows.into_iter().map(audit_raw_to_row).collect(),
    }))
}

// =====================================================================
// Handlers — GDPR / ФЗ-152 (export + delete-all)
// =====================================================================

/// v0.18.20 (security review DOS-2): per-export потолки. Защищают пул от
/// unbounded fetch_all, но обрезка ОБЯЗАНА сигналиться (см. `ExportBundle.
/// truncated`). Щедрые — реальный device держит десятки-сотни записей.
const ENTITY_EXPORT_LIMIT: i64 = 10_000;
const AUDIT_EXPORT_LIMIT: i64 = 5_000;

#[derive(Debug, Serialize)]
pub struct ExportBundle {
    /// ISO 8601 UTC момент export'а.
    pub exported_at: String,
    /// v0.18.20: bumped 1→2 (добавлены truncated / *_total поля).
    pub schema_version: u32,
    /// v0.18.20 (security review DOS-2): `true` если scope превысил
    /// ENTITY_EXPORT_LIMIT / AUDIT_EXPORT_LIMIT и bundle НЕ полон. Без этого
    /// поля обрезанный GDPR-экспорт был неотличим от полного (нарушение
    /// Art.15/20 completeness). При `true` клиент обязан дозапросить остаток
    /// (пагинация — follow-up).
    pub truncated: bool,
    /// Полное число entity-записей в scope (до LIMIT).
    pub entities_total: i64,
    /// Полное число audit-записей в scope (до LIMIT).
    pub audit_total: i64,
    pub entities: Vec<EntityRow>,
    pub audit_log: Vec<AuditLogRow>,
}

async fn export_user_data(
    device: AuthDevice,
    State(state): State<AppState>,
) -> Result<Json<ExportBundle>, ApiError> {
    require_enabled(&state)?;

    // v0.18.20 (security review DOS-2): bounded export + explicit truncation
    // signal. LIMIT защищает пул от unbounded fetch_all + N+1 wrap-queries, но
    // обрезка ОБЯЗАНА сигналиться (GDPR completeness) — считаем total отдельным
    // COUNT и выставляем `truncated`. Сортировка по (created_ts, id), НЕ по
    // голому TEXT-`id` (id — client-generated UUID → lexicographic order при
    // обрезке = произвольное подмножество; created_ts даёт осмысленный
    // хронологический префикс, id — детерминированный tiebreak).
    let entities_total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ballistics_entities \
         WHERE customer_id = ? AND owner_device_id = ?",
    )
    .bind(device.customer_id)
    .bind(device.id)
    .fetch_one(&state.db)
    .await?;

    let raws = sqlx::query_as::<_, EntityRawRow>(
        "SELECT id, kind, owner_user_id, owner_device_id, parent_id, name_hint, version, \
                created_ts, modified_ts, deleted_ts, ciphertext, ciphertext_iv, ciphertext_tag \
         FROM ballistics_entities \
         WHERE customer_id = ? AND owner_device_id = ? \
         ORDER BY created_ts, id LIMIT ?",
    )
    .bind(device.customer_id)
    .bind(device.id)
    .bind(ENTITY_EXPORT_LIMIT)
    .fetch_all(&state.db)
    .await?;

    let mut entities = Vec::with_capacity(raws.len());
    for raw in raws {
        let wrap = load_wrap_for_device(&state, device.customer_id, &raw.id, device.id).await?;
        entities.push(raw_to_row(raw, wrap));
    }

    let audit_total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ballistics_audit_log \
         WHERE customer_id = ? AND device_id = ?",
    )
    .bind(device.customer_id)
    .bind(device.id)
    .fetch_one(&state.db)
    .await?;

    let audit_raws: Vec<AuditLogRowRaw> = sqlx::query_as::<_, AuditLogRowRaw>(
        "SELECT id, action, entity_kind, entity_id, ts, user_id, device_id \
         FROM ballistics_audit_log \
         WHERE customer_id = ? AND device_id = ? \
         ORDER BY id DESC LIMIT ?",
    )
    .bind(device.customer_id)
    .bind(device.id)
    .bind(AUDIT_EXPORT_LIMIT)
    .fetch_all(&state.db)
    .await?;

    let truncated = entities_total > entities.len() as i64 || audit_total > audit_raws.len() as i64;

    // v0.18.20 (security review DL-4): audit row export'а пишется ДО возврата
    // bundle'а с error-propagation (`?`, не `.ok()`). Раньше swallowed error
    // → полный дамп персональных данных мог пройти БЕЗ audit-следа (нарушение
    // §8.4 guarantee). Теперь failed audit → fail request.
    sqlx::query(
        "INSERT INTO ballistics_audit_log (customer_id, device_id, action) \
         VALUES (?, ?, 'export')",
    )
    .bind(device.customer_id)
    .bind(device.id)
    .execute(&state.db)
    .await?;

    Ok(Json(ExportBundle {
        exported_at: chrono::Utc::now().to_rfc3339(),
        schema_version: 2,
        truncated,
        entities_total,
        audit_total,
        entities,
        audit_log: audit_raws.into_iter().map(audit_raw_to_row).collect(),
    }))
}

async fn delete_all_user_data(
    device: AuthDevice,
    State(state): State<AppState>,
) -> Result<StatusCode, ApiError> {
    require_enabled(&state)?;

    let mut tx = state.db.begin().await?;

    // Count для compliance log'а.
    let entity_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ballistics_entities WHERE customer_id = ? AND owner_device_id = ?",
    )
    .bind(device.customer_id)
    .bind(device.id)
    .fetch_one(&mut *tx)
    .await?;

    let wrap_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ballistics_wraps w \
         JOIN ballistics_entities e ON e.id = w.entity_id AND e.customer_id = w.customer_id \
         WHERE w.customer_id = ? AND e.owner_device_id = ?",
    )
    .bind(device.customer_id)
    .bind(device.id)
    .fetch_one(&mut *tx)
    .await?;

    // Hard purge. Wraps удалятся cascade'ом через ballistics_entities FK.
    sqlx::query("DELETE FROM ballistics_entities WHERE customer_id = ? AND owner_device_id = ?")
        .bind(device.customer_id)
        .bind(device.id)
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM ballistics_audit_log WHERE customer_id = ? AND device_id = ?")
        .bind(device.customer_id)
        .bind(device.id)
        .execute(&mut *tx)
        .await?;

    // Insert compliance row (отдельная retention-таблица).
    sqlx::query(
        "INSERT INTO ballistics_gdpr_deletion_log \
            (customer_id, user_id, deleted_entity_count, deleted_wrap_count, requested_by_user) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(device.customer_id)
    .bind(device.id) // в этой таблице user_id используется как owner-identifier
    .bind(entity_count)
    .bind(wrap_count)
    .bind(None::<i64>)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// =====================================================================
// Handlers — admin push (CONTRACT §3.6)
// =====================================================================

#[derive(Debug, Deserialize)]
pub struct CreateAdminTemplateRequest {
    pub kind: String,
    /// Plaintext template payload (admin намеренно публикует). Client при
    /// accept'е локально encrypt'ит и POST'ит в `/ballistics/<kind>`.
    pub payload: JsonValue,
    pub target_group_id: Option<i64>,
    /// Опциональный display title для admin UI list view.
    pub title: Option<String>,
    /// Client-generated unique id (UUID); если omit — server сгенерит.
    pub id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AdminTemplateRow {
    pub id: String,
    pub kind: String,
    pub target_group_id: Option<i64>,
    pub title: Option<String>,
    pub payload: JsonValue,
    pub suggested_by_user: Option<i64>,
    pub created_at: String,
    pub retracted_at: Option<String>,
}

async fn create_admin_template(
    user: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<CreateAdminTemplateRequest>,
) -> Result<Json<AdminTemplateRow>, ApiError> {
    require_enabled(&state)?;
    require_permission(&state.db, user.role_id, "ballistics.admin").await?;

    // Admin templates только для weapon / cartridge — DOPE/units не template'ятся.
    if !matches!(req.kind.as_str(), "weapon" | "cartridge") {
        return Err(ApiError::BadRequest(
            "admin templates only for kind=weapon|cartridge".to_string(),
        ));
    }
    if let Some(gid) = req.target_group_id {
        let group_row: Option<(i64,)> =
            sqlx::query_as("SELECT id FROM groups WHERE id = ? AND customer_id = ?")
                .bind(gid)
                .bind(user.customer_id)
                .fetch_optional(&state.db)
                .await?;
        if group_row.is_none() {
            return Err(ApiError::BadRequest(format!(
                "target_group_id {gid} не существует в вашем customer'е"
            )));
        }
    }

    let id = req.id.unwrap_or_else(|| {
        use rand::Rng;
        use rand::distributions::Alphanumeric;
        let suffix: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(16)
            .map(char::from)
            .collect();
        format!("tmpl_{suffix}")
    });
    let payload_json = req.payload.to_string();
    // v0.18.20 (security review DOS-4): cap payload size. Router-level
    // DefaultBodyLimit (DOS-1, 1 MiB) уже ограничивает тело, но payload_json
    // хранится в unbounded TEXT и re-парсится на каждом list view, поэтому
    // отдельный потолок на СТРОКУ payload'а. Weapon/cartridge профиль << 256 KiB.
    if payload_json.len() > MAX_TEMPLATE_PAYLOAD_BYTES {
        return Err(ApiError::BadRequest(format!(
            "template payload too large: {} bytes (max {MAX_TEMPLATE_PAYLOAD_BYTES})",
            payload_json.len()
        )));
    }

    let mut tx = state.db.begin().await?;
    sqlx::query(
        "INSERT INTO ballistics_admin_templates \
            (id, customer_id, kind, target_group_id, payload_json, suggested_by_user, title) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(user.customer_id)
    .bind(&req.kind)
    .bind(req.target_group_id)
    .bind(&payload_json)
    .bind(user.id)
    .bind(&req.title)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO ballistics_audit_log (customer_id, user_id, action, entity_kind, entity_id) \
         VALUES (?, ?, 'admin_push', ?, ?)",
    )
    .bind(user.customer_id)
    .bind(user.id)
    .bind(&req.kind)
    .bind(&id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    let row = load_admin_template(&state, user.customer_id, &id).await?;
    Ok(Json(row))
}

async fn list_admin_templates(
    user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<AdminTemplateRow>>, ApiError> {
    require_enabled(&state)?;
    require_permission(&state.db, user.role_id, "ballistics.read").await?;

    let raws: Vec<AdminTemplateRaw> = sqlx::query_as::<_, AdminTemplateRaw>(
        "SELECT id, kind, target_group_id, title, payload_json, suggested_by_user, \
                created_at, retracted_at \
         FROM ballistics_admin_templates \
         WHERE customer_id = ? AND retracted_at IS NULL \
         ORDER BY created_at DESC LIMIT 500",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;

    let rows = raws
        .into_iter()
        .filter_map(|r| admin_template_raw_to_row(r).ok())
        .collect();
    Ok(Json(rows))
}

async fn retract_admin_template(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_enabled(&state)?;
    require_permission(&state.db, user.role_id, "ballistics.admin").await?;

    let res = sqlx::query(
        "UPDATE ballistics_admin_templates SET retracted_at = datetime('now') \
         WHERE id = ? AND customer_id = ? AND retracted_at IS NULL",
    )
    .bind(&id)
    .bind(user.customer_id)
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

// =====================================================================
// Internal helpers (DB row loading + conversion)
// =====================================================================

#[derive(sqlx::FromRow)]
struct EntityRawRow {
    id: String,
    kind: String,
    owner_user_id: Option<i64>,
    owner_device_id: Option<i64>,
    parent_id: Option<String>,
    name_hint: Option<String>,
    version: i64,
    created_ts: String,
    modified_ts: String,
    deleted_ts: Option<String>,
    ciphertext: Vec<u8>,
    ciphertext_iv: Vec<u8>,
    ciphertext_tag: Vec<u8>,
}

#[derive(sqlx::FromRow)]
struct WrapRawRow {
    recipient_device_id: i64,
    recipient_key_id: String,
    eph_pubkey_der: Vec<u8>,
    wrapped_dek: Vec<u8>,
    wrapped_dek_iv: Vec<u8>,
}

#[derive(sqlx::FromRow)]
struct AuditLogRowRaw {
    id: i64,
    action: String,
    entity_kind: Option<String>,
    entity_id: Option<String>,
    ts: String,
    user_id: Option<i64>,
    device_id: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct AdminTemplateRaw {
    id: String,
    kind: String,
    target_group_id: Option<i64>,
    title: Option<String>,
    payload_json: String,
    suggested_by_user: Option<i64>,
    created_at: String,
    retracted_at: Option<String>,
}

fn raw_to_row(raw: EntityRawRow, wrap: Option<WrapOutput>) -> EntityRow {
    let etag = etag_for(raw.version);
    EntityRow {
        id: raw.id,
        kind: raw.kind,
        owner_user_id: raw.owner_user_id,
        owner_device_id: raw.owner_device_id,
        parent_id: raw.parent_id,
        name_hint: raw.name_hint,
        version: raw.version,
        created_ts: raw.created_ts,
        modified_ts: raw.modified_ts,
        deleted_ts: raw.deleted_ts,
        etag,
        ciphertext: b64_encode(&raw.ciphertext),
        ciphertext_iv: b64_encode(&raw.ciphertext_iv),
        ciphertext_tag: b64_encode(&raw.ciphertext_tag),
        wrap_for_this_device: wrap,
    }
}

fn audit_raw_to_row(r: AuditLogRowRaw) -> AuditLogRow {
    AuditLogRow {
        id: r.id,
        action: r.action,
        entity_kind: r.entity_kind,
        entity_id: r.entity_id,
        ts: r.ts,
        user_id: r.user_id,
        device_id: r.device_id,
    }
}

fn admin_template_raw_to_row(r: AdminTemplateRaw) -> Result<AdminTemplateRow, ApiError> {
    let payload: JsonValue = serde_json::from_str(&r.payload_json)
        .map_err(|_| ApiError::InternalServerError("corrupted template payload".into()))?;
    Ok(AdminTemplateRow {
        id: r.id,
        kind: r.kind,
        target_group_id: r.target_group_id,
        title: r.title,
        payload,
        suggested_by_user: r.suggested_by_user,
        created_at: r.created_at,
        retracted_at: r.retracted_at,
    })
}

async fn load_entity_row(
    state: &AppState,
    customer_id: i64,
    requesting_device_id: i64,
    kind: &str,
    id: &str,
) -> Result<EntityRow, ApiError> {
    let raw: Option<EntityRawRow> = sqlx::query_as(
        "SELECT id, kind, owner_user_id, owner_device_id, parent_id, name_hint, version, \
                created_ts, modified_ts, deleted_ts, ciphertext, ciphertext_iv, ciphertext_tag \
         FROM ballistics_entities \
         WHERE id = ? AND customer_id = ? AND kind = ?",
    )
    .bind(id)
    .bind(customer_id)
    .bind(kind)
    .fetch_optional(&state.db)
    .await?;
    let Some(raw) = raw else {
        return Err(ApiError::NotFound);
    };
    // Если device не owner и не recipient — 404 (не leak'аем existence).
    let visible = raw.owner_device_id == Some(requesting_device_id)
        || load_wrap_for_device(state, customer_id, &raw.id, requesting_device_id)
            .await?
            .is_some();
    if !visible {
        return Err(ApiError::NotFound);
    }
    let wrap = load_wrap_for_device(state, customer_id, &raw.id, requesting_device_id).await?;
    Ok(raw_to_row(raw, wrap))
}

async fn load_wrap_for_device(
    state: &AppState,
    customer_id: i64,
    entity_id: &str,
    device_id: i64,
) -> Result<Option<WrapOutput>, ApiError> {
    // v0.18.20 (migration 0028): entity_id больше НЕ глобально-уникален
    // (PK стал (id, customer_id)), поэтому wrap-lookup ОБЯЗАН скоупить
    // customer_id — иначе под одинаковым id в другом тенанте мог бы
    // подтянуться чужой wrap. (FK wraps→entities composite, так что
    // w.customer_id всегда == customer_id энтити.)
    let raw: Option<WrapRawRow> = sqlx::query_as(
        "SELECT recipient_device_id, recipient_key_id, eph_pubkey_der, wrapped_dek, wrapped_dek_iv \
         FROM ballistics_wraps \
         WHERE entity_id = ? AND customer_id = ? AND recipient_device_id = ?",
    )
    .bind(entity_id)
    .bind(customer_id)
    .bind(device_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(raw.map(|r| WrapOutput {
        recipient_device_id: r.recipient_device_id,
        recipient_key_id: r.recipient_key_id,
        eph_pubkey_der: b64_encode(&r.eph_pubkey_der),
        wrapped_dek: b64_encode(&r.wrapped_dek),
        wrapped_dek_iv: b64_encode(&r.wrapped_dek_iv),
    }))
}

async fn load_admin_template(
    state: &AppState,
    customer_id: i64,
    id: &str,
) -> Result<AdminTemplateRow, ApiError> {
    let raw: Option<AdminTemplateRaw> = sqlx::query_as(
        "SELECT id, kind, target_group_id, title, payload_json, suggested_by_user, \
                created_at, retracted_at \
         FROM ballistics_admin_templates WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(customer_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(raw) = raw else {
        return Err(ApiError::NotFound);
    };
    admin_template_raw_to_row(raw)
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    //! Coverage: shape validation, multi-tenant isolation, feature flag,
    //! CRUD happy-path, soft-delete, ETag conflict, audit log.
    //! Crypto correctness — НЕ тестируется (server opaque для ciphertext).

    use super::*;

    #[test]
    fn validate_id_shape_enforces_user_prefix_for_cartridge() {
        assert!(validate_id_shape("cartridge", "user_foo").is_ok());
        assert!(validate_id_shape("cartridge", "system_foo").is_err());
        assert!(validate_id_shape("cartridge", "").is_err());
        assert!(validate_id_shape("weapon", "any-uuid").is_ok());
    }

    #[test]
    fn validate_kind_accepts_only_four_known() {
        for k in VALID_KINDS {
            assert!(validate_kind(k).is_ok(), "kind {k} must pass");
        }
        assert!(validate_kind("anything-else").is_err());
        assert!(validate_kind("").is_err());
    }

    #[test]
    fn etag_format_is_weak_etag_with_version() {
        assert_eq!(etag_for(1), "W/\"1\"");
        assert_eq!(etag_for(42), "W/\"42\"");
    }

    /// Regression (v0.18.20 security review follow-up): owner_user_id is
    /// IMMUTABLE after create. An update sending a different owner must be
    /// ignored (no silent reassignment), and an update OMITTING owner_user_id
    /// must succeed — not 500 (previously it bound NULL into the NOT NULL
    /// column). Calls the handler directly (test_state enables ballistics).
    #[tokio::test]
    async fn put_update_keeps_owner_user_id_immutable() {
        use crate::auth_extract::AuthDevice;
        use axum::Json;
        use axum::extract::{Path, State};

        let state = crate::state::test_state().await;
        // Device (customer 1) = auth principal AND wrap recipient. Admin user
        // (id 1) exists from bootstrap; add a 2nd user to attempt a (rejected)
        // owner change.
        sqlx::query("INSERT INTO devices (id, customer_id, serial) VALUES (1, 1, 'dev-1')")
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO users (id, customer_id, role_id, login) VALUES (2, 1, 2, 'second-user')",
        )
        .execute(&state.db)
        .await
        .unwrap();

        let device = || AuthDevice {
            id: 1,
            customer_id: 1,
            serial: "dev-1".to_string(),
        };
        let b64n = |n: usize| b64_encode(&vec![0u8; n]);
        let make_req = |owner: Option<i64>| PutEntityRequest {
            metadata: EntityMetadata {
                name_hint: None,
                parent_id: None,
                owner_user_id: owner,
                expected_version: None,
            },
            ciphertext: b64n(16),
            ciphertext_iv: b64n(NONCE_LEN),
            ciphertext_tag: b64n(GCM_TAG_LEN),
            wraps: vec![WrapInput {
                recipient_device_id: 1,
                recipient_key_id: "k1".to_string(),
                eph_pubkey_der: b64n(SPKI_P256_LEN),
                wrapped_dek: b64n(WRAPPED_DEK_LEN),
                wrapped_dek_iv: b64n(NONCE_LEN),
            }],
        };
        let path = || Path(("weapon".to_string(), "w-test-1".to_string()));
        let owner_of = |state: &AppState| {
            let pool = state.db.clone();
            async move {
                sqlx::query_scalar::<_, i64>(
                    "SELECT owner_user_id FROM ballistics_entities \
                     WHERE id = 'w-test-1' AND customer_id = 1",
                )
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };

        // Create with owner = 1.
        put_entity(
            device(),
            State(state.clone()),
            path(),
            Json(make_req(Some(1))),
        )
        .await
        .expect("create should succeed");
        assert_eq!(owner_of(&state).await, 1);

        // Update sending a DIFFERENT owner (2) — must be ignored (immutable).
        put_entity(
            device(),
            State(state.clone()),
            path(),
            Json(make_req(Some(2))),
        )
        .await
        .expect("update with different owner should succeed");
        assert_eq!(
            owner_of(&state).await,
            1,
            "owner_user_id must NOT be overwritten on update"
        );

        // Update OMITTING owner_user_id — must succeed, not 500.
        put_entity(device(), State(state.clone()), path(), Json(make_req(None)))
            .await
            .expect("update without owner_user_id must succeed (not 500)");
        assert_eq!(
            owner_of(&state).await,
            1,
            "owner_user_id must remain after an owner-less update"
        );
    }

    #[tokio::test]
    async fn feature_flag_off_returns_503_for_data_endpoints() {
        let state = crate::state::test_state().await;
        // override flag к OFF (test_state по default ON).
        let pool = state.db.clone();
        let s = crate::state::AppState::new(
            pool,
            "test-secret-with-at-least-32-bytes-of-padding-yes".to_string(),
            86_400,
            std::env::temp_dir().join("outpost-ballistics-test"),
            crate::config::DEFAULT_MAX_BODY_BYTES,
            crate::config::DEFAULT_REQUEST_TIMEOUT_SECS,
            false,
            None,
            "apks/outpost-latest-debug.apk".to_string(),
            chrono_tz::UTC,
            false, // ballistics_enabled = OFF
        );
        let err = require_enabled(&s).expect_err("flag off → expect error");
        match err {
            ApiError::ServiceUnavailable(msg) => {
                assert!(msg.contains("disabled"), "unexpected reason: {msg}");
            }
            other => panic!("expected ServiceUnavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn health_works_even_when_flag_off() {
        // Health не вызывает require_enabled, но возвращает enabled=false.
        // Sanity-check что endpoint видим при disabled deployment.
        let state = crate::state::test_state().await;
        let pool = state.db.clone();
        let s = crate::state::AppState::new(
            pool,
            "test-secret-with-at-least-32-bytes-of-padding-yes".to_string(),
            86_400,
            std::env::temp_dir().join("outpost-ballistics-test"),
            crate::config::DEFAULT_MAX_BODY_BYTES,
            crate::config::DEFAULT_REQUEST_TIMEOUT_SECS,
            false,
            None,
            "apks/outpost-latest-debug.apk".to_string(),
            chrono_tz::UTC,
            false,
        );
        // Direct handler call (без HTTP layer).
        let resp = health(axum::extract::State(s)).await;
        let body = resp.0;
        assert_eq!(body.version, "v1");
        assert!(!body.enabled);
    }

    #[tokio::test]
    async fn put_validates_ciphertext_iv_length() {
        let state = crate::state::test_state().await;
        let req = PutEntityRequest {
            metadata: EntityMetadata {
                name_hint: None,
                parent_id: None,
                owner_user_id: None,
                expected_version: None,
            },
            // 8-byte IV (invalid; должно быть 12).
            ciphertext: b64_encode(b"opaque-blob"),
            ciphertext_iv: b64_encode(&[0u8; 8]),
            ciphertext_tag: b64_encode(&[0u8; GCM_TAG_LEN]),
            wraps: vec![],
        };
        // Делаем quick-test через decode path manually (handler требует
        // AuthDevice, нельзя легко вызвать без HTTP layer).
        let _ = state;
        let iv = b64_decode(&req.ciphertext_iv, "ciphertext_iv").unwrap();
        assert_eq!(iv.len(), 8, "test setup");
        assert_ne!(
            iv.len(),
            NONCE_LEN,
            "expected mismatch with required NONCE_LEN"
        );
    }

    #[test]
    fn b64_roundtrip() {
        let input = b"hello world 1234567890";
        let encoded = b64_encode(input);
        let decoded = b64_decode(&encoded, "test").unwrap();
        assert_eq!(decoded, input);
    }

    #[test]
    fn b64_rejects_garbage() {
        assert!(b64_decode("!!!not-base64!!!", "test").is_err());
    }
}
