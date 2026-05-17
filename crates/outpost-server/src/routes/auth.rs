//! `/api/v1/auth/*` — login / logout / who-am-I.

use crate::auth as crypto;
use crate::auth_extract::{AuthUser, extract_token};
use crate::client_ip::ClientIp;
use crate::error::ApiError;
use crate::session;
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, request::Parts},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
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

/// `POST /api/v1/auth/login` — exchange credentials for an opaque
/// session token (256 bits hex).
async fn login(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, ApiError> {
    if !state.login_limiter.try_take(ip) {
        tracing::warn!(%ip, login = %req.login, "login rate limit exceeded");
        return Err(ApiError::TooManyRequests);
    }
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

    let _ = sqlx::query("UPDATE users SET last_login_at = datetime('now') WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await;

    let token = session::create_user_session(
        &state.db,
        id,
        customer_id,
        role_id,
        &req.login,
        state.session_ttl_secs,
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok(Json(LoginResponse {
        access_token: token,
        token_type: "Bearer",
        expires_in: state.session_ttl_secs,
        must_change_password: must_change_password != 0,
    }))
}

/// `POST /api/v1/auth/logout` — revoke the caller's current session.
/// Idempotent; returns 204 either way.
async fn logout(
    State(state): State<AppState>,
    parts: AxumPartsBorrow,
) -> Result<StatusCode, ApiError> {
    if let Some(token) = parts.token {
        let _ = session::revoke(&token, &state.db).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Tiny extractor that pulls only the bearer/cookie token out of the
/// request — used by `logout` so it can revoke without first verifying
/// (revoke is idempotent; an unknown token is just a no-op).
struct AxumPartsBorrow {
    token: Option<String>,
}

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for AxumPartsBorrow {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self {
            token: extract_token(parts),
        })
    }
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
