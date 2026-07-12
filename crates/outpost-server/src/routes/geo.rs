//! `/api/v1/geo/*` — GeoJSON reads for the situational maps (Ф1).
//!
//! Current fleet positions come from the latest fix on `devices`; tracks from
//! `device_positions`; markers from `tactical_markers`. All customer-scoped;
//! positions additionally filterable by `class` / `unit_id` for per-view maps.

use crate::auth_extract::AuthUser;
use crate::error::ApiError;
use crate::permission::require_permission;
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/geo/positions", get(positions))
        .route("/api/v1/geo/markers", get(markers))
        .route("/api/v1/geo/devices/{id}/track", get(device_track))
        .route("/api/v1/geo/devices/{id}/metrics", get(device_metrics))
}

#[derive(Debug, Deserialize)]
pub struct PositionsQuery {
    #[serde(default)]
    pub class: Option<String>,
    #[serde(default)]
    pub unit_id: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct PosRow {
    id: i64,
    serial: String,
    display_name: Option<String>,
    device_class: String,
    unit_id: Option<i64>,
    last_lat: f64,
    last_lon: f64,
    last_alt: Option<f64>,
    last_bearing: Option<f64>,
    last_speed: Option<f64>,
    battery_pct: Option<i64>,
    is_online: bool,
    last_seen_at: Option<String>,
}

/// Current fleet positions (latest fix per device) as a GeoJSON FeatureCollection.
async fn positions(
    user: AuthUser,
    State(state): State<AppState>,
    Query(q): Query<PositionsQuery>,
) -> Result<Json<Value>, ApiError> {
    require_permission(&state.db, user.role_id, "devices.read").await?;
    let rows: Vec<PosRow> = sqlx::query_as::<_, PosRow>(
        "SELECT id, serial, display_name, device_class, unit_id, last_lat, last_lon, \
                last_alt, last_bearing, last_speed, battery_pct, is_online, last_seen_at \
         FROM devices \
         WHERE customer_id = ? AND last_lat IS NOT NULL AND last_lon IS NOT NULL \
           AND (? IS NULL OR device_class = ?) \
           AND (? IS NULL OR unit_id = ?) \
         ORDER BY id",
    )
    .bind(user.customer_id)
    .bind(&q.class)
    .bind(&q.class)
    .bind(q.unit_id)
    .bind(q.unit_id)
    .fetch_all(&state.db)
    .await?;
    let features: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "type": "Feature",
                "geometry": {"type": "Point", "coordinates": [r.last_lon, r.last_lat]},
                "properties": {
                    "device_id": r.id,
                    "serial": r.serial,
                    "display_name": r.display_name,
                    "device_class": r.device_class,
                    "unit_id": r.unit_id,
                    "alt": r.last_alt,
                    "bearing": r.last_bearing,
                    "speed": r.last_speed,
                    "battery_pct": r.battery_pct,
                    "is_online": r.is_online,
                    "last_seen_at": r.last_seen_at,
                }
            })
        })
        .collect();
    Ok(Json(json!({"type": "FeatureCollection", "features": features})))
}

#[derive(sqlx::FromRow)]
struct MarkerRow {
    id: String,
    kind: String,
    subtype: Option<String>,
    lat: f64,
    lon: f64,
    alt: Option<f64>,
    confidence: Option<String>,
    label: Option<String>,
    notes: Option<String>,
    reporter_device_id: Option<i64>,
    reporter_callsign: Option<String>,
    expires_at: Option<String>,
}

/// Active fleet tactical markers as a GeoJSON FeatureCollection.
async fn markers(user: AuthUser, State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    require_permission(&state.db, user.role_id, "devices.read").await?;
    let rows: Vec<MarkerRow> = sqlx::query_as::<_, MarkerRow>(
        "SELECT id, kind, subtype, lat, lon, alt, confidence, label, notes, \
                reporter_device_id, reporter_callsign, expires_at \
         FROM tactical_markers \
         WHERE customer_id = ? AND is_active = 1 \
           AND (expires_at IS NULL OR expires_at > datetime('now'))",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    let features: Vec<Value> = rows
        .into_iter()
        .map(|m| {
            json!({
                "type": "Feature",
                "geometry": {"type": "Point", "coordinates": [m.lon, m.lat]},
                "properties": {
                    "id": m.id, "kind": m.kind, "subtype": m.subtype,
                    "alt": m.alt, "confidence": m.confidence, "label": m.label, "notes": m.notes,
                    "reporter_device_id": m.reporter_device_id,
                    "reporter_callsign": m.reporter_callsign,
                    "expires_at": m.expires_at,
                }
            })
        })
        .collect();
    Ok(Json(json!({"type": "FeatureCollection", "features": features})))
}

/// Панель показателей на карте: последние значения каждой метрики устройства
/// (OTLP-приёмник) + счётчики за сутки. Использует тот же запрос «последняя
/// запись по каждому имени», что и страница /devices/{id}/telemetry.
async fn device_metrics(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    require_permission(&state.db, user.role_id, "devices.read").await?;
    let owned: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM devices WHERE id = ? AND customer_id = ?")
            .bind(id)
            .bind(user.customer_id)
            .fetch_optional(&state.db)
            .await?;
    if owned.is_none() {
        return Err(ApiError::NotFound);
    }

    #[derive(sqlx::FromRow)]
    struct MRow {
        name: String,
        value: f64,
        unit: Option<String>,
        ts: String,
    }
    let rows: Vec<MRow> = sqlx::query_as::<_, MRow>(
        "SELECT name, value, unit, ts FROM device_metrics WHERE device_id = ? \
         AND id IN (SELECT MAX(id) FROM device_metrics WHERE device_id = ? GROUP BY name) \
         ORDER BY name LIMIT 60",
    )
    .bind(id)
    .bind(id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let logs_24h: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM device_logs WHERE device_id = ? AND received_at >= datetime('now', '-1 day')",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    let errors_24h: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM device_logs WHERE device_id = ? AND severity_number >= 17 \
         AND received_at >= datetime('now', '-1 day')",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    let metrics: Vec<Value> = rows
        .into_iter()
        .map(|r| json!({"name": r.name, "value": r.value, "unit": r.unit, "ts": r.ts}))
        .collect();
    Ok(Json(json!({
        "device_id": id,
        "metrics": metrics,
        "logs_24h": logs_24h,
        "errors_24h": errors_24h,
    })))
}

#[derive(Debug, Deserialize)]
pub struct TrackQuery {
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// A single device's track (breadcrumb history) as a GeoJSON LineString Feature.
async fn device_track(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<TrackQuery>,
) -> Result<Json<Value>, ApiError> {
    require_permission(&state.db, user.role_id, "devices.read").await?;
    let owned: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM devices WHERE id = ? AND customer_id = ?")
            .bind(id)
            .bind(user.customer_id)
            .fetch_optional(&state.db)
            .await?;
    if owned.is_none() {
        return Err(ApiError::NotFound);
    }
    let limit = q.limit.unwrap_or(1000).clamp(1, 10_000);
    let since = q.since.unwrap_or_default();
    let rows: Vec<(f64, f64)> = sqlx::query_as(
        "SELECT lat, lon FROM device_positions \
         WHERE device_id = ? AND (? = '' OR ts >= ?) ORDER BY ts LIMIT ?",
    )
    .bind(id)
    .bind(&since)
    .bind(&since)
    .bind(limit)
    .fetch_all(&state.db)
    .await?;
    let coords: Vec<Value> = rows.iter().map(|(lat, lon)| json!([lon, lat])).collect();
    // A LineString needs >= 2 points; below that, emit null geometry (MapLibre skips it).
    let geometry = if coords.len() >= 2 {
        json!({"type": "LineString", "coordinates": coords})
    } else {
        Value::Null
    };
    Ok(Json(json!({
        "type": "Feature",
        "geometry": geometry,
        "properties": {"device_id": id, "points": rows.len()}
    })))
}
