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
    Ok(StatusCode::NO_CONTENT)
}

/// Общий приём координат: проверка диапазона, обновление последнего фикса,
/// история с прореживанием и публикация события `position` в живую шину.
/// Используется как быстрым каналом `/api/v1/position`, так и игровым
/// каналом `/api/v1/player/state` (Ф4), чтобы не дублировать логику.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_position(
    state: &AppState,
    device_id: i64,
    customer_id: i64,
    lat: f64,
    lon: f64,
    alt: Option<f64>,
    bearing: Option<f64>,
    speed: Option<f64>,
    accuracy: Option<f64>,
    battery_pct: Option<i64>,
) -> Result<(), ApiError> {
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(ApiError::BadRequest("lat/lon out of range".into()));
    }

    // Прежний фикс — для прореживания истории.
    let prev: (Option<f64>, Option<f64>, Option<String>) = sqlx::query_as(
        "SELECT last_lat, last_lon, last_track_at FROM devices WHERE id = ?",
    )
    .bind(device_id)
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
    .bind(lat)
    .bind(lon)
    .bind(alt)
    .bind(bearing)
    .bind(speed)
    .bind(accuracy)
    .bind(battery_pct)
    .bind(device_id)
    .execute(&state.db)
    .await?;

    if should_append_position(prev.0, prev.1, prev.2.as_deref(), lat, lon) {
        sqlx::query(
            "INSERT INTO device_positions \
               (customer_id, device_id, lat, lon, alt, bearing, speed, accuracy) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(customer_id)
        .bind(device_id)
        .bind(lat)
        .bind(lon)
        .bind(alt)
        .bind(bearing)
        .bind(speed)
        .bind(accuracy)
        .execute(&state.db)
        .await?;
        sqlx::query("UPDATE devices SET last_track_at = datetime('now') WHERE id = ?")
            .bind(device_id)
            .execute(&state.db)
            .await?;
    }

    let payload = serde_json::json!({
        "op": "move",
        "device_id": device_id,
        "lat": lat, "lon": lon,
        "alt": alt, "bearing": bearing, "speed": speed,
    })
    .to_string();
    state.live.publish(LiveEvent {
        customer_id,
        name: "position",
        data: Arc::from(payload.as_str()),
    });

    Ok(())
}
