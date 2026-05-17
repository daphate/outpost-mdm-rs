//! `/api/v1/configurations` — MDM configuration bundles + app assignments.

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
        .route("/api/v1/configurations", get(list).post(create))
        .route(
            "/api/v1/configurations/{id}",
            get(get_one).put(update).delete(delete),
        )
        .route(
            "/api/v1/configurations/{id}/applications",
            get(list_apps).post(add_app),
        )
        .route(
            "/api/v1/configurations/{id}/applications/{app_id}",
            axum::routing::delete(remove_app),
        )
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Configuration {
    pub id: i64,
    pub customer_id: i64,
    pub name: String,
    pub description: Option<String>,
    pub settings_json: String,
    pub kiosk_package: Option<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ConfigApplication {
    pub id: i64,
    pub configuration_id: i64,
    pub application_id: i64,
    pub application_version_id: Option<i64>,
    pub mode: String,
    pub sort_order: i64,
    pub created_at: DateTime<Utc>,
}

async fn list(
    user: AuthUser,
    State(state): State<AppState>,
    Query(page): Query<PageParams>,
) -> Result<Json<Page<Configuration>>, ApiError> {
    require_permission(&state.db, user.role_id, "configurations.read").await?;
    let (limit, offset) = page.clamp();
    let items: Vec<Configuration> = sqlx::query_as::<_, Configuration>(
        "SELECT id, customer_id, name, description, settings_json, kiosk_package, \
                is_active, created_at, updated_at \
         FROM configurations WHERE customer_id = ? ORDER BY name LIMIT ? OFFSET ?",
    )
    .bind(user.customer_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM configurations WHERE customer_id = ?")
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
) -> Result<Json<Configuration>, ApiError> {
    require_permission(&state.db, user.role_id, "configurations.read").await?;
    let c: Option<Configuration> = sqlx::query_as::<_, Configuration>(
        "SELECT id, customer_id, name, description, settings_json, kiosk_package, \
                is_active, created_at, updated_at \
         FROM configurations WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?;
    c.map(Json).ok_or(ApiError::NotFound)
}

#[derive(Debug, Deserialize)]
pub struct CreateConfigRequest {
    pub name: String,
    pub description: Option<String>,
    #[serde(default = "default_settings")]
    pub settings_json: String,
    pub kiosk_package: Option<String>,
}

fn default_settings() -> String {
    "{}".to_string()
}

async fn create(
    user: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<CreateConfigRequest>,
) -> Result<(StatusCode, Json<Configuration>), ApiError> {
    require_permission(&state.db, user.role_id, "configurations.write").await?;
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    // Validate settings_json is parseable JSON.
    serde_json::from_str::<serde_json::Value>(&req.settings_json)
        .map_err(|e| ApiError::BadRequest(format!("settings_json is invalid JSON: {e}")))?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO configurations (customer_id, name, description, settings_json, kiosk_package) \
         VALUES (?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(user.customer_id)
    .bind(&req.name)
    .bind(&req.description)
    .bind(&req.settings_json)
    .bind(&req.kiosk_package)
    .fetch_one(&state.db)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            ApiError::BadRequest(format!("configuration '{}' already exists", req.name))
        }
        _ => ApiError::from(e),
    })?;
    let c: Configuration = sqlx::query_as::<_, Configuration>(
        "SELECT id, customer_id, name, description, settings_json, kiosk_package, \
                is_active, created_at, updated_at FROM configurations WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, Json(c)))
}

#[derive(Debug, Deserialize)]
pub struct UpdateConfigRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub settings_json: Option<String>,
    pub kiosk_package: Option<String>,
    pub is_active: Option<bool>,
}

async fn update(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateConfigRequest>,
) -> Result<Json<Configuration>, ApiError> {
    require_permission(&state.db, user.role_id, "configurations.write").await?;
    let _existing: Configuration = sqlx::query_as::<_, Configuration>(
        "SELECT id, customer_id, name, description, settings_json, kiosk_package, \
                is_active, created_at, updated_at \
         FROM configurations WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;

    if let Some(ref s) = req.settings_json {
        serde_json::from_str::<serde_json::Value>(s)
            .map_err(|e| ApiError::BadRequest(format!("settings_json is invalid JSON: {e}")))?;
    }
    sqlx::query(
        "UPDATE configurations SET \
            name          = COALESCE(?, name), \
            description   = COALESCE(?, description), \
            settings_json = COALESCE(?, settings_json), \
            kiosk_package = COALESCE(?, kiosk_package), \
            is_active     = COALESCE(?, is_active), \
            updated_at    = datetime('now') \
         WHERE id = ?",
    )
    .bind(&req.name)
    .bind(&req.description)
    .bind(&req.settings_json)
    .bind(&req.kiosk_package)
    .bind(req.is_active)
    .bind(id)
    .execute(&state.db)
    .await?;
    let c: Configuration = sqlx::query_as::<_, Configuration>(
        "SELECT id, customer_id, name, description, settings_json, kiosk_package, \
                is_active, created_at, updated_at FROM configurations WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(c))
}

async fn delete(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    require_permission(&state.db, user.role_id, "configurations.write").await?;
    let res = sqlx::query("DELETE FROM configurations WHERE id = ? AND customer_id = ?")
        .bind(id)
        .bind(user.customer_id)
        .execute(&state.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn list_apps(
    user: AuthUser,
    State(state): State<AppState>,
    Path(cfg_id): Path<i64>,
) -> Result<Json<Vec<ConfigApplication>>, ApiError> {
    require_permission(&state.db, user.role_id, "configurations.read").await?;
    let items: Vec<ConfigApplication> = sqlx::query_as::<_, ConfigApplication>(
        "SELECT ca.id, ca.configuration_id, ca.application_id, ca.application_version_id, \
                ca.mode, ca.sort_order, ca.created_at \
         FROM configuration_applications ca \
         JOIN configurations c ON c.id = ca.configuration_id \
         WHERE ca.configuration_id = ? AND c.customer_id = ? \
         ORDER BY ca.sort_order, ca.id",
    )
    .bind(cfg_id)
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(items))
}

#[derive(Debug, Deserialize)]
pub struct AddAppRequest {
    pub application_id: i64,
    pub application_version_id: Option<i64>,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default)]
    pub sort_order: i64,
}

fn default_mode() -> String {
    "install".to_string()
}

async fn add_app(
    user: AuthUser,
    State(state): State<AppState>,
    Path(cfg_id): Path<i64>,
    Json(req): Json<AddAppRequest>,
) -> Result<StatusCode, ApiError> {
    require_permission(&state.db, user.role_id, "configurations.write").await?;
    let owned: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM configurations WHERE id = ? AND customer_id = ?")
            .bind(cfg_id)
            .bind(user.customer_id)
            .fetch_optional(&state.db)
            .await?;
    if owned.is_none() {
        return Err(ApiError::NotFound);
    }
    sqlx::query(
        "INSERT INTO configuration_applications \
            (configuration_id, application_id, application_version_id, mode, sort_order) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(cfg_id)
    .bind(req.application_id)
    .bind(req.application_version_id)
    .bind(&req.mode)
    .bind(req.sort_order)
    .execute(&state.db)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            ApiError::BadRequest("application is already attached to this configuration".into())
        }
        _ => ApiError::from(e),
    })?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_app(
    user: AuthUser,
    State(state): State<AppState>,
    Path((cfg_id, app_id)): Path<(i64, i64)>,
) -> Result<StatusCode, ApiError> {
    require_permission(&state.db, user.role_id, "configurations.write").await?;
    let res = sqlx::query(
        "DELETE FROM configuration_applications \
         WHERE configuration_id = ? AND application_id = ? \
         AND EXISTS (SELECT 1 FROM configurations WHERE id = ? AND customer_id = ?)",
    )
    .bind(cfg_id)
    .bind(app_id)
    .bind(cfg_id)
    .bind(user.customer_id)
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}
