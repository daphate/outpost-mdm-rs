//! HTTP extractors that turn a bearer token into a typed identity.
//!
//! Both extractors look up the presented token in the [`crate::session`]
//! table (rejecting revoked / expired tokens) and additionally verify
//! the underlying user is still active (`users.is_active = 1`) or that
//! the device is still enrolled (`devices.is_enrolled = 1`).

use crate::error::ApiError;
use crate::session::{self, KIND_DEVICE, KIND_USER};
use crate::state::AppState;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;

/// The super-admin role id (seeded in migration 0002). Users in this role
/// see across every unit of their tenant (unit scoping is bypassed).
pub const SUPER_ADMIN_ROLE_ID: i64 = 1;

/// Authenticated user identity, attached to a request by the `AuthUser`
/// extractor.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: i64,
    pub customer_id: i64,
    pub role_id: i64,
    pub login: String,
    /// Org-unit (подразделение) the user is scoped to, if any. `None` (or a
    /// super-admin role) means tenant-wide visibility; otherwise queries are
    /// filtered to this unit and its descendants. See `crate::unit_scope`.
    pub unit_id: Option<i64>,
}

impl AuthUser {
    /// True when the user holds the tenant super-admin role — unit scoping is
    /// bypassed (sees every unit of the tenant).
    pub fn is_super_admin(&self) -> bool {
        self.role_id == SUPER_ADMIN_ROLE_ID
    }
}

/// Extract the session token from either `Authorization: Bearer …`
/// (API clients) or the `outpost_session` cookie (browser).
pub fn extract_token(parts: &Parts) -> Option<String> {
    if let Some(bearer) = parts
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
    {
        return Some(bearer.to_string());
    }
    parts
        .headers
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| {
            raw.split(';')
                .map(str::trim)
                .find_map(|kv| kv.strip_prefix("outpost_session="))
                .map(|s| s.to_string())
        })
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = extract_token(parts).ok_or(ApiError::Unauthorized)?;
        let s = session::verify(&token, &state.db)
            .await
            .map_err(|_| ApiError::InvalidToken)?;
        if s.kind != KIND_USER {
            return Err(ApiError::InvalidToken);
        }
        // Confirm the underlying user is still active, and pull the current
        // unit assignment (used for подразделение scoping — fetched fresh so a
        // re-assignment takes effect without re-login).
        let row: Option<(i64, Option<i64>)> =
            sqlx::query_as("SELECT is_active, unit_id FROM users WHERE id = ?")
                .bind(s.subject_id)
                .fetch_optional(&state.db)
                .await
                .map_err(ApiError::from)?;
        match row {
            Some((1, unit_id)) => Ok(AuthUser {
                id: s.subject_id,
                customer_id: s.customer_id,
                role_id: s.role_id,
                login: s.login,
                unit_id,
            }),
            Some(_) => Err(ApiError::Inactive),
            None => Err(ApiError::InvalidToken),
        }
    }
}

/// Authenticated device identity for device-facing endpoints.
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
        let token = extract_token(parts).ok_or(ApiError::Unauthorized)?;
        let s = session::verify(&token, &state.db)
            .await
            .map_err(|_| ApiError::InvalidToken)?;
        if s.kind != KIND_DEVICE {
            return Err(ApiError::InvalidToken);
        }
        // v0.18.20 (security review AUTH-1): gate on is_enrolled AND
        // devices.is_active AND customers.is_active. Без этого soft-disabled
        // device или device деактивированного тенанта продолжал бы
        // аутентифицироваться по валидному 90-дневному токену (а sliding
        // refresh в /sync продлевал бы его бесконечно). Mirrors AuthUser
        // which already rejects inactive users.
        let row: Option<(i64, i64, String, i64, i64, i64)> = sqlx::query_as(
            "SELECT d.id, d.customer_id, d.serial, d.is_enrolled, d.is_active, c.is_active \
             FROM devices d JOIN customers c ON c.id = d.customer_id WHERE d.id = ?",
        )
        .bind(s.subject_id)
        .fetch_optional(&state.db)
        .await
        .map_err(ApiError::from)?;
        match row {
            // enrolled=1, device active=1, customer active=1 — всё ок.
            Some((id, customer_id, serial, 1, 1, 1)) => Ok(AuthDevice {
                id,
                customer_id,
                serial,
            }),
            // row есть, но не enrolled / device disabled / tenant disabled.
            Some(_) => Err(ApiError::Forbidden),
            None => Err(ApiError::InvalidToken),
        }
    }
}
