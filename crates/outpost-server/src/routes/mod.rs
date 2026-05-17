//! HTTP route modules.
//!
//! One sub-module per logical resource family. Each module owns its
//! handlers + request/response DTOs and exposes a `router()` returning an
//! `axum::Router<AppState>` to be merged into the top-level builder.

pub mod applications;
pub mod auth;
pub mod configurations;
pub mod devices;
pub mod enrollment;
pub mod files;
pub mod groups;
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
        .merge(enrollment::router())
        .merge(web::router())
        .with_state(state)
}
