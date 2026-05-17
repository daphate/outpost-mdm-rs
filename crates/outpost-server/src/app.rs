//! HTTP application factory.
//!
//! Builds a fully-wired `Router` ready to be served by `axum::serve`. The
//! same router is used by `main.rs` for production and by integration
//! tests via `tower::ServiceExt::oneshot`.

use crate::routes;
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderName, StatusCode},
    response::IntoResponse,
    routing::get,
};
use serde::Serialize;
use tower_http::{
    compression::CompressionLayer,
    cors::CorsLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

/// Body of the `/healthz` response — liveness only (does not touch DB).
#[derive(Serialize, Debug)]
pub struct Health {
    pub status: &'static str,
    pub version: &'static str,
}

/// `/healthz` — liveness probe, always returns 200 OK if the process is up.
async fn healthz() -> Json<Health> {
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// Body of the `/readyz` response — full readiness, includes DB ping.
#[derive(Serialize, Debug)]
pub struct Ready {
    pub status: &'static str,
    pub db: &'static str,
}

/// `/readyz` — readiness probe: confirms the database is reachable.
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

/// Build the fully-wired router with state and middleware.
///
/// Middleware ordering (outer → inner request flow):
/// 1. `TraceLayer` — emit structured tracing for each request
/// 2. `SetRequestIdLayer` — inject a UUID into `x-request-id` if absent
/// 3. `PropagateRequestIdLayer` — copy the request id to the response
/// 4. `CorsLayer` — permissive in dev; production should restrict via env
/// 5. `CompressionLayer` — gzip responses where the client accepts it
pub fn build_router(state: AppState) -> Router {
    let request_id_header = HeaderName::from_static("x-request-id");

    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .merge(routes::auth::router())
        .with_state(state)
        .layer(CompressionLayer::new())
        .layer(CorsLayer::permissive())
        .layer(PropagateRequestIdLayer::new(request_id_header.clone()))
        .layer(SetRequestIdLayer::new(request_id_header, MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
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
        assert!(json["version"].as_str().unwrap().starts_with("0."));
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
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["db"], "ok");
    }

    #[tokio::test]
    async fn healthz_emits_request_id_header() {
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
        assert!(
            response.headers().contains_key("x-request-id"),
            "expected x-request-id header in response",
        );
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
}
