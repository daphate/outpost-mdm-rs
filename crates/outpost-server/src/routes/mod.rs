//! HTTP route modules.
//!
//! One sub-module per logical resource family. Each module owns its
//! handlers + request/response DTOs and exposes a `router()` returning an
//! `axum::Router<AppState>` to be merged into the top-level builder.

pub mod applications;
pub mod auth;
pub mod ballistics;
pub mod bundles;
pub mod configurations;
pub mod devices;
pub mod distribute;
pub mod enrollment;
pub mod files;
pub mod groups;
pub mod internal;
pub mod otel;
pub mod prom;
pub mod push;
pub mod settings;
pub mod stats;
pub mod users;
pub mod web;

use crate::state::AppState;
use axum::Router;

/// Compose every resource sub-router into one tree.
pub fn api_v1(state: AppState) -> Router {
    Router::new()
        .merge(auth::router())
        .merge(devices::router())
        .merge(groups::router())
        .merge(applications::router())
        .merge(configurations::router())
        .merge(users::router())
        .merge(settings::router())
        .merge(stats::router())
        .merge(push::router())
        .merge(files::router())
        .merge(distribute::router())
        .merge(enrollment::router())
        .merge(otel::router())
        .merge(prom::router())
        // v0.18.2: nginx auth_request endpoint (Grafana SSO via MDM cookie).
        .merge(internal::router())
        // v0.18.17: ballistics endpoints (BALLISTICS-MDM-CONTRACT v1).
        // Все routes за feature flag (require_enabled() в каждом handler'е).
        // Production deploys должны держать BALLISTICS_ENABLED=false пока
        // expert crypto review per docs/BALLISTICS-CRYPTO-DESIGN.md §6.
        .merge(ballistics::router())
        // 2026-06-03: bundle assignment endpoints (CONTENT-DISTRIBUTION-CONTRACT
        // §«Канал 2» + INSIGHT-054 soldier-v31).
        .merge(bundles::router())
        .merge(web::router())
        .with_state(state)
}
