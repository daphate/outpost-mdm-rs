//! Resolve the originating client IP for a request.
//!
//! Priority:
//! 1. `X-Forwarded-For` header (rightmost entry — set by the immediate
//!    upstream proxy, typically nginx). We accept this *only* on the
//!    assumption that the deploy fronts the binary with a trusted
//!    reverse proxy; the [`docs/DEPLOY.md`](../docs/DEPLOY.md) runbook
//!    documents that nginx + certbot is the expected topology.
//! 2. `axum::extract::ConnectInfo<SocketAddr>` — falls back to the raw
//!    TCP peer address when no proxy header is present (direct-Docker
//!    or `localhost` development).

use axum::extract::ConnectInfo;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[derive(Debug, Clone, Copy)]
pub struct ClientIp(pub IpAddr);

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        if let Some(xff) = parts.headers.get("x-forwarded-for")
            && let Ok(s) = xff.to_str()
            && let Some(last) = s.split(',').next_back().map(str::trim)
            && let Ok(ip) = last.parse::<IpAddr>()
        {
            return Ok(ClientIp(ip));
        }
        let ip = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip())
            .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        Ok(ClientIp(ip))
    }
}
