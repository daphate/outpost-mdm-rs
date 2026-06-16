//! `/api/v1/groups` — device groups + membership assignment.

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
        .route("/api/v1/groups", get(list).post(create))
        .route(
            "/api/v1/groups/{id}",
            get(get_one).put(update).delete(delete),
        )
        .route(
            "/api/v1/groups/{id}/devices",
            get(list_devices).post(add_device),
        )
        .route(
            "/api/v1/groups/{id}/devices/{device_id}",
            axum::routing::delete(remove_device),
        )
        // v0.13: MDM-DEVICE-CONTROL-CONTRACT §1.4 «POST /api/v1/groups/{id}/config»
        // — fan-out update-config push_message на каждый device в группе.
        .route(
            "/api/v1/groups/{id}/config",
            axum::routing::post(post_group_config),
        )
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Group {
    pub id: i64,
    pub customer_id: i64,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

async fn list(
    user: AuthUser,
    State(state): State<AppState>,
    Query(page): Query<PageParams>,
) -> Result<Json<Page<Group>>, ApiError> {
    require_permission(&state.db, user.role_id, "groups.read").await?;
    let (limit, offset) = page.clamp();
    let items: Vec<Group> = sqlx::query_as::<_, Group>(
        "SELECT id, customer_id, name, description, created_at, updated_at \
         FROM groups WHERE customer_id = ? ORDER BY name LIMIT ? OFFSET ?",
    )
    .bind(user.customer_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM groups WHERE customer_id = ?")
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
) -> Result<Json<Group>, ApiError> {
    require_permission(&state.db, user.role_id, "groups.read").await?;
    let g: Option<Group> = sqlx::query_as::<_, Group>(
        "SELECT id, customer_id, name, description, created_at, updated_at \
         FROM groups WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?;
    g.map(Json).ok_or(ApiError::NotFound)
}

#[derive(Debug, Deserialize)]
pub struct CreateGroupRequest {
    pub name: String,
    pub description: Option<String>,
}

async fn create(
    user: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<CreateGroupRequest>,
) -> Result<(StatusCode, Json<Group>), ApiError> {
    require_permission(&state.db, user.role_id, "groups.write").await?;
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO groups (customer_id, name, description) VALUES (?, ?, ?) RETURNING id",
    )
    .bind(user.customer_id)
    .bind(&req.name)
    .bind(&req.description)
    .fetch_one(&state.db)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            ApiError::BadRequest(format!("group '{}' already exists", req.name))
        }
        _ => ApiError::from(e),
    })?;
    let g: Group = sqlx::query_as::<_, Group>(
        "SELECT id, customer_id, name, description, created_at, updated_at \
         FROM groups WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, Json(g)))
}

#[derive(Debug, Deserialize)]
pub struct UpdateGroupRequest {
    pub name: Option<String>,
    pub description: Option<String>,
}

async fn update(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateGroupRequest>,
) -> Result<Json<Group>, ApiError> {
    require_permission(&state.db, user.role_id, "groups.write").await?;
    let _existing: Group = sqlx::query_as::<_, Group>(
        "SELECT id, customer_id, name, description, created_at, updated_at \
         FROM groups WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    sqlx::query(
        "UPDATE groups SET \
            name        = COALESCE(?, name), \
            description = COALESCE(?, description), \
            updated_at  = datetime('now') \
         WHERE id = ?",
    )
    .bind(&req.name)
    .bind(&req.description)
    .bind(id)
    .execute(&state.db)
    .await?;
    let g: Group = sqlx::query_as::<_, Group>(
        "SELECT id, customer_id, name, description, created_at, updated_at \
         FROM groups WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(g))
}

async fn delete(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    require_permission(&state.db, user.role_id, "groups.write").await?;
    let res = sqlx::query("DELETE FROM groups WHERE id = ? AND customer_id = ?")
        .bind(id)
        .bind(user.customer_id)
        .execute(&state.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct DeviceRef {
    id: i64,
    serial: String,
    display_name: Option<String>,
}

async fn list_devices(
    user: AuthUser,
    State(state): State<AppState>,
    Path(group_id): Path<i64>,
) -> Result<Json<Vec<DeviceRef>>, ApiError> {
    require_permission(&state.db, user.role_id, "groups.read").await?;
    let items: Vec<DeviceRef> = sqlx::query_as::<_, DeviceRef>(
        "SELECT d.id, d.serial, d.display_name FROM devices d \
         JOIN device_groups dg ON dg.device_id = d.id \
         WHERE dg.group_id = ? AND d.customer_id = ? \
         ORDER BY d.serial",
    )
    .bind(group_id)
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(items))
}

#[derive(Debug, Deserialize)]
pub struct AddDeviceRequest {
    pub device_id: i64,
}

async fn add_device(
    user: AuthUser,
    State(state): State<AppState>,
    Path(group_id): Path<i64>,
    Json(req): Json<AddDeviceRequest>,
) -> Result<StatusCode, ApiError> {
    require_permission(&state.db, user.role_id, "groups.write").await?;
    // Verify both group and device are tenant-owned.
    let owned: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM groups WHERE id = ? AND customer_id = ?")
            .bind(group_id)
            .bind(user.customer_id)
            .fetch_optional(&state.db)
            .await?;
    if owned.is_none() {
        return Err(ApiError::NotFound);
    }
    let dev_owned: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM devices WHERE id = ? AND customer_id = ?")
            .bind(req.device_id)
            .bind(user.customer_id)
            .fetch_optional(&state.db)
            .await?;
    if dev_owned.is_none() {
        return Err(ApiError::BadRequest("device not found in tenant".into()));
    }
    sqlx::query("INSERT OR IGNORE INTO device_groups (device_id, group_id) VALUES (?, ?)")
        .bind(req.device_id)
        .bind(group_id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_device(
    user: AuthUser,
    State(state): State<AppState>,
    Path((group_id, device_id)): Path<(i64, i64)>,
) -> Result<StatusCode, ApiError> {
    require_permission(&state.db, user.role_id, "groups.write").await?;
    let res = sqlx::query(
        "DELETE FROM device_groups WHERE group_id = ? AND device_id = ? \
         AND EXISTS (SELECT 1 FROM groups WHERE id = ? AND customer_id = ?)",
    )
    .bind(group_id)
    .bind(device_id)
    .bind(group_id)
    .bind(user.customer_id)
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

// ----- v0.13 (MDM-DEVICE-CONTROL-CONTRACT §1.4) ------------------------------

/// rc42 b37+ minimum. Совпадает с константой в `routes/devices.rs`.
const MIN_VERSION_CODE_FOR_UPDATE_CONFIG: i64 = 178;

#[derive(Debug, Deserialize)]
pub struct GroupConfigRequest {
    /// JSON object — ModelPreferences patch. См. MDM-DEVICE-CONTROL-CONTRACT §1.3.
    pub payload: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct GroupConfigResponse {
    pub device_count: i64,
    pub eligible_count: i64,
    pub skipped_old_clients: i64,
    pub command_ids: Vec<i64>,
}

/// Fan-out `update-config` push_message на все устройства группы. Устройства
/// с `app_version_code < 178` (или без known versionCode) пропускаются —
/// учитываются в `skipped_old_clients`. Возвращаемый `command_ids` — IDs тех
/// push_messages которые реально созданы.
async fn post_group_config(
    user: AuthUser,
    State(state): State<AppState>,
    Path(group_id): Path<i64>,
    Json(req): Json<GroupConfigRequest>,
) -> Result<(StatusCode, Json<GroupConfigResponse>), ApiError> {
    require_permission(&state.db, user.role_id, "devices.write").await?;
    let owns: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM groups WHERE id = ? AND customer_id = ?")
            .bind(group_id)
            .bind(user.customer_id)
            .fetch_optional(&state.db)
            .await?;
    if owns.is_none() {
        return Err(ApiError::NotFound);
    }
    let payload_json = serde_json::to_string(&req.payload)
        .map_err(|e| ApiError::BadRequest(format!("payload not serializable: {e}")))?;
    // Берём все устройства группы + их app_version_code.
    let devices: Vec<(i64, Option<i64>)> = sqlx::query_as(
        "SELECT d.id, d.app_version_code \
         FROM devices d \
         JOIN device_groups dg ON dg.device_id = d.id \
         WHERE dg.group_id = ? AND d.customer_id = ?",
    )
    .bind(group_id)
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    let device_count = devices.len() as i64;
    let mut command_ids = Vec::new();
    let mut skipped_old = 0i64;
    for (dev_id, av_code) in &devices {
        match *av_code {
            Some(v) if v >= MIN_VERSION_CODE_FOR_UPDATE_CONFIG => {
                let cmd_id: i64 = sqlx::query_scalar(
                    "INSERT INTO push_messages (customer_id, device_id, command, payload_json, status) \
                     VALUES (?, ?, 'update-config', ?, 'pending') \
                     RETURNING id",
                )
                .bind(user.customer_id)
                .bind(dev_id)
                .bind(&payload_json)
                .fetch_one(&state.db)
                .await?;
                command_ids.push(cmd_id);
            }
            _ => skipped_old += 1,
        }
    }
    let eligible_count = command_ids.len() as i64;
    Ok((
        StatusCode::CREATED,
        Json(GroupConfigResponse {
            device_count,
            eligible_count,
            skipped_old_clients: skipped_old,
            command_ids,
        }),
    ))
}
