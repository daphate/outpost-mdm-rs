//! `AuthUser` extractor — pulls the bearer token, verifies it, and yields
//! the authenticated user identity for downstream handlers.
//!
//! Lives in a separate module from [`crate::auth`] because the extractor
//! is HTTP-aware while [`crate::auth`] is plain crypto.

use crate::auth;
use crate::error::ApiError;
use crate::state::AppState;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;

/// Authenticated user identity, attached to a request by the
/// `AuthUser` extractor.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: i64,
    pub customer_id: i64,
    pub role_id: i64,
    pub login: String,
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or(ApiError::Unauthorized)?;
        let token = header
            .strip_prefix("Bearer ")
            .ok_or(ApiError::Unauthorized)?;
        let claims =
            auth::verify_token(token, &state.jwt_secret).map_err(|_| ApiError::InvalidToken)?;

        if claims.kind != auth::KIND_USER {
            return Err(ApiError::InvalidToken);
        }
        // Sanity check: confirm the user still exists and is active.
        let active: Option<i64> = sqlx::query_scalar("SELECT is_active FROM users WHERE id = ?")
            .bind(claims.sub)
            .fetch_optional(&state.db)
            .await
            .map_err(ApiError::from)?;
        match active {
            Some(1) => Ok(AuthUser {
                id: claims.sub,
                customer_id: claims.customer_id,
                role_id: claims.role_id,
                login: claims.login,
            }),
            Some(_) => Err(ApiError::Inactive),
            None => Err(ApiError::InvalidToken),
        }
    }
}

/// Authenticated device identity, attached to a device-facing request.
#[derive(Debug, Clone)]
pub struct AuthDevice {
    pub id: i64,
    pub customer_id: i64,
    pub serial: String,
}

impl FromRequestParts<AppState> for AuthDevice {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or(ApiError::Unauthorized)?;
        let token = header
            .strip_prefix("Bearer ")
            .ok_or(ApiError::Unauthorized)?;
        let claims =
            auth::verify_token(token, &state.jwt_secret).map_err(|_| ApiError::InvalidToken)?;
        if claims.kind != auth::KIND_DEVICE {
            return Err(ApiError::InvalidToken);
        }
        let row: Option<(i64, i64, String, i64)> =
            sqlx::query_as("SELECT id, customer_id, serial, is_enrolled FROM devices WHERE id = ?")
                .bind(claims.sub)
                .fetch_optional(&state.db)
                .await
                .map_err(ApiError::from)?;
        match row {
            Some((id, customer_id, serial, 1)) => Ok(AuthDevice {
                id,
                customer_id,
                serial,
            }),
            Some(_) => Err(ApiError::Forbidden),
            None => Err(ApiError::InvalidToken),
        }
    }
}
