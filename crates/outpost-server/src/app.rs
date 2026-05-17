//! HTTP application factory.

use crate::routes;
use crate::state::AppState;
use axum::extract::DefaultBodyLimit;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderName, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::get,
};
use serde::Serialize;
use std::time::Duration;
use tower_http::{
    compression::CompressionLayer,
    cors::CorsLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    set_header::SetResponseHeaderLayer,
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

#[derive(Serialize, Debug)]
pub struct Health {
    pub status: &'static str,
    pub version: &'static str,
}

async fn healthz() -> Json<Health> {
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

#[derive(Serialize, Debug)]
pub struct Ready {
    pub status: &'static str,
    pub db: &'static str,
}

async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.db)
        .await
    {
        Ok(_) => (
            StatusCode::OK,
            Json(Ready {
                status: "ok",
                db: "ok",
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "readiness probe DB ping failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(Ready {
                    status: "degraded",
                    db: "unreachable",
                }),
            )
                .into_response()
        }
    }
}

/// Build the fully-wired router with state, middleware, and security
/// headers.
///
/// Layer ordering — top-most outer, request flows down then bubbles up:
/// 1. `TimeoutLayer` — caps per-request wall clock at
///    `state.request_timeout_secs`
/// 2. Security response headers (X-Content-Type-Options, X-Frame-Options,
///    Referrer-Policy, Strict-Transport-Security, X-Robots-Tag,
///    Permissions-Policy) — added `if_not_present` so handlers may override
/// 3. `TraceLayer` — emits structured tracing for each request
/// 4. `SetRequestIdLayer` / `PropagateRequestIdLayer` — UUID per request,
///    surfaced as `x-request-id`
/// 5. `CorsLayer` — permissive in dev; production should restrict via env
/// 6. `CompressionLayer` — gzip responses
/// 7. `DefaultBodyLimit` — caps the request body at `state.max_body_bytes`
pub fn build_router(state: AppState) -> Router {
    let max_body = state.max_body_bytes;
    let timeout = Duration::from_secs(state.request_timeout_secs);

    let request_id_header = HeaderName::from_static("x-request-id");

    let probes: Router = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(state.clone());

    probes
        .merge(routes::api_v1(state))
        // Stack outermost layers first so they wrap everything below.
        .layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            timeout,
        ))
        // Security response headers — each `if_not_present` so handlers may override.
        .layer(set_header_if_absent("x-content-type-options", "nosniff"))
        .layer(set_header_if_absent("x-frame-options", "DENY"))
        .layer(set_header_if_absent("referrer-policy", "no-referrer"))
        .layer(set_header_if_absent(
            "strict-transport-security",
            "max-age=31536000; includeSubDomains",
        ))
        .layer(set_header_if_absent("x-robots-tag", "noindex, nofollow"))
        .layer(set_header_if_absent(
            "permissions-policy",
            "camera=(), microphone=(), geolocation=()",
        ))
        .layer(TraceLayer::new_for_http())
        .layer(PropagateRequestIdLayer::new(request_id_header.clone()))
        .layer(SetRequestIdLayer::new(request_id_header, MakeRequestUuid))
        .layer(CorsLayer::permissive())
        .layer(CompressionLayer::new())
        .layer(DefaultBodyLimit::max(max_body))
}

/// Helper: a `SetResponseHeaderLayer::if_not_present` with both `'static`
/// arguments — used for the OWASP-style hardening headers.
fn set_header_if_absent(
    name: &'static str,
    value: &'static str,
) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::if_not_present(
        HeaderName::from_static(name),
        HeaderValue::from_static(value),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_state;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn app() -> Router {
        build_router(test_state().await)
    }

    #[tokio::test]
    async fn healthz_returns_ok() {
        let response = app()
            .await
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn readyz_returns_ok_when_db_alive() {
        let response = app()
            .await
            .oneshot(
                Request::builder()
                    .uri("/readyz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn unknown_route_returns_404() {
        let response = app()
            .await
            .oneshot(
                Request::builder()
                    .uri("/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn me_without_token_returns_401() {
        let response = app()
            .await
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/me")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn devices_without_token_returns_401() {
        let response = app()
            .await
            .oneshot(
                Request::builder()
                    .uri("/api/v1/devices")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn security_headers_are_set() {
        let response = app()
            .await
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let headers = response.headers();
        assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
        assert_eq!(headers.get("x-frame-options").unwrap(), "DENY");
        assert_eq!(headers.get("referrer-policy").unwrap(), "no-referrer");
        assert!(
            headers
                .get("strict-transport-security")
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("max-age=")
        );
        assert!(headers.get("permissions-policy").is_some());
    }
}
