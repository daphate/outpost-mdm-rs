//! HTML admin UI routes — Askama templates + cookie-based session.

use crate::auth as crypto;
use crate::auth_extract::extract_token;
use crate::client_ip::ClientIp;
use crate::error::ApiError;
use crate::session::{self, KIND_USER};
use crate::state::AppState;
use askama::Template;
use axum::extract::{Form, FromRequestParts, State};
use axum::http::header;
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(root))
        .route("/login", get(login_page).post(login_submit))
        .route("/logout", get(logout))
        .route("/dashboard", get(dashboard))
        .route("/devices", get(devices_page))
}

// ----- Web-session extractor: cookie-or-redirect -------------------------

#[derive(Debug, Clone)]
pub struct WebUser {
    pub id: i64,
    pub customer_id: i64,
    pub role_id: i64,
    pub login: String,
}

impl FromRequestParts<AppState> for WebUser {
    type Rejection = Redirect;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = extract_token(parts).ok_or_else(|| Redirect::to("/login"))?;
        let s = session::verify(&token, &state.db)
            .await
            .map_err(|_| Redirect::to("/login"))?;
        if s.kind != KIND_USER {
            return Err(Redirect::to("/login"));
        }
        let active: Option<i64> = sqlx::query_scalar("SELECT is_active FROM users WHERE id = ?")
            .bind(s.subject_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|_| Redirect::to("/login"))?;
        match active {
            Some(1) => Ok(WebUser {
                id: s.subject_id,
                customer_id: s.customer_id,
                role_id: s.role_id,
                login: s.login,
            }),
            _ => Err(Redirect::to("/login")),
        }
    }
}

// ----- Handlers ----------------------------------------------------------

async fn root() -> Redirect {
    Redirect::to("/dashboard")
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate {
    error: Option<String>,
}

async fn login_page() -> Response {
    render(LoginTemplate { error: None })
}

#[derive(Debug, Deserialize)]
pub struct LoginForm {
    pub login: String,
    pub password: String,
}

async fn login_submit(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    Form(form): Form<LoginForm>,
) -> Response {
    if !state.login_limiter.try_take(ip) {
        tracing::warn!(%ip, login = %form.login, "web login rate limit exceeded");
        return render(LoginTemplate {
            error: Some("Too many login attempts. Try again in a moment.".into()),
        });
    }
    let row: Option<(i64, i64, i64, Option<String>, i64)> = match sqlx::query_as(
        "SELECT id, customer_id, role_id, password_hash, is_active FROM users WHERE login = ?",
    )
    .bind(&form.login)
    .fetch_optional(&state.db)
    .await
    {
        Ok(row) => row,
        Err(e) => {
            tracing::error!(error = %e, "login DB error");
            return render(LoginTemplate {
                error: Some("Internal error. Please try again.".into()),
            });
        }
    };
    let Some((id, customer_id, role_id, password_hash, is_active)) = row else {
        return render(LoginTemplate {
            error: Some("Invalid login or password.".into()),
        });
    };
    if is_active == 0 {
        return render(LoginTemplate {
            error: Some("This account is disabled.".into()),
        });
    }
    let Some(phc) = password_hash else {
        return render(LoginTemplate {
            error: Some("Password not yet initialised — contact administrator.".into()),
        });
    };
    if !crypto::verify_password(&form.password, &phc).unwrap_or(false) {
        return render(LoginTemplate {
            error: Some("Invalid login or password.".into()),
        });
    }

    let token = match session::create_user_session(
        &state.db,
        id,
        customer_id,
        role_id,
        &form.login,
        state.session_ttl_secs,
    )
    .await
    {
        Ok(t) => t,
        Err(_) => {
            return render(LoginTemplate {
                error: Some("Could not issue session.".into()),
            });
        }
    };
    let _ = sqlx::query("UPDATE users SET last_login_at = datetime('now') WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await;

    let mut resp = Redirect::to("/dashboard").into_response();
    set_session_cookie(
        &mut resp,
        &token,
        state.secure_cookies,
        state.session_ttl_secs,
    );
    resp
}

async fn logout(parts_extractor: LogoutToken, State(state): State<AppState>) -> Response {
    if let Some(token) = parts_extractor.token {
        let _ = session::revoke(&token, &state.db).await;
    }
    let mut resp = Redirect::to("/login").into_response();
    clear_session_cookie(&mut resp);
    resp
}

/// Inline extractor that copies the token out of the request so we can
/// revoke it server-side during logout.
struct LogoutToken {
    token: Option<String>,
}

impl<S: Send + Sync> FromRequestParts<S> for LogoutToken {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self {
            token: extract_token(parts),
        })
    }
}

#[derive(Template)]
#[template(path = "dashboard.html")]
struct DashboardTemplate {
    user_login: String,
    stats: FleetStatsView,
}

#[derive(Debug)]
struct FleetStatsView {
    devices_total: i64,
    devices_online: i64,
    devices_enrolled: i64,
    applications_total: i64,
    configurations_total: i64,
    push_pending: i64,
    push_sent_24h: i64,
}

async fn dashboard(user: WebUser, State(state): State<AppState>) -> Result<Response, ApiError> {
    let stats = FleetStatsView {
        devices_total: scalar(
            &state,
            user.customer_id,
            "SELECT COUNT(*) FROM devices WHERE customer_id = ?",
        )
        .await?,
        devices_online: scalar(
            &state,
            user.customer_id,
            "SELECT COUNT(*) FROM devices WHERE customer_id = ? AND is_online = 1",
        )
        .await?,
        devices_enrolled: scalar(
            &state,
            user.customer_id,
            "SELECT COUNT(*) FROM devices WHERE customer_id = ? AND is_enrolled = 1",
        )
        .await?,
        applications_total: scalar(
            &state,
            user.customer_id,
            "SELECT COUNT(*) FROM applications WHERE customer_id = ?",
        )
        .await?,
        configurations_total: scalar(
            &state,
            user.customer_id,
            "SELECT COUNT(*) FROM configurations WHERE customer_id = ?",
        )
        .await?,
        push_pending: scalar(
            &state,
            user.customer_id,
            "SELECT COUNT(*) FROM push_messages WHERE customer_id = ? AND status = 'pending'",
        )
        .await?,
        push_sent_24h: scalar(
            &state,
            user.customer_id,
            "SELECT COUNT(*) FROM push_messages WHERE customer_id = ? AND status IN ('sent','delivered') AND created_at >= datetime('now', '-1 day')",
        )
        .await?,
    };
    Ok(render(DashboardTemplate {
        user_login: user.login,
        stats,
    }))
}

#[derive(Template)]
#[template(path = "devices.html")]
struct DevicesTemplate {
    user_login: String,
    total: i64,
    devices: Vec<DeviceRow>,
}

struct DeviceRow {
    serial: String,
    display_name: String,
    is_online: bool,
    battery: String,
    app_version: String,
    last_seen: String,
}

#[derive(sqlx::FromRow)]
struct DeviceRowRaw {
    serial: String,
    display_name: Option<String>,
    is_online: bool,
    battery_pct: Option<i64>,
    app_version: Option<String>,
    last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
}

async fn devices_page(user: WebUser, State(state): State<AppState>) -> Result<Response, ApiError> {
    let rows: Vec<DeviceRowRaw> = sqlx::query_as::<_, DeviceRowRaw>(
        "SELECT serial, display_name, is_online, battery_pct, app_version, last_seen_at \
         FROM devices WHERE customer_id = ? ORDER BY id DESC LIMIT 200",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM devices WHERE customer_id = ?")
        .bind(user.customer_id)
        .fetch_one(&state.db)
        .await?;
    let devices: Vec<DeviceRow> = rows
        .into_iter()
        .map(|r| DeviceRow {
            serial: r.serial,
            display_name: r.display_name.unwrap_or_else(|| "—".into()),
            is_online: r.is_online,
            battery: r
                .battery_pct
                .map(|b| format!("{b}%"))
                .unwrap_or_else(|| "—".into()),
            app_version: r.app_version.unwrap_or_else(|| "—".into()),
            last_seen: r
                .last_seen_at
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "—".into()),
        })
        .collect();
    Ok(render(DevicesTemplate {
        user_login: user.login,
        total,
        devices,
    }))
}

// ----- helpers -----------------------------------------------------------

async fn scalar(state: &AppState, customer_id: i64, sql: &str) -> Result<i64, ApiError> {
    Ok(sqlx::query_scalar(sql)
        .bind(customer_id)
        .fetch_one(&state.db)
        .await?)
}

fn render<T: Template>(t: T) -> Response {
    match t.render() {
        Ok(body) => Html(body).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "askama render failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": {"code": "internal", "message": "render"}})),
            )
                .into_response()
        }
    }
}

fn set_session_cookie(resp: &mut Response, token: &str, secure: bool, ttl_secs: i64) {
    let cookie = format!(
        "outpost_session={token}; Path=/; HttpOnly; SameSite=Lax{}; Max-Age={ttl_secs}",
        if secure { "; Secure" } else { "" },
    );
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
}

fn clear_session_cookie(resp: &mut Response) {
    let cookie = "outpost_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0".to_string();
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
}
