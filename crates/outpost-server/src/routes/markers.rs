//! `/api/v1/markers` — device-reported tactical markers (Ф1 ingest).
//!
//! Devices push markers they observe (enemy / vehicle / poi / sos). `id` is a
//! client-chosen UUID so re-reports upsert idempotently. Each accepted change is
//! published on the live bus so open maps update without a refetch. Reads are on
//! the operator side in [`crate::routes::geo`].

use crate::auth_extract::AuthDevice;
use crate::error::ApiError;
use crate::state::{AppState, LiveEvent};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    routing::post,
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

const MAX_BATCH: usize = 200;
const KINDS: [&str; 4] = ["enemy", "vehicle", "poi", "sos"];

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/markers", post(upsert_batch))
        .route("/api/v1/markers/{id}", axum::routing::delete(retract))
        // Marker batches are small JSON; cap the body well under the default.
        .layer(DefaultBodyLimit::max(256 * 1024))
}

#[derive(Debug, Deserialize)]
pub struct MarkerIn {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub subtype: Option<String>,
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub alt: Option<f64>,
    #[serde(default)]
    pub confidence: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    /// Time-to-live in seconds; applied to enemy/sos (fading contacts). Ignored
    /// for poi/vehicle, which persist until explicitly retracted.
    #[serde(default)]
    pub ttl_sec: Option<i64>,
}

async fn upsert_batch(
    device: AuthDevice,
    State(state): State<AppState>,
    Json(items): Json<Vec<MarkerIn>>,
) -> Result<StatusCode, ApiError> {
    if items.len() > MAX_BATCH {
        return Err(ApiError::BadRequest(format!(
            "batch too large ({} > {MAX_BATCH})",
            items.len()
        )));
    }
    // The reporter's callsign is its display name (best-effort).
    let callsign: Option<String> =
        sqlx::query_scalar("SELECT display_name FROM devices WHERE id = ?")
            .bind(device.id)
            .fetch_optional(&state.db)
            .await?
            .flatten();

    for m in &items {
        if !KINDS.contains(&m.kind.as_str()) {
            return Err(ApiError::BadRequest(format!(
                "unknown marker kind '{}'",
                m.kind
            )));
        }
        sqlx::query(
            "INSERT INTO tactical_markers \
               (id, customer_id, kind, subtype, lat, lon, alt, confidence, label, notes, \
                reporter_device_id, reporter_callsign, expires_at, updated_at, is_active) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, \
                CASE WHEN ? IN ('enemy','sos') AND ? IS NOT NULL \
                     THEN datetime('now', '+' || ? || ' seconds') ELSE NULL END, \
                datetime('now'), 1) \
             ON CONFLICT(id) DO UPDATE SET \
                kind = excluded.kind, subtype = excluded.subtype, \
                lat = excluded.lat, lon = excluded.lon, alt = excluded.alt, \
                confidence = excluded.confidence, label = excluded.label, notes = excluded.notes, \
                reporter_device_id = excluded.reporter_device_id, \
                reporter_callsign = excluded.reporter_callsign, \
                expires_at = excluded.expires_at, updated_at = datetime('now'), is_active = 1",
        )
        .bind(&m.id)
        .bind(device.customer_id)
        .bind(&m.kind)
        .bind(&m.subtype)
        .bind(m.lat)
        .bind(m.lon)
        .bind(m.alt)
        .bind(&m.confidence)
        .bind(&m.label)
        .bind(&m.notes)
        .bind(device.id)
        .bind(&callsign)
        .bind(&m.kind)
        .bind(m.ttl_sec)
        .bind(m.ttl_sec)
        .execute(&state.db)
        .await?;

        let payload = json!({
            "op": "upsert",
            "id": m.id, "kind": m.kind, "subtype": m.subtype,
            "lat": m.lat, "lon": m.lon, "alt": m.alt,
            "confidence": m.confidence, "label": m.label,
            "reporter_callsign": callsign,
        })
        .to_string();
        state.live.publish(LiveEvent {
            customer_id: device.customer_id,
            name: "marker",
            data: Arc::from(payload.as_str()),
        });
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Soft-retract a marker (sets `is_active = 0`); the reporter or any device in
/// the same tenant may retract. Publishes a removal so maps drop it live.
async fn retract(
    device: AuthDevice,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let res = sqlx::query(
        "UPDATE tactical_markers SET is_active = 0, updated_at = datetime('now') \
         WHERE id = ? AND customer_id = ?",
    )
    .bind(&id)
    .bind(device.customer_id)
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    let payload = json!({"op": "remove", "id": id}).to_string();
    state.live.publish(LiveEvent {
        customer_id: device.customer_id,
        name: "marker",
        data: Arc::from(payload.as_str()),
    });
    Ok(StatusCode::NO_CONTENT)
}
