//! `/api/v1/settings` — installation-wide key/value settings.

use crate::auth_extract::AuthUser;
use crate::error::ApiError;
use crate::permission::require_permission;
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};
use serde::{Deserialize, Serialize};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/settings", get(list))
        .route("/api/v1/settings/{key}", get(get_one).put(set_one))
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Setting {
    pub key: String,
    pub value_json: String,
    pub description: Option<String>,
}

async fn list(
    user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<Setting>>, ApiError> {
    // Settings live in a single global namespace — any authenticated user
    // who can read configurations can read them.
    require_permission(&state.db, user.role_id, "configurations.read").await?;
    let items: Vec<Setting> = sqlx::query_as::<_, Setting>(
        "SELECT key, value_json, description FROM settings ORDER BY key",
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(items))
}

async fn get_one(
    user: AuthUser,
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<Json<Setting>, ApiError> {
    require_permission(&state.db, user.role_id, "configurations.read").await?;
    let s: Option<Setting> = sqlx::query_as::<_, Setting>(
        "SELECT key, value_json, description FROM settings WHERE key = ?",
    )
    .bind(&key)
    .fetch_optional(&state.db)
    .await?;
    s.map(Json).ok_or(ApiError::NotFound)
}

#[derive(Debug, Deserialize)]
pub struct SetSettingRequest {
    pub value_json: String,
    pub description: Option<String>,
}

async fn set_one(
    user: AuthUser,
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<SetSettingRequest>,
) -> Result<StatusCode, ApiError> {
    require_permission(&state.db, user.role_id, "configurations.write").await?;
    serde_json::from_str::<serde_json::Value>(&req.value_json)
        .map_err(|e| ApiError::BadRequest(format!("value_json is invalid JSON: {e}")))?;
    sqlx::query(
        "INSERT INTO settings (key, value_json, description, updated_at) \
         VALUES (?, ?, ?, datetime('now')) \
         ON CONFLICT(key) DO UPDATE SET \
            value_json = excluded.value_json, \
            description = COALESCE(excluded.description, settings.description), \
            updated_at = datetime('now')",
    )
    .bind(&key)
    .bind(&req.value_json)
    .bind(&req.description)
    .execute(&state.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
