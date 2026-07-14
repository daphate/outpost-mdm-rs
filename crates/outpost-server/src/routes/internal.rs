//! Internal endpoints reachable only from nginx via `auth_request`.
//!
//! These routes are NOT meant for direct browser/client traffic — nginx
//! is configured to expose them as `internal` locations only. The path
//! prefix is deliberately weird (`/__mdm_*`) to make accidental external
//! use obvious in access logs.
//!
//! Current users:
//!
//! - `GET /__mdm_auth_check` — single-purpose check for nginx
//!   `auth_request` directive in front of Grafana. Reads the
//!   `outpost_session` cookie, runs it through `session::verify`, and
//!   accepts iff `kind == KIND_USER` AND the user account is still
//!   active. Sessions in the half-authenticated `pending_2fa` state are
//!   rejected — so the second factor must be passed before Grafana
//!   becomes reachable.
//!
//! Wire contract for nginx (matching `auth_request` semantics):
//!
//! - 2xx → upstream request proceeds (Grafana is served).
//! - 401 → upstream request is denied. nginx is configured with
//!   `error_page 401 = @redirect_login` to bounce the browser at the
//!   login page (carrying `redirectTo`).
//! - Any other status → nginx fails the request with 500 by default; we
//!   never emit anything but 200/401.
//!
//! Cost per call: one indexed lookup on `sessions.id_hash` (PRIMARY KEY)
//! plus one cached read on `users.is_active`. ~0.1 ms on the WAL'd
//! SQLite pool — fine for the per-page-load auth_request fan-out.

use axum::{
    Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};

use crate::auth_extract::SUPER_ADMIN_ROLE_ID;
use crate::session::{self, KIND_USER};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/__mdm_auth_check", get(auth_check))
}

/// Cookie name shared with `routes::web::extract_token`. Keeping it
/// hard-coded here (rather than reaching into `routes::web::*`) avoids
/// pulling the entire web tree into this minimal-surface endpoint.
const SESSION_COOKIE: &str = "outpost_session";

/// Pull the session token out of the `Cookie:` header.
///
/// Mirrors the parsing in `routes::web::extract_token` — both must accept
/// the same cookie name for the auth_request flow to share state with
/// the regular Web UI session.
fn extract_session_token(headers: &HeaderMap) -> Option<String> {
    let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
    for kv in cookie_header.split(';') {
        let kv = kv.trim();
        if let Some(v) = kv.strip_prefix(&format!("{SESSION_COOKIE}=")) {
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

async fn auth_check(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(token) = extract_session_token(&headers) else {
        return (StatusCode::UNAUTHORIZED, "no session").into_response();
    };

    let Ok(s) = session::verify(&token, &state.db).await else {
        return (StatusCode::UNAUTHORIZED, "invalid session").into_response();
    };

    // Reject pending-2FA sessions — they must complete the second
    // factor before Grafana becomes reachable.
    if s.kind != KIND_USER {
        return (StatusCode::UNAUTHORIZED, "second factor required").into_response();
    }

    // Account must still be active (an admin can flip `is_active = 0` to lock a
    // user out instantly, even after a session is issued). We also pull the
    // CURRENT role here rather than trusting the role cached in the session at
    // issue time — a demotion must take effect immediately at this gate.
    let row: Option<(i64, i64)> =
        sqlx::query_as("SELECT is_active, role_id FROM users WHERE id = ?")
            .bind(s.subject_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    let Some((1, role_id)) = row else {
        return (StatusCode::UNAUTHORIZED, "user deactivated").into_response();
    };

    // Tenant-awareness: on the central multi-tenant hub, Grafana is a single
    // anonymous-admin instance that sees EVERY tenant's data. Gate it to
    // super-admins (hub operators) so a regular tenant user cannot reach the
    // shared, cross-tenant Grafana. Per-tenant scoping (Grafana orgs keyed off
    // the `x-auth-customer-id` header below) is the follow-up, and lives in
    // Grafana config, not here. On a single-tenant deploy the admin IS the
    // super-admin, so this changes nothing.
    if role_id != SUPER_ADMIN_ROLE_ID {
        return (
            StatusCode::UNAUTHORIZED,
            "grafana restricted to super-admin",
        )
            .into_response();
    }

    // Pass identity + tenant to the upstream (Grafana) via response headers.
    // nginx `auth_request_set` can copy these into request headers when
    // forwarding to the protected upstream, enabling auth.proxy / X-WEBAUTH-USER
    // style auto-login and (later) per-tenant org routing.
    let mut resp = (StatusCode::OK, "ok").into_response();
    if let Ok(login_hv) = HeaderValue::from_str(&s.login) {
        resp.headers_mut().insert("x-auth-user-login", login_hv);
    }
    if let Ok(id_hv) = HeaderValue::from_str(&s.subject_id.to_string()) {
        resp.headers_mut().insert("x-auth-user-id", id_hv);
    }
    if let Ok(cid_hv) = HeaderValue::from_str(&s.customer_id.to_string()) {
        resp.headers_mut().insert("x-auth-customer-id", cid_hv);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_state;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn app() -> Router {
        let state = test_state().await;
        router().with_state(state)
    }

    #[tokio::test]
    async fn rejects_request_without_cookie() {
        let resp = app()
            .await
            .oneshot(
                Request::builder()
                    .uri("/__mdm_auth_check")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_garbage_cookie() {
        let resp = app()
            .await
            .oneshot(
                Request::builder()
                    .uri("/__mdm_auth_check")
                    .header("cookie", "outpost_session=not-a-real-token-just-junk")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn super_admin_allowed_and_carries_tenant_header() {
        let state = test_state().await;
        let app = router().with_state(state.clone());
        // The seeded admin (id 1) is super-admin (role 1), active, tenant 1.
        let token = crate::session::create_user_session(
            &state.db,
            1,
            1,
            crate::auth_extract::SUPER_ADMIN_ROLE_ID,
            "admin",
            3600,
        )
        .await
        .unwrap();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/__mdm_auth_check")
                    .header("cookie", format!("outpost_session={token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("x-auth-customer-id").unwrap(), "1");
    }

    #[tokio::test]
    async fn regular_tenant_user_denied_shared_grafana() {
        let state = test_state().await;
        let app = router().with_state(state.clone());
        // A non-super-admin (role 2, "operator") in the same tenant.
        sqlx::query(
            "INSERT INTO users (customer_id, role_id, login, is_active) VALUES (1, 2, 'op', 1)",
        )
        .execute(&state.db)
        .await
        .unwrap();
        let uid: i64 = sqlx::query_scalar("SELECT id FROM users WHERE login = 'op'")
            .fetch_one(&state.db)
            .await
            .unwrap();
        let token = crate::session::create_user_session(&state.db, uid, 1, 2, "op", 3600)
            .await
            .unwrap();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/__mdm_auth_check")
                    .header("cookie", format!("outpost_session={token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
