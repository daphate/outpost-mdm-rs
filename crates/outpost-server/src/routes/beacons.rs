//! `/api/v1/beacons` — виртуальные маяки (гео-зоны угроз) для класса «игроки».
//!
//! Оператор ставит маяки на карте (`POST`/`DELETE`, право `push.send`); приложение
//! игрока забирает активные (`GET /active`, токен устройства) и гео-фенсит их по
//! GPS. Список для операторской карты — `GET /api/v1/beacons` (`devices.read`).
//! Маяки — общие на арендатора (единые игровые зоны, без unit-скоупа).

use crate::auth_extract::{AuthDevice, AuthUser};
use crate::error::ApiError;
use crate::permission::require_permission;
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    routing::get,
};
use serde::{Deserialize, Serialize};

/// Буквы угроз: R радиация, A аномалия, M ментал, C контролёр, B бюрер,
/// Z зов Монолита, H лечение (Оазис).
const TYPES: [&str; 7] = ["R", "A", "M", "C", "B", "Z", "H"];

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/beacons", get(list).post(create))
        .route("/api/v1/beacons/{id}", axum::routing::delete(remove))
        .route("/api/v1/beacons/active", get(active))
        .layer(DefaultBodyLimit::max(64 * 1024))
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Beacon {
    pub id: i64,
    pub beacon_type: String,
    pub coeff: i64,
    pub radius_m: f64,
    pub lat: f64,
    pub lon: f64,
    pub label: Option<String>,
    pub is_active: bool,
}

/// Операторский список маяков арендатора (для карты).
async fn list(user: AuthUser, State(state): State<AppState>) -> Result<Json<Vec<Beacon>>, ApiError> {
    require_permission(&state.db, user.role_id, "devices.read").await?;
    let rows: Vec<Beacon> = sqlx::query_as::<_, Beacon>(
        "SELECT id, beacon_type, coeff, radius_m, lat, lon, label, is_active \
         FROM virtual_beacons WHERE customer_id = ? ORDER BY id",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

#[derive(Debug, Deserialize)]
pub struct BeaconIn {
    pub beacon_type: String,
    #[serde(default = "default_coeff")]
    pub coeff: i64,
    #[serde(default = "default_radius")]
    pub radius_m: f64,
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub label: Option<String>,
}
fn default_coeff() -> i64 {
    100
}
fn default_radius() -> f64 {
    30.0
}

/// Поставить маяк (гейм-мастерское действие).
async fn create(
    user: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<BeaconIn>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    require_permission(&state.db, user.role_id, "push.send").await?;
    if !TYPES.contains(&req.beacon_type.as_str()) {
        return Err(ApiError::BadRequest(
            "beacon_type должен быть один из R,A,M,C,B,Z,H".into(),
        ));
    }
    if !(-90.0..=90.0).contains(&req.lat) || !(-180.0..=180.0).contains(&req.lon) {
        return Err(ApiError::BadRequest("lat/lon вне диапазона".into()));
    }
    let coeff = req.coeff.clamp(1, 999);
    let radius = req.radius_m.clamp(1.0, 2000.0);
    let label = req
        .label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(80).collect::<String>());
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO virtual_beacons \
           (customer_id, beacon_type, coeff, radius_m, lat, lon, label, created_by) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(user.customer_id)
    .bind(&req.beacon_type)
    .bind(coeff)
    .bind(radius)
    .bind(req.lat)
    .bind(req.lon)
    .bind(&label)
    .bind(user.id)
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, Json(serde_json::json!({ "id": id }))))
}

/// Убрать маяк.
async fn remove(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    require_permission(&state.db, user.role_id, "push.send").await?;
    let res = sqlx::query("DELETE FROM virtual_beacons WHERE id = ? AND customer_id = ?")
        .bind(id)
        .bind(user.customer_id)
        .execute(&state.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Активные маяки для устройства игрока (гео-фенсинг — на стороне приложения).
async fn active(
    device: AuthDevice,
    State(state): State<AppState>,
) -> Result<Json<Vec<Beacon>>, ApiError> {
    let rows: Vec<Beacon> = sqlx::query_as::<_, Beacon>(
        "SELECT id, beacon_type, coeff, radius_m, lat, lon, label, is_active \
         FROM virtual_beacons WHERE customer_id = ? AND is_active = 1 ORDER BY id",
    )
    .bind(device.customer_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}
