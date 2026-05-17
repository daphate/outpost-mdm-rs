//! `/api/v1/stats` — fleet-level rollups.

use crate::auth_extract::AuthUser;
use crate::error::ApiError;
use crate::permission::require_permission;
use crate::state::AppState;
use axum::{Json, Router, extract::State, routing::get};
use serde::Serialize;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/stats/fleet", get(fleet))
}

#[derive(Debug, Serialize)]
pub struct FleetStats {
    pub devices_total: i64,
    pub devices_online: i64,
    pub devices_enrolled: i64,
    pub applications_total: i64,
    pub configurations_total: i64,
    pub push_pending: i64,
    pub push_sent_24h: i64,
}

async fn fleet(
    user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<FleetStats>, ApiError> {
    require_permission(&state.db, user.role_id, "devices.read").await?;
    let customer_id = user.customer_id;

    let devices_total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM devices WHERE customer_id = ?")
            .bind(customer_id)
            .fetch_one(&state.db)
            .await?;
    let devices_online: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM devices WHERE customer_id = ? AND is_online = 1")
            .bind(customer_id)
            .fetch_one(&state.db)
            .await?;
    let devices_enrolled: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM devices WHERE customer_id = ? AND is_enrolled = 1",
    )
    .bind(customer_id)
    .fetch_one(&state.db)
    .await?;
    let applications_total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM applications WHERE customer_id = ?")
            .bind(customer_id)
            .fetch_one(&state.db)
            .await?;
    let configurations_total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM configurations WHERE customer_id = ?")
            .bind(customer_id)
            .fetch_one(&state.db)
            .await?;
    let push_pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM push_messages WHERE customer_id = ? AND status = 'pending'",
    )
    .bind(customer_id)
    .fetch_one(&state.db)
    .await?;
    let push_sent_24h: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM push_messages \
         WHERE customer_id = ? AND status IN ('sent','delivered') \
         AND created_at >= datetime('now', '-1 day')",
    )
    .bind(customer_id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(FleetStats {
        devices_total,
        devices_online,
        devices_enrolled,
        applications_total,
        configurations_total,
        push_pending,
        push_sent_24h,
    }))
}
