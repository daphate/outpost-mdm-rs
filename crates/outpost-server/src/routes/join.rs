//! `/api/v1/join` — самрегистрация игроков STALKER по общему коду игры (режим B).
//!
//! Для массовых открытых LARP, где заводить каждого игрока вручную дорого.
//! Админ включает режим по своему заказчику (`/join/enable` → генерируется
//! join-секрет + join-QR), игрок сканирует один общий QR, вводит позывной, и
//! сервер САМ создаёт устройство класса `stalker_player` и выдаёт ему
//! 90-дневный токен. Это сознательное ослабление модели «админ заводит каждое
//! устройство» — допустимо для игры, поэтому гейтится наличием join-секрета в
//! профиле заказчика (у тактических арендаторов он не задаётся).
//!
//! Публичный `POST /api/v1/join` — без аутентификации, гейт = сам секрет.
//! `POST /api/v1/join/enable` / `/disable` — админ (`configurations.write`).

use crate::auth::generate_password;
use crate::auth_extract::AuthUser;
use crate::error::ApiError;
use crate::permission::require_permission;
use crate::session::create_device_session;
use crate::state::AppState;
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// 90 дней — как у обычного enroll (см. `enrollment::DEVICE_TOKEN_TTL_SECS`).
const DEVICE_TOKEN_TTL_SECS: i64 = 60 * 60 * 24 * 90;
/// Предел авто-созданных join-игроков на заказчика (защита от спама поверх
/// секрета-гейта).
const JOIN_CAP: i64 = 2000;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/join", post(join))
        .route("/api/v1/join/enable", post(enable))
        .route("/api/v1/join/disable", post(disable))
}

#[derive(Debug, Deserialize)]
pub struct JoinRequest {
    pub join_secret: String,
    #[serde(default)]
    pub callsign: Option<String>,
    #[serde(default)]
    pub app_version: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct JoinResponse {
    pub device_id: i64,
    pub device_token: String,
    pub expires_in: i64,
}

async fn join(
    State(state): State<AppState>,
    Json(req): Json<JoinRequest>,
) -> Result<Json<JoinResponse>, ApiError> {
    if req.join_secret.trim().is_empty() {
        return Err(ApiError::Unauthorized);
    }
    // Гейт — секрет. Высокоэнтропийный (32 симв.), поэтому прямой lookup.
    let row: Option<(i64, Option<i64>)> = sqlx::query_as(
        "SELECT customer_id, join_unit_id FROM customer_profiles \
         WHERE join_secret IS NOT NULL AND join_secret = ?",
    )
    .bind(&req.join_secret)
    .fetch_optional(&state.db)
    .await?;
    let (customer_id, join_unit_id) = row.ok_or(ApiError::Unauthorized)?;

    // Предел на заказчика — защита от спама поверх секрета.
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM devices \
         WHERE customer_id = ? AND device_class = 'stalker_player' AND serial LIKE 'join-%'",
    )
    .bind(customer_id)
    .fetch_one(&state.db)
    .await?;
    if count >= JOIN_CAP {
        return Err(ApiError::TooManyRequests);
    }

    let callsign: String = req
        .callsign
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Игрок")
        .chars()
        .take(40)
        .collect();
    let serial = format!("join-{}", uuid::Uuid::new_v4().simple());

    let device_id: i64 = sqlx::query_scalar(
        "INSERT INTO devices \
           (customer_id, serial, display_name, device_class, unit_id, \
            app_version, is_enrolled, is_active, last_seen_at, is_online) \
         VALUES (?, ?, ?, 'stalker_player', ?, ?, 1, 1, datetime('now'), 1) RETURNING id",
    )
    .bind(customer_id)
    .bind(&serial)
    .bind(&callsign)
    .bind(join_unit_id)
    .bind(&req.app_version)
    .fetch_one(&state.db)
    .await?;

    let token = create_device_session(
        &state.db,
        device_id,
        customer_id,
        &serial,
        DEVICE_TOKEN_TTL_SECS,
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok(Json(JoinResponse {
        device_id,
        device_token: token,
        expires_in: DEVICE_TOKEN_TTL_SECS,
    }))
}

/// `outpost-join://v1/<base64url(json)>` — join-QR (общий на игру), парный к
/// `encode_enrollment_uri` для режима A.
fn encode_join_uri(server_url: &str, join_secret: &str) -> String {
    use base64::Engine;
    let json = json!({"server_url": server_url, "join_secret": join_secret}).to_string();
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.as_bytes());
    format!("outpost-join://v1/{b64}")
}

fn qrcode_svg(payload: &str) -> String {
    use qrcode::{QrCode, render::svg};
    match QrCode::new(payload.as_bytes()) {
        Ok(code) => code
            .render::<svg::Color<'_>>()
            .min_dimensions(240, 240)
            .quiet_zone(true)
            .build(),
        Err(e) => format!("QR generation failed: {e}"),
    }
}

/// Включить/перевыпустить join-код для заказчика админа. Возвращает секрет,
/// join-URI и готовый QR-SVG для показа игрокам.
async fn enable(user: AuthUser, State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    require_permission(&state.db, user.role_id, "configurations.write").await?;
    let secret = generate_password(32);
    sqlx::query(
        "INSERT INTO customer_profiles (customer_id, join_secret) VALUES (?, ?) \
         ON CONFLICT(customer_id) DO UPDATE SET join_secret = excluded.join_secret, \
           updated_at = datetime('now')",
    )
    .bind(user.customer_id)
    .bind(&secret)
    .execute(&state.db)
    .await?;

    let server_url: Option<String> = sqlx::query_scalar(
        "SELECT json_extract(value_json, '$') FROM settings WHERE key = 'server.enrollment_base_url'",
    )
    .fetch_optional(&state.db)
    .await?
    .flatten();
    let uri = encode_join_uri(server_url.as_deref().unwrap_or(""), &secret);
    let qr = qrcode_svg(&uri);

    Ok(Json(json!({
        "join_secret": secret,
        "join_uri": uri,
        "qr_svg": qr,
        "server_url": server_url,
    })))
}

/// Выключить самрегистрацию (стереть join-секрет). Ранее выданные токены живут.
async fn disable(user: AuthUser, State(state): State<AppState>) -> Result<StatusCode, ApiError> {
    require_permission(&state.db, user.role_id, "configurations.write").await?;
    sqlx::query(
        "UPDATE customer_profiles SET join_secret = NULL, updated_at = datetime('now') \
         WHERE customer_id = ?",
    )
    .bind(user.customer_id)
    .execute(&state.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
