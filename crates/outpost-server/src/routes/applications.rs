//! `/api/v1/applications` — APK / artifact catalog + versions.
//!
//! Upload of binary content is handled in P5 (`POST .../upload` and
//! signed download URLs); P4 covers metadata CRUD.

use crate::auth_extract::AuthUser;
use crate::error::ApiError;
use crate::page::{Page, PageParams};
use crate::permission::require_permission;
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/applications", get(list).post(create))
        .route(
            "/api/v1/applications/{id}",
            get(get_one).put(update).delete(delete),
        )
        .route(
            "/api/v1/applications/{id}/versions",
            get(list_versions).post(create_version),
        )
        .route(
            "/api/v1/applications/{id}/versions/{version_id}",
            axum::routing::delete(delete_version),
        )
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Application {
    pub id: i64,
    pub customer_id: i64,
    pub package_name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub kind: String,
    pub icon_path: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct AppVersion {
    pub id: i64,
    pub application_id: i64,
    pub version_code: i64,
    pub version_name: String,
    pub file_path: String,
    pub file_size_bytes: i64,
    pub sha256: String,
    pub min_sdk: Option<i64>,
    pub is_active: bool,
    pub notes: Option<String>,
    pub uploaded_by: Option<i64>,
    pub uploaded_at: DateTime<Utc>,
}

async fn list(
    user: AuthUser,
    State(state): State<AppState>,
    Query(page): Query<PageParams>,
) -> Result<Json<Page<Application>>, ApiError> {
    require_permission(&state.db, user.role_id, "applications.read").await?;
    let (limit, offset) = page.clamp();
    let items: Vec<Application> = sqlx::query_as::<_, Application>(
        "SELECT id, customer_id, package_name, display_name, description, kind, icon_path, \
                created_at, updated_at \
         FROM applications WHERE customer_id = ? ORDER BY package_name LIMIT ? OFFSET ?",
    )
    .bind(user.customer_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM applications WHERE customer_id = ?")
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
) -> Result<Json<Application>, ApiError> {
    require_permission(&state.db, user.role_id, "applications.read").await?;
    let app: Option<Application> = sqlx::query_as::<_, Application>(
        "SELECT id, customer_id, package_name, display_name, description, kind, icon_path, \
                created_at, updated_at \
         FROM applications WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?;
    app.map(Json).ok_or(ApiError::NotFound)
}

#[derive(Debug, Deserialize)]
pub struct CreateApplicationRequest {
    pub package_name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    #[serde(default = "default_kind")]
    pub kind: String,
}

fn default_kind() -> String {
    "apk".to_string()
}

async fn create(
    user: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<CreateApplicationRequest>,
) -> Result<(StatusCode, Json<Application>), ApiError> {
    require_permission(&state.db, user.role_id, "applications.write").await?;
    if req.package_name.trim().is_empty() {
        return Err(ApiError::BadRequest("package_name is required".into()));
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO applications (customer_id, package_name, display_name, description, kind) \
         VALUES (?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(user.customer_id)
    .bind(&req.package_name)
    .bind(&req.display_name)
    .bind(&req.description)
    .bind(&req.kind)
    .fetch_one(&state.db)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            ApiError::BadRequest(format!("application '{}' already exists", req.package_name))
        }
        _ => ApiError::from(e),
    })?;
    let app: Application = sqlx::query_as::<_, Application>(
        "SELECT id, customer_id, package_name, display_name, description, kind, icon_path, \
                created_at, updated_at FROM applications WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, Json(app)))
}

#[derive(Debug, Deserialize)]
pub struct UpdateApplicationRequest {
    pub display_name: Option<String>,
    pub description: Option<String>,
}

async fn update(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateApplicationRequest>,
) -> Result<Json<Application>, ApiError> {
    require_permission(&state.db, user.role_id, "applications.write").await?;
    let _existing: Application = sqlx::query_as::<_, Application>(
        "SELECT id, customer_id, package_name, display_name, description, kind, icon_path, \
                created_at, updated_at \
         FROM applications WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;

    sqlx::query(
        "UPDATE applications SET \
            display_name = COALESCE(?, display_name), \
            description  = COALESCE(?, description), \
            updated_at   = datetime('now') \
         WHERE id = ?",
    )
    .bind(&req.display_name)
    .bind(&req.description)
    .bind(id)
    .execute(&state.db)
    .await?;

    let app: Application = sqlx::query_as::<_, Application>(
        "SELECT id, customer_id, package_name, display_name, description, kind, icon_path, \
                created_at, updated_at FROM applications WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(app))
}

async fn delete(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    require_permission(&state.db, user.role_id, "applications.write").await?;
    let res = sqlx::query("DELETE FROM applications WHERE id = ? AND customer_id = ?")
        .bind(id)
        .bind(user.customer_id)
        .execute(&state.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn list_versions(
    user: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<i64>,
) -> Result<Json<Vec<AppVersion>>, ApiError> {
    require_permission(&state.db, user.role_id, "applications.read").await?;
    let items: Vec<AppVersion> = sqlx::query_as::<_, AppVersion>(
        "SELECT v.id, v.application_id, v.version_code, v.version_name, v.file_path, \
                v.file_size_bytes, v.sha256, v.min_sdk, v.is_active, v.notes, \
                v.uploaded_by, v.uploaded_at \
         FROM application_versions v \
         JOIN applications a ON a.id = v.application_id \
         WHERE v.application_id = ? AND a.customer_id = ? \
         ORDER BY v.version_code DESC",
    )
    .bind(app_id)
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(items))
}

#[derive(Debug, Deserialize)]
pub struct CreateVersionRequest {
    pub version_code: i64,
    pub version_name: String,
    pub file_path: String,
    pub file_size_bytes: i64,
    pub sha256: String,
    pub min_sdk: Option<i64>,
    #[serde(default)]
    pub is_active: bool,
    pub notes: Option<String>,
}

async fn create_version(
    user: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<i64>,
    Json(req): Json<CreateVersionRequest>,
) -> Result<(StatusCode, Json<AppVersion>), ApiError> {
    require_permission(&state.db, user.role_id, "applications.write").await?;
    let owned: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM applications WHERE id = ? AND customer_id = ?")
            .bind(app_id)
            .bind(user.customer_id)
            .fetch_optional(&state.db)
            .await?;
    if owned.is_none() {
        return Err(ApiError::NotFound);
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO application_versions \
            (application_id, version_code, version_name, file_path, file_size_bytes, sha256, \
             min_sdk, is_active, notes, uploaded_by) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(app_id)
    .bind(req.version_code)
    .bind(&req.version_name)
    .bind(&req.file_path)
    .bind(req.file_size_bytes)
    .bind(&req.sha256)
    .bind(req.min_sdk)
    .bind(req.is_active)
    .bind(&req.notes)
    .bind(user.id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => ApiError::BadRequest(format!(
            "version_code {} already exists for this application",
            req.version_code
        )),
        _ => ApiError::from(e),
    })?;
    let v: AppVersion = sqlx::query_as::<_, AppVersion>(
        "SELECT id, application_id, version_code, version_name, file_path, \
                file_size_bytes, sha256, min_sdk, is_active, notes, uploaded_by, uploaded_at \
         FROM application_versions WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, Json(v)))
}

async fn delete_version(
    user: AuthUser,
    State(state): State<AppState>,
    Path((app_id, version_id)): Path<(i64, i64)>,
) -> Result<StatusCode, ApiError> {
    require_permission(&state.db, user.role_id, "applications.write").await?;
    let res = sqlx::query(
        "DELETE FROM application_versions \
         WHERE id = ? AND application_id = ? \
         AND EXISTS (SELECT 1 FROM applications WHERE id = ? AND customer_id = ?)",
    )
    .bind(version_id)
    .bind(app_id)
    .bind(app_id)
    .bind(user.customer_id)
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}
