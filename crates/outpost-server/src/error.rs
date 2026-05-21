//! Unified HTTP error type.
//!
//! Handlers return `Result<T, ApiError>`; axum's `IntoResponse` blanket
//! impl over `Result<T, E>` produces a JSON error body with a stable
//! `code` for machine consumers and an opaque `message` for humans.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("user is inactive")]
    Inactive,
    #[error("password not initialised — contact administrator")]
    NotBootstrapped,
    #[error("missing or malformed Authorization header")]
    Unauthorized,
    #[error("invalid or expired token")]
    InvalidToken,
    #[error("insufficient permissions")]
    Forbidden,
    #[error("resource not found")]
    NotFound,
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("too many requests — try again later")]
    TooManyRequests,
    #[error("internal server error")]
    Internal,
    /// v0.18.17: feature-disabled endpoint. Используется для ballistics
    /// routes при `BALLISTICS_ENABLED=false`.
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),
    /// v0.18.17: optimistic concurrency mismatch (per BALLISTICS-MDM-
    /// CONTRACT §5.1 ETag conflict).
    #[error("precondition failed: {0}")]
    PreconditionFailed(String),
    /// v0.18.17: internal error с явным message (vs `Internal` который
    /// маскирует под "internal server error").
    #[error("internal server error: {0}")]
    InternalServerError(String),
}

#[derive(Serialize)]
struct ErrorBody {
    error: ErrorPayload,
}

#[derive(Serialize)]
struct ErrorPayload {
    code: &'static str,
    message: String,
}

impl ApiError {
    fn http_status(&self) -> StatusCode {
        match self {
            Self::InvalidCredentials | Self::NotBootstrapped => StatusCode::UNAUTHORIZED,
            Self::Inactive => StatusCode::FORBIDDEN,
            Self::Unauthorized | Self::InvalidToken => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            Self::ServiceUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::PreconditionFailed(_) => StatusCode::PRECONDITION_FAILED,
            Self::InternalServerError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::InvalidCredentials => "invalid_credentials",
            Self::Inactive => "user_inactive",
            Self::NotBootstrapped => "not_bootstrapped",
            Self::Unauthorized => "unauthorized",
            Self::InvalidToken => "invalid_token",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::BadRequest(_) => "bad_request",
            Self::TooManyRequests => "too_many_requests",
            Self::Internal => "internal",
            Self::ServiceUnavailable(_) => "service_unavailable",
            Self::PreconditionFailed(_) => "precondition_failed",
            Self::InternalServerError(_) => "internal",
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.http_status();
        if matches!(self, Self::Internal | Self::InternalServerError(_)) {
            tracing::error!(error = %self, "internal server error");
        }
        let body = ErrorBody {
            error: ErrorPayload {
                code: self.code(),
                message: self.to_string(),
            },
        };
        (status, Json(body)).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        tracing::error!(error = %err, "sqlx error");
        Self::Internal
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        tracing::error!(error = ?err, "anyhow error");
        Self::Internal
    }
}
