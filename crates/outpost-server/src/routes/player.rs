//! `/api/v1/player/state` — игровой канал класса «игроки STALKER» (Ф4).
//!
//! Приложение net.afterday.compas шлёт сюда позицию вместе с игровым
//! состоянием (радиация, здоровье, угроза, артефакты). Позиция проходит через
//! общий приём [`crate::routes::position::apply_position`] (история + живая
//! шина), игровое состояние — upsert в `player_states` и публикация события
//! `player` в шину для вида `/map/players`.

use crate::auth_extract::AuthDevice;
use crate::error::ApiError;
use crate::routes::position::apply_position;
use crate::state::{AppState, LiveEvent};
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use serde::Deserialize;
use std::sync::Arc;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/player/state", post(ingest))
}

#[derive(Debug, Deserialize)]
pub struct PlayerReport {
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub alt: Option<f64>,
    #[serde(default)]
    pub bearing: Option<f64>,
    #[serde(default)]
    pub speed: Option<f64>,
    #[serde(default)]
    pub accuracy: Option<f64>,
    #[serde(default)]
    pub battery_pct: Option<i64>,
    // Игровое состояние.
    #[serde(default)]
    pub radiation: Option<f64>,
    #[serde(default)]
    pub health: Option<f64>,
    #[serde(default)]
    pub threat_level: Option<String>,
    #[serde(default)]
    pub artifacts: Option<i64>,
    /// Произвольное расширение (сериализуется как строка JSON).
    #[serde(default)]
    pub detail: Option<serde_json::Value>,
}

async fn ingest(
    device: AuthDevice,
    State(state): State<AppState>,
    Json(req): Json<PlayerReport>,
) -> Result<StatusCode, ApiError> {
    // Позиция + история + событие "position" — общий код с быстрым каналом.
    apply_position(
        &state,
        device.id,
        device.customer_id,
        req.lat,
        req.lon,
        req.alt,
        req.bearing,
        req.speed,
        req.accuracy,
        req.battery_pct,
    )
    .await?;

    let detail_str = req.detail.as_ref().map(|v| v.to_string());
    sqlx::query(
        "INSERT INTO player_states \
           (device_id, customer_id, radiation, health, threat_level, artifacts, detail_json, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, datetime('now')) \
         ON CONFLICT(device_id) DO UPDATE SET \
           customer_id = excluded.customer_id, \
           radiation = excluded.radiation, \
           health = excluded.health, \
           threat_level = excluded.threat_level, \
           artifacts = excluded.artifacts, \
           detail_json = excluded.detail_json, \
           updated_at = datetime('now')",
    )
    .bind(device.id)
    .bind(device.customer_id)
    .bind(req.radiation)
    .bind(req.health)
    .bind(&req.threat_level)
    .bind(req.artifacts)
    .bind(&detail_str)
    .execute(&state.db)
    .await?;

    let payload = serde_json::json!({
        "device_id": device.id,
        "radiation": req.radiation,
        "health": req.health,
        "threat_level": req.threat_level,
        "artifacts": req.artifacts,
    })
    .to_string();
    state.live.publish(LiveEvent {
        customer_id: device.customer_id,
        name: "player",
        data: Arc::from(payload.as_str()),
    });

    Ok(StatusCode::NO_CONTENT)
}
