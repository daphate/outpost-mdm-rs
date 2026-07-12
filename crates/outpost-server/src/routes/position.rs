//! `POST /api/v1/position` — лёгкий приёмник позиции от устройства (Ф2).
//!
//! Быстрый агент телеметрии шлёт сюда координаты раз в несколько секунд.
//! В отличие от `/api/v1/sync`, обработчик не трогает очередь push-команд и
//! снимок состояния — только обновляет последний фикс, ведёт историю с
//! прореживанием и публикует событие в живую шину карт. Поэтому частые
//! вызовы безопасны: команды не помечаются отправленными и не теряются.

use crate::auth_extract::AuthDevice;
use crate::error::ApiError;
use crate::routes::enrollment::should_append_position;
use crate::state::{AppState, LiveEvent};
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use serde::Deserialize;
use std::sync::Arc;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/position", post(ingest))
}

#[derive(Debug, Deserialize)]
pub struct PositionReport {
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
}

async fn ingest(
    device: AuthDevice,
    State(state): State<AppState>,
    Json(req): Json<PositionReport>,
) -> Result<StatusCode, ApiError> {
    if !(-90.0..=90.0).contains(&req.lat) || !(-180.0..=180.0).contains(&req.lon) {
        return Err(ApiError::BadRequest("lat/lon out of range".into()));
    }

    // Прежний фикс — для прореживания истории.
    let prev: (Option<f64>, Option<f64>, Option<String>) = sqlx::query_as(
        "SELECT last_lat, last_lon, last_track_at FROM devices WHERE id = ?",
    )
    .bind(device.id)
    .fetch_optional(&state.db)
    .await?
    .unwrap_or((None, None, None));

    sqlx::query(
        "UPDATE devices SET \
            last_lat      = ?, \
            last_lon      = ?, \
            last_alt      = COALESCE(?, last_alt), \
            last_bearing  = COALESCE(?, last_bearing), \
            last_speed    = COALESCE(?, last_speed), \
            last_accuracy = COALESCE(?, last_accuracy), \
            battery_pct   = COALESCE(?, battery_pct), \
            last_seen_at  = datetime('now'), \
            is_online     = 1, \
            updated_at    = datetime('now') \
         WHERE id = ?",
    )
    .bind(req.lat)
    .bind(req.lon)
    .bind(req.alt)
    .bind(req.bearing)
    .bind(req.speed)
    .bind(req.accuracy)
    .bind(req.battery_pct)
    .bind(device.id)
    .execute(&state.db)
    .await?;

    if should_append_position(prev.0, prev.1, prev.2.as_deref(), req.lat, req.lon) {
        sqlx::query(
            "INSERT INTO device_positions \
               (customer_id, device_id, lat, lon, alt, bearing, speed, accuracy) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(device.customer_id)
        .bind(device.id)
        .bind(req.lat)
        .bind(req.lon)
        .bind(req.alt)
        .bind(req.bearing)
        .bind(req.speed)
        .bind(req.accuracy)
        .execute(&state.db)
        .await?;
        sqlx::query("UPDATE devices SET last_track_at = datetime('now') WHERE id = ?")
            .bind(device.id)
            .execute(&state.db)
            .await?;
    }

    let payload = serde_json::json!({
        "op": "move",
        "device_id": device.id,
        "lat": req.lat, "lon": req.lon,
        "alt": req.alt, "bearing": req.bearing, "speed": req.speed,
    })
    .to_string();
    state.live.publish(LiveEvent {
        customer_id: device.customer_id,
        name: "position",
        data: Arc::from(payload.as_str()),
    });

    Ok(StatusCode::NO_CONTENT)
}
