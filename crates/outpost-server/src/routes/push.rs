//! `/api/v1/push` — schedule and inspect push commands to devices.
//!
//! The scheduler tick task that fans `push_schedule` → `push_messages` is
//! implemented in P6.

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
        .route("/api/v1/push/messages", get(list_messages))
        .route(
            "/api/v1/push/messages/{id}",
            get(get_message).delete(cancel_message),
        )
        .route(
            "/api/v1/push/schedule",
            get(list_schedule).post(create_schedule),
        )
        .route(
            "/api/v1/push/schedule/{id}",
            axum::routing::delete(cancel_schedule),
        )
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PushMessage {
    pub id: i64,
    pub customer_id: i64,
    pub device_id: i64,
    pub command: String,
    pub payload_json: String,
    pub status: String,
    pub schedule_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub sent_at: Option<DateTime<Utc>>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PushSchedule {
    pub id: i64,
    pub customer_id: i64,
    pub device_id: Option<i64>,
    pub group_id: Option<i64>,
    pub configuration_id: Option<i64>,
    pub command: String,
    pub payload_json: String,
    pub due_at: Option<DateTime<Utc>>,
    pub cron_expr: Option<String>,
    pub status: String,
    pub created_by: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

async fn list_messages(
    user: AuthUser,
    State(state): State<AppState>,
    Query(page): Query<PageParams>,
) -> Result<Json<Page<PushMessage>>, ApiError> {
    require_permission(&state.db, user.role_id, "push.send").await?;
    let (limit, offset) = page.clamp();
    let items: Vec<PushMessage> = sqlx::query_as::<_, PushMessage>(
        "SELECT id, customer_id, device_id, command, payload_json, status, schedule_id, \
                created_at, sent_at, delivered_at, last_error \
         FROM push_messages WHERE customer_id = ? \
         ORDER BY created_at DESC LIMIT ? OFFSET ?",
    )
    .bind(user.customer_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM push_messages WHERE customer_id = ?")
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

async fn get_message(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<PushMessage>, ApiError> {
    require_permission(&state.db, user.role_id, "push.send").await?;
    let m: Option<PushMessage> = sqlx::query_as::<_, PushMessage>(
        "SELECT id, customer_id, device_id, command, payload_json, status, schedule_id, \
                created_at, sent_at, delivered_at, last_error \
         FROM push_messages WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?;
    m.map(Json).ok_or(ApiError::NotFound)
}

async fn cancel_message(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    require_permission(&state.db, user.role_id, "push.send").await?;
    let res = sqlx::query(
        "UPDATE push_messages SET status = 'cancelled' \
         WHERE id = ? AND customer_id = ? AND status = 'pending'",
    )
    .bind(id)
    .bind(user.customer_id)
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn list_schedule(
    user: AuthUser,
    State(state): State<AppState>,
    Query(page): Query<PageParams>,
) -> Result<Json<Page<PushSchedule>>, ApiError> {
    require_permission(&state.db, user.role_id, "push.send").await?;
    let (limit, offset) = page.clamp();
    let items: Vec<PushSchedule> = sqlx::query_as::<_, PushSchedule>(
        "SELECT id, customer_id, device_id, group_id, configuration_id, command, payload_json, \
                due_at, cron_expr, status, created_by, created_at, last_run_at, last_error \
         FROM push_schedule WHERE customer_id = ? \
         ORDER BY created_at DESC LIMIT ? OFFSET ?",
    )
    .bind(user.customer_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM push_schedule WHERE customer_id = ?")
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

#[derive(Debug, Deserialize)]
pub struct CreateScheduleRequest {
    pub device_id: Option<i64>,
    pub group_id: Option<i64>,
    pub configuration_id: Option<i64>,
    pub command: String,
    #[serde(default = "default_payload")]
    pub payload_json: String,
    pub due_at: Option<String>,
    pub cron_expr: Option<String>,
}

fn default_payload() -> String {
    "{}".to_string()
}

async fn create_schedule(
    user: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<CreateScheduleRequest>,
) -> Result<(StatusCode, Json<PushSchedule>), ApiError> {
    require_permission(&state.db, user.role_id, "push.send").await?;
    if req.command.trim().is_empty() {
        return Err(ApiError::BadRequest("command is required".into()));
    }
    serde_json::from_str::<serde_json::Value>(&req.payload_json)
        .map_err(|e| ApiError::BadRequest(format!("payload_json is invalid JSON: {e}")))?;
    if req.due_at.is_none() && req.cron_expr.is_none() {
        return Err(ApiError::BadRequest(
            "at least one of due_at or cron_expr is required".into(),
        ));
    }
    // Targeting: at most one of (device_id, group_id, configuration_id)
    let targets = [req.device_id, req.group_id, req.configuration_id]
        .iter()
        .filter(|o| o.is_some())
        .count();
    if targets > 1 {
        return Err(ApiError::BadRequest(
            "at most one of device_id / group_id / configuration_id may be set".into(),
        ));
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO push_schedule \
            (customer_id, device_id, group_id, configuration_id, command, payload_json, \
             due_at, cron_expr, created_by) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(user.customer_id)
    .bind(req.device_id)
    .bind(req.group_id)
    .bind(req.configuration_id)
    .bind(&req.command)
    .bind(&req.payload_json)
    .bind(&req.due_at)
    .bind(&req.cron_expr)
    .bind(user.id)
    .fetch_one(&state.db)
    .await?;
    let s: PushSchedule = sqlx::query_as::<_, PushSchedule>(
        "SELECT id, customer_id, device_id, group_id, configuration_id, command, payload_json, \
                due_at, cron_expr, status, created_by, created_at, last_run_at, last_error \
         FROM push_schedule WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, Json(s)))
}

async fn cancel_schedule(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    require_permission(&state.db, user.role_id, "push.send").await?;
    let res = sqlx::query(
        "UPDATE push_schedule SET status = 'cancelled' \
         WHERE id = ? AND customer_id = ? AND status != 'cancelled'",
    )
    .bind(id)
    .bind(user.customer_id)
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}
