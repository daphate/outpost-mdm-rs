//! HTTP application factory.
//!
//! Build a fully-wired `Router` ready to be served by `axum::serve`. The
//! same router is used by `main.rs` for production and by integration
//! tests via `tower::ServiceExt::oneshot`.

use axum::{Json, Router, http::HeaderName, routing::get};
use serde::Serialize;
use tower_http::{
    compression::CompressionLayer,
    cors::CorsLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

/// Body of the `/healthz` response.
#[derive(Serialize, Debug)]
pub struct Health {
    pub status: &'static str,
    pub version: &'static str,
}

/// `/healthz` handler — returns `200 OK` with build version.
async fn healthz() -> Json<Health> {
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// Build the fully-wired router with all middleware.
///
/// Middleware ordering (outer → inner):
/// 1. `TraceLayer` — emit structured tracing for each request
/// 2. `SetRequestIdLayer` — inject a UUID into `x-request-id` if absent
/// 3. `PropagateRequestIdLayer` — copy request id to response
/// 4. `CorsLayer` — permissive in dev; production should restrict via env
/// 5. `CompressionLayer` — gzip responses where the client accepts it
pub fn build_router() -> Router {
    let request_id_header = HeaderName::from_static("x-request-id");

    Router::new()
        .route("/healthz", get(healthz))
        .layer(CompressionLayer::new())
        .layer(CorsLayer::permissive())
        .layer(PropagateRequestIdLayer::new(request_id_header.clone()))
        .layer(SetRequestIdLayer::new(request_id_header, MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_returns_ok() {
        let app = build_router();
        let response = app
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
    async fn healthz_emits_request_id_header() {
        let app = build_router();
        let response = app
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
            "expected x-request-id header in response, got: {:?}",
            response.headers()
        );
    }

    #[tokio::test]
    async fn unknown_route_returns_404() {
        let app = build_router();
        let response = app
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
}
