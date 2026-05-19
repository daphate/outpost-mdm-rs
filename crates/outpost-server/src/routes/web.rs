//! HTML admin UI routes — Askama templates + cookie-based session.

use crate::auth as crypto;
use crate::auth_extract::extract_token;
use crate::client_ip::ClientIp;
use crate::error::ApiError;
use crate::i18n;
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
        // Devices: list, create form-target, per-device edit/enroll/push/delete.
        .route("/devices", get(devices_page))
        .route("/devices/new", post(devices_create))
        .route(
            "/devices/{id}/edit",
            get(device_edit_view).post(device_edit_post),
        )
        .route("/devices/{id}/delete", post(device_delete))
        .route(
            "/devices/{id}/enroll",
            get(device_enroll_view).post(device_enroll_post),
        )
        .route("/devices/{id}/enroll/file", get(device_enroll_download))
        // v0.13 (Settings Sync §1.4): admin-form для отправки update-config
        // push'а. Парсит form-data `payload` как JSON object, INSERT'ит в
        // push_messages. JSON API эквивалент — `POST /api/v1/devices/{id}/config`.
        .route("/devices/{id}/config-form", post(device_config_form))
        .route(
            "/devices/{id}/push",
            get(device_push_view).post(device_push_post),
        )
        // Groups
        .route("/groups", get(groups_page))
        .route("/groups/new", post(groups_create))
        .route(
            "/groups/{id}/edit",
            get(group_edit_view).post(group_edit_post),
        )
        .route("/groups/{id}/delete", post(group_delete))
        // v0.18.6: добавить/убрать устройство в группе. Membership
        // mutations не дублируют configuration_app_add/remove pattern
        // (configurations используют group через apps, не devices напрямую).
        .route("/groups/{id}/members", post(group_member_add))
        .route(
            "/groups/{id}/members/{device_id}/delete",
            post(group_member_remove),
        )
        // Applications: APK + asset upload, edit, versions, delete.
        .route("/applications", get(applications_page))
        .route("/applications/upload", post(applications_upload))
        .route(
            "/applications/{id}/edit",
            get(application_edit_view).post(application_edit_post),
        )
        .route("/applications/{id}/delete", post(application_delete))
        .route(
            "/applications/{id}/versions",
            get(application_versions_view).post(application_version_add),
        )
        .route(
            "/applications/{id}/versions/{vid}/delete",
            post(application_version_delete),
        )
        // v0.12 Tier-2 — rollout policy management
        .route(
            "/applications/{id}/rollouts",
            get(application_rollouts_view).post(application_rollout_create),
        )
        .route(
            "/applications/{id}/rollouts/{rid}/phase",
            post(application_rollout_phase),
        )
        // Configurations
        .route("/configurations", get(configurations_page))
        .route("/configurations/new", post(configurations_create))
        .route(
            "/configurations/{id}/edit",
            get(configuration_edit_view).post(configuration_edit_post),
        )
        .route("/configurations/{id}/delete", post(configuration_delete))
        .route(
            "/configurations/{id}/apps",
            post(configuration_app_add),
        )
        .route(
            "/configurations/{id}/apps/{app_id}/delete",
            post(configuration_app_remove),
        )
        // v0.18.8: пометить конфигурацию как customer-default.
        .route(
            "/configurations/{id}/make-default",
            post(configuration_make_default),
        )
        // Push schedule (cross-device / cross-group)
        .route("/push", get(push_page))
        .route("/push/new", post(push_create))
        // Users
        .route("/users", get(users_page))
        .route("/users/new", post(users_create))
        .route(
            "/users/{id}/edit",
            get(user_edit_view).post(user_edit_post),
        )
        .route("/users/{id}/toggle-active", post(users_toggle_active))
        .route("/users/{id}/delete", post(users_delete))
        .route("/users/{id}/reset-password", post(users_admin_reset_password))
        // Roles + per-role permissions
        .route("/roles", get(roles_page))
        // Files (generic uploaded files browser)
        .route("/files", get(files_page))
        .route("/files/upload", post(files_upload))
        .route("/files/{id}/delete", post(files_delete))
        // v0.15 (MDM-DEVICE-CONTROL-CONTRACT §2): admin web UI для encrypted
        // distribution. GET — форма target picker, POST — translate в JSON
        // API + редирект назад в /files c flash.
        .route(
            "/files/{id}/distribute",
            get(file_distribute_view).post(file_distribute_form),
        )
        // POST из form (alias action чтоб не путать с GET).
        .route("/files/{id}/distribute-form", post(file_distribute_form))
        // v0.18.12: multi-file distribution в один target (device/group/fleet).
        // Принимает Vec<file_ids> + target, цикл вызывает do_distribute_file
        // для каждого. Идемпотентно — повторное распределение того же файла
        // на тот же target создаст новую encrypted_distribution row, но
        // дедупликация по sha256 + recipient выполняется в do_distribute_file.
        .route("/files/bulk-distribute", post(files_bulk_distribute))
        // v0.15 (MDM-DEVICE-CONTROL-CONTRACT §3): destructive admin commands
        // через web form'ы (alternative к JSON API в routes/devices.rs).
        .route(
            "/devices/{id}/rotate-cloudru-creds-form",
            post(device_rotate_cloudru_creds_form),
        )
        .route(
            "/devices/{id}/revoke-enrollment-form",
            post(device_revoke_enrollment_form),
        )
        .route(
            "/devices/{id}/remote-wipe-form",
            post(device_remote_wipe_form),
        )
        // v0.18.15 (Phase 27): structured update-config form + install-apk push.
        .route(
            "/devices/{id}/config-structured",
            post(device_config_structured_form),
        )
        .route(
            "/devices/{id}/install-apk-form",
            post(device_install_apk_form),
        )
        // Server-wide settings
        .route("/settings", get(settings_page).post(settings_save))
        .route("/settings/language", post(settings_language))
        // Self-profile (email, etc)
        .route("/profile", get(profile_view).post(profile_save))
        // Telemetry — fleet overview, per-device drill-down, per-device log stream
        .route("/telemetry", get(telemetry_overview))
        .route("/devices/{id}/telemetry", get(device_telemetry_view))
        .route("/devices/{id}/logs", get(device_logs_view))
        // Customers (multi-tenant) — super-admin only.
        .route("/customers", get(customers_page).post(customers_create))
        .route("/customers/new", post(customers_create))
        .route(
            "/customers/{id}/edit",
            get(customer_edit_view).post(customer_edit_post),
        )
        .route("/customers/{id}/toggle-active", post(customer_toggle_active))
        .route("/customers/{id}/switch", post(customer_switch))
        // 2FA TOTP — every authenticated user can enrol; login flow uses the
        // separate /login/2fa step.
        .route("/me/2fa", get(me_2fa_view))
        .route("/me/2fa/setup", post(me_2fa_setup))
        .route("/me/2fa/verify", post(me_2fa_verify))
        .route("/me/2fa/cancel", post(me_2fa_cancel))
        .route("/me/2fa/disable", post(me_2fa_disable))
        .route("/login/2fa", get(login_2fa_page).post(login_2fa_submit))
        // Public self-service signup. Gated by a server-wide settings flag
        // (`signup.enabled`). Off by default.
        .route("/signup", get(signup_view).post(signup_submit))
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
    /// The customer this session is scoped to right now. Equal to the
    /// authenticated user's home tenant in the common case; differs when
    /// a super-admin has "switched into" another tenant via the
    /// `outpost_acting` cookie. Every query that filters by tenant should
    /// use this value, not the home_customer_id.
    pub customer_id: i64,
    /// The customer the user actually belongs to. Authoritative for
    /// authorisation; the customer-switch overlay only mutates the active
    /// scope, not membership.
    pub home_customer_id: i64,
    pub role_id: i64,
    pub login: String,
    /// True when the user's role is the super-admin role (id = 1). Computed
    /// once at extract time so handlers can short-circuit cross-tenant
    /// checks without a second DB round-trip.
    pub is_super_admin: bool,
    /// UI locale resolved from the `outpost_lang` cookie / Accept-Language
    /// header. Russian by default (per the Outpost deployment audience).
    pub locale: crate::i18n::Locale,
}

impl WebUser {
    /// Translated strings for this user's current locale.
    pub fn s(&self) -> &'static crate::i18n::Strings {
        self.locale.strings()
    }
}

impl WebUser {
    /// Reject early if the current user is not super-admin.
    ///
    /// The lint says `Result<_, Response>` carries a ~150-byte Err
    /// variant — that's true of every handler in this file already, so
    /// accept the local opt-out.
    #[allow(clippy::result_large_err)]
    pub fn require_super_admin(&self) -> Result<(), Response> {
        if self.is_super_admin {
            Ok(())
        } else {
            Err((StatusCode::FORBIDDEN, "Super-admin required").into_response())
        }
    }
}

/// v0.18.1: paths exempt from the `must_change_password` redirect.
///
/// A user with `must_change_password = 1` is herded to `/me/password`
/// from every other page. These routes are the exceptions:
///
/// - `/me/password` — destination itself; redirecting it would loop.
/// - `/logout` — let them sign out without forcing a change first.
/// - `/static/*` — admin Web UI CSS/JS bundles (см. embedded assets в `app.rs`).
///
/// `/healthz`, `/readyz`, `/login`, and everything under `/api/` aren't
/// in the picture: they don't run through the `WebUser` extractor at all.
fn is_password_change_exempt_path(path: &str) -> bool {
    path.starts_with("/me/password")
        || path == "/logout"
        || path.starts_with("/static/")
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
        let user_row: Option<(i64, i64)> = sqlx::query_as(
            "SELECT is_active, COALESCE(must_change_password, 0) FROM users WHERE id = ?",
        )
        .bind(s.subject_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|_| Redirect::to("/login"))?;
        let Some((active, must_change)) = user_row else {
            return Err(Redirect::to("/login"));
        };
        if active != 1 {
            return Err(Redirect::to("/login"));
        }

        // v0.18.1: enforce password change for users who haven't rotated their
        // bootstrap / admin-reset password yet. The check happens BEFORE the
        // extractor returns Ok(WebUser) so handlers don't see authenticated
        // sessions until they've completed the rotation.
        if must_change != 0 && !is_password_change_exempt_path(parts.uri.path()) {
            return Err(Redirect::to("/me/password"));
        }

        let is_super_admin: bool = sqlx::query_scalar::<_, i64>(
            "SELECT is_super_admin FROM user_roles WHERE id = ?",
        )
        .bind(s.role_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|_| Redirect::to("/login"))?
        .map(|n| n != 0)
        .unwrap_or(false);

        // Customer-switch overlay: super-admin only. The cookie value is the
        // numeric customer_id they want to act as. Any other user with the
        // cookie set is ignored (cookie is harmless — they can't escalate).
        let mut active_customer_id = s.customer_id;
        if is_super_admin {
            if let Some(acting) = read_cookie(parts, "outpost_acting")
                .and_then(|v| v.parse::<i64>().ok())
            {
                let exists: Option<i64> = sqlx::query_scalar(
                    "SELECT 1 FROM customers WHERE id = ? AND is_active = 1",
                )
                .bind(acting)
                .fetch_optional(&state.db)
                .await
                .ok()
                .flatten();
                if exists.is_some() {
                    active_customer_id = acting;
                }
            }
        }

        let locale = crate::i18n::from_request(parts);
        Ok(WebUser {
            id: s.subject_id,
            customer_id: active_customer_id,
            home_customer_id: s.customer_id,
            role_id: s.role_id,
            login: s.login,
            is_super_admin,
            locale,
        })
    }
}

fn read_cookie(parts: &Parts, name: &str) -> Option<String> {
    let hdr = parts.headers.get(header::COOKIE)?.to_str().ok()?;
    for kv in hdr.split(';') {
        let kv = kv.trim();
        if let Some(v) = kv.strip_prefix(&format!("{name}=")) {
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
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
            error: Some("Слишком много попыток входа. Попробуйте через минуту.".into()),
        });
    }
    #[derive(sqlx::FromRow)]
    struct LoginRow {
        id: i64,
        customer_id: i64,
        role_id: i64,
        password_hash: Option<String>,
        is_active: i64,
        totp_enabled: i64,
    }
    let row: Option<LoginRow> = match sqlx::query_as::<_, LoginRow>(
        "SELECT id, customer_id, role_id, password_hash, is_active, totp_enabled \
         FROM users WHERE login = ?",
    )
    .bind(&form.login)
    .fetch_optional(&state.db)
    .await
    {
        Ok(row) => row,
        Err(e) => {
            tracing::error!(error = %e, "login DB error");
            return render(LoginTemplate {
                error: Some("Внутренняя ошибка. Попробуйте снова.".into()),
            });
        }
    };
    let Some(LoginRow {
        id,
        customer_id,
        role_id,
        password_hash,
        is_active,
        totp_enabled,
    }) = row
    else {
        return render(LoginTemplate {
            error: Some("Неверный логин или пароль.".into()),
        });
    };
    if is_active == 0 {
        return render(LoginTemplate {
            error: Some("Учётная запись отключена.".into()),
        });
    }
    let Some(phc) = password_hash else {
        return render(LoginTemplate {
            error: Some("Пароль не задан — обратитесь к администратору.".into()),
        });
    };
    if !crypto::verify_password(&form.password, &phc).unwrap_or(false) {
        return render(LoginTemplate {
            error: Some("Неверный логин или пароль.".into()),
        });
    }

    // 2FA gate: if the user has TOTP enabled, issue a short-lived
    // pending-2FA session and bounce them to /login/2fa for the second
    // factor. The pending session token rides in the cookie just like a
    // normal session, but its `kind = pending_2fa` keeps every protected
    // route inaccessible until upgraded.
    if totp_enabled != 0 {
        let pending = match session::create_pending_2fa_session(
            &state.db,
            id,
            customer_id,
            role_id,
            &form.login,
        )
        .await
        {
            Ok(t) => t,
            Err(_) => {
                return render(LoginTemplate {
                    error: Some("Не удалось создать сессию.".into()),
                });
            }
        };
        let mut resp = Redirect::to("/login/2fa").into_response();
        // Pending cookie has a 5-minute Max-Age — matches the session TTL.
        set_pending_2fa_cookie(&mut resp, &pending, state.secure_cookies);
        return resp;
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
                error: Some("Не удалось создать сессию.".into()),
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

fn set_pending_2fa_cookie(resp: &mut Response, token: &str, secure: bool) {
    let cookie = format!(
        "outpost_pending_2fa={token}; Path=/; HttpOnly; SameSite=Lax{}; Max-Age=300",
        if secure { "; Secure" } else { "" },
    );
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().append(header::SET_COOKIE, v);
    }
}

fn clear_pending_2fa_cookie(resp: &mut Response) {
    let cookie = "outpost_pending_2fa=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0";
    if let Ok(v) = HeaderValue::from_str(cookie) {
        resp.headers_mut().append(header::SET_COOKIE, v);
    }
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
    /// v0.18.6: members groups, comma-rendered into UI as Tailwind badges.
    /// Empty Vec → ячейка показывает «—».
    groups: Vec<String>,
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
    /// SQLite GROUP_CONCAT of group names (LEFT JOIN device_groups +
    /// groups). NULL if device is in no groups.
    group_names: Option<String>,
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
    // v0.18.6: LEFT JOIN device_groups + groups, агрегируем имена через
    // GROUP_CONCAT. Сортировка вторичным ключом по g.name даёт стабильный
    // порядок tags для одного и того же device.
    let rows: Vec<DeviceRowRaw> = sqlx::query_as::<_, DeviceRowRaw>(
        "SELECT d.id, d.serial, d.display_name, d.is_enrolled, d.is_online, \
                d.battery_pct, d.app_version, d.last_seen_at, \
                (SELECT GROUP_CONCAT(g.name, '\u{1f}') FROM device_groups dg \
                 JOIN groups g ON g.id = dg.group_id \
                 WHERE dg.device_id = d.id ORDER BY g.name) AS group_names \
         FROM devices d WHERE d.customer_id = ? ORDER BY d.id DESC LIMIT 200",
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
            // U+001F (Unit Separator) — гарантированно не встречается в
            // именах групп (validated при создании), безопасный delimiter
            // для GROUP_CONCAT vs запятая (которая может быть в названии).
            groups: r
                .group_names
                .map(|s| s.split('\u{1f}').map(|x| x.to_string()).collect())
                .unwrap_or_default(),
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
        return render_devices(&user, &state, None, Some("Серийный номер обязателен".into()))
            .await
            .map_err(|e| e.into_response());
    }
    let display_name = req.display_name.as_deref().map(|s| s.trim()).filter(|s| !s.is_empty());
    // v0.18.8: новые устройства автоматически получают customer-default
    // configuration. NULL — допустимо (customer без default настроен), тогда
    // device создаётся с configuration_id = NULL (поведение pre-v0.18.8).
    // Admin может поменять любой device-config через /devices/{id}/edit.
    let default_config_id: Option<i64> = sqlx::query_scalar(
        "SELECT default_configuration_id FROM customers WHERE id = ?",
    )
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .flatten();
    let res = sqlx::query(
        "INSERT INTO devices (customer_id, serial, display_name, configuration_id) VALUES (?, ?, ?, ?)",
    )
    .bind(user.customer_id)
    .bind(serial)
    .bind(display_name)
    .bind(default_config_id)
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
    id: i64,
    name: String,
    description: String,
    member_count: i64,
    created_at: String,
    /// v0.18.6: устройства в этой группе. Используется в expandable
    /// <details> на /groups для показа состава и удаления membership'а.
    members: Vec<GroupMemberRow>,
    /// v0.18.6: устройства этого customer'а НЕ в этой группе — для
    /// dropdown'а «добавить устройство». Лимит 200 (как `devices_page`),
    /// если у customer'а будет >200 устройств — этот UI перейдёт на
    /// search-based вариант, но не на текущей шкале.
    eligible_devices: Vec<GroupMemberRow>,
}

#[derive(Clone)]
struct GroupMemberRow {
    id: i64,
    serial: String,
    display_name: String,
}

#[derive(sqlx::FromRow)]
struct GroupRowRaw {
    id: i64,
    name: String,
    description: Option<String>,
    member_count: i64,
    created_at: String,
}

#[derive(sqlx::FromRow)]
struct GroupMemberRaw {
    id: i64,
    serial: String,
    display_name: Option<String>,
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
        "SELECT g.id, g.name, g.description, \
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
    // v0.18.6: fetch full device list once и распределить per-group.
    // N+1 queries (по группе на каждый member-fetch) — расточительно,
    // одним запросом всех customer-devices + одним запросом всего
    // membership'а получаем O(M+G) вместо O(G·M).
    let all_devices: Vec<GroupMemberRaw> = sqlx::query_as::<_, GroupMemberRaw>(
        "SELECT id, serial, display_name FROM devices WHERE customer_id = ? ORDER BY serial LIMIT 200",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;

    #[derive(sqlx::FromRow)]
    struct MembershipRow {
        group_id: i64,
        device_id: i64,
    }
    let memberships: Vec<MembershipRow> = sqlx::query_as::<_, MembershipRow>(
        "SELECT dg.group_id, dg.device_id FROM device_groups dg \
         JOIN groups g ON g.id = dg.group_id \
         WHERE g.customer_id = ?",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;

    let groups = rows
        .into_iter()
        .map(|r| {
            let member_ids: std::collections::HashSet<i64> = memberships
                .iter()
                .filter(|m| m.group_id == r.id)
                .map(|m| m.device_id)
                .collect();
            let (members, eligible_devices): (Vec<_>, Vec<_>) = all_devices
                .iter()
                .map(|d| GroupMemberRow {
                    id: d.id,
                    serial: d.serial.clone(),
                    display_name: d.display_name.clone().unwrap_or_else(|| "—".into()),
                })
                .partition(|d| member_ids.contains(&d.id));
            GroupRow {
                id: r.id,
                name: r.name,
                description: r.description.unwrap_or_else(|| "—".into()),
                member_count: r.member_count,
                created_at: state.fmt_ts(&r.created_at),
                members,
                eligible_devices,
            }
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
        return render_groups(&user, &state, None, Some("Название обязательно".into()))
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
    id: i64,
    package_name: String,
    display_name: String,
    kind: String,
    version_count: i64,
    latest_version: String,
}

#[derive(sqlx::FromRow)]
struct AppRowRaw {
    id: i64,
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
        "SELECT a.id, a.package_name, a.display_name, a.kind, \
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
            id: r.id,
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
    id: i64,
    name: String,
    description: String,
    kiosk_package: String,
    is_active: bool,
    /// v0.18.8: эта конфигурация — `customers.default_configuration_id`.
    /// При создании новых устройств получают её настройки автоматически.
    is_default: bool,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct ConfigRowRaw {
    id: i64,
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
        "SELECT id, name, description, kiosk_package, is_active, updated_at \
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
    // v0.18.8: какой config назначен customer-default. NULL — не назначен,
    // тогда ни одна из строк не получает is_default=true.
    let default_config_id: Option<i64> = sqlx::query_scalar(
        "SELECT default_configuration_id FROM customers WHERE id = ?",
    )
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .flatten();
    let configs = rows
        .into_iter()
        .map(|r| ConfigRow {
            id: r.id,
            name: r.name,
            description: r.description.unwrap_or_else(|| "—".into()),
            kiosk_package: r.kiosk_package.unwrap_or_else(|| "—".into()),
            is_active: r.is_active,
            is_default: Some(r.id) == default_config_id,
            updated_at: state.fmt_ts(&r.updated_at),
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
        return render_configs(&user, &state, None, Some("Название обязательно".into()))
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
    /// v0.18.16: список APK-версий для install-apk dropdown'а.
    /// Те же кандидаты что и на /devices/{id}/edit но с дополнительным
    /// payload (sha256/size/url) — JS соберёт JSON.
    apk_versions: Vec<PushApkVersionOption>,
    /// v0.18.16: те же model registry опции что и в DeviceEditTemplate —
    /// для structured update-config panel.
    llm_options: Vec<ConfigOptionLabel>,
    translator_llm_options: Vec<ConfigOptionLabel>,
    vlm_options: Vec<ConfigOptionLabel>,
    stt_options: Vec<ConfigOptionLabel>,
    tts_mode_options: Vec<ConfigOptionLabel>,
    answer_mode_options: Vec<ConfigOptionLabel>,
    translator_mode_options: Vec<ConfigOptionLabel>,
    translator_audio_mode_options: Vec<ConfigOptionLabel>,
    log_level_options: Vec<ConfigOptionLabel>,
    cpu_thread_count_options: Vec<ConfigOptionLabel>,
    flash: Option<String>,
    create_error: Option<String>,
}

#[derive(sqlx::FromRow)]
struct PushApkVersionOption {
    id: i64,
    label: String,
    version_code: i64,
    version_name: String,
    sha256: String,
    file_size_bytes: i64,
    source_url: Option<String>,
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
    let apk_versions: Vec<PushApkVersionOption> = sqlx::query_as(
        "SELECT av.id, \
                av.version_name || ' (code ' || av.version_code || ', sha ' || \
                substr(av.sha256, 1, 8) || '…)' AS label, \
                av.version_code, av.version_name, av.sha256, av.file_size_bytes, av.source_url \
         FROM application_versions av \
         JOIN applications a ON a.id = av.application_id \
         WHERE a.customer_id = ? AND a.kind = 'apk' \
         ORDER BY av.version_code DESC LIMIT 200",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    let messages = rows
        .into_iter()
        .map(|r| PushRow {
            created_at: state.fmt_ts(&r.created_at),
            device_serial: r.device_serial,
            command: r.command,
            status: r.status,
            delivered_at: r.delivered_at.as_deref().map(|s| state.fmt_ts(s)).unwrap_or_else(|| "—".into()),
        })
        .collect();
    let mut resp = render(PushTemplate {
        user_login: user.login.clone(),
        pending,
        sent_24h,
        messages,
        target_devices,
        target_groups,
        apk_versions,
        llm_options: llm_options(),
        translator_llm_options: translator_llm_options(),
        vlm_options: vlm_options(),
        stt_options: stt_options(),
        tts_mode_options: tts_mode_options(),
        answer_mode_options: answer_mode_options(),
        translator_mode_options: translator_mode_options(),
        translator_audio_mode_options: translator_audio_mode_options(),
        log_level_options: log_level_options(),
        cpu_thread_count_options: cpu_thread_count_options(),
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
            last_login_at: r.last_login_at.as_deref().map(|s| state.fmt_ts(s)).unwrap_or_else(|| "—".into()),
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
        return render_users(&user, &state, None, Some("Логин обязателен".into()))
            .await
            .map_err(|e| e.into_response());
    }
    if req.password.len() < 8 {
        return render_users(
            &user,
            &state,
            None,
            Some("Пароль должен быть не короче 8 символов".into()),
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

// ----- /users/{id}/edit (admin edits another user's profile) -------------

#[derive(Template)]
#[template(path = "user_edit.html")]
struct UserEditTemplate {
    user_login: String,
    /// id редактируемого user'а (для form action url).
    target_id: i64,
    target_login: String,
    email: String,
    display_name: String,
    comment: String,
    phone: String,
    tg: String,
    role_name: String,
    last_login_at: String,
    created_at: String,
    is_self: bool,
    flash: Option<String>,
    error: Option<String>,
}

async fn user_edit_view(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    flash: FlashCookie,
) -> Result<Response, ApiError> {
    render_user_edit(&user, &state, id, flash.0, None).await
}

async fn render_user_edit(
    user: &WebUser,
    state: &AppState,
    id: i64,
    flash: Option<String>,
    error: Option<String>,
) -> Result<Response, ApiError> {
    #[derive(sqlx::FromRow)]
    struct UserEditRaw {
        login: String,
        email: Option<String>,
        display_name: Option<String>,
        comment: Option<String>,
        phone: Option<String>,
        tg: Option<String>,
        role_name: String,
        last_login_at: Option<String>,
        created_at: String,
    }
    let row: Option<UserEditRaw> = sqlx::query_as::<_, UserEditRaw>(
        "SELECT u.login, u.email, u.display_name, u.comment, u.phone, u.tg, \
                r.name AS role_name, u.last_login_at, u.created_at \
         FROM users u JOIN user_roles r ON r.id = u.role_id \
         WHERE u.id = ? AND u.customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(r) = row else {
        return Err(ApiError::NotFound);
    };
    let mut resp = render(UserEditTemplate {
        user_login: user.login.clone(),
        target_id: id,
        target_login: r.login,
        email: r.email.unwrap_or_default(),
        display_name: r.display_name.unwrap_or_default(),
        comment: r.comment.unwrap_or_default(),
        phone: r.phone.unwrap_or_default(),
        tg: r.tg.unwrap_or_default(),
        role_name: r.role_name,
        last_login_at: r
            .last_login_at
            .as_deref()
            .map(|s| state.fmt_ts(s))
            .unwrap_or_else(|| "—".into()),
        created_at: state.fmt_ts(&r.created_at),
        is_self: id == user.id,
        flash,
        error,
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

async fn user_edit_post(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(req): Form<ProfileForm>,
) -> Result<Response, ApiError> {
    // Verify target user в том же customer'е (multi-tenant scoping).
    let exists: Option<(i64,)> = sqlx::query_as(
        "SELECT id FROM users WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?;
    if exists.is_none() {
        return Err(ApiError::NotFound);
    }
    let email = normalize_profile_field(&req.email, false);
    let display_name = normalize_profile_field(&req.display_name, false);
    let comment = normalize_profile_field(&req.comment, false);
    let phone = normalize_profile_field(&req.phone, false);
    let tg = normalize_profile_field(&req.tg, true);
    sqlx::query(
        "UPDATE users SET email = ?, display_name = ?, comment = ?, phone = ?, tg = ?, \
                          updated_at = datetime('now') WHERE id = ? AND customer_id = ?",
    )
    .bind(email)
    .bind(display_name)
    .bind(comment)
    .bind(phone)
    .bind(tg)
    .bind(id)
    .bind(user.customer_id)
    .execute(&state.db)
    .await?;
    Ok(redirect_with_flash(
        &format!("/users/{id}/edit"),
        "Профиль пользователя сохранён.",
    ))
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

// v0.18.9: fmt_ts перенесена в `AppState::fmt_ts` (state.rs) — теперь она
// конвертирует UTC из БД в server timezone (default Europe/Moscow,
// настраивается через /settings → server.timezone). Все callsites в этом
// файле переписаны на `state.fmt_ts(&...)`.

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
    /// APK download QR — SVG presigned-URL на Cloud.ru. `None` если
    /// `cloudru_signer` не сконфигурирован (CLOUDRU_* env'ы не заданы).
    /// Template условно рендерит блок «Шаг 1 — установить приложение».
    apk_qr_svg: Option<String>,
    /// Сама ссылка под QR — текстом, для копирования вручную или для
    /// admin'а который хочет переслать в Telegram. None ↔ apk_qr_svg None.
    apk_download_url: Option<String>,
    /// Срок действия presigned URL — UI показывает «до DD.MM.YYYY HH:MM UTC»
    /// чтобы admin понимал когда нужно перегенерировать страницу.
    apk_url_expires_human: Option<String>,
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

/// `GET /devices/{id}/enroll/file` — download enrollment payload as
/// `enrollment.json`. Same JSON object as the QR-encoded payload; intended for
/// offline flash-drive bootstrap (oператор кладёт файл на флешку → переносит
/// на /sdcard/Outpost/enrollment.json → app at start zachisляется без QR).
///
/// 404 if there's no active secret — admin must regenerate first via POST.
async fn device_enroll_download(
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
    let Some(secret) = secret else {
        return Err(ApiError::NotFound);
    };
    let server_url: String = sqlx::query_scalar(
        "SELECT json_extract(value_json, '$') FROM settings WHERE key = 'server.enrollment_base_url'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .flatten()
    .unwrap_or_else(|| "https://mdm.secondf8n.tech".to_string());
    let payload = enrollment_payload_json(&server_url, user.customer_id, id, &secret);
    let body = serde_json::to_string_pretty(&payload).unwrap_or_default();
    // Make the filename device-distinct so an admin downloading multiple
    // payloads doesn't get five copies of `enrollment.json` overwriting one another.
    let fname = format!("outpost-enrollment-{}.json", sanitize_filename(&serial));
    let mut resp = (
        axum::http::StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, "application/json"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                &format!("attachment; filename=\"{fname}\""),
            ),
        ],
        body,
    )
        .into_response();
    resp.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok(resp)
}

/// Strip path-traversal-sensitive characters from a serial before using it
/// in the Content-Disposition filename. Keeps alphanum + dash + underscore.
fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
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
        let payload = enrollment_payload_json(&server_url, user.customer_id, device_id, s);
        let payload_text = serde_json::to_string_pretty(&payload).unwrap_or_default();
        // v0.10: QR encodes `outpost-mdm://v1/<base64url(json)>` so the Outpost-Android
        // client can branch by URI-scheme (legacy RBAC scheme is `outpost-enroll://v1/...`,
        // we never want them confused). Plain JSON остаётся в payload_text для
        // manual-paste fallback на случай если камера капризничает.
        let svg = qrcode_svg(&encode_enrollment_uri(&payload));
        (payload_text, svg)
    } else {
        (String::new(), String::new())
    };

    // v0.16 §A — APK download QR. Если Cloud.ru presigner сконфигурирован
    // через CLOUDRU_* env'ы, генерируем presigned URL на latest APK pointer
    // (`apks/latest/app-debug.apk` by default) на 7 дней и кодируем в QR.
    // Юзер сканирует, открывает в браузере, скачивает, ставит — всё без
    // ручного копи-паста ссылок из Telegram.
    let (apk_qr_svg, apk_download_url, apk_url_expires_human) =
        if let Some(signer) = state.cloudru_signer.as_ref() {
            const TTL_SECS: u64 = crate::cloudru_signer::SIGV4_MAX_EXPIRES_SECS; // 7 дней
            let now = chrono::Utc::now();
            let url = signer.presigned_get_url_at(&state.cloudru_apk_key, TTL_SECS, now);
            let svg = qrcode_svg(&url);
            let expires_at = now + chrono::Duration::seconds(TTL_SECS as i64);
            let expires_str = expires_at.format("%d.%m.%Y %H:%M UTC").to_string();
            (Some(svg), Some(url), Some(expires_str))
        } else {
            (None, None, None)
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
        apk_qr_svg,
        apk_download_url,
        apk_url_expires_human,
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

/// Build the canonical enrollment payload JSON object. Schema is shared by:
///   - QR-code (wrapped in `outpost-mdm://v1/<base64url>` — see [`encode_enrollment_uri`])
///   - Manual-paste fallback (plain JSON, pretty-printed in the UI)
///   - `enrollment.json` download (offline flash-drive bootstrap)
///
/// Outpost-Android client parses this via `MdmEnrollmentTicket.parse` and
/// POSTs `device_id` + `enrollment_secret` to `/api/v1/enroll` to receive
/// the long-lived `device_token`.
fn enrollment_payload_json(
    server_url: &str,
    customer_id: i64,
    device_id: i64,
    secret: &str,
) -> serde_json::Value {
    serde_json::json!({
        "server_url": server_url,
        "customer_id": customer_id,
        "device_id": device_id,
        "enrollment_secret": secret,
    })
}

/// Encode the enrollment payload as `outpost-mdm://v1/<base64url(JSON)>`.
///
/// The URI-scheme prefix is what lets the client distinguish an MDM ticket
/// from the legacy RBAC `outpost-enroll://v1/...` scheme — they share the
/// QR-scanner UI but route to different enrollment HTTP endpoints.
fn encode_enrollment_uri(payload: &serde_json::Value) -> String {
    use base64::Engine;
    let json = payload.to_string(); // compact, single-line
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.as_bytes());
    format!("outpost-mdm://v1/{b64}")
}

// v0.18.15: DevicePushTemplate удалён вместе с device_push.html — страница
// /devices/{id}/push теперь 303-redirect'ит на /devices/{id}/edit.

/// v0.18.15 (Phase 27): страница /devices/{id}/push устарела — функционал
/// полностью переехал на /devices/{id}/edit «Настроить устройство»
/// (structured update-config + install-apk + sensitive admin commands).
/// GET сюда — 303 redirect на /edit. POST оставлен в `device_push_post`
/// для backward compat (curl-скрипты и закладки).
async fn device_push_view(
    _user: WebUser,
    State(_state): State<AppState>,
    Path(id): Path<i64>,
) -> Response {
    axum::response::Redirect::to(&format!("/devices/{id}/edit")).into_response()
}

#[derive(Debug, Deserialize)]
struct DevicePushForm {
    command: String,
    payload_json: Option<String>,
    due_at: Option<String>,
}

/// Backward-compat для curl-скриптов / закладок, бьющих POST в
/// /devices/{id}/push. GET той же ручки теперь редиректит на /edit
/// (см. device_push_view). После успешного scheduling редиректит туда же.
async fn device_push_post(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(req): Form<DevicePushForm>,
) -> Result<Response, ApiError> {
    let serial_exists: Option<(i64,)> =
        sqlx::query_as("SELECT id FROM devices WHERE id = ? AND customer_id = ?")
            .bind(id)
            .bind(user.customer_id)
            .fetch_optional(&state.db)
            .await?;
    if serial_exists.is_none() {
        return Err(ApiError::NotFound);
    }
    let command = req.command.trim();
    if command.is_empty() {
        return Ok(redirect_with_flash(
            &format!("/devices/{id}/edit"),
            "Command is required.",
        ));
    }
    let payload = req
        .payload_json
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("{}");
    if let Err(e) = serde_json::from_str::<serde_json::Value>(payload) {
        return Ok(redirect_with_flash(
            &format!("/devices/{id}/edit"),
            &format!("payload_json is not valid JSON: {e}"),
        ));
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
        &format!("/devices/{id}/edit"),
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
        return Ok(render_err("Новый пароль должен быть не короче 8 символов".into()).await);
    }
    if req.new_password != req.confirm_password {
        return Ok(render_err("Новый пароль и подтверждение не совпадают".into()).await);
    }
    // Verify current
    let stored_hash: Option<String> =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?")
            .bind(user.id)
            .fetch_optional(&state.db)
            .await?
            .flatten();
    let Some(hash) = stored_hash else {
        return Ok(render_err("Не удалось проверить текущий пароль (нет хэша в БД)".into()).await);
    };
    if !crypto::verify_password(&req.current_password, &hash).unwrap_or(false) {
        return Ok(render_err("Текущий пароль введён неверно".into()).await);
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

// =====================================================================
// Phase 21 — Edit/delete + new resource pages (files, roles, settings, profile)
// =====================================================================

// ----- Device edit / delete ------------------------------------------------

/// Один вариант в dropdown'е «Настроить устройство → быстрая настройка».
/// Используется для LLM / VLM / STT / TTS-voice выпадушек, а также для enum'ов
/// типа `tts_mode` / `answer_mode` / `log_level`.
///
/// `value` идёт в JSON payload (filename `.gguf` для моделей; имя варианта
/// enum'а для перечислений — должно ТОЧНО соответствовать тому что
/// `ModelPreferences.setXxx` принимает на клиенте, см.
/// `MDM-DEVICE-CONTROL-CONTRACT.md §1.3`).
///
/// `label` — человекочитаемая подпись для admin'а.
///
/// `description` — короткий hint про сценарий применения. Пустой если нечего
/// сказать сверх label'а.
#[derive(Clone)]
pub struct ConfigOptionLabel {
    pub value: String,
    pub label: String,
    pub description: String,
}

impl ConfigOptionLabel {
    fn new(value: &str, label: &str, description: &str) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            description: description.into(),
        }
    }
}

/// Известные модели для preferred_llm / preferred_translator_llm / preferred_vlm /
/// preferred_stt. Hardcoded потому что (1) набор меняется раз в несколько
/// недель, (2) добавление новой модели требует ещё и upload на mirror +
/// bootstrap-manifest update — лишний редеплой server'а не блокер.
///
/// **Source of truth**: AR Hud `ModelRegistry.kt` в
/// `tactical-ar-hud/prototypes/outpost-android/app/src/main/java/ru/tacticalar/outpost/ml/ModelRegistry.kt`.
/// Filename'ы должны точно совпадать (client matching на `endsWith(filename)`
/// в installLocal). Размеры тоже оттуда — реальные байты в R2 / Cloud.ru
/// бакетах.
///
/// **Tier guidance** (из ModelRegistry comments):
/// - T0 = 4 ГБ RAM (Realme Note 60X и аналоги) — только лёгкие модели
/// - T1 = 6–8 ГБ RAM — основной парк, до 7B Q4
/// - T2 = 8–12 ГБ RAM (Ulefone, Honor 400 Pro) — комфортно 9B Q4 + Whisper
///
/// Vosk **не включён** в STT dropdown — это wake-word engine («штаб, приём»),
/// отдельный канал от STT main pipeline.
fn llm_options() -> Vec<ConfigOptionLabel> {
    vec![
        ConfigOptionLabel::new(
            "qwen3-4b-soldier-v25-Q4_K_M.gguf",
            "Soldier V25 4B Q4_K_M — рекомендуется (T1+, 2.5 ГБ)",
            "Fine-tune Qwen3-4B на полевой корпус (raskat'нут 2026-05-18). \
             Полевой помощник РФ — НЕ для перевода / поэзии.",
        ),
        ConfigOptionLabel::new(
            "qwen3-4b-soldier-v24-q4_k_m.gguf",
            "Soldier V24 4B Q4_K_M — legacy (T1+, 2.5 ГБ)",
            "Предыдущий Soldier fine-tune. Оставлен для отката если V25 \
             покажет регрессию в полях.",
        ),
        ConfigOptionLabel::new(
            "qwen2.5-1.5b-instruct-Q4_K_M.gguf",
            "Qwen2.5 1.5B Instruct Q4_K_M — облегчённая (T0+, 1.1 ГБ)",
            "Для устройств 4 ГБ RAM (Realme Note 60X и аналоги). \
             Bundled в \"Минимум\".",
        ),
        ConfigOptionLabel::new(
            "qwen2.5-3b-instruct-q4_k_m.gguf",
            "Qwen2.5 3B Instruct Q4_K_M — generic (T1+, 1.9 ГБ)",
            "Базовый instruct, без полевого fine-tune. General-purpose.",
        ),
        ConfigOptionLabel::new(
            "qwen2.5-3b-instruct-Q5_K_M.gguf",
            "Qwen2.5 3B Instruct Q5_K_M — generic выше качества (T1+, 2.4 ГБ)",
            "Q5 квантизация 3B. Требуется 8+ ГБ RAM. Заметно качественнее Q4.",
        ),
        ConfigOptionLabel::new(
            "qwen2.5-7b-instruct-q4_k_m.gguf",
            "Qwen2.5 7B Instruct Q4_K_M — флагман (T1+, 4.7 ГБ)",
            "Multilingual flagship. Лучшее RU/EN/UK/ZH среди ~5 ГБ моделей. \
             Ловит поэтический регистр. ~5 ГБ resident с KV-cache.",
        ),
        ConfigOptionLabel::new(
            "qwen3.5-9b-q4_k_m.gguf",
            "Qwen3.5 9B Q4_K_M — новейший (T2+, 5.9 ГБ)",
            "Q3.5 series Feb 2026. Лучший перевод в open-source ≤6 ГБ. \
             Vision-language pipeline, но text-only inference работает идеально.",
        ),
    ]
}

fn translator_llm_options() -> Vec<ConfigOptionLabel> {
    vec![
        ConfigOptionLabel::new(
            "HY-MT1.5-1.8B-Q4_K_M.gguf",
            "Hunyuan MT 1.5 1.8B Q4_K_M — translation-specific (T0+, 1.1 ГБ)",
            "Оптимизирован специально под перевод. Идёт первым кандидатом \
             если устройство translation-heavy.",
        ),
        ConfigOptionLabel::new(
            "qwen2.5-3b-instruct-q4_k_m.gguf",
            "Qwen2.5 3B Instruct Q4_K_M — рекомендуется (T1+, 1.9 ГБ)",
            "Сбалансированный generic для on-device перевода.",
        ),
        ConfigOptionLabel::new(
            "qwen2.5-1.5b-instruct-Q4_K_M.gguf",
            "Qwen2.5 1.5B Instruct Q4_K_M — для T0 (1.1 ГБ)",
            "Облегчённый вариант если устройство уже грузит другой LLM.",
        ),
        ConfigOptionLabel::new(
            "qwen2.5-7b-instruct-q4_k_m.gguf",
            "Qwen2.5 7B Instruct Q4_K_M — флагман перевода (T1+, 4.7 ГБ)",
            "Топ-качество RU/EN/UK/ZH/JA среди ~5 ГБ open-source.",
        ),
        ConfigOptionLabel::new(
            "qwen3.5-9b-q4_k_m.gguf",
            "Qwen3.5 9B Q4_K_M — новейший (T2+, 5.9 ГБ)",
            "Лучший перевод в open-source ≤6 ГБ. Feb 2026.",
        ),
    ]
}

fn vlm_options() -> Vec<ConfigOptionLabel> {
    vec![
        ConfigOptionLabel::new(
            "qwen2-vl-2b-instruct-q4_k_m.gguf",
            "Qwen2-VL 2B Instruct Q4_K_M — multilingual (T1+, 986 МБ + 1.3 ГБ mmproj)",
            "Cyrillic-friendly VLM. OCRBench 794. Bundled в \"Рекомендуемый\". \
             Требует парный mmproj.",
        ),
        ConfigOptionLabel::new(
            "qwen3vl-8b-instruct-q4_k_m.gguf",
            "Qwen3-VL 8B Instruct Q4_K_M — флагман для камеры (T2+, 5 ГБ + 1.2 ГБ mmproj)",
            "Vision tower поколением выше Qwen2-VL. +20-30% точности на \
             грибах/растениях/животных/технике/лекарствах. ~7-8 ГБ resident.",
        ),
        ConfigOptionLabel::new(
            "moondream2-text-model-f16.gguf",
            "Moondream2 F16 — English-only (T1+, 2.8 ГБ + 0.9 ГБ mmproj)",
            "Phi-1.5 backbone. OCR 61.2, English-only по карточке HF. Альтернатива \
             если Qwen2-VL не подходит.",
        ),
        ConfigOptionLabel::new(
            "smolvlm-500m-decoder-q4.onnx",
            "SmolVLM 500M decoder Q4 — облегчённый ONNX (T0+, 229 МБ)",
            "ONNX runtime (не llama.cpp). English-only. Самый маленький VLM. \
             Для устройств где Qwen2-VL не помещается.",
        ),
    ]
}

fn stt_options() -> Vec<ConfigOptionLabel> {
    vec![
        ConfigOptionLabel::new(
            "ggml-tiny-q5_1.bin",
            "Whisper tiny Q5_1 — T0 (32 МБ)",
            "Работает на самых слабых устройствах. Хуже по точности на длинных \
             репликах. Bundled в \"Минимум\".",
        ),
        ConfigOptionLabel::new(
            "ggml-base-q5_1.bin",
            "Whisper base Q5_1 — рекомендуется (60 МБ, T1+)",
            "Baseline качество. Default в production-конфиге.",
        ),
        ConfigOptionLabel::new(
            "ggml-small-q5_1.bin",
            "Whisper small Q5_1 — выше точность (190 МБ, T1+)",
            "Заметно лучше base на нюансах артикуляции. Медленнее.",
        ),
        ConfigOptionLabel::new(
            "ggml-large-v3-turbo-q5_0.bin",
            "Whisper large-v3-turbo Q5_0 — флагман (574 МБ, T2+)",
            "Newer arch, 3-5× быстрее large-v3 при сравнимом качестве. \
             Только для мощных устройств.",
        ),
    ]
}

fn tts_mode_options() -> Vec<ConfigOptionLabel> {
    vec![
        ConfigOptionLabel::new(
            "Off",
            "Off — звук всегда выключен",
            "Тишина. Текст-only сценарии.",
        ),
        ConfigOptionLabel::new(
            "WakeWordOnly",
            "WakeWordOnly — озвучка только после wake-word",
            "Default. Минимум звукового шума в окопе.",
        ),
        ConfigOptionLabel::new(
            "Always",
            "Always — озвучивать все ответы",
            "Тренировочные / show-and-tell сценарии.",
        ),
    ]
}

fn answer_mode_options() -> Vec<ConfigOptionLabel> {
    vec![
        ConfigOptionLabel::new(
            "Auto",
            "Auto — client сам решает (default)",
            "Search / FastAssistant / FullAssistant выбирается по сложности запроса.",
        ),
        ConfigOptionLabel::new(
            "Search",
            "Search — только RAG-поиск",
            "Чистый retrieval, без LLM-генерации. Самый быстрый.",
        ),
        ConfigOptionLabel::new(
            "Assistant",
            "Assistant — всегда LLM-ответ",
            "Каждый запрос идёт в LLM. Медленно, но качественнее.",
        ),
    ]
}

fn translator_mode_options() -> Vec<ConfigOptionLabel> {
    vec![
        ConfigOptionLabel::new(
            "Local",
            "Local — только on-device LLM (default)",
            "Offline-first. Privacy-friendly.",
        ),
        ConfigOptionLabel::new(
            "Auto",
            "Auto — local + cloud fallback",
            "Если cloud_enabled и local не справился — пробует cloud.",
        ),
        ConfigOptionLabel::new(
            "Cloud",
            "Cloud — всегда облако",
            "Требует translator_cloud_enabled=true. Быстрее и качественнее, но нужна сеть.",
        ),
    ]
}

fn translator_audio_mode_options() -> Vec<ConfigOptionLabel> {
    vec![
        ConfigOptionLabel::new(
            "SpeakerphoneBoth",
            "SpeakerphoneBoth — оба слышат через динамик (default)",
            "Двое лицом к лицу, телефон между ними.",
        ),
        ConfigOptionLabel::new(
            "HeadsetSplit",
            "HeadsetSplit — RU→FOR в динамик, FOR→RU в гарнитуру",
            "Хозяин телефона в гарнитуре, иностранец слышит свой перевод вслух.",
        ),
    ]
}

fn log_level_options() -> Vec<ConfigOptionLabel> {
    vec![
        ConfigOptionLabel::new(
            "OFF",
            "OFF — без логов",
            "Production-mode (max privacy, нет тел. данных в OTLP).",
        ),
        ConfigOptionLabel::new(
            "BASIC",
            "BASIC — только метрики и события",
            "Counts, timings, errors. Без bodies prompt'ов/ответов.",
        ),
        ConfigOptionLabel::new(
            "VERBOSE",
            "VERBOSE — всё включая bodies (beta)",
            "Полные тексты chat/translator/VLM в OTLP. Beta-mode.",
        ),
    ]
}

fn cpu_thread_count_options() -> Vec<ConfigOptionLabel> {
    let mut out = vec![ConfigOptionLabel::new(
        "0",
        "0 — auto (client выбирает по tier)",
        "Default. Hardware detection.",
    )];
    for n in 2..=8i64 {
        out.push(ConfigOptionLabel::new(
            &n.to_string(),
            &format!("{n} threads"),
            "",
        ));
    }
    out
}

#[derive(Template)]
#[template(path = "device_edit.html")]
struct DeviceEditTemplate {
    user_login: String,
    device_id: i64,
    serial: String,
    display_name: String,
    is_active: bool,
    current_configuration_id: Option<i64>,
    configurations: Vec<ConfigOption>,
    groups: Vec<GroupCheckbox>,
    /// v0.12 Tier-2: список APK-версий для dropdown «закрепить версию».
    /// Filter'нут: только APK для этого customer'а, отсортирован по
    /// version_code DESC. Без сжатой пагинации — у одного приложения
    /// rare когда > 50 версий.
    app_versions: Vec<AppVersionOption>,
    /// Текущая закреплённая версия (NULL = follow rollout policy).
    pinned_version_id: Option<i64>,
    /// Что сейчас сообщает устройство — для UI «есть отставание / на target».
    current_app_version_name: Option<String>,
    current_app_version_code: Option<i64>,
    /// v0.13 (Settings Sync §1): snapshot ModelPreferences с устройства,
    /// pretty-printed JSON для UI viewer'а. Если устройство ещё не reportил
    /// b37+ — пустая строка.
    current_state_pretty: String,
    /// Monotonic счётчик; 0 если устройство ещё не reportil.
    current_state_version: i64,
    /// Когда был последний state-snapshot reporting. None если ещё не было.
    current_state_seen_at: Option<String>,
    /// Текстовое сообщение для admin'а под формой update-config: причина
    /// почему форма disabled (старый клиент / нет state).
    update_config_blocker: Option<String>,
    /// v0.18.15 (Phase 27): что сейчас в device.current_state_json для каждой
    /// known key — рендерим рядом с dropdown'ом «было: X». Пустая map если
    /// устройство ещё не reportilось.
    current_settings: std::collections::BTreeMap<String, String>,
    /// v0.18.15: dropdown options для structured config form.
    llm_options: Vec<ConfigOptionLabel>,
    translator_llm_options: Vec<ConfigOptionLabel>,
    vlm_options: Vec<ConfigOptionLabel>,
    stt_options: Vec<ConfigOptionLabel>,
    tts_mode_options: Vec<ConfigOptionLabel>,
    answer_mode_options: Vec<ConfigOptionLabel>,
    translator_mode_options: Vec<ConfigOptionLabel>,
    translator_audio_mode_options: Vec<ConfigOptionLabel>,
    log_level_options: Vec<ConfigOptionLabel>,
    cpu_thread_count_options: Vec<ConfigOptionLabel>,
    flash: Option<String>,
    error: Option<String>,
}

#[derive(sqlx::FromRow)]
struct ConfigOption {
    id: i64,
    name: String,
}

#[derive(sqlx::FromRow)]
struct AppVersionOption {
    id: i64,
    label: String, // "rc42-b35 (code 176, sha 36c93e1f…)"
}

struct GroupCheckbox {
    id: i64,
    name: String,
    assigned: bool,
}

async fn device_edit_view(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    flash: FlashCookie,
) -> Result<Response, ApiError> {
    render_device_edit(&user, &state, id, flash.0, None).await
}

async fn render_device_edit(
    user: &WebUser,
    state: &AppState,
    id: i64,
    flash: Option<String>,
    error: Option<String>,
) -> Result<Response, ApiError> {
    let row: Option<(
        String,
        Option<String>,
        bool,
        Option<i64>,
        Option<i64>,
        Option<String>,
        Option<i64>,
        String,
        i64,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT serial, display_name, is_active, configuration_id, pinned_version_id, \
                app_version, app_version_code, \
                current_state_json, current_state_version, current_state_seen_at \
         FROM devices WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?;
    let Some((
        serial,
        display_name,
        is_active,
        current_configuration_id,
        pinned_version_id,
        current_app_version_name,
        current_app_version_code,
        current_state_json_raw,
        current_state_version,
        current_state_seen_at,
    )) = row
    else {
        return Err(ApiError::NotFound);
    };
    // Pretty-print state JSON для UI; пустая строка если ещё ничего не reportilось.
    let current_state_pretty = if current_state_version > 0 {
        serde_json::from_str::<serde_json::Value>(&current_state_json_raw)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok())
            .unwrap_or_default()
    } else {
        String::new()
    };
    // Backward-compat gate: update-config работает с rc42 b37+ (versionCode >= 178).
    const MIN_VC: i64 = 178;
    let update_config_blocker = match current_app_version_code {
        None => Some(
            "устройство ещё не reportilo app_version_code — обновится при первом /sync"
                .to_string(),
        ),
        Some(v) if v < MIN_VC => Some(format!(
            "устройство на app_version_code={v}; нужно >= {MIN_VC} (rc42 b37+) для update-config"
        )),
        _ => None,
    };
    let configurations: Vec<ConfigOption> = sqlx::query_as::<_, ConfigOption>(
        "SELECT id, name FROM configurations WHERE customer_id = ? ORDER BY name LIMIT 500",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    let group_rows: Vec<(i64, String, Option<i64>)> = sqlx::query_as(
        "SELECT g.id, g.name, dg.device_id \
         FROM groups g \
         LEFT JOIN device_groups dg ON dg.group_id = g.id AND dg.device_id = ? \
         WHERE g.customer_id = ? ORDER BY g.name LIMIT 500",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    let groups: Vec<GroupCheckbox> = group_rows
        .into_iter()
        .map(|(gid, name, dev_match)| GroupCheckbox {
            id: gid,
            name,
            assigned: dev_match.is_some(),
        })
        .collect();
    // APK versions для pin-dropdown'а: оба `discovery` и `uploaded` rows.
    let app_versions: Vec<AppVersionOption> = sqlx::query_as(
        "SELECT av.id, \
                av.version_name || ' (code ' || av.version_code || ', sha ' || \
                substr(av.sha256, 1, 8) || '…)' AS label \
         FROM application_versions av \
         JOIN applications a ON a.id = av.application_id \
         WHERE a.customer_id = ? AND a.kind = 'apk' \
         ORDER BY av.version_code DESC LIMIT 200",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    // v0.18.15: распарсить current_state_json в плоскую map для рендеринга
    // рядом с dropdown'ами. Игнорируем nested objects / arrays — на форме
    // показываем только примитивы (числа, строки, bool).
    let mut current_settings: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    if current_state_version > 0 {
        if let Ok(serde_json::Value::Object(map)) =
            serde_json::from_str::<serde_json::Value>(&current_state_json_raw)
        {
            for (k, v) in map {
                let stringified = match v {
                    serde_json::Value::String(s) => s,
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    serde_json::Value::Null => "null".to_string(),
                    _ => continue, // skip nested objects/arrays
                };
                current_settings.insert(k, stringified);
            }
        }
    }
    let mut resp = render(DeviceEditTemplate {
        user_login: user.login.clone(),
        device_id: id,
        serial,
        display_name: display_name.unwrap_or_default(),
        is_active,
        current_configuration_id,
        configurations,
        groups,
        app_versions,
        pinned_version_id,
        current_app_version_name,
        current_app_version_code,
        current_state_pretty,
        current_state_version,
        current_state_seen_at,
        update_config_blocker,
        current_settings,
        llm_options: llm_options(),
        translator_llm_options: translator_llm_options(),
        vlm_options: vlm_options(),
        stt_options: stt_options(),
        tts_mode_options: tts_mode_options(),
        answer_mode_options: answer_mode_options(),
        translator_mode_options: translator_mode_options(),
        translator_audio_mode_options: translator_audio_mode_options(),
        log_level_options: log_level_options(),
        cpu_thread_count_options: cpu_thread_count_options(),
        flash,
        error,
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

async fn device_edit_post(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    let form = parse_form(&body);
    let req_display_name = form.first("display_name").map(|s| s.to_string());
    let req_configuration_id = form.first("configuration_id").map(|s| s.to_string());
    let req_is_active = form.first("is_active").map(|s| s.to_string());
    let req_pinned_version_id = form.first("pinned_version_id").map(|s| s.to_string());
    let req_group_ids: Vec<i64> = form
        .all("group_ids")
        .iter()
        .filter_map(|s| s.parse::<i64>().ok())
        .collect();
    let exists: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM devices WHERE id = ? AND customer_id = ?")
            .bind(id)
            .bind(user.customer_id)
            .fetch_optional(&state.db)
            .await?;
    if exists.is_none() {
        return Err(ApiError::NotFound);
    }
    let display_name = req_display_name
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let config_id: Option<i64> = req_configuration_id
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<i64>().ok());
    let is_active: i64 = req_is_active
        .as_deref()
        .map(|s| s.trim())
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(1);
    // Pin-version: пустая строка = NULL (отвязать пин), иначе валидируем
    // что эта application_versions реально принадлежит customer'у.
    let pinned_version_id: Option<i64> = match req_pinned_version_id.as_deref() {
        None | Some("") => None,
        Some(s) => match s.trim().parse::<i64>().ok() {
            Some(v) => {
                let owned: Option<i64> = sqlx::query_scalar(
                    "SELECT av.id FROM application_versions av \
                     JOIN applications a ON a.id = av.application_id \
                     WHERE av.id = ? AND a.customer_id = ?",
                )
                .bind(v)
                .bind(user.customer_id)
                .fetch_optional(&state.db)
                .await?;
                if owned.is_none() {
                    return Err(ApiError::BadRequest(
                        "pinned_version_id does not belong to this customer".into(),
                    ));
                }
                Some(v)
            }
            None => None,
        },
    };

    let mut tx = state.db.begin().await?;
    sqlx::query(
        "UPDATE devices SET display_name = ?, configuration_id = ?, is_active = ?, \
                            pinned_version_id = ?, updated_at = datetime('now') \
         WHERE id = ?",
    )
    .bind(display_name)
    .bind(config_id)
    .bind(is_active)
    .bind(pinned_version_id)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM device_groups WHERE device_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    for gid in &req_group_ids {
        sqlx::query(
            "INSERT INTO device_groups (device_id, group_id) \
             SELECT ?, id FROM groups WHERE id = ? AND customer_id = ?",
        )
        .bind(id)
        .bind(gid)
        .bind(user.customer_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(redirect_with_flash(
        &format!("/devices/{id}/edit"),
        "Device updated.",
    ))
}

/// v0.13: web-форма для admin-инициированного `update-config` push'а.
/// Принимает form-data `payload` где значение — pretty-JSON object, парсит,
/// и создаёт push_message с command='update-config'. После — flash + redirect
/// обратно на /devices/{id}/edit.
async fn device_config_form(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> Response {
    let form = parse_form(&body);
    let payload_raw = form.first("payload").unwrap_or("").trim();
    if payload_raw.is_empty() {
        return redirect_with_flash(
            &format!("/devices/{id}/edit"),
            "Пустой payload — нужно ввести JSON object.",
        );
    }
    let parsed: serde_json::Value = match serde_json::from_str(payload_raw) {
        Ok(v) => v,
        Err(e) => {
            return redirect_with_flash(
                &format!("/devices/{id}/edit"),
                &format!("Не JSON: {e}"),
            );
        }
    };
    if !parsed.is_object() {
        return redirect_with_flash(
            &format!("/devices/{id}/edit"),
            "payload должен быть JSON object (например {\"preferred_llm\": \"...\"}).",
        );
    }
    // Verify device + version gate.
    let row: Option<(Option<i64>,)> = sqlx::query_as(
        "SELECT app_version_code FROM devices WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);
    let Some((av,)) = row else {
        return redirect_with_flash(&format!("/devices/{id}/edit"), "Устройство не найдено.");
    };
    const MIN_VC: i64 = 178;
    match av {
        None => {
            return redirect_with_flash(
                &format!("/devices/{id}/edit"),
                "Устройство ещё не reportilo app_version_code — дождись первого /sync.",
            );
        }
        Some(v) if v < MIN_VC => {
            return redirect_with_flash(
                &format!("/devices/{id}/edit"),
                &format!("Старый клиент (versionCode={v}); нужен >= {MIN_VC} (rc42 b37+)."),
            );
        }
        _ => {}
    }
    let canonical = serde_json::to_string(&parsed).unwrap_or_else(|_| "{}".to_string());
    let res = sqlx::query_scalar::<_, i64>(
        "INSERT INTO push_messages (customer_id, device_id, command, payload_json, status) \
         VALUES (?, ?, 'update-config', ?, 'pending') RETURNING id",
    )
    .bind(user.customer_id)
    .bind(id)
    .bind(&canonical)
    .fetch_one(&state.db)
    .await;
    match res {
        Ok(cmd_id) => redirect_with_flash(
            &format!("/devices/{id}/edit"),
            &format!("update-config поставлен в очередь (command_id={cmd_id}); устройство применит на ≤30мин"),
        ),
        Err(e) => {
            tracing::error!(error = %e, "device_config_form insert failed");
            redirect_with_flash(&format!("/devices/{id}/edit"), "DB error при создании push_message.")
        }
    }
}

// ----- v0.15 (MDM-DEVICE-CONTROL-CONTRACT §2/§3) admin Web UI handlers ------

#[derive(Template)]
#[template(path = "file_distribute.html")]
struct FileDistributeTemplate {
    user_login: String,
    file_id: i64,
    original_name: String,
    size_human: String,
    sha256_short: String,
    devices: Vec<DistributeDeviceOption>,
    groups: Vec<GroupOption>,
    flash: Option<String>,
    error: Option<String>,
}

#[derive(sqlx::FromRow)]
struct DistributeDeviceOption {
    id: i64,
    serial: String,
    display_name: Option<String>,
}

async fn file_distribute_view(
    user: WebUser,
    State(state): State<AppState>,
    Path(file_id): Path<i64>,
    flash: FlashCookie,
) -> Result<Response, ApiError> {
    render_file_distribute(&user, &state, file_id, flash.0, None).await
}

async fn render_file_distribute(
    user: &WebUser,
    state: &AppState,
    file_id: i64,
    flash: Option<String>,
    error: Option<String>,
) -> Result<Response, ApiError> {
    let row: Option<(String, i64, String)> = sqlx::query_as(
        "SELECT original_name, file_size_bytes, sha256 \
         FROM uploaded_files WHERE id = ? AND customer_id = ?",
    )
    .bind(file_id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?;
    let Some((original_name, size, sha)) = row else {
        return Err(ApiError::NotFound);
    };
    let devices: Vec<DistributeDeviceOption> = sqlx::query_as(
        "SELECT id, serial, display_name FROM devices \
         WHERE customer_id = ? AND is_active = 1 \
         ORDER BY serial LIMIT 500",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    let groups: Vec<GroupOption> = sqlx::query_as(
        "SELECT id, name FROM groups WHERE customer_id = ? ORDER BY name LIMIT 200",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    let mut resp = render(FileDistributeTemplate {
        user_login: user.login.clone(),
        file_id,
        original_name,
        size_human: format_size(size),
        sha256_short: sha.chars().take(16).collect(),
        devices,
        groups,
        flash,
        error,
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

/// POST form-data → JSON API call. Возвращает редирект на /files c flash.
async fn file_distribute_form(
    user: WebUser,
    State(state): State<AppState>,
    Path(file_id): Path<i64>,
    body: axum::body::Bytes,
) -> Response {
    let form = parse_form(&body);
    let target_type = form.first("target_type").unwrap_or("");
    let kind = form.first("kind").unwrap_or("arbitrary_blob").to_string();
    let filename = form.first("filename").unwrap_or("").trim().to_string();
    let expires_at = form
        .first("expires_at")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    if filename.is_empty() {
        return redirect_with_flash(
            &format!("/files/{file_id}/distribute"),
            "filename обязателен.",
        );
    }

    let target_json = match target_type {
        "device" => {
            let Some(dev_id) = form.first("target_device_id").and_then(|s| s.parse::<i64>().ok())
            else {
                return redirect_with_flash(
                    &format!("/files/{file_id}/distribute"),
                    "Не выбрано устройство.",
                );
            };
            serde_json::json!({"type": "device", "id": dev_id})
        }
        "group" => {
            let Some(g_id) = form.first("target_group_id").and_then(|s| s.parse::<i64>().ok())
            else {
                return redirect_with_flash(
                    &format!("/files/{file_id}/distribute"),
                    "Не выбрана группа.",
                );
            };
            serde_json::json!({"type": "group", "id": g_id})
        }
        "customer_fleet" => serde_json::json!({"type": "customer_fleet"}),
        _ => {
            return redirect_with_flash(
                &format!("/files/{file_id}/distribute"),
                "Не выбран target_type.",
            );
        }
    };

    // Вызываем internal helper distribute logic. Чтобы не дублировать
    // 200 строк, экспортируем `do_distribute_file` из routes/distribute.rs.
    let req = crate::routes::distribute::DistributeRequestRaw {
        target: target_json,
        filename,
        kind,
        expires_at,
        notes: None,
    };
    match crate::routes::distribute::do_distribute_file(&state, &user.into(), file_id, req).await {
        Ok(resp) => redirect_with_flash(
            "/files",
            &format!(
                "Зашифровано и поставлено в очередь: {} получателей, {} команд (skipped: {} без pubkey, {} legacy)",
                resp.eligible_count,
                resp.command_ids.len(),
                resp.skipped_no_pubkey,
                resp.skipped_old_clients,
            ),
        ),
        Err(ApiError::BadRequest(msg)) => redirect_with_flash(
            &format!("/files/{file_id}/distribute"),
            &format!("Ошибка: {msg}"),
        ),
        Err(ApiError::NotFound) => redirect_with_flash("/files", "Файл не найден."),
        Err(e) => {
            tracing::error!(error = ?e, "distribute form failed");
            redirect_with_flash(
                &format!("/files/{file_id}/distribute"),
                "Внутренняя ошибка сервера.",
            )
        }
    }
}

/// v0.18.12: загрузить N файлов одной операцией на один target.
///
/// HTML form parses через `parse_form` (стандартный helper в этом файле для
/// POST'ов чтобы избежать axum::Form ограничений на repeated keys). Поле
/// `file_ids` повторяется N раз (по одному на checkbox), `target_type`
/// + `target_device_id`/`target_group_id` — один на форму.
///
/// Делает loop по file_ids, для каждого вызывает существующий
/// `do_distribute_file` с тем же target. Накопленные результаты — total
/// command_ids, total skipped, partial-failures.
async fn files_bulk_distribute(
    user: WebUser,
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> Response {
    let form = parse_form(&body);
    let file_ids: Vec<i64> = form
        .all("file_ids")
        .into_iter()
        .filter_map(|s| s.parse::<i64>().ok())
        .collect();
    if file_ids.is_empty() {
        return redirect_with_flash("/files", "Не выбрано ни одного файла.");
    }
    let target_type = form.first("target_type").unwrap_or("");
    let target_json = match target_type {
        "device" => {
            let Some(dev_id) = form
                .first("target_device_id")
                .and_then(|s| s.parse::<i64>().ok())
            else {
                return redirect_with_flash("/files", "Не выбрано устройство.");
            };
            serde_json::json!({"type": "device", "id": dev_id})
        }
        "group" => {
            let Some(g_id) = form
                .first("target_group_id")
                .and_then(|s| s.parse::<i64>().ok())
            else {
                return redirect_with_flash("/files", "Не выбрана группа.");
            };
            serde_json::json!({"type": "group", "id": g_id})
        }
        "customer_fleet" => serde_json::json!({"type": "customer_fleet"}),
        _ => {
            return redirect_with_flash("/files", "Не выбран target_type.");
        }
    };

    let mut total_commands: i64 = 0;
    let mut total_skipped_no_pubkey: i64 = 0;
    let mut total_skipped_legacy: i64 = 0;
    let mut failures: Vec<String> = Vec::new();
    let actor: crate::routes::distribute::DistributeActor = (&user).into();

    for file_id in &file_ids {
        // Pull original_name из uploaded_files для filename поля.
        let original_name: Option<String> = sqlx::query_scalar(
            "SELECT original_name FROM uploaded_files WHERE id = ? AND customer_id = ?",
        )
        .bind(file_id)
        .bind(user.customer_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
        let Some(filename) = original_name else {
            failures.push(format!("file_id={file_id} not found"));
            continue;
        };
        let req = crate::routes::distribute::DistributeRequestRaw {
            target: target_json.clone(),
            filename,
            kind: "arbitrary_blob".to_string(),
            expires_at: None,
            notes: Some(format!("bulk-distribute by user {} via UI", user.login)),
        };
        match crate::routes::distribute::do_distribute_file(&state, &actor, *file_id, req).await {
            Ok(resp) => {
                total_commands += resp.command_ids.len() as i64;
                total_skipped_no_pubkey += resp.skipped_no_pubkey;
                total_skipped_legacy += resp.skipped_old_clients;
            }
            Err(e) => failures.push(format!("file_id={file_id}: {e:?}")),
        }
    }

    let msg = if failures.is_empty() {
        format!(
            "Bulk-distribute: {} файлов → {} команд, skipped pubkey={}, legacy={}",
            file_ids.len(),
            total_commands,
            total_skipped_no_pubkey,
            total_skipped_legacy,
        )
    } else {
        format!(
            "Bulk-distribute (с ошибками): {} команд, skipped pubkey={}, legacy={}; failures: {}",
            total_commands,
            total_skipped_no_pubkey,
            total_skipped_legacy,
            failures.join("; "),
        )
    };
    redirect_with_flash("/files", &msg)
}

// ----- §3 device-command form handlers --------------------------------------

/// v0.18.15 (Phase 27): structured update-config form. В отличие от
/// `device_config_form` который принимает raw JSON, здесь имеется по одному
/// HTML form field на каждый whitelist key из `MDM-DEVICE-CONTROL-CONTRACT
/// §1.3`. Пустое значение fields трактуется как «не менять» (key пропускается
/// в итоговом payload'е). Special-cased `*_TRISTATE` для bool fields, где
/// "" = не менять, "true"/"false" — соответствующее присвоение.
///
/// На итоговый push_message пишется тот же `command='update-config'` что и
/// у raw-JSON формы — для клиента это identical contract.
async fn device_config_structured_form(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> Response {
    let form = parse_form(&body);
    // Каждый key добавляется в payload только если value не пустое.
    // Type coercion: bools "true"/"false", ints парсятся, остальное — strings.
    let mut payload = serde_json::Map::new();

    // Filename / enum string keys.
    for key in [
        "preferred_llm",
        "preferred_translator_llm",
        "preferred_vlm",
        "preferred_stt",
        "tts_mode",
        "answer_mode",
        "translator_mode",
        "translator_audio_mode",
        "log_level",
    ] {
        if let Some(raw) = form.first(key) {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                payload.insert(key.into(), serde_json::Value::String(trimmed.into()));
            }
        }
    }
    // Integer keys.
    for key in ["cpu_thread_count"] {
        if let Some(raw) = form.first(key) {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                if let Ok(n) = trimmed.parse::<i64>() {
                    payload.insert(key.into(), serde_json::Value::Number(n.into()));
                }
            }
        }
    }
    // Tri-state bool keys: "" = skip, "true"/"false" = присвоить.
    for key in [
        "wake_word_enabled",
        "translator_cloud_enabled",
        "show_build_badge",
        "telemetry_enabled",
    ] {
        if let Some(raw) = form.first(key) {
            match raw.trim() {
                "true" => {
                    payload.insert(key.into(), serde_json::Value::Bool(true));
                }
                "false" => {
                    payload.insert(key.into(), serde_json::Value::Bool(false));
                }
                _ => {} // skip empty / unrecognised
            }
        }
    }

    if payload.is_empty() {
        return redirect_with_flash(
            &format!("/devices/{id}/edit"),
            "Ничего не выбрано — все поля стояли на «не менять». Команда не отправлена.",
        );
    }

    queue_device_command_form(
        &state,
        &user,
        id,
        "update-config",
        serde_json::Value::Object(payload),
    )
    .await
}

/// v0.18.15 (Phase 27): admin-initiated `install-apk` push. Ставит в очередь
/// push_message с command='install-apk' и payload {version_code, version_name,
/// sha256, url, size_bytes}. Client (rc≥X b≥Y, точное число согласуется
/// с AR Hud team — см. `MDM-DEVICE-CONTROL-CONTRACT.md §3.4`) применяет:
/// — качает APK с url
/// — verify'ит sha256
/// — вызывает PackageInstaller с user-prompt (silent-install требует
///   Device-Owner DPM, реализация on AR Hud стороне).
///
/// До тех пор пока AR Hud не реализует client-side — команда будет
/// сохраняться в `applied_commands` со status='error', message='unknown
/// command' или просто игнорироваться (зависит от client behaviour для
/// unknown commands). Безопасно — пользовательских данных не задевает.
async fn device_install_apk_form(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> Response {
    let form = parse_form(&body);
    let version_id_raw = form.first("version_id").unwrap_or("").trim();
    let version_id: i64 = match version_id_raw.parse() {
        Ok(v) => v,
        Err(_) => {
            return redirect_with_flash(
                &format!("/devices/{id}/edit"),
                "Выберите версию APK из dropdown'а.",
            );
        }
    };
    // Lookup app version row + verify customer ownership через JOIN на applications.
    let row: Option<(i64, String, String, i64, Option<String>)> = sqlx::query_as(
        "SELECT av.version_code, av.version_name, av.sha256, av.file_size_bytes, av.source_url \
         FROM application_versions av \
         JOIN applications a ON a.id = av.application_id \
         WHERE av.id = ? AND a.customer_id = ? AND a.kind = 'apk'",
    )
    .bind(version_id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);
    let Some((version_code, version_name, sha256, size_bytes, source_url)) = row else {
        return redirect_with_flash(
            &format!("/devices/{id}/edit"),
            "Версия APK не найдена в customer scope.",
        );
    };
    let Some(url) = source_url else {
        return redirect_with_flash(
            &format!("/devices/{id}/edit"),
            "У этой версии APK нет source_url — устройство не сможет её скачать. \
             Загрузите версию с source_url через /applications или раскатайте через rollouts.",
        );
    };
    let payload = serde_json::json!({
        "version_code": version_code,
        "version_name": version_name,
        "sha256": sha256,
        "size_bytes": size_bytes,
        "url": url,
    });
    queue_device_command_form(&state, &user, id, "install-apk", payload).await
}

async fn device_rotate_cloudru_creds_form(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> Response {
    let form = parse_form(&body);
    let tenant_id = form.first("tenant_id").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let key_id = form.first("key_id").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let secret = form.first("secret").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if tenant_id.is_none() && key_id.is_none() && secret.is_none() {
        return redirect_with_flash(
            &format!("/devices/{id}/edit"),
            "Хотя бы одно поле tenant_id/key_id/secret обязательно.",
        );
    }
    let payload = serde_json::json!({
        "tenant_id": tenant_id,
        "key_id": key_id,
        "secret": secret,
    });
    queue_device_command_form(&state, &user, id, "rotate-cloudru-creds", payload).await
}

async fn device_revoke_enrollment_form(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> Response {
    let form = parse_form(&body);
    let reason = form
        .first("reason")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "admin-initiated".to_string());
    let payload = serde_json::json!({"reason": reason});
    queue_device_command_form(&state, &user, id, "revoke-enrollment", payload).await
}

async fn device_remote_wipe_form(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> Response {
    let form = parse_form(&body);
    let scope = form
        .first("scope")
        .map(|s| s.trim().to_string())
        .filter(|s| s == "app-data" || s == "factory-reset")
        .unwrap_or_else(|| "app-data".to_string());
    let reason = form
        .first("reason")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "admin-initiated".to_string());
    let payload = serde_json::json!({"scope": scope, "reason": reason});
    queue_device_command_form(&state, &user, id, "remote-wipe", payload).await
}

async fn queue_device_command_form(
    state: &AppState,
    user: &WebUser,
    device_id: i64,
    command: &str,
    payload: serde_json::Value,
) -> Response {
    let row: Option<(Option<i64>,)> = sqlx::query_as(
        "SELECT app_version_code FROM devices WHERE id = ? AND customer_id = ?",
    )
    .bind(device_id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);
    let Some((av,)) = row else {
        return redirect_with_flash(&format!("/devices/{device_id}/edit"), "Устройство не найдено.");
    };
    const MIN_VC: i64 = 178;
    match av {
        None => {
            return redirect_with_flash(
                &format!("/devices/{device_id}/edit"),
                "Устройство ещё не reportilo app_version_code.",
            );
        }
        Some(v) if v < MIN_VC => {
            return redirect_with_flash(
                &format!("/devices/{device_id}/edit"),
                &format!("Старый клиент (versionCode={v}); нужен >= {MIN_VC}."),
            );
        }
        _ => {}
    }
    let payload_json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    let res = sqlx::query_scalar::<_, i64>(
        "INSERT INTO push_messages (customer_id, device_id, command, payload_json, status) \
         VALUES (?, ?, ?, ?, 'pending') RETURNING id",
    )
    .bind(user.customer_id)
    .bind(device_id)
    .bind(command)
    .bind(&payload_json)
    .fetch_one(&state.db)
    .await;
    match res {
        Ok(cmd_id) => {
            tracing::warn!(
                actor_user = user.id,
                target_device = device_id,
                command = %command,
                cmd_id,
                "admin issued device command via web form"
            );
            redirect_with_flash(
                &format!("/devices/{device_id}/edit"),
                &format!("{command} поставлен в очередь (command_id={cmd_id})"),
            )
        }
        Err(_) => redirect_with_flash(
            &format!("/devices/{device_id}/edit"),
            "DB error при создании push_message.",
        ),
    }
}

/// Minimal multi-valued x-www-form-urlencoded parser. axum's `Form`
/// extractor goes through `serde_urlencoded` which deserializes each key
/// once and rejects `Vec<_>` fields — this helper handles the multi-check
/// case (e.g. `group_ids=1&group_ids=2`) without dragging in `axum-extra`
/// or `serde_html_form`.
struct ParsedForm {
    pairs: Vec<(String, String)>,
}

impl ParsedForm {
    fn first<'a>(&'a self, key: &str) -> Option<&'a str> {
        self.pairs
            .iter()
            .find_map(|(k, v)| if k == key { Some(v.as_str()) } else { None })
    }
    fn all<'a>(&'a self, key: &str) -> Vec<&'a str> {
        self.pairs
            .iter()
            .filter_map(|(k, v)| if k == key { Some(v.as_str()) } else { None })
            .collect()
    }
}

fn parse_form(body: &[u8]) -> ParsedForm {
    let s = std::str::from_utf8(body).unwrap_or("");
    let mut pairs = Vec::new();
    for piece in s.split('&') {
        if piece.is_empty() {
            continue;
        }
        let (k, v) = piece.split_once('=').unwrap_or((piece, ""));
        pairs.push((percent_decode(k), percent_decode(v)));
    }
    ParsedForm { pairs }
}

async fn device_delete(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Response {
    let res = sqlx::query("DELETE FROM devices WHERE id = ? AND customer_id = ?")
        .bind(id)
        .bind(user.customer_id)
        .execute(&state.db)
        .await;
    match res {
        Ok(r) if r.rows_affected() > 0 => redirect_with_flash("/devices", "Device deleted."),
        Ok(_) => redirect_with_flash("/devices", "Device not found."),
        Err(e) => {
            tracing::error!(error = %e, "device_delete failed");
            redirect_with_flash("/devices", "Database error.")
        }
    }
}

// ----- Group edit / delete -------------------------------------------------

#[derive(Template)]
#[template(path = "group_edit.html")]
struct GroupEditTemplate {
    user_login: String,
    group_id: i64,
    name: String,
    description: String,
    member_count: i64,
    flash: Option<String>,
    error: Option<String>,
}

async fn group_edit_view(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    flash: FlashCookie,
) -> Result<Response, ApiError> {
    render_group_edit(&user, &state, id, flash.0, None).await
}

async fn render_group_edit(
    user: &WebUser,
    state: &AppState,
    id: i64,
    flash: Option<String>,
    error: Option<String>,
) -> Result<Response, ApiError> {
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT name, description FROM groups WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?;
    let Some((name, description)) = row else {
        return Err(ApiError::NotFound);
    };
    let member_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM device_groups WHERE group_id = ?")
            .bind(id)
            .fetch_one(&state.db)
            .await?;
    let mut resp = render(GroupEditTemplate {
        user_login: user.login.clone(),
        group_id: id,
        name,
        description: description.unwrap_or_default(),
        member_count,
        flash,
        error,
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

#[derive(Debug, Deserialize)]
struct GroupEditForm {
    name: String,
    description: Option<String>,
}

async fn group_edit_post(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(req): Form<GroupEditForm>,
) -> Result<Response, Response> {
    let name = req.name.trim();
    if name.is_empty() {
        return render_group_edit(&user, &state, id, None, Some("Название обязательно".into()))
            .await
            .map_err(|e| e.into_response());
    }
    let description = req
        .description
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let res = sqlx::query(
        "UPDATE groups SET name = ?, description = ?, updated_at = datetime('now') \
         WHERE id = ? AND customer_id = ?",
    )
    .bind(name)
    .bind(description)
    .bind(id)
    .bind(user.customer_id)
    .execute(&state.db)
    .await;
    match res {
        Ok(r) if r.rows_affected() > 0 => Ok(redirect_with_flash(
            &format!("/groups/{id}/edit"),
            "Group updated.",
        )),
        Ok(_) => Err((StatusCode::NOT_FOUND, "Group not found").into_response()),
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => render_group_edit(
            &user,
            &state,
            id,
            None,
            Some(format!("Group '{name}' already exists")),
        )
        .await
        .map_err(|e| e.into_response()),
        Err(e) => {
            tracing::error!(error = %e, "group_edit_post failed");
            render_group_edit(&user, &state, id, None, Some("Database error".into()))
                .await
                .map_err(|e| e.into_response())
        }
    }
}

async fn group_delete(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Response {
    let res = sqlx::query("DELETE FROM groups WHERE id = ? AND customer_id = ?")
        .bind(id)
        .bind(user.customer_id)
        .execute(&state.db)
        .await;
    match res {
        Ok(r) if r.rows_affected() > 0 => redirect_with_flash("/groups", "Group deleted."),
        Ok(_) => redirect_with_flash("/groups", "Group not found."),
        Err(e) => {
            tracing::error!(error = %e, "group_delete failed");
            redirect_with_flash("/groups", "Database error.")
        }
    }
}

// v0.18.6: per-group device membership mutations. Both check
// customer_id ownership of BOTH the group AND the device before any
// row touch — admin of customer A cannot drag devices of customer B
// into their groups.

#[derive(Debug, Deserialize)]
struct GroupMemberAddForm {
    device_id: i64,
}

async fn group_member_add(
    user: WebUser,
    State(state): State<AppState>,
    Path(group_id): Path<i64>,
    Form(req): Form<GroupMemberAddForm>,
) -> Response {
    // Verify the group belongs to this customer.
    let group_ok: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM groups WHERE id = ? AND customer_id = ?",
    )
    .bind(group_id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    if group_ok.is_none() {
        return redirect_with_flash("/groups", "Group not found.");
    }
    // Verify the device belongs to this customer.
    let device_ok: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM devices WHERE id = ? AND customer_id = ?",
    )
    .bind(req.device_id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    if device_ok.is_none() {
        return redirect_with_flash("/groups", "Device not found.");
    }
    // INSERT OR IGNORE — повторное добавление того же device idempotent.
    let res = sqlx::query(
        "INSERT OR IGNORE INTO device_groups(device_id, group_id) VALUES(?, ?)",
    )
    .bind(req.device_id)
    .bind(group_id)
    .execute(&state.db)
    .await;
    match res {
        Ok(_) => redirect_with_flash("/groups", "Device added to group."),
        Err(e) => {
            tracing::error!(error = %e, "group_member_add failed");
            redirect_with_flash("/groups", "Database error.")
        }
    }
}

async fn group_member_remove(
    user: WebUser,
    State(state): State<AppState>,
    Path((group_id, device_id)): Path<(i64, i64)>,
) -> Response {
    // Single DELETE с двойной join-проверкой на customer_id —
    // и группа, и устройство должны принадлежать тому же customer'у.
    let res = sqlx::query(
        "DELETE FROM device_groups \
         WHERE device_id = ? AND group_id = ? \
           AND device_id IN (SELECT id FROM devices WHERE customer_id = ?) \
           AND group_id IN (SELECT id FROM groups WHERE customer_id = ?)",
    )
    .bind(device_id)
    .bind(group_id)
    .bind(user.customer_id)
    .bind(user.customer_id)
    .execute(&state.db)
    .await;
    match res {
        Ok(_) => redirect_with_flash("/groups", "Device removed from group."),
        Err(e) => {
            tracing::error!(error = %e, "group_member_remove failed");
            redirect_with_flash("/groups", "Database error.")
        }
    }
}

// ----- User delete + admin reset password ----------------------------------

async fn users_delete(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Response {
    if id == user.id {
        return redirect_with_flash("/users", "Cannot delete your own account.");
    }
    let res = sqlx::query("DELETE FROM users WHERE id = ? AND customer_id = ?")
        .bind(id)
        .bind(user.customer_id)
        .execute(&state.db)
        .await;
    match res {
        Ok(r) if r.rows_affected() > 0 => redirect_with_flash("/users", "User deleted."),
        Ok(_) => redirect_with_flash("/users", "User not found."),
        Err(e) => {
            tracing::error!(error = %e, "users_delete failed");
            redirect_with_flash("/users", "Database error.")
        }
    }
}

async fn users_admin_reset_password(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Response {
    // Generate a one-time password, hash it, set must_change_password=1.
    // Show the plain password as a flash message (operator copies it once).
    let one_time = crypto::generate_password(16);
    let phc = match crypto::hash_password(&one_time) {
        Ok(s) => s,
        Err(_) => return redirect_with_flash("/users", "Password hash error."),
    };
    let res = sqlx::query(
        "UPDATE users SET password_hash = ?, must_change_password = 1, updated_at = datetime('now') \
         WHERE id = ? AND customer_id = ?",
    )
    .bind(&phc)
    .bind(id)
    .bind(user.customer_id)
    .execute(&state.db)
    .await;
    match res {
        Ok(r) if r.rows_affected() > 0 => redirect_with_flash(
            "/users",
            &format!("New one-time password (must change on first login): {one_time}"),
        ),
        Ok(_) => redirect_with_flash("/users", "User not found."),
        Err(e) => {
            tracing::error!(error = %e, "admin_reset_password failed");
            redirect_with_flash("/users", "Database error.")
        }
    }
}

// ----- Application edit / delete / versions --------------------------------

#[derive(Template)]
#[template(path = "application_edit.html")]
struct AppEditTemplate {
    user_login: String,
    app_id: i64,
    package_name: String,
    display_name: String,
    description: String,
    kind_options: Vec<(&'static str, bool)>,
    flash: Option<String>,
    error: Option<String>,
}

const APP_KINDS: &[&str] = &[
    "apk",
    "llm-model",
    "mmproj",
    "whisper",
    "tts",
    "knowledge-db",
    "mbtiles",
    "config",
];

async fn application_edit_view(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    flash: FlashCookie,
) -> Result<Response, ApiError> {
    render_app_edit(&user, &state, id, flash.0, None).await
}

async fn render_app_edit(
    user: &WebUser,
    state: &AppState,
    id: i64,
    flash: Option<String>,
    error: Option<String>,
) -> Result<Response, ApiError> {
    let row: Option<(String, Option<String>, Option<String>, String)> = sqlx::query_as(
        "SELECT package_name, display_name, description, kind \
         FROM applications WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?;
    let Some((package_name, display_name, description, kind)) = row else {
        return Err(ApiError::NotFound);
    };
    let kind_options: Vec<(&'static str, bool)> =
        APP_KINDS.iter().map(|k| (*k, *k == kind.as_str())).collect();
    let mut resp = render(AppEditTemplate {
        user_login: user.login.clone(),
        app_id: id,
        package_name,
        display_name: display_name.unwrap_or_default(),
        description: description.unwrap_or_default(),
        kind_options,
        flash,
        error,
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

#[derive(Debug, Deserialize)]
struct AppEditForm {
    display_name: Option<String>,
    description: Option<String>,
    kind: String,
}

async fn application_edit_post(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(req): Form<AppEditForm>,
) -> Result<Response, Response> {
    let kind = req.kind.trim();
    if kind.is_empty() {
        return render_app_edit(&user, &state, id, None, Some("Тип обязателен".into()))
            .await
            .map_err(|e| e.into_response());
    }
    let display_name = req
        .display_name
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let description = req
        .description
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let res = sqlx::query(
        "UPDATE applications SET display_name = ?, description = ?, kind = ?, updated_at = datetime('now') \
         WHERE id = ? AND customer_id = ?",
    )
    .bind(display_name)
    .bind(description)
    .bind(kind)
    .bind(id)
    .bind(user.customer_id)
    .execute(&state.db)
    .await;
    match res {
        Ok(r) if r.rows_affected() > 0 => Ok(redirect_with_flash(
            &format!("/applications/{id}/edit"),
            "Application updated.",
        )),
        Ok(_) => Err((StatusCode::NOT_FOUND, "App not found").into_response()),
        Err(e) => {
            tracing::error!(error = %e, "app_edit_post failed");
            render_app_edit(&user, &state, id, None, Some("Database error".into()))
                .await
                .map_err(|e| e.into_response())
        }
    }
}

async fn application_delete(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Response {
    let res = sqlx::query("DELETE FROM applications WHERE id = ? AND customer_id = ?")
        .bind(id)
        .bind(user.customer_id)
        .execute(&state.db)
        .await;
    match res {
        Ok(r) if r.rows_affected() > 0 => {
            redirect_with_flash("/applications", "Application deleted.")
        }
        Ok(_) => redirect_with_flash("/applications", "Application not found."),
        Err(e) => {
            tracing::error!(error = %e, "application_delete failed");
            redirect_with_flash("/applications", "Database error.")
        }
    }
}

#[derive(Template)]
#[template(path = "application_versions.html")]
struct AppVersionsTemplate {
    user_login: String,
    app_id: i64,
    package_name: String,
    versions: Vec<AppVersionRow>,
    flash: Option<String>,
    create_error: Option<String>,
}

struct AppVersionRow {
    id: i64,
    version_code: i64,
    version_name: String,
    file_size: String,
    sha256_short: String,
    uploaded_at: String,
    /// `Some(url)` для версий, найденных APK watcher'ом на upstream-mirror'е;
    /// `None` для версий, загруженных через POST /applications/{id}/versions
    /// (multipart upload). UI рисует разный affordance: для discovered —
    /// «Открыть на mirror», для uploaded — «Скачать с MDM».
    source_url: Option<String>,
    /// `true` если файла нет на диске MDM (file_path = ''), `false` для
    /// uploaded версий. Discovered-rows показывают пометку «metadata-only».
    metadata_only: bool,
}

#[derive(sqlx::FromRow)]
struct AppVersionRowRaw {
    id: i64,
    version_code: i64,
    version_name: String,
    file_size_bytes: i64,
    sha256: String,
    uploaded_at: String,
    file_path: String,
    source_url: Option<String>,
}

async fn application_versions_view(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    flash: FlashCookie,
) -> Result<Response, ApiError> {
    render_app_versions(&user, &state, id, flash.0, None).await
}

async fn render_app_versions(
    user: &WebUser,
    state: &AppState,
    id: i64,
    flash: Option<String>,
    create_error: Option<String>,
) -> Result<Response, ApiError> {
    let pkg: Option<String> =
        sqlx::query_scalar("SELECT package_name FROM applications WHERE id = ? AND customer_id = ?")
            .bind(id)
            .bind(user.customer_id)
            .fetch_optional(&state.db)
            .await?;
    let Some(package_name) = pkg else {
        return Err(ApiError::NotFound);
    };
    let rows: Vec<AppVersionRowRaw> = sqlx::query_as::<_, AppVersionRowRaw>(
        "SELECT id, version_code, version_name, file_size_bytes, sha256, uploaded_at, \
                file_path, source_url \
         FROM application_versions WHERE application_id = ? ORDER BY version_code DESC LIMIT 200",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let versions = rows
        .into_iter()
        .map(|r| AppVersionRow {
            id: r.id,
            version_code: r.version_code,
            version_name: r.version_name,
            file_size: format_size(r.file_size_bytes),
            sha256_short: r.sha256.chars().take(12).collect(),
            uploaded_at: state.fmt_ts(&r.uploaded_at),
            metadata_only: r.file_path.is_empty(),
            source_url: r.source_url,
        })
        .collect();
    let mut resp = render(AppVersionsTemplate {
        user_login: user.login.clone(),
        app_id: id,
        package_name,
        versions,
        flash,
        create_error,
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

async fn application_version_add(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    multipart: Multipart,
) -> Response {
    match try_add_app_version(&user, &state, id, multipart).await {
        Ok(()) => redirect_with_flash(
            &format!("/applications/{id}/versions"),
            "Version uploaded.",
        ),
        Err(msg) => render_app_versions(&user, &state, id, None, Some(msg))
            .await
            .unwrap_or_else(|_| {
                (StatusCode::INTERNAL_SERVER_ERROR, "render failed").into_response()
            }),
    }
}

async fn try_add_app_version(
    user: &WebUser,
    state: &AppState,
    app_id: i64,
    mut multipart: Multipart,
) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let owned: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM applications WHERE id = ? AND customer_id = ?")
            .bind(app_id)
            .bind(user.customer_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|_| "database error".to_string())?;
    if owned.is_none() {
        return Err("application not found".into());
    }
    let mut version_code: Option<i64> = None;
    let mut version_name = String::new();
    let mut notes: Option<String> = None;
    let mut bytes: Option<Vec<u8>> = None;
    let mut original: Option<String> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| format!("multipart: {e}"))?
    {
        match field.name().unwrap_or("") {
            "version_code" => {
                let raw = field.text().await.map_err(|e| format!("{e}"))?;
                version_code = raw.trim().parse::<i64>().ok();
            }
            "version_name" => version_name = field.text().await.map_err(|e| format!("{e}"))?,
            "notes" => notes = field.text().await.ok().filter(|s| !s.trim().is_empty()),
            "file" => {
                original = field.file_name().map(|s| s.to_string());
                let data = field.bytes().await.map_err(|e| format!("{e}"))?;
                bytes = Some(data.to_vec());
            }
            _ => {}
        }
    }
    let vcode = version_code.ok_or_else(|| "version_code must be an integer".to_string())?;
    if version_name.trim().is_empty() {
        return Err("version_name is required".into());
    }
    let bytes = bytes.ok_or_else(|| "file is required".to_string())?;
    let original_name = original.ok_or_else(|| "file has no name".to_string())?;
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
    let res = sqlx::query(
        "INSERT INTO application_versions \
            (application_id, version_code, version_name, file_path, file_size_bytes, sha256, is_active, notes, uploaded_by) \
         VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?)",
    )
    .bind(app_id)
    .bind(vcode)
    .bind(version_name.trim())
    .bind(&stored.relative_path)
    .bind(bytes.len() as i64)
    .bind(&sha)
    .bind(notes.as_deref())
    .bind(user.id)
    .execute(&state.db)
    .await;
    match res {
        Ok(_) => Ok(()),
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
            Err(format!("version_code {vcode} already exists for this app"))
        }
        Err(e) => {
            tracing::error!(error = %e, "insert version failed");
            Err("database error".into())
        }
    }
}

async fn application_version_delete(
    user: WebUser,
    State(state): State<AppState>,
    Path((id, vid)): Path<(i64, i64)>,
) -> Response {
    // Verify ownership through application table
    let owned: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM application_versions v JOIN applications a ON a.id = v.application_id \
         WHERE v.id = ? AND a.id = ? AND a.customer_id = ?",
    )
    .bind(vid)
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);
    if owned.is_none() {
        return redirect_with_flash(
            &format!("/applications/{id}/versions"),
            "Version not found.",
        );
    }
    let res = sqlx::query("DELETE FROM application_versions WHERE id = ?")
        .bind(vid)
        .execute(&state.db)
        .await;
    match res {
        Ok(_) => redirect_with_flash(
            &format!("/applications/{id}/versions"),
            "Version deleted.",
        ),
        Err(e) => {
            tracing::error!(error = %e, "version delete failed");
            redirect_with_flash(
                &format!("/applications/{id}/versions"),
                "Database error.",
            )
        }
    }
}

// ----- v0.12 Tier-2: APK rollouts (canary / fleet / paused / rolled_back) ---

#[derive(Template)]
#[template(path = "application_rollouts.html")]
struct AppRolloutsTemplate {
    user_login: String,
    app_id: i64,
    package_name: String,
    rollouts: Vec<RolloutRow>,
    versions: Vec<AppVersionOption>,
    groups: Vec<GroupOption>,
    flash: Option<String>,
    error: Option<String>,
}

struct RolloutRow {
    id: i64,
    target_version_label: String,
    group_name: Option<String>,
    phase: String,
    canary_until_at: Option<String>,
    crash_threshold_pct: f64,
    created_at: String,
    rolled_back_at: Option<String>,
    rolled_back_reason: Option<String>,
    notes: Option<String>,
}

#[derive(sqlx::FromRow)]
struct RolloutRowRaw {
    id: i64,
    target_version_id: i64,
    target_version_code: i64,
    target_version_name: String,
    group_id: Option<i64>,
    group_name: Option<String>,
    phase: String,
    canary_until_at: Option<String>,
    crash_threshold_pct: f64,
    created_at: String,
    rolled_back_at: Option<String>,
    rolled_back_reason: Option<String>,
    notes: Option<String>,
}

async fn application_rollouts_view(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    flash: FlashCookie,
) -> Result<Response, ApiError> {
    render_app_rollouts(&user, &state, id, flash.0, None).await
}

async fn render_app_rollouts(
    user: &WebUser,
    state: &AppState,
    id: i64,
    flash: Option<String>,
    error: Option<String>,
) -> Result<Response, ApiError> {
    let pkg: Option<String> =
        sqlx::query_scalar("SELECT package_name FROM applications WHERE id = ? AND customer_id = ?")
            .bind(id)
            .bind(user.customer_id)
            .fetch_optional(&state.db)
            .await?;
    let Some(package_name) = pkg else {
        return Err(ApiError::NotFound);
    };
    let raw: Vec<RolloutRowRaw> = sqlx::query_as(
        "SELECT r.id, r.target_version_id, av.version_code AS target_version_code, \
                av.version_name AS target_version_name, \
                r.group_id, g.name AS group_name, \
                r.phase, r.canary_until_at, r.crash_threshold_pct, r.created_at, \
                r.rolled_back_at, r.rolled_back_reason, r.notes \
         FROM application_rollouts r \
         JOIN application_versions av ON av.id = r.target_version_id \
         LEFT JOIN groups g ON g.id = r.group_id \
         WHERE r.application_id = ? \
         ORDER BY r.created_at DESC LIMIT 100",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let rollouts: Vec<RolloutRow> = raw
        .into_iter()
        .map(|r| RolloutRow {
            id: r.id,
            target_version_label: format!(
                "{} (code {})",
                r.target_version_name, r.target_version_code
            ),
            group_name: r.group_name,
            phase: r.phase,
            canary_until_at: r.canary_until_at,
            crash_threshold_pct: r.crash_threshold_pct,
            created_at: state.fmt_ts(&r.created_at),
            rolled_back_at: r.rolled_back_at.map(|s| state.fmt_ts(&s)),
            rolled_back_reason: r.rolled_back_reason,
            notes: r.notes,
        })
        .collect();
    let versions: Vec<AppVersionOption> = sqlx::query_as(
        "SELECT av.id, \
                av.version_name || ' (code ' || av.version_code || ', sha ' || \
                substr(av.sha256, 1, 8) || '…)' AS label \
         FROM application_versions av \
         WHERE av.application_id = ? \
         ORDER BY av.version_code DESC LIMIT 200",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let groups: Vec<GroupOption> = sqlx::query_as(
        "SELECT id, name FROM groups WHERE customer_id = ? ORDER BY name LIMIT 200",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    let mut resp = render(AppRolloutsTemplate {
        user_login: user.login.clone(),
        app_id: id,
        package_name,
        rollouts,
        versions,
        groups,
        flash,
        error,
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

async fn application_rollout_create(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    // Verify the application belongs to this customer.
    let owned: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM applications WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?;
    if owned.is_none() {
        return Err(ApiError::NotFound);
    }
    let form = parse_form(&body);
    let target_version_id = form
        .first("target_version_id")
        .and_then(|s| s.parse::<i64>().ok());
    let Some(target_version_id) = target_version_id else {
        return render_app_rollouts(
            &user,
            &state,
            id,
            None,
            Some("Не выбрана target версия.".into()),
        )
        .await;
    };
    // Validate target version belongs to this application.
    let valid: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM application_versions WHERE id = ? AND application_id = ?",
    )
    .bind(target_version_id)
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    if valid.is_none() {
        return render_app_rollouts(
            &user,
            &state,
            id,
            None,
            Some("Версия не принадлежит этому приложению.".into()),
        )
        .await;
    }
    let group_id: Option<i64> = form
        .first("group_id")
        .and_then(|s| if s.is_empty() { None } else { s.parse::<i64>().ok() });
    let phase = if group_id.is_some() { "canary" } else { "fleet" };
    let canary_until_at: Option<String> = form
        .first("canary_until_at")
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
    let crash_threshold_pct: f64 = form
        .first("crash_threshold_pct")
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| (0.0..=100.0).contains(v))
        .unwrap_or(5.0);
    let notes: Option<String> = form
        .first("notes")
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());

    sqlx::query(
        "INSERT INTO application_rollouts \
            (application_id, target_version_id, group_id, phase, canary_until_at, \
             crash_threshold_pct, created_by, notes) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(target_version_id)
    .bind(group_id)
    .bind(phase)
    .bind(canary_until_at.as_deref())
    .bind(crash_threshold_pct)
    .bind(user.id)
    .bind(notes.as_deref())
    .execute(&state.db)
    .await?;
    Ok(redirect_with_flash(
        &format!("/applications/{id}/rollouts"),
        "Rollout создан.",
    ))
}

async fn application_rollout_phase(
    user: WebUser,
    State(state): State<AppState>,
    Path((id, rid)): Path<(i64, i64)>,
    body: axum::body::Bytes,
) -> Response {
    let form = parse_form(&body);
    let new_phase = form.first("phase").unwrap_or("");
    let valid_phase = matches!(new_phase, "canary" | "fleet" | "paused" | "rolled_back");
    if !valid_phase {
        return redirect_with_flash(
            &format!("/applications/{id}/rollouts"),
            "Недопустимая фаза.",
        );
    }
    // Verify rollout belongs to this customer.
    let owned: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM application_rollouts r \
         JOIN applications a ON a.id = r.application_id \
         WHERE r.id = ? AND a.id = ? AND a.customer_id = ?",
    )
    .bind(rid)
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);
    if owned.is_none() {
        return redirect_with_flash(
            &format!("/applications/{id}/rollouts"),
            "Rollout не найден.",
        );
    }
    let res = if new_phase == "rolled_back" {
        sqlx::query(
            "UPDATE application_rollouts SET phase = ?, updated_at = datetime('now'), \
                rolled_back_at = datetime('now'), rolled_back_reason = ? \
             WHERE id = ?",
        )
        .bind(new_phase)
        .bind("Manual rollback by admin")
        .bind(rid)
        .execute(&state.db)
        .await
    } else {
        sqlx::query(
            "UPDATE application_rollouts SET phase = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(new_phase)
        .bind(rid)
        .execute(&state.db)
        .await
    };
    match res {
        Ok(_) => redirect_with_flash(
            &format!("/applications/{id}/rollouts"),
            &format!("Rollout {rid} → {new_phase}"),
        ),
        Err(_) => redirect_with_flash(
            &format!("/applications/{id}/rollouts"),
            "Database error.",
        ),
    }
}

// ----- Configuration edit / delete / app linking ---------------------------

#[derive(Template)]
#[template(path = "configuration_edit.html")]
struct ConfigEditTemplate {
    user_login: String,
    config_id: i64,
    name: String,
    description: String,
    kiosk_package: String,
    is_active: bool,
    settings_json: String,
    assigned_apps: Vec<ConfigAppRow>,
    available_apps: Vec<AvailableAppOption>,
    flash: Option<String>,
    error: Option<String>,
}

#[derive(sqlx::FromRow)]
struct AvailableAppOption {
    id: i64,
    package_name: String,
}

struct ConfigAppRow {
    app_id: i64,
    package_name: String,
    pinned_version: String,
    mode: String,
}

#[derive(sqlx::FromRow)]
struct ConfigAppRowRaw {
    app_id: i64,
    package_name: String,
    pinned_version: Option<String>,
    mode: String,
}

async fn configuration_edit_view(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    flash: FlashCookie,
) -> Result<Response, ApiError> {
    render_config_edit(&user, &state, id, flash.0, None).await
}

async fn render_config_edit(
    user: &WebUser,
    state: &AppState,
    id: i64,
    flash: Option<String>,
    error: Option<String>,
) -> Result<Response, ApiError> {
    #[derive(sqlx::FromRow)]
    struct ConfigEditRaw {
        name: String,
        description: Option<String>,
        kiosk_package: Option<String>,
        is_active: bool,
        settings_json: String,
    }
    let row: Option<ConfigEditRaw> = sqlx::query_as::<_, ConfigEditRaw>(
        "SELECT name, description, kiosk_package, is_active, settings_json \
         FROM configurations WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(ConfigEditRaw {
        name,
        description,
        kiosk_package,
        is_active,
        settings_json,
    }) = row
    else {
        return Err(ApiError::NotFound);
    };
    let assigned_raw: Vec<ConfigAppRowRaw> = sqlx::query_as::<_, ConfigAppRowRaw>(
        "SELECT a.id AS app_id, a.package_name, v.version_name AS pinned_version, ca.mode \
         FROM configuration_applications ca \
         JOIN applications a ON a.id = ca.application_id \
         LEFT JOIN application_versions v ON v.id = ca.application_version_id \
         WHERE ca.configuration_id = ? ORDER BY a.package_name",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let assigned_apps = assigned_raw
        .into_iter()
        .map(|r| ConfigAppRow {
            app_id: r.app_id,
            package_name: r.package_name,
            pinned_version: r.pinned_version.unwrap_or_else(|| "(latest)".into()),
            mode: r.mode,
        })
        .collect();
    let available_apps: Vec<AvailableAppOption> = sqlx::query_as::<_, AvailableAppOption>(
        "SELECT id, package_name FROM applications \
         WHERE customer_id = ? \
           AND id NOT IN (SELECT application_id FROM configuration_applications WHERE configuration_id = ?) \
         ORDER BY package_name LIMIT 500",
    )
    .bind(user.customer_id)
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let mut resp = render(ConfigEditTemplate {
        user_login: user.login.clone(),
        config_id: id,
        name,
        description: description.unwrap_or_default(),
        kiosk_package: kiosk_package.unwrap_or_default(),
        is_active,
        settings_json,
        assigned_apps,
        available_apps,
        flash,
        error,
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

#[derive(Debug, Deserialize)]
struct ConfigEditForm {
    name: String,
    description: Option<String>,
    kiosk_package: Option<String>,
    is_active: Option<String>,
    settings_json: Option<String>,
}

async fn configuration_edit_post(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(req): Form<ConfigEditForm>,
) -> Result<Response, Response> {
    let name = req.name.trim();
    if name.is_empty() {
        return render_config_edit(&user, &state, id, None, Some("Название обязательно".into()))
            .await
            .map_err(|e| e.into_response());
    }
    let settings = req
        .settings_json
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("{}");
    if let Err(e) = serde_json::from_str::<serde_json::Value>(settings) {
        return render_config_edit(
            &user,
            &state,
            id,
            None,
            Some(format!("settings_json invalid: {e}")),
        )
        .await
        .map_err(|err| err.into_response());
    }
    let description = req
        .description
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let kiosk = req
        .kiosk_package
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let is_active: i64 = req
        .is_active
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(1);
    let res = sqlx::query(
        "UPDATE configurations SET name = ?, description = ?, kiosk_package = ?, is_active = ?, \
                                   settings_json = ?, updated_at = datetime('now') \
         WHERE id = ? AND customer_id = ?",
    )
    .bind(name)
    .bind(description)
    .bind(kiosk)
    .bind(is_active)
    .bind(settings)
    .bind(id)
    .bind(user.customer_id)
    .execute(&state.db)
    .await;
    match res {
        Ok(r) if r.rows_affected() > 0 => Ok(redirect_with_flash(
            &format!("/configurations/{id}/edit"),
            "Configuration updated.",
        )),
        Ok(_) => Err((StatusCode::NOT_FOUND, "Configuration not found").into_response()),
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => render_config_edit(
            &user,
            &state,
            id,
            None,
            Some(format!("Configuration '{name}' already exists")),
        )
        .await
        .map_err(|e| e.into_response()),
        Err(e) => {
            tracing::error!(error = %e, "config_edit_post failed");
            render_config_edit(&user, &state, id, None, Some("Database error".into()))
                .await
                .map_err(|e| e.into_response())
        }
    }
}

async fn configuration_delete(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Response {
    let res = sqlx::query("DELETE FROM configurations WHERE id = ? AND customer_id = ?")
        .bind(id)
        .bind(user.customer_id)
        .execute(&state.db)
        .await;
    match res {
        Ok(r) if r.rows_affected() > 0 => {
            redirect_with_flash("/configurations", "Configuration deleted.")
        }
        Ok(_) => redirect_with_flash("/configurations", "Configuration not found."),
        Err(e) => {
            tracing::error!(error = %e, "config_delete failed");
            redirect_with_flash("/configurations", "Database error.")
        }
    }
}

#[derive(Debug, Deserialize)]
struct ConfigAddAppForm {
    application_id: i64,
    mode: String,
}

async fn configuration_app_add(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(req): Form<ConfigAddAppForm>,
) -> Response {
    // Verify both belong to this tenant
    let pair: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM configurations c, applications a \
         WHERE c.id = ? AND a.id = ? AND c.customer_id = ? AND a.customer_id = ?",
    )
    .bind(id)
    .bind(req.application_id)
    .bind(user.customer_id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);
    if pair.is_none() {
        return redirect_with_flash(
            &format!("/configurations/{id}/edit"),
            "Configuration or application not found.",
        );
    }
    let res = sqlx::query(
        "INSERT INTO configuration_applications (configuration_id, application_id, mode) \
         VALUES (?, ?, ?)",
    )
    .bind(id)
    .bind(req.application_id)
    .bind(req.mode.trim())
    .execute(&state.db)
    .await;
    match res {
        Ok(_) => redirect_with_flash(
            &format!("/configurations/{id}/edit"),
            "Application added.",
        ),
        Err(e) => {
            tracing::error!(error = %e, "config_app_add failed");
            redirect_with_flash(
                &format!("/configurations/{id}/edit"),
                "Could not add application (already assigned?)",
            )
        }
    }
}

async fn configuration_app_remove(
    user: WebUser,
    State(state): State<AppState>,
    Path((id, app_id)): Path<(i64, i64)>,
) -> Response {
    let owned: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM configurations WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);
    if owned.is_none() {
        return redirect_with_flash(
            &format!("/configurations/{id}/edit"),
            "Configuration not found.",
        );
    }
    let _ = sqlx::query(
        "DELETE FROM configuration_applications WHERE configuration_id = ? AND application_id = ?",
    )
    .bind(id)
    .bind(app_id)
    .execute(&state.db)
    .await;
    redirect_with_flash(
        &format!("/configurations/{id}/edit"),
        "Application removed.",
    )
}

/// v0.18.8: пометить конфигурацию как `customers.default_configuration_id`.
/// Любая существующая default-конфига этого customer'а перестаёт быть
/// default'ной (default — это **single pointer на customer**, не
/// флаг per-config).
///
/// Existing devices с уже назначенной конфигурацией НЕ перенастраиваются —
/// admin может это сделать вручную через /devices/{id}/edit или групповым
/// SQL'ем. Меняется только поведение для **новых** enrollment'ов.
async fn configuration_make_default(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Response {
    // Verify ownership.
    let owned: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM configurations WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);
    if owned.is_none() {
        return redirect_with_flash("/configurations", "Configuration not found.");
    }
    let res = sqlx::query(
        "UPDATE customers SET default_configuration_id = ?, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .execute(&state.db)
    .await;
    match res {
        Ok(_) => redirect_with_flash("/configurations", "Default configuration updated."),
        Err(e) => {
            tracing::error!(error = %e, "configuration_make_default failed");
            redirect_with_flash("/configurations", "Database error.")
        }
    }
}

// ----- /files generic browser ---------------------------------------------

#[derive(Template)]
#[template(path = "files.html")]
struct FilesTemplate {
    user_login: String,
    total: i64,
    files: Vec<FileRow>,
    /// v0.18.12: устройства/группы для dropdown'а в bulk-distribute bar.
    /// Те же sources что в FileDistributeTemplate, но переиспользуем
    /// существующие structs.
    target_devices: Vec<DistributeDeviceOption>,
    target_groups: Vec<GroupOption>,
    flash: Option<String>,
    create_error: Option<String>,
}

struct FileRow {
    id: i64,
    original_name: String,
    kind: String,
    size_human: String,
    sha256_short: String,
    uploaded_at: String,
}

#[derive(sqlx::FromRow)]
struct FileRowRaw {
    id: i64,
    original_name: String,
    kind: String,
    file_size_bytes: i64,
    sha256: String,
    uploaded_at: String,
}

async fn files_page(
    user: WebUser,
    State(state): State<AppState>,
    flash: FlashCookie,
) -> Result<Response, ApiError> {
    render_files(&user, &state, flash.0, None).await
}

async fn render_files(
    user: &WebUser,
    state: &AppState,
    flash: Option<String>,
    create_error: Option<String>,
) -> Result<Response, ApiError> {
    let rows: Vec<FileRowRaw> = sqlx::query_as::<_, FileRowRaw>(
        "SELECT id, original_name, kind, file_size_bytes, sha256, uploaded_at \
         FROM uploaded_files WHERE customer_id = ? ORDER BY uploaded_at DESC LIMIT 500",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM uploaded_files WHERE customer_id = ?")
            .bind(user.customer_id)
            .fetch_one(&state.db)
            .await?;
    let files = rows
        .into_iter()
        .map(|r| FileRow {
            id: r.id,
            original_name: r.original_name,
            kind: r.kind,
            size_human: format_size(r.file_size_bytes),
            sha256_short: r.sha256.chars().take(12).collect(),
            uploaded_at: state.fmt_ts(&r.uploaded_at),
        })
        .collect();
    // v0.18.12: targets для bulk-distribute bar.
    let target_devices: Vec<DistributeDeviceOption> = sqlx::query_as(
        "SELECT id, serial, display_name FROM devices \
         WHERE customer_id = ? AND is_active = 1 \
         ORDER BY serial LIMIT 500",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let target_groups: Vec<GroupOption> = sqlx::query_as(
        "SELECT id, name FROM groups WHERE customer_id = ? ORDER BY name LIMIT 200",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let mut resp = render(FilesTemplate {
        user_login: user.login.clone(),
        total,
        files,
        target_devices,
        target_groups,
        flash,
        create_error,
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

async fn files_upload(
    user: WebUser,
    State(state): State<AppState>,
    multipart: Multipart,
) -> Response {
    match try_upload_files(&user, &state, multipart).await {
        Ok(0) => render_files(&user, &state, None, Some("Не было файлов в форме".into()))
            .await
            .unwrap_or_else(|_| {
                (StatusCode::INTERNAL_SERVER_ERROR, "render failed").into_response()
            }),
        Ok(1) => redirect_with_flash("/files", "Файл загружен."),
        Ok(n) => redirect_with_flash("/files", &format!("Загружено файлов: {n}.")),
        Err(msg) => render_files(&user, &state, None, Some(msg))
            .await
            .unwrap_or_else(|_| {
                (StatusCode::INTERNAL_SERVER_ERROR, "render failed").into_response()
            }),
    }
}

/// v0.18.12: multi-file upload. Принимает 1..N полей `file` в одном
/// multipart-запросе (HTML5 `<input multiple>` или drag-drop нескольких
/// файлов в dropzone). Каждый файл сохраняется в storage и получает
/// отдельную строку в `uploaded_files`. Поле `kind` (опционально) —
/// применяется ко всем файлам в batch'е.
///
/// Возвращает количество успешно сохранённых файлов. На первой ошибке
/// сохранения — откатываемся: ранее уже сохранённые в этом batch'е
/// файлы НЕ удаляются (admin может почистить через /files delete),
/// но в БД нужные строки также не появятся для уже-битого файла.
/// Это компромисс — атомарный rollback требует двухфазной операции
/// storage+DB.
async fn try_upload_files(
    user: &WebUser,
    state: &AppState,
    mut multipart: Multipart,
) -> Result<usize, String> {
    use sha2::{Digest, Sha256};
    // Сначала вычитываем потенциальный `kind` (если присутствует, применяется
    // ко всем file-частям batch'а). Потом обрабатываем все `file` parts по
    // мере поступления.
    let mut kind = "generic".to_string();
    let mut saved: usize = 0;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| format!("multipart: {e}"))?
    {
        match field.name().unwrap_or("") {
            "kind" => {
                if let Ok(v) = field.text().await {
                    if !v.trim().is_empty() {
                        kind = v;
                    }
                }
            }
            "file" => {
                let original_name = field
                    .file_name()
                    .map(|s| s.to_string())
                    .ok_or_else(|| "file part без filename".to_string())?;
                if original_name.trim().is_empty() {
                    // HTML5 multiple-input иногда шлёт пустой file part если
                    // юзер выбрал и потом снял выбор — игнорируем.
                    continue;
                }
                let content_type = field.content_type().map(|s| s.to_string());
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| format!("read part bytes: {e}"))?;
                if bytes.is_empty() {
                    // Пустой файл — пропускаем.
                    continue;
                }
                let extension = std::path::Path::new(&original_name)
                    .extension()
                    .and_then(|e| e.to_str());
                let stored =
                    crate::storage::write_bytes(state.app_files_dir.as_ref(), &bytes, extension)
                        .await
                        .map_err(|e| {
                            tracing::error!(error = %e, name = %original_name, "storage write failed");
                            format!("storage write failed для {original_name}")
                        })?;
                let mut hasher = Sha256::new();
                hasher.update(&bytes);
                let sha = hex::encode(hasher.finalize());
                sqlx::query(
                    "INSERT INTO uploaded_files \
                        (customer_id, file_path, original_name, content_type, file_size_bytes, sha256, kind, uploaded_by) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(user.customer_id)
                .bind(&stored.relative_path)
                .bind(&original_name)
                .bind(&content_type)
                .bind(bytes.len() as i64)
                .bind(&sha)
                .bind(&kind)
                .bind(user.id)
                .execute(&state.db)
                .await
                .map_err(|e| {
                    tracing::error!(error = %e, name = %original_name, "files insert failed");
                    format!("database error для {original_name}")
                })?;
                saved += 1;
            }
            _ => {}
        }
    }
    Ok(saved)
}

async fn files_delete(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Response {
    let res = sqlx::query("DELETE FROM uploaded_files WHERE id = ? AND customer_id = ?")
        .bind(id)
        .bind(user.customer_id)
        .execute(&state.db)
        .await;
    match res {
        Ok(r) if r.rows_affected() > 0 => redirect_with_flash("/files", "File deleted."),
        Ok(_) => redirect_with_flash("/files", "File not found."),
        Err(e) => {
            tracing::error!(error = %e, "files_delete failed");
            redirect_with_flash("/files", "Database error.")
        }
    }
}

// ----- /roles read-only ----------------------------------------------------

#[derive(Template)]
#[template(path = "roles.html")]
struct RolesTemplate {
    user_login: String,
    roles: Vec<RoleRow>,
    permissions: Vec<PermissionRow>,
}

struct RoleRow {
    name: String,
    description: String,
    is_super_admin: bool,
    permissions: Vec<String>,
    user_count: i64,
}

struct PermissionRow {
    name: String,
    description: String,
}

async fn roles_page(user: WebUser, State(state): State<AppState>) -> Result<Response, ApiError> {
    let role_rows: Vec<(i64, String, Option<String>, bool)> = sqlx::query_as(
        "SELECT id, name, description, is_super_admin FROM user_roles ORDER BY id",
    )
    .fetch_all(&state.db)
    .await?;
    let mut roles = Vec::with_capacity(role_rows.len());
    for (rid, name, description, is_super_admin) in role_rows {
        let perms: Vec<(String,)> = sqlx::query_as(
            "SELECT p.name FROM user_role_permissions rp \
             JOIN permissions p ON p.id = rp.permission_id \
             WHERE rp.role_id = ? ORDER BY p.name",
        )
        .bind(rid)
        .fetch_all(&state.db)
        .await?;
        let user_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM users WHERE role_id = ? AND customer_id = ?",
        )
        .bind(rid)
        .bind(user.customer_id)
        .fetch_one(&state.db)
        .await?;
        roles.push(RoleRow {
            name,
            description: description.unwrap_or_default(),
            is_super_admin,
            permissions: perms.into_iter().map(|t| t.0).collect(),
            user_count,
        });
    }
    let perm_rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT name, description FROM permissions ORDER BY name")
            .fetch_all(&state.db)
            .await?;
    let permissions = perm_rows
        .into_iter()
        .map(|(name, description)| PermissionRow {
            name,
            description: description.unwrap_or_default(),
        })
        .collect();
    Ok(render(RolesTemplate {
        user_login: user.login,
        roles,
        permissions,
    }))
}

// ----- /settings server-wide settings --------------------------------------

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsTemplate {
    user_login: String,
    enrollment_base_url: String,
    default_sync_interval: i64,
    max_upload_mb: i64,
    branding_display_name: String,
    /// v0.18.9: currently-selected IANA timezone name (e.g. "Europe/Moscow").
    current_timezone: String,
    /// v0.18.9: full IANA timezone list для dropdown. ~600 значений из
    /// chrono_tz::TZ_VARIANTS. Это много, но admin Settings — не страница
    /// под нагрузкой, render OK. Тип `String` (а не `&'static str`) ради
    /// Askama equality в template (`tz == current_timezone`) — PartialEq
    /// между &str и String не реализован.
    all_timezones: Vec<String>,
    /// v0.18.16: текущий формат datetime в виде id ('ru' / 'iso' / 'eu' / 'us').
    current_dt_format: String,
    /// Все варианты datetime-формата для dropdown'а.
    all_dt_formats: Vec<DateFormatOption>,
    raw_entries: Vec<SettingEntry>,
    flash: Option<String>,
    error: Option<String>,
    current_locale: &'static str,
}

struct DateFormatOption {
    id: String,
    label: String,
}

struct SettingEntry {
    key: String,
    value_json: String,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct SettingEntryRaw {
    key: String,
    value_json: String,
    updated_at: String,
}

async fn settings_page(
    user: WebUser,
    State(state): State<AppState>,
    flash: FlashCookie,
) -> Result<Response, ApiError> {
    render_settings(&user, &state, flash.0, None).await
}

async fn render_settings(
    user: &WebUser,
    state: &AppState,
    flash: Option<String>,
    error: Option<String>,
) -> Result<Response, ApiError> {
    let raw: Vec<SettingEntryRaw> = sqlx::query_as::<_, SettingEntryRaw>(
        "SELECT key, value_json, updated_at FROM settings ORDER BY key",
    )
    .fetch_all(&state.db)
    .await?;
    let mut enrollment_base_url = String::new();
    let mut default_sync_interval: i64 = 60;
    let mut max_upload_mb: i64 = 200;
    let mut branding_display_name = String::from("Outpost MDM");
    let mut current_timezone = String::from("Europe/Moscow");
    for r in &raw {
        match r.key.as_str() {
            "server.enrollment_base_url" => {
                enrollment_base_url = strip_json_quotes(&r.value_json);
            }
            "server.default_sync_interval" => {
                if let Ok(n) = r.value_json.trim().parse::<i64>() {
                    default_sync_interval = n;
                }
            }
            "server.max_upload_mb" => {
                if let Ok(n) = r.value_json.trim().parse::<i64>() {
                    max_upload_mb = n;
                }
            }
            "branding.display_name" => {
                branding_display_name = strip_json_quotes(&r.value_json);
            }
            "server.timezone" => {
                current_timezone = strip_json_quotes(&r.value_json);
            }
            _ => {}
        }
    }
    let raw_entries = raw
        .into_iter()
        .map(|r| SettingEntry {
            key: r.key,
            value_json: r.value_json,
            updated_at: state.fmt_ts(&r.updated_at),
        })
        .collect();
    // v0.18.9: TZ_VARIANTS — массив из 596 IANA timezone names в Rust 1.85
    // chrono-tz 0.10. Owned String (а не &'static str) для PartialEq с
    // current_timezone в Askama template.
    let all_timezones: Vec<String> = chrono_tz::TZ_VARIANTS
        .iter()
        .map(|tz| tz.name().to_string())
        .collect();
    let current_dt_format = state.dt_format().as_id().to_string();
    let all_dt_formats: Vec<DateFormatOption> = crate::state::DateFormat::all()
        .iter()
        .map(|f| DateFormatOption {
            id: f.as_id().to_string(),
            label: f.label().to_string(),
        })
        .collect();
    let mut resp = render(SettingsTemplate {
        user_login: user.login.clone(),
        enrollment_base_url,
        default_sync_interval,
        max_upload_mb,
        branding_display_name,
        current_timezone,
        all_timezones,
        current_dt_format,
        all_dt_formats,
        raw_entries,
        flash,
        error,
        current_locale: user.locale.code(),
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

fn strip_json_quotes(s: &str) -> String {
    let t = s.trim();
    if let Some(stripped) = t
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
    {
        stripped.to_string()
    } else {
        t.to_string()
    }
}

#[derive(Debug, Deserialize)]
struct SettingsForm {
    enrollment_base_url: Option<String>,
    default_sync_interval: Option<String>,
    max_upload_mb: Option<String>,
    branding_display_name: Option<String>,
    /// v0.18.9: IANA timezone (Europe/Moscow, UTC, …).
    timezone: Option<String>,
    /// v0.18.16: datetime format id ('ru' / 'iso' / 'eu' / 'us').
    datetime_format: Option<String>,
}

async fn settings_save(
    user: WebUser,
    State(state): State<AppState>,
    Form(req): Form<SettingsForm>,
) -> Result<Response, ApiError> {
    let mut tx = state.db.begin().await?;
    upsert_setting(
        &mut tx,
        "server.enrollment_base_url",
        &json_quote(req.enrollment_base_url.as_deref().unwrap_or("").trim()),
    )
    .await?;
    upsert_setting(
        &mut tx,
        "server.default_sync_interval",
        req.default_sync_interval
            .as_deref()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .map(|n| n.to_string())
            .unwrap_or_else(|| "60".to_string())
            .as_str(),
    )
    .await?;
    upsert_setting(
        &mut tx,
        "server.max_upload_mb",
        req.max_upload_mb
            .as_deref()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .map(|n| n.to_string())
            .unwrap_or_else(|| "200".to_string())
            .as_str(),
    )
    .await?;
    upsert_setting(
        &mut tx,
        "branding.display_name",
        &json_quote(req.branding_display_name.as_deref().unwrap_or("").trim()),
    )
    .await?;
    // v0.18.9: timezone — валидируем перед сохранением. Невалидное значение
    // (например 'кириллица' или typo) — flash error, ничего не меняем.
    let tz_input = req.timezone.as_deref().unwrap_or("").trim();
    let new_tz: chrono_tz::Tz = tz_input.parse().map_err(|_| {
        tracing::warn!(
            invalid_tz = %tz_input,
            "settings_save: tz парсинг fail — keeping previous"
        );
        ApiError::BadRequest(format!(
            "Часовой пояс «{tz_input}» не распознан. Должно быть IANA-имя, например Europe/Moscow."
        ))
    })?;
    upsert_setting(&mut tx, "server.timezone", &json_quote(tz_input)).await?;
    // v0.18.16: datetime format. Невалидный id → fallback на Ru с warning
    // (из DateFormat::from_id), что юзеру show-ит как «сохранено» с тем
    // что мы реально приняли.
    let dt_input = req.datetime_format.as_deref().unwrap_or("ru").trim();
    let new_dt = crate::state::DateFormat::from_id(dt_input);
    upsert_setting(
        &mut tx,
        "server.datetime_format",
        &json_quote(new_dt.as_id()),
    )
    .await?;
    tx.commit().await?;
    // Hot-reload tz + dt_format в AppState — admin UI сразу подхватит без restart'а.
    state.set_tz(new_tz);
    state.set_dt_format(new_dt);
    let _ = user;
    Ok(redirect_with_flash("/settings", "Settings saved."))
}

async fn upsert_setting<'a>(
    tx: &mut sqlx::Transaction<'a, sqlx::Sqlite>,
    key: &str,
    value_json: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO settings (key, value_json, updated_at) VALUES (?, ?, datetime('now')) \
         ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value_json)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn json_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

// ----- /profile self-edit --------------------------------------------------

#[derive(Template)]
#[template(path = "profile.html")]
struct ProfileTemplate {
    user_login: String,
    login: String,
    email: String,
    /// v0.18.16: новые поля профиля.
    display_name: String,
    comment: String,
    phone: String,
    tg: String,
    role_name: String,
    last_login_at: String,
    created_at: String,
    flash: Option<String>,
    error: Option<String>,
}

async fn profile_view(
    user: WebUser,
    State(state): State<AppState>,
    flash: FlashCookie,
) -> Result<Response, ApiError> {
    render_profile(&user, &state, flash.0, None).await
}

async fn render_profile(
    user: &WebUser,
    state: &AppState,
    flash: Option<String>,
    error: Option<String>,
) -> Result<Response, ApiError> {
    #[derive(sqlx::FromRow)]
    struct ProfileRaw {
        login: String,
        email: Option<String>,
        display_name: Option<String>,
        comment: Option<String>,
        phone: Option<String>,
        tg: Option<String>,
        role_name: String,
        last_login_at: Option<String>,
        created_at: String,
    }
    let row: Option<ProfileRaw> = sqlx::query_as::<_, ProfileRaw>(
        "SELECT u.login, u.email, u.display_name, u.comment, u.phone, u.tg, \
                r.name AS role_name, u.last_login_at, u.created_at \
         FROM users u JOIN user_roles r ON r.id = u.role_id WHERE u.id = ?",
    )
    .bind(user.id)
    .fetch_optional(&state.db)
    .await?;
    let Some(ProfileRaw {
        login,
        email,
        display_name,
        comment,
        phone,
        tg,
        role_name,
        last_login_at,
        created_at,
    }) = row
    else {
        return Err(ApiError::NotFound);
    };
    let mut resp = render(ProfileTemplate {
        user_login: user.login.clone(),
        login,
        email: email.unwrap_or_default(),
        display_name: display_name.unwrap_or_default(),
        comment: comment.unwrap_or_default(),
        phone: phone.unwrap_or_default(),
        tg: tg.unwrap_or_default(),
        role_name,
        last_login_at: last_login_at.as_deref().map(|s| state.fmt_ts(s)).unwrap_or_else(|| "—".into()),
        created_at: state.fmt_ts(&created_at),
        flash,
        error,
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

#[derive(Debug, Deserialize)]
struct ProfileForm {
    email: Option<String>,
    display_name: Option<String>,
    comment: Option<String>,
    phone: Option<String>,
    tg: Option<String>,
}

/// Normalize строку из form: trim, empty → None (хранится NULL в БД).
/// Также для tg отрезает ведущий `@` (юзер может ввести `@username` или
/// просто `username` — нормализуем к одному виду).
fn normalize_profile_field(raw: &Option<String>, strip_at: bool) -> Option<String> {
    let s = raw.as_deref()?.trim();
    if s.is_empty() {
        return None;
    }
    let s = if strip_at {
        s.strip_prefix('@').unwrap_or(s)
    } else {
        s
    };
    if s.is_empty() {
        return None;
    }
    Some(s.to_string())
}

async fn profile_save(
    user: WebUser,
    State(state): State<AppState>,
    Form(req): Form<ProfileForm>,
) -> Result<Response, ApiError> {
    let email = normalize_profile_field(&req.email, false);
    let display_name = normalize_profile_field(&req.display_name, false);
    let comment = normalize_profile_field(&req.comment, false);
    let phone = normalize_profile_field(&req.phone, false);
    let tg = normalize_profile_field(&req.tg, true);
    sqlx::query(
        "UPDATE users SET email = ?, display_name = ?, comment = ?, phone = ?, tg = ?, \
                          updated_at = datetime('now') WHERE id = ?",
    )
    .bind(email)
    .bind(display_name)
    .bind(comment)
    .bind(phone)
    .bind(tg)
    .bind(user.id)
    .execute(&state.db)
    .await?;
    Ok(redirect_with_flash("/profile", "Profile saved."))
}

// ----- shared formatters ---------------------------------------------------

fn format_size(bytes: i64) -> String {
    let b = bytes as f64;
    if bytes >= 1_073_741_824 {
        format!("{:.2} GiB", b / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MiB", b / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KiB", b / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

// =====================================================================
// Phase 22 — Telemetry UI (OTLP-backed; reads device_logs/_metrics/_traces)
// =====================================================================

#[derive(Template)]
#[template(path = "telemetry.html")]
struct TelemetryOverviewTemplate {
    user_login: String,
    active_devices: i64,
    logs_24h: i64,
    errors_24h: i64,
    metrics_24h: i64,
    traces_24h: i64,
    last_ingest: String,
    top_devices: Vec<TopDeviceRow>,
    recent_errors: Vec<RecentErrorRow>,
    top_metrics: Vec<TopMetricRow>,
    /// v0.18.1: внешний URL Grafana (если admin Web UI и Grafana живут
    /// на разных hostname'ах — например admin на public TLS, Grafana на
    /// tailscale-only FQDN). Берётся из `settings.server.grafana_base_url`;
    /// если ключ пустой/отсутствует, шаблон fallback'ится на относительный
    /// `/grafana/` (backwards compat).
    grafana_url: String,
}

struct TopDeviceRow {
    id: i64,
    serial: String,
    logs: i64,
    errors: i64,
    last_seen: String,
}

struct RecentErrorRow {
    ts: String,
    device_id: i64,
    serial: String,
    severity_text: String,
    body: String,
}

struct TopMetricRow {
    name: String,
    points: i64,
    devices: i64,
    latest: String,
}

async fn telemetry_overview(
    user: WebUser,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    let active_devices: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT device_id) FROM device_logs \
         WHERE customer_id = ? AND received_at >= datetime('now', '-1 day')",
    )
    .bind(user.customer_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    let logs_24h: i64 = scalar(
        &state,
        user.customer_id,
        "SELECT COUNT(*) FROM device_logs WHERE customer_id = ? AND received_at >= datetime('now', '-1 day')",
    )
    .await
    .unwrap_or(0);
    let errors_24h: i64 = scalar(
        &state,
        user.customer_id,
        "SELECT COUNT(*) FROM device_logs WHERE customer_id = ? AND severity_number >= 17 AND received_at >= datetime('now', '-1 day')",
    )
    .await
    .unwrap_or(0);
    let metrics_24h: i64 = scalar(
        &state,
        user.customer_id,
        "SELECT COUNT(*) FROM device_metrics WHERE customer_id = ? AND received_at >= datetime('now', '-1 day')",
    )
    .await
    .unwrap_or(0);
    let traces_24h: i64 = scalar(
        &state,
        user.customer_id,
        "SELECT COUNT(*) FROM device_traces WHERE customer_id = ? AND received_at >= datetime('now', '-1 day')",
    )
    .await
    .unwrap_or(0);
    let last_ingest: String = sqlx::query_scalar::<_, Option<String>>(
        "SELECT MAX(received_at) FROM device_logs WHERE customer_id = ?",
    )
    .bind(user.customer_id)
    .fetch_one(&state.db)
    .await
    .ok()
    .flatten()
    .map(|s| state.fmt_ts(&s))
    .unwrap_or_else(|| "—".into());

    #[derive(sqlx::FromRow)]
    struct TopDevRaw {
        id: i64,
        serial: String,
        logs: i64,
        errors: i64,
        last_seen: Option<String>,
    }
    let top_raw: Vec<TopDevRaw> = sqlx::query_as::<_, TopDevRaw>(
        "SELECT d.id, d.serial, \
                COUNT(l.id) AS logs, \
                SUM(CASE WHEN l.severity_number >= 17 THEN 1 ELSE 0 END) AS errors, \
                MAX(l.received_at) AS last_seen \
         FROM devices d \
         LEFT JOIN device_logs l ON l.device_id = d.id AND l.received_at >= datetime('now', '-1 day') \
         WHERE d.customer_id = ? \
         GROUP BY d.id \
         ORDER BY logs DESC, d.id DESC \
         LIMIT 10",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let top_devices: Vec<TopDeviceRow> = top_raw
        .into_iter()
        .filter(|r| r.logs > 0)
        .map(|r| TopDeviceRow {
            id: r.id,
            serial: r.serial,
            logs: r.logs,
            errors: r.errors,
            last_seen: r.last_seen.as_deref().map(|s| state.fmt_ts(s)).unwrap_or_else(|| "—".into()),
        })
        .collect();

    #[derive(sqlx::FromRow)]
    struct ErrRaw {
        ts: String,
        device_id: i64,
        serial: String,
        severity_text: String,
        body: String,
    }
    let err_raw: Vec<ErrRaw> = sqlx::query_as::<_, ErrRaw>(
        "SELECT l.ts, l.device_id, d.serial, l.severity_text, l.body \
         FROM device_logs l JOIN devices d ON d.id = l.device_id \
         WHERE l.customer_id = ? AND l.severity_number >= 17 \
         ORDER BY l.id DESC LIMIT 20",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let recent_errors: Vec<RecentErrorRow> = err_raw
        .into_iter()
        .map(|r| RecentErrorRow {
            ts: state.fmt_ts(&r.ts),
            device_id: r.device_id,
            serial: r.serial,
            severity_text: r.severity_text,
            body: trim_to(&r.body, 80),
        })
        .collect();

    #[derive(sqlx::FromRow)]
    struct TopMRaw {
        name: String,
        points: i64,
        devices: i64,
        latest: Option<f64>,
    }
    let metric_raw: Vec<TopMRaw> = sqlx::query_as::<_, TopMRaw>(
        "SELECT name, COUNT(*) AS points, COUNT(DISTINCT device_id) AS devices, \
                (SELECT value FROM device_metrics m2 WHERE m2.name = m1.name AND m2.customer_id = m1.customer_id ORDER BY m2.id DESC LIMIT 1) AS latest \
         FROM device_metrics m1 \
         WHERE customer_id = ? AND received_at >= datetime('now', '-1 day') \
         GROUP BY name \
         ORDER BY points DESC \
         LIMIT 20",
    )
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let top_metrics: Vec<TopMetricRow> = metric_raw
        .into_iter()
        .map(|r| TopMetricRow {
            name: r.name,
            points: r.points,
            devices: r.devices,
            latest: r
                .latest
                .map(|v| format!("{v}"))
                .unwrap_or_else(|| "—".into()),
        })
        .collect();

    // v0.18.1: pull external Grafana URL from settings; relative fallback.
    let grafana_url: String = sqlx::query_scalar(
        "SELECT json_extract(value_json, '$') FROM settings WHERE key = 'server.grafana_base_url'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .flatten()
    .filter(|s: &String| !s.is_empty())
    .unwrap_or_else(|| "/grafana/".to_string());

    Ok(render(TelemetryOverviewTemplate {
        user_login: user.login,
        active_devices,
        logs_24h,
        errors_24h,
        metrics_24h,
        traces_24h,
        last_ingest,
        top_devices,
        recent_errors,
        top_metrics,
        grafana_url,
    }))
}

#[derive(Template)]
#[template(path = "device_telemetry.html")]
struct DeviceTelemetryTemplate {
    user_login: String,
    device_id: i64,
    serial: String,
    counts: DeviceCounts,
    latest_metrics: Vec<DeviceMetricRow>,
    recent_spans: Vec<DeviceSpanRow>,
    recent_logs: Vec<DeviceLogRow>,
}

struct DeviceCounts {
    logs_24h: i64,
    errors_24h: i64,
    metrics_24h: i64,
    traces_24h: i64,
    last_ingest: String,
}

struct DeviceMetricRow {
    name: String,
    value: String,
    unit: String,
    ts: String,
}

struct DeviceSpanRow {
    name: String,
    duration_ms: i64,
    status_code: i64,
    start_ts: String,
}

struct DeviceLogRow {
    ts: String,
    severity_number: i64,
    severity_text: String,
    /// Full body (capped at 8 KB чтобы HTML страница оставалась bounded).
    body: String,
    /// v0.18.7: char-safe preview ~200 символов для <details> summary.
    /// Раньше template делал `l.body[..200]` — byte-slice через
    /// границу UTF-8 character'а паниковал на кириллице
    /// (см. 2026-05-19 panic loop на /devices/9/telemetry, где
    /// chat.response с русским текстом крашил сервер). Теперь
    /// preview формируется в Rust через `trim_to` который режет
    /// по char-границам.
    body_preview: String,
    /// Full attrs JSON (untrimmed — обычно небольшой).
    attrs_full: String,
    /// Char-safe preview ~100 символов для attrs.
    attrs_preview: String,
}

async fn device_telemetry_view(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    let serial: Option<String> =
        sqlx::query_scalar("SELECT serial FROM devices WHERE id = ? AND customer_id = ?")
            .bind(id)
            .bind(user.customer_id)
            .fetch_optional(&state.db)
            .await?;
    let Some(serial) = serial else {
        return Err(ApiError::NotFound);
    };

    let logs_24h: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM device_logs WHERE device_id = ? AND received_at >= datetime('now', '-1 day')",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    let errors_24h: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM device_logs WHERE device_id = ? AND severity_number >= 17 AND received_at >= datetime('now', '-1 day')",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    let metrics_24h: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM device_metrics WHERE device_id = ? AND received_at >= datetime('now', '-1 day')",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    let traces_24h: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM device_traces WHERE device_id = ? AND received_at >= datetime('now', '-1 day')",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    let last_ingest: String = sqlx::query_scalar::<_, Option<String>>(
        "SELECT MAX(received_at) FROM device_logs WHERE device_id = ?",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await
    .ok()
    .flatten()
    .map(|s| state.fmt_ts(&s))
    .unwrap_or_else(|| "—".into());

    #[derive(sqlx::FromRow)]
    struct MRaw {
        name: String,
        value: f64,
        unit: Option<String>,
        ts: String,
    }
    let m_raw: Vec<MRaw> = sqlx::query_as::<_, MRaw>(
        "SELECT name, value, unit, ts FROM device_metrics WHERE device_id = ? \
         AND id IN (SELECT MAX(id) FROM device_metrics WHERE device_id = ? GROUP BY name) \
         ORDER BY name LIMIT 40",
    )
    .bind(id)
    .bind(id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let latest_metrics: Vec<DeviceMetricRow> = m_raw
        .into_iter()
        .map(|r| DeviceMetricRow {
            name: r.name,
            value: format!("{}", r.value),
            unit: r.unit.unwrap_or_else(|| "".into()),
            ts: state.fmt_ts(&r.ts),
        })
        .collect();

    #[derive(sqlx::FromRow)]
    struct SRaw {
        name: String,
        duration_ms: i64,
        status_code: i64,
        start_ts: String,
    }
    let s_raw: Vec<SRaw> = sqlx::query_as::<_, SRaw>(
        "SELECT name, duration_ms, status_code, start_ts FROM device_traces \
         WHERE device_id = ? ORDER BY id DESC LIMIT 20",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let recent_spans: Vec<DeviceSpanRow> = s_raw
        .into_iter()
        .map(|r| DeviceSpanRow {
            name: r.name,
            duration_ms: r.duration_ms,
            status_code: r.status_code,
            start_ts: state.fmt_ts(&r.start_ts),
        })
        .collect();

    #[derive(sqlx::FromRow)]
    struct LRaw {
        ts: String,
        severity_number: i64,
        severity_text: String,
        body: String,
        attrs_json: String,
    }
    let l_raw: Vec<LRaw> = sqlx::query_as::<_, LRaw>(
        "SELECT ts, severity_number, severity_text, body, attrs_json \
         FROM device_logs WHERE device_id = ? ORDER BY id DESC LIMIT 20",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    // v0.18.7: per CLIENT-TELEMETRY-CONTRACT.md client sends full prompts
    // and full LLM responses in `body` (beta mode). Cap full body at 8 KB
    // чтобы HTML страница оставалась bounded. Preview-поля формируются
    // через trim_to (char-safe) — раньше template делал byte-slice
    // `l.body[..200]` и панически крашил на кириллических границах.
    let recent_logs: Vec<DeviceLogRow> = l_raw
        .into_iter()
        .map(|r| {
            let full_body = trim_to(&r.body, 8192);
            let body_preview = trim_to(&r.body, 200);
            let attrs_preview = trim_to(&r.attrs_json, 100);
            DeviceLogRow {
                ts: state.fmt_ts(&r.ts),
                severity_number: r.severity_number,
                severity_text: r.severity_text,
                body: full_body,
                body_preview,
                attrs_full: r.attrs_json,
                attrs_preview,
            }
        })
        .collect();

    Ok(render(DeviceTelemetryTemplate {
        user_login: user.login,
        device_id: id,
        serial,
        counts: DeviceCounts {
            logs_24h,
            errors_24h,
            metrics_24h,
            traces_24h,
            last_ingest,
        },
        latest_metrics,
        recent_spans,
        recent_logs,
    }))
}

#[derive(Template)]
#[template(path = "device_logs.html")]
struct DeviceLogsTemplate {
    user_login: String,
    device_id: i64,
    serial: String,
    total: i64,
    logs: Vec<DeviceLogStreamRow>,
    min_severity: i64,
    q: String,
    since: String,
    limit: i64,
}

struct DeviceLogStreamRow {
    ts: String,
    severity_number: i64,
    severity_text: String,
    body: String,
    attrs_preview: String,
    trace_short: String,
}

#[derive(Debug, Deserialize)]
struct LogsFilter {
    min_severity: Option<i64>,
    q: Option<String>,
    since: Option<String>,
    limit: Option<i64>,
}

async fn device_logs_view(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    axum::extract::Query(filter): axum::extract::Query<LogsFilter>,
) -> Result<Response, ApiError> {
    let serial: Option<String> =
        sqlx::query_scalar("SELECT serial FROM devices WHERE id = ? AND customer_id = ?")
            .bind(id)
            .bind(user.customer_id)
            .fetch_optional(&state.db)
            .await?;
    let Some(serial) = serial else {
        return Err(ApiError::NotFound);
    };

    let min_severity = filter.min_severity.unwrap_or(1).clamp(1, 24);
    let since = filter
        .since
        .as_deref()
        .map(str::to_string)
        .unwrap_or_else(|| "24h".to_string());
    let since_sql = match since.as_str() {
        "1h" => "-1 hours",
        "6h" => "-6 hours",
        "7d" => "-7 days",
        "30d" => "-30 days",
        _ => "-1 days",
    };
    let q = filter.q.as_deref().unwrap_or("").trim().to_string();
    let limit = filter.limit.unwrap_or(200).clamp(10, 1000);
    let like = if q.is_empty() {
        "%".to_string()
    } else {
        format!("%{q}%")
    };

    #[derive(sqlx::FromRow)]
    struct StreamRaw {
        ts: String,
        severity_number: i64,
        severity_text: String,
        body: String,
        attrs_json: String,
        trace_id: Option<String>,
    }
    let stream: Vec<StreamRaw> = sqlx::query_as::<_, StreamRaw>(
        &format!(
            "SELECT ts, severity_number, severity_text, body, attrs_json, trace_id \
             FROM device_logs \
             WHERE device_id = ? \
               AND severity_number >= ? \
               AND received_at >= datetime('now', ?) \
               AND body LIKE ? \
             ORDER BY id DESC LIMIT {limit}"
        ),
    )
    .bind(id)
    .bind(min_severity)
    .bind(since_sql)
    .bind(&like)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM device_logs \
         WHERE device_id = ? AND severity_number >= ? \
           AND received_at >= datetime('now', ?) \
           AND body LIKE ?",
    )
    .bind(id)
    .bind(min_severity)
    .bind(since_sql)
    .bind(&like)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    let logs: Vec<DeviceLogStreamRow> = stream
        .into_iter()
        .map(|r| DeviceLogStreamRow {
            ts: state.fmt_ts(&r.ts),
            severity_number: r.severity_number,
            severity_text: r.severity_text,
            body: trim_to(&r.body, 500),
            attrs_preview: trim_to(&r.attrs_json, 200),
            trace_short: r
                .trace_id
                .as_deref()
                .map(|t| t.chars().take(12).collect::<String>())
                .unwrap_or_default(),
        })
        .collect();

    Ok(render(DeviceLogsTemplate {
        user_login: user.login,
        device_id: id,
        serial,
        total,
        logs,
        min_severity,
        q,
        since,
        limit,
    }))
}

fn trim_to(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}…")
    }
}

// =====================================================================
// Phase 23 — Customer / 2FA / Signup
// =====================================================================

// ----- /customers (super-admin only) ---------------------------------------

#[derive(Template)]
#[template(path = "customers.html")]
struct CustomersTemplate {
    user_login: String,
    total: i64,
    customers: Vec<CustomerListRow>,
    flash: Option<String>,
    create_error: Option<String>,
}

struct CustomerListRow {
    id: i64,
    name: String,
    kind: String,
    is_active: bool,
    device_count: i64,
    user_count: i64,
    created_at: String,
}

#[derive(sqlx::FromRow)]
struct CustomerListRaw {
    id: i64,
    name: String,
    kind: String,
    is_active: bool,
    device_count: i64,
    user_count: i64,
    created_at: String,
}

async fn customers_page(
    user: WebUser,
    State(state): State<AppState>,
    flash: FlashCookie,
) -> Result<Response, Response> {
    user.require_super_admin()?;
    render_customers(&user, &state, flash.0, None)
        .await
        .map_err(|e| e.into_response())
}

async fn render_customers(
    user: &WebUser,
    state: &AppState,
    flash: Option<String>,
    create_error: Option<String>,
) -> Result<Response, ApiError> {
    let rows: Vec<CustomerListRaw> = sqlx::query_as::<_, CustomerListRaw>(
        "SELECT c.id, c.name, c.kind, c.is_active, \
                (SELECT COUNT(*) FROM devices d WHERE d.customer_id = c.id) AS device_count, \
                (SELECT COUNT(*) FROM users  u WHERE u.customer_id = c.id) AS user_count, \
                c.created_at \
         FROM customers c ORDER BY c.id LIMIT 500",
    )
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM customers")
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
    let customers = rows
        .into_iter()
        .map(|r| CustomerListRow {
            id: r.id,
            name: r.name,
            kind: r.kind,
            is_active: r.is_active,
            device_count: r.device_count,
            user_count: r.user_count,
            created_at: state.fmt_ts(&r.created_at),
        })
        .collect();
    let mut resp = render(CustomersTemplate {
        user_login: user.login.clone(),
        total,
        customers,
        flash,
        create_error,
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

#[derive(Debug, Deserialize)]
struct NewCustomerForm {
    name: String,
    description: Option<String>,
    kind: Option<String>,
}

async fn customers_create(
    user: WebUser,
    State(state): State<AppState>,
    Form(req): Form<NewCustomerForm>,
) -> Result<Response, Response> {
    user.require_super_admin()?;
    let name = req.name.trim();
    if name.is_empty() {
        return render_customers(&user, &state, None, Some("Название обязательно".into()))
            .await
            .map_err(|e| e.into_response());
    }
    let description = req
        .description
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let kind = req
        .kind
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| matches!(*s, "production" | "demo" | "test"))
        .unwrap_or("production");
    let res = sqlx::query(
        "INSERT INTO customers (name, description, kind) VALUES (?, ?, ?)",
    )
    .bind(name)
    .bind(description)
    .bind(kind)
    .execute(&state.db)
    .await;
    match res {
        Ok(_) => Ok(redirect_with_flash("/customers", "Customer created.")),
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => render_customers(
            &user,
            &state,
            None,
            Some(format!("Customer '{name}' already exists")),
        )
        .await
        .map_err(|e| e.into_response()),
        Err(e) => {
            tracing::error!(error = %e, "customers_create insert failed");
            render_customers(&user, &state, None, Some("Database error".into()))
                .await
                .map_err(|e| e.into_response())
        }
    }
}

#[derive(Template)]
#[template(path = "customer_edit.html")]
struct CustomerEditTemplate {
    user_login: String,
    customer_id: i64,
    name: String,
    description: String,
    metadata_json: String,
    kind_options: Vec<(&'static str, bool)>,
    device_count: i64,
    user_count: i64,
    flash: Option<String>,
    error: Option<String>,
}

const CUSTOMER_KINDS: &[&str] = &["production", "demo", "test"];

async fn customer_edit_view(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    flash: FlashCookie,
) -> Result<Response, Response> {
    user.require_super_admin()?;
    render_customer_edit(&user, &state, id, flash.0, None)
        .await
        .map_err(|e| e.into_response())
}

async fn render_customer_edit(
    user: &WebUser,
    state: &AppState,
    id: i64,
    flash: Option<String>,
    error: Option<String>,
) -> Result<Response, ApiError> {
    #[derive(sqlx::FromRow)]
    struct Row {
        name: String,
        description: Option<String>,
        kind: String,
        metadata_json: String,
    }
    let row: Option<Row> = sqlx::query_as::<_, Row>(
        "SELECT name, description, kind, metadata_json FROM customers WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    let Some(Row {
        name,
        description,
        kind,
        metadata_json,
    }) = row
    else {
        return Err(ApiError::NotFound);
    };
    let device_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM devices WHERE customer_id = ?")
        .bind(id)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
    let user_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE customer_id = ?")
        .bind(id)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
    let kind_options = CUSTOMER_KINDS.iter().map(|k| (*k, *k == kind.as_str())).collect();
    let mut resp = render(CustomerEditTemplate {
        user_login: user.login.clone(),
        customer_id: id,
        name,
        description: description.unwrap_or_default(),
        metadata_json,
        kind_options,
        device_count,
        user_count,
        flash,
        error,
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

#[derive(Debug, Deserialize)]
struct CustomerEditForm {
    name: String,
    description: Option<String>,
    kind: String,
    metadata_json: Option<String>,
}

async fn customer_edit_post(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(req): Form<CustomerEditForm>,
) -> Result<Response, Response> {
    user.require_super_admin()?;
    let name = req.name.trim();
    if name.is_empty() {
        return render_customer_edit(&user, &state, id, None, Some("Название обязательно".into()))
            .await
            .map_err(|e| e.into_response());
    }
    let kind = req.kind.trim();
    if !CUSTOMER_KINDS.contains(&kind) {
        return render_customer_edit(&user, &state, id, None, Some("Unknown kind".into()))
            .await
            .map_err(|e| e.into_response());
    }
    let metadata = req
        .metadata_json
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("{}");
    if let Err(e) = serde_json::from_str::<serde_json::Value>(metadata) {
        return render_customer_edit(
            &user,
            &state,
            id,
            None,
            Some(format!("metadata_json invalid: {e}")),
        )
        .await
        .map_err(|err| err.into_response());
    }
    let description = req
        .description
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let res = sqlx::query(
        "UPDATE customers SET name = ?, description = ?, kind = ?, metadata_json = ?, \
                              updated_at = datetime('now') WHERE id = ?",
    )
    .bind(name)
    .bind(description)
    .bind(kind)
    .bind(metadata)
    .bind(id)
    .execute(&state.db)
    .await;
    match res {
        Ok(r) if r.rows_affected() > 0 => Ok(redirect_with_flash(
            &format!("/customers/{id}/edit"),
            "Customer updated.",
        )),
        Ok(_) => Err((StatusCode::NOT_FOUND, "Not found").into_response()),
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => render_customer_edit(
            &user,
            &state,
            id,
            None,
            Some(format!("Customer '{name}' already exists")),
        )
        .await
        .map_err(|e| e.into_response()),
        Err(e) => {
            tracing::error!(error = %e, "customer_edit_post failed");
            render_customer_edit(&user, &state, id, None, Some("Database error".into()))
                .await
                .map_err(|e| e.into_response())
        }
    }
}

async fn customer_toggle_active(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, Response> {
    user.require_super_admin()?;
    // Don't allow disabling the customer the super-admin is logged into.
    if id == user.home_customer_id {
        return Ok(redirect_with_flash(
            "/customers",
            "Cannot disable your own home tenant.",
        ));
    }
    let res = sqlx::query(
        "UPDATE customers SET is_active = 1 - is_active, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(id)
    .execute(&state.db)
    .await;
    Ok(match res {
        Ok(r) if r.rows_affected() > 0 => {
            redirect_with_flash("/customers", "Customer status toggled.")
        }
        Ok(_) => redirect_with_flash("/customers", "Customer not found."),
        Err(e) => {
            tracing::error!(error = %e, "customer_toggle_active failed");
            redirect_with_flash("/customers", "Database error.")
        }
    })
}

async fn customer_switch(
    user: WebUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, Response> {
    user.require_super_admin()?;
    // Verify the customer exists and is active.
    let exists: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM customers WHERE id = ? AND is_active = 1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    if exists.is_none() {
        return Ok(redirect_with_flash(
            "/customers",
            "Customer not found or disabled.",
        ));
    }
    let mut resp = Redirect::to("/dashboard").into_response();
    // 24 hour Max-Age, scoped to /. SameSite=Lax so following a link from
    // /customers picks it up immediately.
    let cookie = format!(
        "outpost_acting={id}; Path=/; HttpOnly; SameSite=Lax{}; Max-Age=86400",
        if state.secure_cookies { "; Secure" } else { "" },
    );
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().append(header::SET_COOKIE, v);
    }
    set_flash_cookie(
        &mut resp,
        &format!("Now acting as customer #{id}"),
    );
    Ok(resp)
}

// ----- /me/2fa (TOTP setup) ------------------------------------------------

#[derive(Template)]
#[template(path = "me_2fa.html")]
struct Me2faTemplate {
    user_login: String,
    totp_enabled: bool,
    setup_secret: Option<String>,
    qr_svg: String,
    flash: Option<String>,
    error: Option<String>,
    recovery_codes: Option<Vec<String>>,
}

async fn me_2fa_view(
    user: WebUser,
    State(state): State<AppState>,
    flash: FlashCookie,
) -> Result<Response, ApiError> {
    let row: Option<(i64, Option<String>)> = sqlx::query_as(
        "SELECT totp_enabled, totp_secret FROM users WHERE id = ?",
    )
    .bind(user.id)
    .fetch_optional(&state.db)
    .await?;
    let (totp_enabled, current_secret) = row.unwrap_or((0, None));
    // If totp is NOT enabled but a secret exists, we're in mid-setup —
    // show the QR for that secret. If enabled, hide the secret.
    let (setup_secret, qr_svg) = if totp_enabled == 0 {
        match current_secret {
            Some(s) => {
                let uri = crate::totp::otpauth_uri(&s, "Outpost MDM", &user.login);
                (Some(s), qrcode_svg(&uri))
            }
            None => (None, String::new()),
        }
    } else {
        (None, String::new())
    };
    let mut resp = render(Me2faTemplate {
        user_login: user.login,
        totp_enabled: totp_enabled != 0,
        setup_secret,
        qr_svg,
        flash: flash.0,
        error: None,
        recovery_codes: None,
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

async fn me_2fa_setup(
    user: WebUser,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    // Fresh secret — overrides any previous half-enrolled secret. Doesn't
    // touch totp_enabled (still 0 until /verify succeeds).
    let secret = crate::totp::generate_secret();
    sqlx::query("UPDATE users SET totp_secret = ?, totp_enabled = 0 WHERE id = ?")
        .bind(&secret)
        .bind(user.id)
        .execute(&state.db)
        .await?;
    Ok(Redirect::to("/me/2fa").into_response())
}

#[derive(Debug, Deserialize)]
struct Me2faVerifyForm {
    code: String,
}

async fn me_2fa_verify(
    user: WebUser,
    State(state): State<AppState>,
    Form(req): Form<Me2faVerifyForm>,
) -> Result<Response, ApiError> {
    let secret: Option<String> =
        sqlx::query_scalar("SELECT totp_secret FROM users WHERE id = ?")
            .bind(user.id)
            .fetch_optional(&state.db)
            .await?
            .flatten();
    let Some(secret) = secret else {
        return Ok(Redirect::to("/me/2fa").into_response());
    };
    if !crate::totp::verify(&secret, req.code.trim()) {
        // Re-render with error.
        let uri = crate::totp::otpauth_uri(&secret, "Outpost MDM", &user.login);
        let qr = qrcode_svg(&uri);
        let mut resp = render(Me2faTemplate {
            user_login: user.login,
            totp_enabled: false,
            setup_secret: Some(secret),
            qr_svg: qr,
            flash: None,
            error: Some("Code did not match. Try again — codes change every 30 s.".into()),
            recovery_codes: None,
        });
        clear_flash_cookie(&mut resp);
        return Ok(resp);
    }
    // Generate 10 single-use recovery codes; show them once.
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE users SET totp_enabled = 1, updated_at = datetime('now') WHERE id = ?")
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM totp_recovery_codes WHERE user_id = ?")
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    let mut plain_codes = Vec::with_capacity(10);
    for _ in 0..10 {
        let code = generate_recovery_code();
        let hash = crypto::hash_password(&code).map_err(|_| ApiError::Internal)?;
        sqlx::query(
            "INSERT INTO totp_recovery_codes (user_id, code_hash) VALUES (?, ?)",
        )
        .bind(user.id)
        .bind(&hash)
        .execute(&mut *tx)
        .await?;
        plain_codes.push(code);
    }
    tx.commit().await?;
    let mut resp = render(Me2faTemplate {
        user_login: user.login,
        totp_enabled: true,
        setup_secret: None,
        qr_svg: String::new(),
        flash: Some("2FA enabled. Save the recovery codes shown below — they will not be displayed again.".into()),
        error: None,
        recovery_codes: Some(plain_codes),
    });
    clear_flash_cookie(&mut resp);
    Ok(resp)
}

async fn me_2fa_cancel(
    user: WebUser,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    sqlx::query("UPDATE users SET totp_secret = NULL, totp_enabled = 0 WHERE id = ? AND totp_enabled = 0")
        .bind(user.id)
        .execute(&state.db)
        .await?;
    Ok(redirect_with_flash("/me/2fa", "Setup cancelled."))
}

async fn me_2fa_disable(
    user: WebUser,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE users SET totp_secret = NULL, totp_enabled = 0 WHERE id = ?")
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM totp_recovery_codes WHERE user_id = ?")
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(redirect_with_flash("/me/2fa", "2FA disabled."))
}

fn generate_recovery_code() -> String {
    // Human-readable: 4 groups of 4 alphanumerics, dash-separated.
    let mut out = String::with_capacity(19);
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let alphabet: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    for g in 0..4 {
        for _ in 0..4 {
            let idx = rng.gen_range(0..alphabet.len());
            out.push(alphabet[idx] as char);
        }
        if g < 3 {
            out.push('-');
        }
    }
    out
}

// ----- /login/2fa (second factor) ------------------------------------------

#[derive(Template)]
#[template(path = "login_2fa.html")]
struct Login2faTemplate {
    pending_token: String,
    error: Option<String>,
}

/// Extractor that reads `outpost_pending_2fa` cookie and verifies the
/// session kind is `pending_2fa`. Returns the pending Session info.
struct Pending2fa(session::Session);

impl FromRequestParts<AppState> for Pending2fa {
    type Rejection = Redirect;
    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = read_cookie(parts, "outpost_pending_2fa")
            .ok_or_else(|| Redirect::to("/login"))?;
        let s = session::verify(&token, &state.db)
            .await
            .map_err(|_| Redirect::to("/login"))?;
        if s.kind != session::KIND_PENDING_2FA {
            return Err(Redirect::to("/login"));
        }
        Ok(Pending2fa(s))
    }
}

async fn login_2fa_page(pending: Pending2fa) -> Response {
    // No flash-cookie clear here — we still need the pending-2FA cookie.
    render(Login2faTemplate {
        pending_token: pending.0.id_hash.clone(),
        error: None,
    })
}

#[derive(Debug, Deserialize)]
struct Login2faForm {
    code: Option<String>,
    recovery_code: Option<String>,
}

async fn login_2fa_submit(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    pending: Pending2fa,
    Form(req): Form<Login2faForm>,
) -> Response {
    if !state.login_limiter.try_take(ip) {
        return render(Login2faTemplate {
            pending_token: pending.0.id_hash.clone(),
            error: Some("Too many attempts. Try again in a moment.".into()),
        });
    }
    let user_id = pending.0.subject_id;
    let secret: Option<String> =
        sqlx::query_scalar("SELECT totp_secret FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
            .flatten();
    let Some(secret) = secret else {
        return render(Login2faTemplate {
            pending_token: pending.0.id_hash.clone(),
            error: Some("2FA is not set up on this account. Sign in again.".into()),
        });
    };

    let mut ok = false;
    let code = req.code.as_deref().unwrap_or("").trim();
    if !code.is_empty() && crate::totp::verify(&secret, code) {
        ok = true;
    }
    if !ok {
        let recovery = req.recovery_code.as_deref().unwrap_or("").trim();
        if !recovery.is_empty() {
            ok = consume_recovery_code(&state, user_id, recovery)
                .await
                .unwrap_or(false);
        }
    }

    if !ok {
        return render(Login2faTemplate {
            pending_token: pending.0.id_hash.clone(),
            error: Some("Code did not match.".into()),
        });
    }

    // Upgrade the pending session: issue a fresh full-strength user session
    // and revoke the pending one. (Two-step so the pending token can't be
    // replayed.)
    let token = match session::create_user_session(
        &state.db,
        user_id,
        pending.0.customer_id,
        pending.0.role_id,
        &pending.0.login,
        state.session_ttl_secs,
    )
    .await
    {
        Ok(t) => t,
        Err(_) => {
            return render(Login2faTemplate {
                pending_token: pending.0.id_hash.clone(),
                error: Some("Не удалось создать сессию.".into()),
            });
        }
    };
    // Pending-session row will expire on its own in <5 min, but revoke it
    // immediately so the cookie can't be reused.
    let _ = session::revoke_all_for_subject(
        &state.db,
        session::KIND_PENDING_2FA,
        user_id,
    )
    .await;
    let _ = sqlx::query("UPDATE users SET last_login_at = datetime('now') WHERE id = ?")
        .bind(user_id)
        .execute(&state.db)
        .await;
    let mut resp = Redirect::to("/dashboard").into_response();
    set_session_cookie(&mut resp, &token, state.secure_cookies, state.session_ttl_secs);
    clear_pending_2fa_cookie(&mut resp);
    resp
}

async fn consume_recovery_code(
    state: &AppState,
    user_id: i64,
    plain: &str,
) -> Result<bool, sqlx::Error> {
    // We can't query by hash (each row has a different salt). Pull all
    // unused codes, attempt verify against each; on match, mark used.
    #[derive(sqlx::FromRow)]
    struct Row {
        id: i64,
        code_hash: String,
    }
    let rows: Vec<Row> = sqlx::query_as::<_, Row>(
        "SELECT id, code_hash FROM totp_recovery_codes WHERE user_id = ? AND used_at IS NULL",
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await?;
    for r in rows {
        if crypto::verify_password(plain, &r.code_hash).unwrap_or(false) {
            sqlx::query(
                "UPDATE totp_recovery_codes SET used_at = datetime('now') WHERE id = ?",
            )
            .bind(r.id)
            .execute(&state.db)
            .await?;
            return Ok(true);
        }
    }
    Ok(false)
}

// ----- /signup (public, opt-in) --------------------------------------------

#[derive(Template)]
#[template(path = "signup.html")]
struct SignupTemplate {
    signup_enabled: bool,
    customer_name: String,
    login: String,
    email: String,
    error: Option<String>,
}

async fn signup_view(State(state): State<AppState>) -> Response {
    let enabled = signup_is_enabled(&state).await;
    render(SignupTemplate {
        signup_enabled: enabled,
        customer_name: String::new(),
        login: String::new(),
        email: String::new(),
        error: None,
    })
}

#[derive(Debug, Deserialize)]
struct SignupForm {
    customer_name: String,
    login: String,
    email: String,
    password: String,
}

async fn signup_submit(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    Form(req): Form<SignupForm>,
) -> Response {
    if !signup_is_enabled(&state).await {
        return render(SignupTemplate {
            signup_enabled: false,
            customer_name: req.customer_name,
            login: req.login,
            email: req.email,
            error: None,
        });
    }
    // Reuse the login rate limiter — signup is just as brute-forceable.
    if !state.login_limiter.try_take(ip) {
        return render(SignupTemplate {
            signup_enabled: true,
            customer_name: req.customer_name.clone(),
            login: req.login.clone(),
            email: req.email.clone(),
            error: Some("Too many signup attempts. Try again in a moment.".into()),
        });
    }
    let cname = req.customer_name.trim();
    let login = req.login.trim();
    let email = req.email.trim();
    if cname.len() < 2 {
        return render_signup_error(&req, "Organisation name must be ≥2 chars");
    }
    if login.len() < 2 {
        return render_signup_error(&req, "Login must be ≥2 chars");
    }
    if email.is_empty() || !email.contains('@') {
        return render_signup_error(&req, "Email looks invalid");
    }
    if req.password.len() < 12 {
        return render_signup_error(&req, "Password must be ≥12 chars");
    }

    let phc = match crypto::hash_password(&req.password) {
        Ok(s) => s,
        Err(_) => return render_signup_error(&req, "Password hash error"),
    };

    let mut tx = match state.db.begin().await {
        Ok(t) => t,
        Err(_) => return render_signup_error(&req, "Database error"),
    };
    let customer_id: i64 = match sqlx::query_scalar(
        "INSERT INTO customers (name, kind) VALUES (?, 'production') RETURNING id",
    )
    .bind(cname)
    .fetch_one(&mut *tx)
    .await
    {
        Ok(id) => id,
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
            return render_signup_error(&req, "An organisation with that name already exists");
        }
        Err(e) => {
            tracing::error!(error = %e, "signup customer insert failed");
            return render_signup_error(&req, "Database error");
        }
    };
    // role_id = 2 (admin) — admin of THEIR tenant only, not super-admin.
    let user_insert = sqlx::query(
        "INSERT INTO users (customer_id, role_id, login, email, password_hash, is_active) \
         VALUES (?, 2, ?, ?, ?, 1)",
    )
    .bind(customer_id)
    .bind(login)
    .bind(email)
    .bind(&phc)
    .execute(&mut *tx)
    .await;
    match user_insert {
        Ok(_) => {}
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
            return render_signup_error(&req, "That login is already taken");
        }
        Err(e) => {
            tracing::error!(error = %e, "signup user insert failed");
            return render_signup_error(&req, "Database error");
        }
    }
    if let Err(e) = tx.commit().await {
        tracing::error!(error = %e, "signup tx commit failed");
        return render_signup_error(&req, "Database error");
    }
    tracing::info!(customer = cname, login, "tenant signed up via /signup");
    // Issue a session immediately so they land on /dashboard logged-in.
    let row: Option<(i64, i64)> =
        sqlx::query_as("SELECT id, role_id FROM users WHERE login = ?")
            .bind(login)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    let Some((user_id, role_id)) = row else {
        return render_signup_error(&req, "Internal error — please sign in manually");
    };
    let token = match session::create_user_session(
        &state.db,
        user_id,
        customer_id,
        role_id,
        login,
        state.session_ttl_secs,
    )
    .await
    {
        Ok(t) => t,
        Err(_) => return render_signup_error(&req, "Internal error"),
    };
    let mut resp = Redirect::to("/dashboard").into_response();
    set_session_cookie(&mut resp, &token, state.secure_cookies, state.session_ttl_secs);
    resp
}

fn render_signup_error(req: &SignupForm, msg: &str) -> Response {
    render(SignupTemplate {
        signup_enabled: true,
        customer_name: req.customer_name.clone(),
        login: req.login.clone(),
        email: req.email.clone(),
        error: Some(msg.into()),
    })
}

async fn signup_is_enabled(state: &AppState) -> bool {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT value_json FROM settings WHERE key = 'signup.enabled'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    matches!(row.as_deref().map(str::trim), Some("true") | Some("\"true\""))
}

// ----- Phase 24 — i18n language switcher ----------------------------------

#[derive(Debug, Deserialize)]
struct LanguageForm {
    locale: String,
}

/// POST /settings/language — set the `outpost_lang` cookie so subsequent
/// requests resolve the chosen UI locale.
async fn settings_language(
    user: WebUser,
    State(state): State<AppState>,
    Form(req): Form<LanguageForm>,
) -> Response {
    let _ = user;
    let chosen = i18n::parse_locale(&req.locale).unwrap_or(i18n::Locale::DEFAULT);
    let mut resp = Redirect::to("/settings").into_response();
    let cookie = format!(
        "outpost_lang={}; Path=/; SameSite=Lax{}; Max-Age=31536000",
        chosen.code(),
        if state.secure_cookies { "; Secure" } else { "" },
    );
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().append(header::SET_COOKIE, v);
    }
    set_flash_cookie(&mut resp, &format!("Язык: {}", chosen.label()));
    resp
}

#[cfg(test)]
mod tests {
    //! v0.18.10: regression-тесты для pure helpers в этом файле.
    //! Полный integration-coverage handler'ов — в `app.rs` и `internal.rs`.
    use super::*;

    /// v0.18.7 regression: byte-slicing на multi-byte UTF-8 character
    /// раньше панически валил сервер. trim_to должен корректно резать
    /// по char-boundary, не байтам.
    #[test]
    fn trim_to_cyrillic_does_not_panic_at_byte_boundary() {
        // Строка где 100-й байт находится ВНУТРИ 2-байтовой кириллической 'н'.
        // 99 байт 'x' + 'н' (байты 99..101) + ещё текст.
        let s = format!("{}{}", "x".repeat(99), "ного хвоста для проверки границы");
        // На v0.18.6 это была бы паника `end byte index 100 is not a char boundary`.
        let result = trim_to(&s, 100);
        // 100 chars + '…' маркер.
        assert!(
            result.chars().count() <= 101,
            "trim_to обрезал больше 101 char'а: {} chars",
            result.chars().count()
        );
        assert!(result.ends_with('…'), "expected '…' suffix at truncation");
    }

    #[test]
    fn trim_to_short_string_passes_through() {
        assert_eq!(trim_to("hello", 100), "hello");
        assert_eq!(trim_to("", 100), "");
    }

    #[test]
    fn trim_to_exact_boundary_no_ellipsis() {
        // Если ровно max chars — обрезки нет, '…' не добавляется.
        let s = "x".repeat(50);
        let result = trim_to(&s, 50);
        assert_eq!(result, s);
        assert!(!result.ends_with('…'));
    }

    #[test]
    fn trim_to_pure_cyrillic_at_max() {
        // Полностью кириллический body — типичный случай chat.response.
        let s = "Привет, боец! Используй жгут CAT на 10-15 см проксимальнее ".repeat(10);
        let result = trim_to(&s, 200);
        assert_eq!(result.chars().count(), 201, "200 chars + '…'");
    }
}
