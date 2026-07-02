//! `/api/v1/files` (admin auth) and `/files/signed/{token}` (public).
//!
//! Generic file catalog: admin uploads via multipart, server stores under
//! `$APP_FILES_DIR`, records sha256 + size in `uploaded_files`. Devices
//! download via HMAC-signed URLs that don't require an Authorization
//! header (the token IS the proof).

use crate::auth_extract::AuthUser;
use crate::error::ApiError;
use crate::page::{Page, PageParams};
use crate::permission::require_permission;
use crate::signed_url;
use crate::state::AppState;
use crate::storage;
use axum::body::Body;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/files", get(list))
        .route("/api/v1/files/upload", post(upload))
        .route("/api/v1/files/{id}", get(get_one).delete(delete_one))
        .route("/api/v1/files/{id}/signed-url", get(make_signed_url))
        // PUBLIC download — no AuthUser, token IS the proof:
        .route("/files/signed/{token}", get(download_signed))
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct UploadedFile {
    pub id: i64,
    pub customer_id: i64,
    pub file_path: String,
    pub original_name: String,
    pub content_type: Option<String>,
    pub file_size_bytes: i64,
    pub sha256: String,
    pub kind: String,
    pub uploaded_by: Option<i64>,
    pub uploaded_at: DateTime<Utc>,
}

async fn list(
    user: AuthUser,
    State(state): State<AppState>,
    Query(page): Query<PageParams>,
) -> Result<Json<Page<UploadedFile>>, ApiError> {
    require_permission(&state.db, user.role_id, "files.read").await?;
    let (limit, offset) = page.clamp();
    let items: Vec<UploadedFile> = sqlx::query_as::<_, UploadedFile>(
        "SELECT id, customer_id, file_path, original_name, content_type, file_size_bytes, \
                sha256, kind, uploaded_by, uploaded_at \
         FROM uploaded_files WHERE customer_id = ? \
         ORDER BY uploaded_at DESC LIMIT ? OFFSET ?",
    )
    .bind(user.customer_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM uploaded_files WHERE customer_id = ?")
            .bind(user.customer_id)
            .fetch_one(&state.db)
            .await?;
    Ok(Json(Page {
        items,
        total,
        limit,
        offset,
    }))
}

async fn get_one(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<UploadedFile>, ApiError> {
    require_permission(&state.db, user.role_id, "files.read").await?;
    let f: Option<UploadedFile> = sqlx::query_as::<_, UploadedFile>(
        "SELECT id, customer_id, file_path, original_name, content_type, file_size_bytes, \
                sha256, kind, uploaded_by, uploaded_at \
         FROM uploaded_files WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?;
    f.map(Json).ok_or(ApiError::NotFound)
}

async fn upload(
    user: AuthUser,
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<UploadedFile>), ApiError> {
    require_permission(&state.db, user.role_id, "files.write").await?;

    let mut original_name: Option<String> = None;
    let mut content_type: Option<String> = None;
    let mut kind: String = "generic".to_string();
    let mut bytes: Option<Vec<u8>> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("malformed multipart: {e}")))?
    {
        let field_name = field.name().unwrap_or("").to_string();
        match field_name.as_str() {
            "file" => {
                original_name = field.file_name().map(|s| s.to_string());
                content_type = field.content_type().map(|s| s.to_string());
                let data = field
                    .bytes()
                    .await
                    .map_err(|e| ApiError::BadRequest(format!("read file bytes: {e}")))?;
                bytes = Some(data.to_vec());
            }
            "kind" => {
                kind = field
                    .text()
                    .await
                    .map_err(|e| ApiError::BadRequest(format!("read kind: {e}")))?;
            }
            _ => { /* ignore unknown */ }
        }
    }

    let bytes = bytes.ok_or_else(|| ApiError::BadRequest("missing 'file' part".into()))?;
    let original_name =
        original_name.ok_or_else(|| ApiError::BadRequest("missing filename".into()))?;

    let extension = std::path::Path::new(&original_name)
        .extension()
        .and_then(|e| e.to_str());

    let stored = storage::write_bytes(state.app_files_dir.as_ref(), &bytes, extension)
        .await
        .map_err(|e| {
            tracing::error!(error = ?e, "store file");
            ApiError::Internal
        })?;

    let id: i64 = sqlx::query_scalar(
        "INSERT INTO uploaded_files \
            (customer_id, file_path, original_name, content_type, file_size_bytes, sha256, \
             kind, uploaded_by) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(user.customer_id)
    .bind(&stored.relative_path)
    .bind(&original_name)
    .bind(&content_type)
    .bind(stored.size)
    .bind(&stored.sha256)
    .bind(&kind)
    .bind(user.id)
    .fetch_one(&state.db)
    .await?;

    let f: UploadedFile = sqlx::query_as::<_, UploadedFile>(
        "SELECT id, customer_id, file_path, original_name, content_type, file_size_bytes, \
                sha256, kind, uploaded_by, uploaded_at \
         FROM uploaded_files WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, Json(f)))
}

async fn delete_one(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    require_permission(&state.db, user.role_id, "files.write").await?;
    let row: Option<(String,)> =
        sqlx::query_as("SELECT file_path FROM uploaded_files WHERE id = ? AND customer_id = ?")
            .bind(id)
            .bind(user.customer_id)
            .fetch_optional(&state.db)
            .await?;
    let Some((file_path,)) = row else {
        return Err(ApiError::NotFound);
    };
    sqlx::query("DELETE FROM uploaded_files WHERE id = ? AND customer_id = ?")
        .bind(id)
        .bind(user.customer_id)
        .execute(&state.db)
        .await?;
    if let Ok(abs) = storage::resolve_under_root(state.app_files_dir.as_ref(), &file_path) {
        let _ = tokio::fs::remove_file(abs).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct SignedUrlQuery {
    #[serde(default = "default_expires_in")]
    pub expires_in: i64,
}

fn default_expires_in() -> i64 {
    300
}

#[derive(Debug, Serialize)]
pub struct SignedUrlResponse {
    pub url: String,
    pub expires_in: i64,
}

async fn make_signed_url(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<SignedUrlQuery>,
) -> Result<Json<SignedUrlResponse>, ApiError> {
    require_permission(&state.db, user.role_id, "files.read").await?;
    let exists: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM uploaded_files WHERE id = ? AND customer_id = ?")
            .bind(id)
            .bind(user.customer_id)
            .fetch_optional(&state.db)
            .await?;
    if exists.is_none() {
        return Err(ApiError::NotFound);
    }
    let ttl = q.expires_in.clamp(10, 86_400);
    let token = signed_url::sign(id, ttl, &state.app_secret);
    Ok(Json(SignedUrlResponse {
        url: format!("/files/signed/{token}"),
        expires_in: ttl,
    }))
}

/// Public download endpoint — verifies the HMAC token, streams the file.
async fn download_signed(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    let verified = match signed_url::verify(&token, &state.app_secret) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "signed-url verify rejected");
            return (StatusCode::FORBIDDEN, "forbidden").into_response();
        }
    };
    let row: Option<(String, String, Option<String>, i64)> = match sqlx::query_as(
        "SELECT file_path, original_name, content_type, file_size_bytes \
         FROM uploaded_files WHERE id = ?",
    )
    .bind(verified.file_id)
    .fetch_optional(&state.db)
    .await
    {
        Ok(row) => row,
        Err(e) => {
            tracing::error!(error = %e, "signed-url DB lookup");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal").into_response();
        }
    };
    let Some((file_path, original_name, content_type, _file_size_bytes)) = row else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };

    let abs = match storage::resolve_under_root(state.app_files_dir.as_ref(), &file_path) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "signed-url resolve_under_root");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal").into_response();
        }
    };

    let file = match tokio::fs::File::open(&abs).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(error = %e, path = %abs.display(), "signed-url open");
            return (StatusCode::NOT_FOUND, "missing file").into_response();
        }
    };

    let ct = content_type.unwrap_or_else(|| "application/octet-stream".into());
    // Стримим файл чанками через ReaderStream, а не буферизуем целиком в RAM.
    // Прежний `Vec::with_capacity(file_size_bytes as usize)` (а) паниковал
    // capacity-overflow, если file_size_bytes в БД оказывался отрицательным
    // (panic="abort" → падение всего процесса), и (б) на APK до 200 МБ под
    // cgroup MemoryMax=512M выбивал OOM. Content-Length берём из фактического
    // размера файла на диске, не из (потенциально устаревшего) значения в БД.
    let on_disk_len = tokio::fs::metadata(&abs).await.ok().map(|m| m.len());
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, ct)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{original_name}\""),
        );
    if let Some(len) = on_disk_len {
        builder = builder.header(header::CONTENT_LENGTH, len);
    }
    match builder.body(Body::from_stream(tokio_util::io::ReaderStream::new(file))) {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!(error = %e, "signed-url response build");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal").into_response()
        }
    }
}
