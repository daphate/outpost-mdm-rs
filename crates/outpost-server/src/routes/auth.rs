//! `/api/v1/auth/*` — login and "who am I".

use crate::auth as crypto;
use crate::auth_extract::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/me", get(me))
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub login: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_in: i64,
    pub must_change_password: bool,
}

/// `POST /api/v1/auth/login` — exchange credentials for a JWT.
async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, ApiError> {
    let row: Option<(i64, i64, i64, Option<String>, i64, i64)> = sqlx::query_as(
        "SELECT id, customer_id, role_id, password_hash, is_active, must_change_password \
         FROM users WHERE login = ?",
    )
    .bind(&req.login)
    .fetch_optional(&state.db)
    .await?;

    let (id, customer_id, role_id, password_hash, is_active, must_change_password) =
        row.ok_or(ApiError::InvalidCredentials)?;
    if is_active == 0 {
        return Err(ApiError::Inactive);
    }
    let phc = password_hash.ok_or(ApiError::NotBootstrapped)?;
    if !crypto::verify_password(&req.password, &phc).unwrap_or(false) {
        return Err(ApiError::InvalidCredentials);
    }

    // Update last_login_at on a best-effort basis (do not fail the
    // login on an audit-table write error).
    let _ = sqlx::query("UPDATE users SET last_login_at = datetime('now') WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await;

    let token = crypto::issue_token(
        id,
        customer_id,
        role_id,
        &req.login,
        &state.jwt_secret,
        state.jwt_ttl_secs,
    )
    .map_err(|_| ApiError::Internal)?;

    Ok(Json(LoginResponse {
        access_token: token,
        token_type: "Bearer",
        expires_in: state.jwt_ttl_secs,
        must_change_password: must_change_password != 0,
    }))
}

#[derive(Debug, Serialize)]
pub struct MeResponse {
    pub id: i64,
    pub customer_id: i64,
    pub role_id: i64,
    pub login: String,
}

/// `GET /api/v1/auth/me` — return the caller's identity.
async fn me(user: AuthUser) -> Json<MeResponse> {
    Json(MeResponse {
        id: user.id,
        customer_id: user.customer_id,
        role_id: user.role_id,
        login: user.login,
    })
}
