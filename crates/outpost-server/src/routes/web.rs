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
        .route("/groups", get(groups_page))
        .route("/applications", get(applications_page))
        .route("/configurations", get(configurations_page))
        .route("/push", get(push_page))
        .route("/users", get(users_page))
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

// ----- groups ------------------------------------------------------------

#[derive(Template)]
#[template(path = "groups.html")]
struct GroupsTemplate {
    user_login: String,
    total: i64,
    groups: Vec<GroupRow>,
}

struct GroupRow {
    name: String,
    description: String,
    member_count: i64,
    created_at: String,
}

#[derive(sqlx::FromRow)]
struct GroupRowRaw {
    name: String,
    description: Option<String>,
    member_count: i64,
    created_at: String,
}

async fn groups_page(user: WebUser, State(state): State<AppState>) -> Result<Response, ApiError> {
    let rows: Vec<GroupRowRaw> = sqlx::query_as::<_, GroupRowRaw>(
        "SELECT g.name, g.description, \
                (SELECT COUNT(*) FROM device_groups dg WHERE dg.group_id = g.id) AS member_count, \
                g.created_at \
         FROM groups g WHERE g.customer_id = ? ORDER BY g.name LIMIT 200",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = scalar(
        &state,
        user.customer_id,
        "SELECT COUNT(*) FROM groups WHERE customer_id = ?",
    )
    .await?;
    let groups = rows
        .into_iter()
        .map(|r| GroupRow {
            name: r.name,
            description: r.description.unwrap_or_else(|| "—".into()),
            member_count: r.member_count,
            created_at: fmt_ts(&r.created_at),
        })
        .collect();
    Ok(render(GroupsTemplate {
        user_login: user.login,
        total,
        groups,
    }))
}

// ----- applications ------------------------------------------------------

#[derive(Template)]
#[template(path = "applications.html")]
struct AppsTemplate {
    user_login: String,
    total: i64,
    apps: Vec<AppRow>,
}

struct AppRow {
    package_name: String,
    display_name: String,
    kind: String,
    version_count: i64,
    latest_version: String,
}

#[derive(sqlx::FromRow)]
struct AppRowRaw {
    package_name: String,
    display_name: Option<String>,
    kind: String,
    version_count: i64,
    latest_version: Option<String>,
}

async fn applications_page(
    user: WebUser,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    let rows: Vec<AppRowRaw> = sqlx::query_as::<_, AppRowRaw>(
        "SELECT a.package_name, a.display_name, a.kind, \
                (SELECT COUNT(*) FROM application_versions v WHERE v.application_id = a.id) AS version_count, \
                (SELECT v.version_name FROM application_versions v WHERE v.application_id = a.id ORDER BY v.version_code DESC LIMIT 1) AS latest_version \
         FROM applications a WHERE a.customer_id = ? ORDER BY a.package_name LIMIT 200",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = scalar(
        &state,
        user.customer_id,
        "SELECT COUNT(*) FROM applications WHERE customer_id = ?",
    )
    .await?;
    let apps = rows
        .into_iter()
        .map(|r| AppRow {
            package_name: r.package_name,
            display_name: r.display_name.unwrap_or_else(|| "—".into()),
            kind: r.kind,
            version_count: r.version_count,
            latest_version: r.latest_version.unwrap_or_else(|| "—".into()),
        })
        .collect();
    Ok(render(AppsTemplate {
        user_login: user.login,
        total,
        apps,
    }))
}

// ----- configurations ----------------------------------------------------

#[derive(Template)]
#[template(path = "configurations.html")]
struct ConfigsTemplate {
    user_login: String,
    total: i64,
    configs: Vec<ConfigRow>,
}

struct ConfigRow {
    name: String,
    description: String,
    kiosk_package: String,
    is_active: bool,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct ConfigRowRaw {
    name: String,
    description: Option<String>,
    kiosk_package: Option<String>,
    is_active: bool,
    updated_at: String,
}

async fn configurations_page(
    user: WebUser,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    let rows: Vec<ConfigRowRaw> = sqlx::query_as::<_, ConfigRowRaw>(
        "SELECT name, description, kiosk_package, is_active, updated_at \
         FROM configurations WHERE customer_id = ? ORDER BY name LIMIT 200",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = scalar(
        &state,
        user.customer_id,
        "SELECT COUNT(*) FROM configurations WHERE customer_id = ?",
    )
    .await?;
    let configs = rows
        .into_iter()
        .map(|r| ConfigRow {
            name: r.name,
            description: r.description.unwrap_or_else(|| "—".into()),
            kiosk_package: r.kiosk_package.unwrap_or_else(|| "—".into()),
            is_active: r.is_active,
            updated_at: fmt_ts(&r.updated_at),
        })
        .collect();
    Ok(render(ConfigsTemplate {
        user_login: user.login,
        total,
        configs,
    }))
}

// ----- push messages -----------------------------------------------------

#[derive(Template)]
#[template(path = "push.html")]
struct PushTemplate {
    user_login: String,
    pending: i64,
    sent_24h: i64,
    messages: Vec<PushRow>,
}

struct PushRow {
    created_at: String,
    device_serial: String,
    command: String,
    status: String,
    delivered_at: String,
}

#[derive(sqlx::FromRow)]
struct PushRowRaw {
    created_at: String,
    device_serial: String,
    command: String,
    status: String,
    delivered_at: Option<String>,
}

async fn push_page(user: WebUser, State(state): State<AppState>) -> Result<Response, ApiError> {
    let rows: Vec<PushRowRaw> = sqlx::query_as::<_, PushRowRaw>(
        "SELECT p.created_at, d.serial AS device_serial, p.command, p.status, p.delivered_at \
         FROM push_messages p \
         JOIN devices d ON d.id = p.device_id \
         WHERE p.customer_id = ? \
         ORDER BY p.id DESC LIMIT 100",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    let pending = scalar(
        &state,
        user.customer_id,
        "SELECT COUNT(*) FROM push_messages WHERE customer_id = ? AND status = 'pending'",
    )
    .await?;
    let sent_24h = scalar(
        &state,
        user.customer_id,
        "SELECT COUNT(*) FROM push_messages WHERE customer_id = ? AND status IN ('sent','delivered') AND created_at >= datetime('now', '-1 day')",
    )
    .await?;
    let messages = rows
        .into_iter()
        .map(|r| PushRow {
            created_at: fmt_ts(&r.created_at),
            device_serial: r.device_serial,
            command: r.command,
            status: r.status,
            delivered_at: r.delivered_at.as_deref().map(fmt_ts).unwrap_or_else(|| "—".into()),
        })
        .collect();
    Ok(render(PushTemplate {
        user_login: user.login,
        pending,
        sent_24h,
        messages,
    }))
}

// ----- users -------------------------------------------------------------

#[derive(Template)]
#[template(path = "users.html")]
struct UsersTemplate {
    user_login: String,
    total: i64,
    users: Vec<UserRow>,
}

struct UserRow {
    login: String,
    email: String,
    role_name: String,
    is_active: bool,
    must_change_password: bool,
    last_login_at: String,
}

#[derive(sqlx::FromRow)]
struct UserRowRaw {
    login: String,
    email: Option<String>,
    role_name: String,
    is_active: bool,
    must_change_password: bool,
    last_login_at: Option<String>,
}

async fn users_page(user: WebUser, State(state): State<AppState>) -> Result<Response, ApiError> {
    let rows: Vec<UserRowRaw> = sqlx::query_as::<_, UserRowRaw>(
        "SELECT u.login, u.email, r.name AS role_name, u.is_active, u.must_change_password, u.last_login_at \
         FROM users u JOIN user_roles r ON r.id = u.role_id \
         WHERE u.customer_id = ? ORDER BY u.login LIMIT 200",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    let total = scalar(
        &state,
        user.customer_id,
        "SELECT COUNT(*) FROM users WHERE customer_id = ?",
    )
    .await?;
    let users = rows
        .into_iter()
        .map(|r| UserRow {
            login: r.login,
            email: r.email.unwrap_or_else(|| "—".into()),
            role_name: r.role_name,
            is_active: r.is_active,
            must_change_password: r.must_change_password,
            last_login_at: r.last_login_at.as_deref().map(fmt_ts).unwrap_or_else(|| "—".into()),
        })
        .collect();
    Ok(render(UsersTemplate {
        user_login: user.login,
        total,
        users,
    }))
}

// ----- helpers -----------------------------------------------------------

async fn scalar(state: &AppState, customer_id: i64, sql: &str) -> Result<i64, ApiError> {
    Ok(sqlx::query_scalar(sql)
        .bind(customer_id)
        .fetch_one(&state.db)
        .await?)
}

/// Best-effort prettifier for the SQLite `datetime('now')` TEXT format
/// (`YYYY-MM-DD HH:MM:SS`). Anything we can't parse passes through verbatim
/// so the UI never crashes on a stale row.
fn fmt_ts(s: &str) -> String {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| s.to_string())
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
