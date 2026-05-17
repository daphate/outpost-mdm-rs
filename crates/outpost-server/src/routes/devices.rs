//! `/api/v1/devices` — list / get / create / update / delete + telemetry.

use crate::auth_extract::AuthUser;
use crate::error::ApiError;
use crate::page::{Page, PageParams};
use crate::permission::require_permission;
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/devices", get(list).post(create))
        .route(
            "/api/v1/devices/{id}",
            get(get_one).put(update).delete(delete),
        )
        .route("/api/v1/devices/{id}/telemetry", get(get_telemetry))
        // v0.13 (MDM-DEVICE-CONTROL-CONTRACT §1.4):
        .route("/api/v1/devices/{id}/state", get(get_state))
        .route("/api/v1/devices/{id}/config", axum::routing::post(post_config))
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Device {
    pub id: i64,
    pub customer_id: i64,
    pub serial: String,
    pub display_name: Option<String>,
    pub app_version: Option<String>,
    pub os_version: Option<String>,
    pub battery_pct: Option<i64>,
    pub last_lat: Option<f64>,
    pub last_lon: Option<f64>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub is_online: bool,
    pub is_enrolled: bool,
    pub is_active: bool,
    pub metadata_json: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

async fn list(
    user: AuthUser,
    State(state): State<AppState>,
    Query(page): Query<PageParams>,
) -> Result<Json<Page<Device>>, ApiError> {
    require_permission(&state.db, user.role_id, "devices.read").await?;
    let (limit, offset) = page.clamp();

    let items: Vec<Device> = sqlx::query_as::<_, Device>(
        "SELECT id, customer_id, serial, display_name, app_version, os_version, \
                battery_pct, last_lat, last_lon, last_seen_at, is_online, is_enrolled, \
                is_active, metadata_json, created_at, updated_at \
         FROM devices WHERE customer_id = ? ORDER BY id DESC LIMIT ? OFFSET ?",
    )
    .bind(user.customer_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM devices WHERE customer_id = ?")
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
) -> Result<Json<Device>, ApiError> {
    require_permission(&state.db, user.role_id, "devices.read").await?;
    let device: Option<Device> = sqlx::query_as::<_, Device>(
        "SELECT id, customer_id, serial, display_name, app_version, os_version, \
                battery_pct, last_lat, last_lon, last_seen_at, is_online, is_enrolled, \
                is_active, metadata_json, created_at, updated_at \
         FROM devices WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?;
    device.map(Json).ok_or(ApiError::NotFound)
}

#[derive(Debug, Deserialize)]
pub struct CreateDeviceRequest {
    pub serial: String,
    pub display_name: Option<String>,
}

async fn create(
    user: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<CreateDeviceRequest>,
) -> Result<(axum::http::StatusCode, Json<Device>), ApiError> {
    require_permission(&state.db, user.role_id, "devices.write").await?;
    if req.serial.trim().is_empty() {
        return Err(ApiError::BadRequest("serial is required".into()));
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO devices (customer_id, serial, display_name) \
         VALUES (?, ?, ?) RETURNING id",
    )
    .bind(user.customer_id)
    .bind(&req.serial)
    .bind(&req.display_name)
    .fetch_one(&state.db)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => ApiError::BadRequest(format!(
            "device with serial '{}' already exists",
            req.serial
        )),
        _ => ApiError::from(e),
    })?;

    let device: Device = sqlx::query_as::<_, Device>(
        "SELECT id, customer_id, serial, display_name, app_version, os_version, \
                battery_pct, last_lat, last_lon, last_seen_at, is_online, is_enrolled, \
                is_active, metadata_json, created_at, updated_at \
         FROM devices WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok((axum::http::StatusCode::CREATED, Json(device)))
}

#[derive(Debug, Deserialize)]
pub struct UpdateDeviceRequest {
    pub display_name: Option<String>,
    pub is_active: Option<bool>,
}

async fn update(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateDeviceRequest>,
) -> Result<Json<Device>, ApiError> {
    require_permission(&state.db, user.role_id, "devices.write").await?;
    // Verify existence + tenant ownership before mutating.
    let _existing: Device = sqlx::query_as::<_, Device>(
        "SELECT id, customer_id, serial, display_name, app_version, os_version, \
                battery_pct, last_lat, last_lon, last_seen_at, is_online, is_enrolled, \
                is_active, metadata_json, created_at, updated_at \
         FROM devices WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;

    sqlx::query(
        "UPDATE devices SET \
            display_name = COALESCE(?, display_name), \
            is_active    = COALESCE(?, is_active), \
            updated_at   = datetime('now') \
         WHERE id = ?",
    )
    .bind(&req.display_name)
    .bind(req.is_active)
    .bind(id)
    .execute(&state.db)
    .await?;

    let device: Device = sqlx::query_as::<_, Device>(
        "SELECT id, customer_id, serial, display_name, app_version, os_version, \
                battery_pct, last_lat, last_lon, last_seen_at, is_online, is_enrolled, \
                is_active, metadata_json, created_at, updated_at \
         FROM devices WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(device))
}

async fn delete(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<axum::http::StatusCode, ApiError> {
    require_permission(&state.db, user.role_id, "devices.write").await?;
    let res = sqlx::query("DELETE FROM devices WHERE id = ? AND customer_id = ?")
        .bind(id)
        .bind(user.customer_id)
        .execute(&state.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Telemetry {
    pub battery_pct: Option<i64>,
    pub last_lat: Option<f64>,
    pub last_lon: Option<f64>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub is_online: bool,
}

async fn get_telemetry(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Telemetry>, ApiError> {
    require_permission(&state.db, user.role_id, "devices.read").await?;
    let t: Option<Telemetry> = sqlx::query_as::<_, Telemetry>(
        "SELECT battery_pct, last_lat, last_lon, last_seen_at, is_online \
         FROM devices WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?;
    t.map(Json).ok_or(ApiError::NotFound)
}

// ----- v0.13 Settings Sync (MDM-DEVICE-CONTROL-CONTRACT §1.4) --------------

/// `GET /api/v1/devices/{id}/state` — что устройство сообщало о своих
/// ModelPreferences в последнем /sync. Возвращает {version, seen_at, state}
/// в формате, идентичном request body field `current_state`.
#[derive(Debug, Serialize)]
pub struct DeviceState {
    pub version: i64,
    pub seen_at: Option<String>,
    pub state: serde_json::Value,
}

async fn get_state(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<DeviceState>, ApiError> {
    require_permission(&state.db, user.role_id, "devices.read").await?;
    let row: Option<(String, i64, Option<String>)> = sqlx::query_as(
        "SELECT current_state_json, current_state_version, current_state_seen_at \
         FROM devices WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?;
    let Some((json_str, version, seen_at)) = row else {
        return Err(ApiError::NotFound);
    };
    let value: serde_json::Value =
        serde_json::from_str(&json_str).unwrap_or(serde_json::Value::Null);
    Ok(Json(DeviceState {
        version,
        seen_at,
        state: value,
    }))
}

/// `POST /api/v1/devices/{id}/config` — admin отправляет patch
/// ModelPreferences-настроек устройству. Internally создаёт push_message
/// с command='update-config'. Устройство применит на следующем /sync
/// (≤30 мин default polling interval).
#[derive(Debug, Deserialize)]
pub struct ConfigUpdateRequest {
    pub payload: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct ConfigUpdateResponse {
    pub command_id: i64,
}

/// Минимальный versionCode клиента который понимает `update-config` command.
/// Rc42 b37 = 178 (см. MDM-DEVICE-CONTROL-CONTRACT.md §4 «Migration & backward
/// compatibility»). Старые клиенты не имеют SyncCommandDispatcher и не смогут
/// обработать команду — admin'у возвращаем 400 чтобы не плодить мёртвые
/// push_messages.
const MIN_VERSION_CODE_FOR_UPDATE_CONFIG: i64 = 178;

async fn post_config(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<ConfigUpdateRequest>,
) -> Result<(axum::http::StatusCode, Json<ConfigUpdateResponse>), ApiError> {
    require_permission(&state.db, user.role_id, "devices.write").await?;
    // Verify device exists в этом customer-scope + проверяем app_version_code.
    let row: Option<(Option<i64>,)> = sqlx::query_as(
        "SELECT app_version_code FROM devices WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?;
    let Some((app_version_code,)) = row else {
        return Err(ApiError::NotFound);
    };
    // Backward-compat gate: устройство ещё не дотягивает до b37+ → не сможет
    // обработать update-config. Возвращаем 400 чтобы admin понимал.
    match app_version_code {
        None => {
            return Err(ApiError::BadRequest(
                "device has not reported app_version_code yet; нужно дождаться первого /sync с rc42 b37+ клиентом".into(),
            ));
        }
        Some(v) if v < MIN_VERSION_CODE_FOR_UPDATE_CONFIG => {
            return Err(ApiError::BadRequest(format!(
                "device on app_version_code={v}, requires >= {MIN_VERSION_CODE_FOR_UPDATE_CONFIG} (rc42 b37+) for update-config support"
            )));
        }
        Some(_) => {}
    }
    // payload — JSON object, e.g. {"preferred_llm": "qwen2-vl-2b-instruct-q4_k_m.gguf"}.
    // Не валидируем ключи здесь — клиент в SyncCommandDispatcher знает
    // mapping; неизвестные ключи возвращаются в ACK как error.
    let payload_json = serde_json::to_string(&req.payload).map_err(|e| {
        ApiError::BadRequest(format!("payload not serializable: {e}"))
    })?;
    let cmd_id: i64 = sqlx::query_scalar(
        "INSERT INTO push_messages (customer_id, device_id, command, payload_json, status) \
         VALUES (?, ?, 'update-config', ?, 'pending') \
         RETURNING id",
    )
    .bind(user.customer_id)
    .bind(id)
    .bind(payload_json)
    .fetch_one(&state.db)
    .await?;
    Ok((
        axum::http::StatusCode::CREATED,
        Json(ConfigUpdateResponse { command_id: cmd_id }),
    ))
}
