//! HTML admin UI routes — Askama templates + cookie-based session.

use crate::auth as crypto;
use crate::auth_extract::extract_token;
use crate::client_ip::ClientIp;
use crate::error::ApiError;
use crate::session::{self, KIND_USER};
use crate::state::AppState;
use askama::Template;
use axum::extract::{Form, FromRequestParts, Multipart, Path, State};
use axum::http::header;
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(root))
        .route("/login", get(login_page).post(login_submit))
        .route("/logout", get(logout))
        .route("/dashboard", get(dashboard))
        // Devices: list, create form-target, per-device enroll + push views.
        .route("/devices", get(devices_page))
        .route("/devices/new", post(devices_create))
        .route(
            "/devices/{id}/enroll",
            get(device_enroll_view).post(device_enroll_post),
        )
        .route(
            "/devices/{id}/push",
            get(device_push_view).post(device_push_post),
        )
        // Groups
        .route("/groups", get(groups_page))
        .route("/groups/new", post(groups_create))
        // Applications: APK + asset upload (multipart, single round-trip).
        .route("/applications", get(applications_page))
        .route("/applications/upload", post(applications_upload))
        // Configurations
        .route("/configurations", get(configurations_page))
        .route("/configurations/new", post(configurations_create))
        // Push schedule (cross-device / cross-group)
        .route("/push", get(push_page))
        .route("/push/new", post(push_create))
        // Users
        .route("/users", get(users_page))
        .route("/users/new", post(users_create))
        .route("/users/{id}/toggle-active", post(users_toggle_active))
        // Current-user password change
        .route(
            "/me/password",
            get(me_password_view).post(me_password_post),
        )
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
    flash: Option<String>,
    create_error: Option<String>,
}

struct DeviceRow {
    id: i64,
    serial: String,
    display_name: String,
    is_enrolled: bool,
    is_online: bool,
    battery: String,
    app_version: String,
    last_seen: String,
}

#[derive(sqlx::FromRow)]
struct DeviceRowRaw {
    id: i64,
    serial: String,
    display_name: Option<String>,
    is_enrolled: bool,
    is_online: bool,
    battery_pct: Option<i64>,
    app_version: Option<String>,
    last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
}

async fn devices_page(
    user: WebUser,
    State(state): State<AppState>,
    flash: FlashCookie,
) -> Result<Response, ApiError> {
    render_devices(&user, &state, flash.0, None).await
}

async fn render_devices(
    user: &WebUser,
    state: &AppState,
    flash: Option<String>,
    create_error: Option<String>,
) -> Result<Response, ApiError> {
    let rows: Vec<DeviceRowRaw> = sqlx::query_as::<_, DeviceRowRaw>(
        "SELECT id, serial, display_name, is_enrolled, is_online, battery_pct, app_version, last_seen_at \
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
            id: r.id,
            serial: r.serial,
            display_name: r.display_name.unwrap_or_else(|| "—".into()),
            is_enrolled: r.is_enrolled,
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
    let mut resp = render(DevicesTemplate {
        user_login: user.login.clone(),
        total,
        devices,
        flash,
        create_error,
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

#[derive(Debug, Deserialize)]
struct NewDeviceForm {
    serial: String,
    display_name: Option<String>,
}

async fn devices_create(
    user: WebUser,
    State(state): State<AppState>,
    Form(req): Form<NewDeviceForm>,
) -> Result<Response, Response> {
    let serial = req.serial.trim();
    if serial.is_empty() {
        return render_devices(&user, &state, None, Some("Serial is required".into()))
            .await
            .map_err(|e| e.into_response());
    }
    let display_name = req.display_name.as_deref().map(|s| s.trim()).filter(|s| !s.is_empty());
    let res = sqlx::query(
        "INSERT INTO devices (customer_id, serial, display_name) VALUES (?, ?, ?)",
    )
    .bind(user.customer_id)
    .bind(serial)
    .bind(display_name)
    .execute(&state.db)
    .await;
    match res {
        Ok(_) => Ok(redirect_with_flash("/devices", "Device created.")),
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => Ok(render_devices(
            &user,
            &state,
            None,
            Some(format!("Device with serial '{}' already exists", serial)),
        )
        .await
        .map_err(|e| e.into_response())?),
        Err(e) => {
            tracing::error!(error = %e, "devices_create insert failed");
            Ok(render_devices(
                &user,
                &state,
                None,
                Some("Database error — see server logs".into()),
            )
            .await
            .map_err(|e| e.into_response())?)
        }
    }
}

// ----- groups ------------------------------------------------------------

#[derive(Template)]
#[template(path = "groups.html")]
struct GroupsTemplate {
    user_login: String,
    total: i64,
    groups: Vec<GroupRow>,
    flash: Option<String>,
    create_error: Option<String>,
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

async fn groups_page(
    user: WebUser,
    State(state): State<AppState>,
    flash: FlashCookie,
) -> Result<Response, ApiError> {
    render_groups(&user, &state, flash.0, None).await
}

async fn render_groups(
    user: &WebUser,
    state: &AppState,
    flash: Option<String>,
    create_error: Option<String>,
) -> Result<Response, ApiError> {
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
        state,
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
    let mut resp = render(GroupsTemplate {
        user_login: user.login.clone(),
        total,
        groups,
        flash,
        create_error,
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

#[derive(Debug, Deserialize)]
struct NewGroupForm {
    name: String,
    description: Option<String>,
}

async fn groups_create(
    user: WebUser,
    State(state): State<AppState>,
    Form(req): Form<NewGroupForm>,
) -> Result<Response, Response> {
    let name = req.name.trim();
    if name.is_empty() {
        return render_groups(&user, &state, None, Some("Name is required".into()))
            .await
            .map_err(|e| e.into_response());
    }
    let description = req.description.as_deref().map(|s| s.trim()).filter(|s| !s.is_empty());
    let res = sqlx::query("INSERT INTO groups (customer_id, name, description) VALUES (?, ?, ?)")
        .bind(user.customer_id)
        .bind(name)
        .bind(description)
        .execute(&state.db)
        .await;
    match res {
        Ok(_) => Ok(redirect_with_flash("/groups", "Group created.")),
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => Ok(render_groups(
            &user,
            &state,
            None,
            Some(format!("Group '{}' already exists", name)),
        )
        .await
        .map_err(|e| e.into_response())?),
        Err(e) => {
            tracing::error!(error = %e, "groups_create insert failed");
            Ok(render_groups(&user, &state, None, Some("Database error".into()))
                .await
                .map_err(|e| e.into_response())?)
        }
    }
}

// ----- applications ------------------------------------------------------

#[derive(Template)]
#[template(path = "applications.html")]
struct AppsTemplate {
    user_login: String,
    total: i64,
    apps: Vec<AppRow>,
    flash: Option<String>,
    create_error: Option<String>,
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
    flash: FlashCookie,
) -> Result<Response, ApiError> {
    render_apps(&user, &state, flash.0, None).await
}

async fn render_apps(
    user: &WebUser,
    state: &AppState,
    flash: Option<String>,
    create_error: Option<String>,
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
        state,
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
    let mut resp = render(AppsTemplate {
        user_login: user.login.clone(),
        total,
        apps,
        flash,
        create_error,
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

async fn applications_upload(
    user: WebUser,
    State(state): State<AppState>,
    multipart: Multipart,
) -> Response {
    match try_applications_upload(&user, &state, multipart).await {
        Ok(()) => redirect_with_flash("/applications", "Application uploaded."),
        Err(msg) => render_apps(&user, &state, None, Some(msg))
            .await
            .unwrap_or_else(|_| {
                (StatusCode::INTERNAL_SERVER_ERROR, "render failed").into_response()
            }),
    }
}

async fn try_applications_upload(
    user: &WebUser,
    state: &AppState,
    mut multipart: Multipart,
) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let mut package_name = String::new();
    let mut display_name: Option<String> = None;
    let mut kind = "apk".to_string();
    let mut version_code: Option<i64> = None;
    let mut version_name = String::new();
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut file_original: Option<String> = None;
    let mut file_content_type: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| format!("multipart error: {e}"))?
    {
        match field.name().unwrap_or("") {
            "package_name" => {
                package_name = field
                    .text()
                    .await
                    .map_err(|e| format!("package_name: {e}"))?
            }
            "display_name" => {
                display_name = field.text().await.ok().filter(|s| !s.trim().is_empty())
            }
            "kind" => {
                if let Ok(v) = field.text().await {
                    if !v.trim().is_empty() {
                        kind = v;
                    }
                }
            }
            "version_code" => {
                let raw = field
                    .text()
                    .await
                    .map_err(|e| format!("version_code: {e}"))?;
                version_code = raw.trim().parse::<i64>().ok();
            }
            "version_name" => {
                version_name = field
                    .text()
                    .await
                    .map_err(|e| format!("version_name: {e}"))?
            }
            "file" => {
                file_original = field.file_name().map(|s| s.to_string());
                file_content_type = field.content_type().map(|s| s.to_string());
                let data = field
                    .bytes()
                    .await
                    .map_err(|e| format!("file read: {e}"))?;
                file_bytes = Some(data.to_vec());
            }
            _ => {}
        }
    }

    if package_name.trim().is_empty() {
        return Err("package_name is required".into());
    }
    if version_name.trim().is_empty() {
        return Err("version_name is required".into());
    }
    let version_code =
        version_code.ok_or_else(|| "version_code must be a positive integer".to_string())?;
    let bytes = file_bytes.ok_or_else(|| "file is required".to_string())?;
    let original_name = file_original.ok_or_else(|| "file has no filename".to_string())?;

    let extension = std::path::Path::new(&original_name)
        .extension()
        .and_then(|e| e.to_str());

    let stored = crate::storage::write_bytes(state.app_files_dir.as_ref(), &bytes, extension)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "storage write failed");
            "storage write failed".to_string()
        })?;

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let sha = hex::encode(hasher.finalize());

    let mut tx = state.db.begin().await.map_err(|e| {
        tracing::error!(error = %e, "tx begin failed");
        "database error".to_string()
    })?;

    // Find-or-create application by (customer_id, package_name).
    let app_id_opt: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM applications WHERE customer_id = ? AND package_name = ?",
    )
    .bind(user.customer_id)
    .bind(package_name.trim())
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "find application failed");
        "database error".to_string()
    })?;
    let application_id: i64 = if let Some(id) = app_id_opt {
        id
    } else {
        sqlx::query_scalar(
            "INSERT INTO applications (customer_id, package_name, display_name, kind) \
             VALUES (?, ?, ?, ?) RETURNING id",
        )
        .bind(user.customer_id)
        .bind(package_name.trim())
        .bind(display_name.as_deref())
        .bind(&kind)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "insert application failed");
            "database error".to_string()
        })?
    };

    let file_size = bytes.len() as i64;

    sqlx::query(
        "INSERT INTO uploaded_files \
            (customer_id, file_path, original_name, content_type, file_size_bytes, sha256, kind, uploaded_by) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(user.customer_id)
    .bind(&stored.relative_path)
    .bind(&original_name)
    .bind(&file_content_type)
    .bind(file_size)
    .bind(&sha)
    .bind(&kind)
    .bind(user.id)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "insert uploaded_file failed");
        "database error".to_string()
    })?;

    match sqlx::query(
        "INSERT INTO application_versions \
            (application_id, version_code, version_name, file_path, file_size_bytes, sha256, is_active, uploaded_by) \
         VALUES (?, ?, ?, ?, ?, ?, 1, ?)",
    )
    .bind(application_id)
    .bind(version_code)
    .bind(version_name.trim())
    .bind(&stored.relative_path)
    .bind(file_size)
    .bind(&sha)
    .bind(user.id)
    .execute(&mut *tx)
    .await
    {
        Ok(_) => {
            tx.commit().await.map_err(|e| {
                tracing::error!(error = %e, "tx commit failed");
                "database error".to_string()
            })?;
            Ok(())
        }
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
            let _ = tx.rollback().await;
            Err(format!(
                "version_code {version_code} already exists for this package"
            ))
        }
        Err(e) => {
            tracing::error!(error = %e, "insert version failed");
            let _ = tx.rollback().await;
            Err("database error".into())
        }
    }
}

// ----- configurations ----------------------------------------------------

#[derive(Template)]
#[template(path = "configurations.html")]
struct ConfigsTemplate {
    user_login: String,
    total: i64,
    configs: Vec<ConfigRow>,
    flash: Option<String>,
    create_error: Option<String>,
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
    flash: FlashCookie,
) -> Result<Response, ApiError> {
    render_configs(&user, &state, flash.0, None).await
}

async fn render_configs(
    user: &WebUser,
    state: &AppState,
    flash: Option<String>,
    create_error: Option<String>,
) -> Result<Response, ApiError> {
    let rows: Vec<ConfigRowRaw> = sqlx::query_as::<_, ConfigRowRaw>(
        "SELECT name, description, kiosk_package, is_active, updated_at \
         FROM configurations WHERE customer_id = ? ORDER BY name LIMIT 200",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = scalar(
        state,
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
    let mut resp = render(ConfigsTemplate {
        user_login: user.login.clone(),
        total,
        configs,
        flash,
        create_error,
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

#[derive(Debug, Deserialize)]
struct NewConfigForm {
    name: String,
    description: Option<String>,
    kiosk_package: Option<String>,
    settings_json: Option<String>,
}

async fn configurations_create(
    user: WebUser,
    State(state): State<AppState>,
    Form(req): Form<NewConfigForm>,
) -> Result<Response, Response> {
    let name = req.name.trim();
    if name.is_empty() {
        return render_configs(&user, &state, None, Some("Name is required".into()))
            .await
            .map_err(|e| e.into_response());
    }
    let description = req.description.as_deref().map(|s| s.trim()).filter(|s| !s.is_empty());
    let kiosk_package = req.kiosk_package.as_deref().map(|s| s.trim()).filter(|s| !s.is_empty());
    let settings_json = req
        .settings_json
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("{}");
    if let Err(e) = serde_json::from_str::<serde_json::Value>(settings_json) {
        return render_configs(
            &user,
            &state,
            None,
            Some(format!("settings_json is not valid JSON: {e}")),
        )
        .await
        .map_err(|err| err.into_response());
    }
    let res = sqlx::query(
        "INSERT INTO configurations (customer_id, name, description, settings_json, kiosk_package) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(user.customer_id)
    .bind(name)
    .bind(description)
    .bind(settings_json)
    .bind(kiosk_package)
    .execute(&state.db)
    .await;
    match res {
        Ok(_) => Ok(redirect_with_flash("/configurations", "Configuration created.")),
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => Ok(render_configs(
            &user,
            &state,
            None,
            Some(format!("Configuration '{}' already exists", name)),
        )
        .await
        .map_err(|e| e.into_response())?),
        Err(e) => {
            tracing::error!(error = %e, "configurations_create insert failed");
            Ok(render_configs(&user, &state, None, Some("Database error".into()))
                .await
                .map_err(|e| e.into_response())?)
        }
    }
}

// ----- push messages -----------------------------------------------------

#[derive(Template)]
#[template(path = "push.html")]
struct PushTemplate {
    user_login: String,
    pending: i64,
    sent_24h: i64,
    messages: Vec<PushRow>,
    target_devices: Vec<DeviceOption>,
    target_groups: Vec<GroupOption>,
    flash: Option<String>,
    create_error: Option<String>,
}

#[derive(sqlx::FromRow)]
struct DeviceOption {
    id: i64,
    serial: String,
}

#[derive(sqlx::FromRow)]
struct GroupOption {
    id: i64,
    name: String,
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

async fn push_page(
    user: WebUser,
    State(state): State<AppState>,
    flash: FlashCookie,
) -> Result<Response, ApiError> {
    render_push(&user, &state, flash.0, None).await
}

async fn render_push(
    user: &WebUser,
    state: &AppState,
    flash: Option<String>,
    create_error: Option<String>,
) -> Result<Response, ApiError> {
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
        state,
        user.customer_id,
        "SELECT COUNT(*) FROM push_messages WHERE customer_id = ? AND status = 'pending'",
    )
    .await?;
    let sent_24h = scalar(
        state,
        user.customer_id,
        "SELECT COUNT(*) FROM push_messages WHERE customer_id = ? AND status IN ('sent','delivered') AND created_at >= datetime('now', '-1 day')",
    )
    .await?;
    let target_devices: Vec<DeviceOption> = sqlx::query_as::<_, DeviceOption>(
        "SELECT id, serial FROM devices WHERE customer_id = ? ORDER BY serial LIMIT 200",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    let target_groups: Vec<GroupOption> = sqlx::query_as::<_, GroupOption>(
        "SELECT id, name FROM groups WHERE customer_id = ? ORDER BY name LIMIT 200",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
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
    let mut resp = render(PushTemplate {
        user_login: user.login.clone(),
        pending,
        sent_24h,
        messages,
        target_devices,
        target_groups,
        flash,
        create_error,
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

#[derive(Debug, Deserialize)]
struct NewPushForm {
    target: String,
    command: String,
    payload_json: Option<String>,
    due_at: Option<String>,
}

async fn push_create(
    user: WebUser,
    State(state): State<AppState>,
    Form(req): Form<NewPushForm>,
) -> Result<Response, Response> {
    let (device_id, group_id) = match req.target.split_once(':') {
        Some(("device", id)) => match id.parse::<i64>() {
            Ok(n) => (Some(n), None),
            Err(_) => {
                return render_push(&user, &state, None, Some("Invalid device target".into()))
                    .await
                    .map_err(|e| e.into_response());
            }
        },
        Some(("group", id)) => match id.parse::<i64>() {
            Ok(n) => (None, Some(n)),
            Err(_) => {
                return render_push(&user, &state, None, Some("Invalid group target".into()))
                    .await
                    .map_err(|e| e.into_response());
            }
        },
        _ => {
            return render_push(&user, &state, None, Some("Select a target".into()))
                .await
                .map_err(|e| e.into_response());
        }
    };
    let command = req.command.trim();
    if command.is_empty() {
        return render_push(&user, &state, None, Some("Command is required".into()))
            .await
            .map_err(|e| e.into_response());
    }
    let payload = req
        .payload_json
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("{}");
    if let Err(e) = serde_json::from_str::<serde_json::Value>(payload) {
        return render_push(
            &user,
            &state,
            None,
            Some(format!("payload_json is not valid JSON: {e}")),
        )
        .await
        .map_err(|err| err.into_response());
    }
    // due_at from <input type="datetime-local"> arrives as "2026-05-17T12:34" (no tz).
    // Treat as UTC; if blank, leave NULL (scheduler will pick it up on next tick).
    let due_at_iso = req
        .due_at
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.ends_with('Z') || s.contains('+') {
                s.to_string()
            } else {
                format!("{s}:00Z")
            }
        });
    let res = sqlx::query(
        "INSERT INTO push_schedule \
            (customer_id, device_id, group_id, configuration_id, command, payload_json, due_at, created_by) \
         VALUES (?, ?, ?, NULL, ?, ?, ?, ?)",
    )
    .bind(user.customer_id)
    .bind(device_id)
    .bind(group_id)
    .bind(command)
    .bind(payload)
    .bind(&due_at_iso)
    .bind(user.id)
    .execute(&state.db)
    .await;
    match res {
        Ok(_) => Ok(redirect_with_flash("/push", "Push scheduled.")),
        Err(e) => {
            tracing::error!(error = %e, "push_create insert failed");
            Ok(render_push(&user, &state, None, Some("Database error".into()))
                .await
                .map_err(|e| e.into_response())?)
        }
    }
}

// ----- users -------------------------------------------------------------

#[derive(Template)]
#[template(path = "users.html")]
struct UsersTemplate {
    user_login: String,
    current_user_id: i64,
    total: i64,
    users: Vec<UserRow>,
    flash: Option<String>,
    create_error: Option<String>,
}

struct UserRow {
    id: i64,
    login: String,
    email: String,
    role_name: String,
    is_active: bool,
    must_change_password: bool,
    last_login_at: String,
}

#[derive(sqlx::FromRow)]
struct UserRowRaw {
    id: i64,
    login: String,
    email: Option<String>,
    role_name: String,
    is_active: bool,
    must_change_password: bool,
    last_login_at: Option<String>,
}

async fn users_page(
    user: WebUser,
    State(state): State<AppState>,
    flash: FlashCookie,
) -> Result<Response, ApiError> {
    render_users(&user, &state, flash.0, None).await
}

async fn render_users(
    user: &WebUser,
    state: &AppState,
    flash: Option<String>,
    create_error: Option<String>,
) -> Result<Response, ApiError> {
    let rows: Vec<UserRowRaw> = sqlx::query_as::<_, UserRowRaw>(
        "SELECT u.id, u.login, u.email, r.name AS role_name, u.is_active, u.must_change_password, u.last_login_at \
         FROM users u JOIN user_roles r ON r.id = u.role_id \
         WHERE u.customer_id = ? ORDER BY u.login LIMIT 200",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    let total = scalar(
        state,
        user.customer_id,
        "SELECT COUNT(*) FROM users WHERE customer_id = ?",
    )
    .await?;
    let users = rows
        .into_iter()
        .map(|r| UserRow {
            id: r.id,
            login: r.login,
            email: r.email.unwrap_or_else(|| "—".into()),
            role_name: r.role_name,
            is_active: r.is_active,
            must_change_password: r.must_change_password,
            last_login_at: r.last_login_at.as_deref().map(fmt_ts).unwrap_or_else(|| "—".into()),
        })
        .collect();
    let mut resp = render(UsersTemplate {
        user_login: user.login.clone(),
        current_user_id: user.id,
        total,
        users,
        flash,
        create_error,
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

#[derive(Debug, Deserialize)]
struct NewUserForm {
    login: String,
    email: Option<String>,
    password: String,
    role_id: i64,
}

async fn users_create(
    user: WebUser,
    State(state): State<AppState>,
    Form(req): Form<NewUserForm>,
) -> Result<Response, Response> {
    let login = req.login.trim();
    if login.is_empty() {
        return render_users(&user, &state, None, Some("Login is required".into()))
            .await
            .map_err(|e| e.into_response());
    }
    if req.password.len() < 8 {
        return render_users(
            &user,
            &state,
            None,
            Some("Password must be at least 8 characters".into()),
        )
        .await
        .map_err(|e| e.into_response());
    }
    if !(2..=4).contains(&req.role_id) {
        return render_users(
            &user,
            &state,
            None,
            Some("Invalid role (must be admin / operator / viewer)".into()),
        )
        .await
        .map_err(|e| e.into_response());
    }
    let email = req.email.as_deref().map(|s| s.trim()).filter(|s| !s.is_empty());
    let phc = match crypto::hash_password(&req.password) {
        Ok(s) => s,
        Err(_) => {
            return render_users(&user, &state, None, Some("Password hash error".into()))
                .await
                .map_err(|e| e.into_response());
        }
    };
    let res = sqlx::query(
        "INSERT INTO users (customer_id, role_id, login, email, password_hash, is_active) \
         VALUES (?, ?, ?, ?, ?, 1)",
    )
    .bind(user.customer_id)
    .bind(req.role_id)
    .bind(login)
    .bind(email)
    .bind(&phc)
    .execute(&state.db)
    .await;
    match res {
        Ok(_) => Ok(redirect_with_flash("/users", "User created.")),
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => Ok(render_users(
            &user,
            &state,
            None,
            Some(format!("Login '{login}' already exists")),
        )
        .await
        .map_err(|e| e.into_response())?),
        Err(e) => {
            tracing::error!(error = %e, "users_create insert failed");
            Ok(render_users(&user, &state, None, Some("Database error".into()))
                .await
                .map_err(|e| e.into_response())?)
        }
    }
}

async fn users_toggle_active(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Response {
    if id == user.id {
        return redirect_with_flash("/users", "Cannot deactivate your own account.");
    }
    let res = sqlx::query(
        "UPDATE users SET is_active = 1 - is_active, updated_at = datetime('now') \
         WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .execute(&state.db)
    .await;
    match res {
        Ok(r) if r.rows_affected() > 0 => redirect_with_flash("/users", "User status updated."),
        Ok(_) => redirect_with_flash("/users", "User not found."),
        Err(e) => {
            tracing::error!(error = %e, "users_toggle_active failed");
            redirect_with_flash("/users", "Database error.")
        }
    }
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

// ----- flash messaging (single-shot success banners across POST→redirect) ----

/// Extractor that pulls the `outpost_flash` cookie value (if any) out of the
/// incoming request. The companion `clear_flash_cookie` MUST be called on
/// the rendered response so the banner only fires once.
pub struct FlashCookie(pub Option<String>);

impl FromRequestParts<AppState> for FlashCookie {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let hdr = parts.headers.get(header::COOKIE).and_then(|v| v.to_str().ok());
        let Some(hdr) = hdr else { return Ok(FlashCookie(None)) };
        for kv in hdr.split(';') {
            let kv = kv.trim();
            if let Some(v) = kv.strip_prefix("outpost_flash=") {
                let decoded = percent_decode(v);
                if !decoded.is_empty() {
                    return Ok(FlashCookie(Some(decoded)));
                }
            }
        }
        Ok(FlashCookie(None))
    }
}

fn percent_decode(s: &str) -> String {
    // RFC 3986 — we only emit `%20` and `%25` ourselves; decode generically.
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_digit(bytes[i + 1]), hex_digit(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8(out).unwrap_or_default()
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn percent_encode_minimal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn set_flash_cookie(resp: &mut Response, msg: &str) {
    let encoded = percent_encode_minimal(msg);
    let cookie = format!("outpost_flash={encoded}; Path=/; SameSite=Lax; Max-Age=30");
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        // Append (not insert) so we don't stomp on Set-Session-Cookie / others.
        resp.headers_mut().append(header::SET_COOKIE, v);
    }
}

fn clear_flash_cookie(resp: &mut Response) {
    let cookie = "outpost_flash=; Path=/; SameSite=Lax; Max-Age=0";
    if let Ok(v) = HeaderValue::from_str(cookie) {
        resp.headers_mut().append(header::SET_COOKIE, v);
    }
}

fn redirect_with_flash(target: &str, msg: &str) -> Response {
    let mut resp = Redirect::to(target).into_response();
    set_flash_cookie(&mut resp, msg);
    resp
}

// ----- per-device pages: enroll + push -------------------------------------

#[derive(Template)]
#[template(path = "device_enroll.html")]
struct DeviceEnrollTemplate {
    user_login: String,
    device_id: i64,
    serial: String,
    secret: Option<String>,
    payload_json: String,
    qr_svg: String,
    server_url: String,
    error: Option<String>,
}

async fn device_enroll_view(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT serial, enrollment_secret FROM devices WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?;
    let (serial, secret) = row.ok_or(ApiError::NotFound)?;
    Ok(render_device_enroll(&user, &state, id, &serial, secret, None).await)
}

async fn device_enroll_post(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    let serial_row: Option<(String,)> =
        sqlx::query_as("SELECT serial FROM devices WHERE id = ? AND customer_id = ?")
            .bind(id)
            .bind(user.customer_id)
            .fetch_optional(&state.db)
            .await?;
    let Some((serial,)) = serial_row else {
        return Err(ApiError::NotFound);
    };
    let secret = crypto::generate_password(32);
    sqlx::query(
        "UPDATE devices SET enrollment_secret = ?, is_enrolled = 0, updated_at = datetime('now') \
         WHERE id = ?",
    )
    .bind(&secret)
    .bind(id)
    .execute(&state.db)
    .await?;
    Ok(render_device_enroll(&user, &state, id, &serial, Some(secret), None).await)
}

async fn render_device_enroll(
    user: &WebUser,
    state: &AppState,
    device_id: i64,
    serial: &str,
    secret: Option<String>,
    error: Option<String>,
) -> Response {
    let server_url: String = sqlx::query_scalar(
        "SELECT json_extract(value_json, '$') FROM settings WHERE key = 'server.enrollment_base_url'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .flatten()
    .unwrap_or_else(|| "https://mdm.secondf8n.tech".to_string());

    let (payload_json, qr_svg) = if let Some(ref s) = secret {
        let payload = serde_json::json!({
            "server_url": server_url,
            "customer_id": user.customer_id,
            "device_id": device_id,
            "enrollment_secret": s,
        });
        let payload_text = serde_json::to_string_pretty(&payload).unwrap_or_default();
        let svg = qrcode_svg(&payload.to_string());
        (payload_text, svg)
    } else {
        (String::new(), String::new())
    };

    render(DeviceEnrollTemplate {
        user_login: user.login.clone(),
        device_id,
        serial: serial.to_string(),
        secret,
        payload_json,
        qr_svg,
        server_url,
        error,
    })
}

fn qrcode_svg(payload: &str) -> String {
    use qrcode::{QrCode, render::svg};
    match QrCode::new(payload.as_bytes()) {
        Ok(code) => code
            .render::<svg::Color<'_>>()
            .min_dimensions(240, 240)
            .quiet_zone(true)
            .build(),
        Err(e) => format!("<p class='text-red-600'>QR generation failed: {e}</p>"),
    }
}

#[derive(Template)]
#[template(path = "device_push.html")]
struct DevicePushTemplate {
    user_login: String,
    device_id: i64,
    serial: String,
    flash: Option<String>,
    error: Option<String>,
}

async fn device_push_view(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    flash: FlashCookie,
) -> Result<Response, ApiError> {
    let serial_row: Option<(String,)> =
        sqlx::query_as("SELECT serial FROM devices WHERE id = ? AND customer_id = ?")
            .bind(id)
            .bind(user.customer_id)
            .fetch_optional(&state.db)
            .await?;
    let Some((serial,)) = serial_row else {
        return Err(ApiError::NotFound);
    };
    let mut resp = render(DevicePushTemplate {
        user_login: user.login,
        device_id: id,
        serial,
        flash: flash.0,
        error: None,
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

#[derive(Debug, Deserialize)]
struct DevicePushForm {
    command: String,
    payload_json: Option<String>,
    due_at: Option<String>,
}

async fn device_push_post(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(req): Form<DevicePushForm>,
) -> Result<Response, ApiError> {
    let serial_row: Option<(String,)> =
        sqlx::query_as("SELECT serial FROM devices WHERE id = ? AND customer_id = ?")
            .bind(id)
            .bind(user.customer_id)
            .fetch_optional(&state.db)
            .await?;
    let Some((serial,)) = serial_row else {
        return Err(ApiError::NotFound);
    };
    let command = req.command.trim();
    if command.is_empty() {
        let mut resp = render(DevicePushTemplate {
            user_login: user.login,
            device_id: id,
            serial,
            flash: None,
            error: Some("Command is required".into()),
        });
        clear_flash_cookie(&mut resp);
        return Ok(resp);
    }
    let payload = req
        .payload_json
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("{}");
    if let Err(e) = serde_json::from_str::<serde_json::Value>(payload) {
        let mut resp = render(DevicePushTemplate {
            user_login: user.login,
            device_id: id,
            serial,
            flash: None,
            error: Some(format!("payload_json is not valid JSON: {e}")),
        });
        clear_flash_cookie(&mut resp);
        return Ok(resp);
    }
    let due_at_iso = req
        .due_at
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.ends_with('Z') || s.contains('+') {
                s.to_string()
            } else {
                format!("{s}:00Z")
            }
        });
    sqlx::query(
        "INSERT INTO push_schedule \
            (customer_id, device_id, group_id, configuration_id, command, payload_json, due_at, created_by) \
         VALUES (?, ?, NULL, NULL, ?, ?, ?, ?)",
    )
    .bind(user.customer_id)
    .bind(id)
    .bind(command)
    .bind(payload)
    .bind(&due_at_iso)
    .bind(user.id)
    .execute(&state.db)
    .await?;
    Ok(redirect_with_flash(
        &format!("/devices/{id}/push"),
        "Push scheduled.",
    ))
}

// ----- /me/password ---------------------------------------------------------

#[derive(Template)]
#[template(path = "me_password.html")]
struct MePasswordTemplate {
    user_login: String,
    must_change: bool,
    flash: Option<String>,
    error: Option<String>,
}

async fn me_password_view(
    user: WebUser,
    State(state): State<AppState>,
    flash: FlashCookie,
) -> Result<Response, ApiError> {
    let must_change: bool = sqlx::query_scalar(
        "SELECT COALESCE(must_change_password, 0) FROM users WHERE id = ?",
    )
    .bind(user.id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(false);
    let mut resp = render(MePasswordTemplate {
        user_login: user.login,
        must_change,
        flash: flash.0,
        error: None,
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

#[derive(Debug, Deserialize)]
struct ChangePasswordForm {
    current_password: String,
    new_password: String,
    confirm_password: String,
}

async fn me_password_post(
    user: WebUser,
    State(state): State<AppState>,
    Form(req): Form<ChangePasswordForm>,
) -> Result<Response, ApiError> {
    let render_err = |msg: String| async {
        let mut resp = render(MePasswordTemplate {
            user_login: user.login.clone(),
            must_change: false,
            flash: None,
            error: Some(msg),
        });
        clear_flash_cookie(&mut resp);
        resp
    };

    if req.new_password.len() < 8 {
        return Ok(render_err("New password must be at least 8 characters".into()).await);
    }
    if req.new_password != req.confirm_password {
        return Ok(render_err("New password and confirmation do not match".into()).await);
    }
    // Verify current
    let stored_hash: Option<String> =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?")
            .bind(user.id)
            .fetch_optional(&state.db)
            .await?
            .flatten();
    let Some(hash) = stored_hash else {
        return Ok(render_err("Cannot verify current password (no hash on record)".into()).await);
    };
    if !crypto::verify_password(&req.current_password, &hash).unwrap_or(false) {
        return Ok(render_err("Current password is incorrect".into()).await);
    }
    let new_phc = match crypto::hash_password(&req.new_password) {
        Ok(h) => h,
        Err(_) => return Ok(render_err("Password hash error".into()).await),
    };
    sqlx::query(
        "UPDATE users SET password_hash = ?, must_change_password = 0, updated_at = datetime('now') \
         WHERE id = ?",
    )
    .bind(&new_phc)
    .bind(user.id)
    .execute(&state.db)
    .await?;
    Ok(redirect_with_flash("/me/password", "Password updated."))
}
