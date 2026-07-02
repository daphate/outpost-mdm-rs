//! Resolve the originating client IP for a request.
//!
//! Priority:
//! 1. `X-Forwarded-For` header (rightmost entry — set by the immediate
//!    upstream proxy, typically nginx) — **only** when proxy headers are
//!    trusted (`TRUST_PROXY_HEADERS`, default on). Behind nginx this is the
//!    real client IP; without a trusted proxy the header is attacker-forgeable
//!    and would let anyone spoof their IP to dodge the login rate-limiter, so
//!    set `TRUST_PROXY_HEADERS=0` for direct-exposed deployments.
//! 2. `axum::extract::ConnectInfo<SocketAddr>` — the raw TCP peer address,
//!    used when proxy headers are untrusted or absent.

use axum::extract::ConnectInfo;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::OnceLock;

/// Whether to trust the `X-Forwarded-For` header. Default `true` (the
/// documented topology fronts the binary with nginx). Set `TRUST_PROXY_HEADERS`
/// to `0`/`false` when the process is directly exposed, so a client can't forge
/// its source IP. Cached — env is read once per process.
fn trust_proxy_headers() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("TRUST_PROXY_HEADERS")
            .map(|s| !matches!(s.trim(), "0" | "false" | "FALSE" | "no"))
            .unwrap_or(true)
    })
}

#[derive(Debug, Clone, Copy)]
pub struct ClientIp(pub IpAddr);

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        if trust_proxy_headers()
            && let Some(xff) = parts.headers.get("x-forwarded-for")
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
