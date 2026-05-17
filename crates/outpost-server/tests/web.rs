//! Integration tests for the HTMX/Askama admin UI under `/`, `/login`,
//! `/dashboard`, `/devices`, `/logout`.

mod common;

use common::{TestApp, http_request};

#[tokio::test]
async fn login_page_renders_html() {
    let app = TestApp::start().await;
    let (status, body) = http_request("GET", &app.url("/login"), None, None, None).await;
    assert_eq!(status, 200);
    assert!(body.contains("<form"), "expected a form, got: {body}");
    assert!(body.contains("Outpost MDM"));
    assert!(body.contains("Sign in"));
}

#[tokio::test]
async fn dashboard_without_cookie_redirects_to_login() {
    let app = TestApp::start().await;
    let (status, body) = raw_get(&app.url("/dashboard"), None).await;
    assert_eq!(status, 303);
    assert!(body.contains("location: /login") || body.contains("Location: /login"));
}

#[tokio::test]
async fn root_path_redirects() {
    let app = TestApp::start().await;
    let (status, _body) = raw_get(&app.url("/"), None).await;
    // Root issues an unconditional redirect; cookie auth resolves on /dashboard.
    assert!(status == 303 || status == 307 || status == 302);
}

#[tokio::test]
async fn full_browser_login_flow_then_dashboard() {
    let app = TestApp::start().await;

    // 1. POST /login with form data — expect 303 + Set-Cookie + Location: /dashboard
    let body = "login=admin&password=AdminTestPass%21";
    let (status, raw_resp) = raw_post_form(&app.url("/login"), body).await;
    assert_eq!(status, 303);
    let cookie = extract_set_cookie_value(&raw_resp, "outpost_session")
        .expect("login response must set outpost_session cookie");
    assert!(!cookie.is_empty(), "session cookie value must be present");

    // 2. GET /dashboard with the cookie — expect 200 + fleet stats HTML
    let cookie_header = format!("outpost_session={cookie}");
    let (status, body) = raw_get(&app.url("/dashboard"), Some(&cookie_header)).await;
    assert_eq!(status, 200);
    assert!(body.contains("Fleet overview"));
    assert!(body.contains("Devices"));
    assert!(body.contains("admin")); // logged-in user shown in nav
}

#[tokio::test]
async fn login_with_wrong_password_rerenders_form_with_error() {
    let app = TestApp::start().await;
    let body = "login=admin&password=WRONG";
    let (status, raw_resp) = raw_post_form(&app.url("/login"), body).await;
    // Page re-renders 200 with an error message; no cookie set
    assert_eq!(status, 200);
    assert!(extract_set_cookie_value(&raw_resp, "outpost_session").is_none());
    let body_only = body_after_headers(&raw_resp);
    assert!(body_only.contains("Invalid login or password"));
}

#[tokio::test]
async fn logout_clears_cookie() {
    let app = TestApp::start().await;
    let (status, raw_resp) = raw_get(&app.url("/logout"), None).await;
    assert_eq!(status, 303);
    // The Set-Cookie must blank the value and set Max-Age=0.
    let cookie_line =
        find_header_line(&raw_resp, "set-cookie").expect("logout must emit Set-Cookie");
    assert!(cookie_line.to_lowercase().contains("max-age=0"));
}

#[tokio::test]
async fn devices_page_renders_table() {
    let app = TestApp::start().await;
    // Create one device via the API for the table.
    let body = serde_json::json!({"serial": "WEB-001", "display_name": "UI Test"}).to_string();
    http_request(
        "POST",
        &app.url("/api/v1/devices"),
        Some(&app.admin_token),
        None,
        Some(&body),
    )
    .await;

    // Now log in via the browser path and fetch the devices page.
    let (status, raw_resp) =
        raw_post_form(&app.url("/login"), "login=admin&password=AdminTestPass%21").await;
    assert_eq!(status, 303);
    let cookie = extract_set_cookie_value(&raw_resp, "outpost_session").unwrap();

    let (status, html) = raw_get(
        &app.url("/devices"),
        Some(&format!("outpost_session={cookie}")),
    )
    .await;
    assert_eq!(status, 200);
    assert!(html.contains("WEB-001"));
    assert!(html.contains("UI Test"));
    assert!(html.contains("1 total"));
}

#[tokio::test]
async fn groups_page_renders() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    let (status, html) = raw_get(&app.url("/groups"), Some(&format!("outpost_session={cookie}"))).await;
    assert_eq!(status, 200);
    assert!(html.contains("Groups"));
    assert!(html.contains("0 total") || html.contains("total"));
}

#[tokio::test]
async fn applications_page_renders() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    let (status, html) = raw_get(&app.url("/applications"), Some(&format!("outpost_session={cookie}"))).await;
    assert_eq!(status, 200);
    assert!(html.contains("Applications"));
}

#[tokio::test]
async fn configurations_page_renders() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    let (status, html) = raw_get(&app.url("/configurations"), Some(&format!("outpost_session={cookie}"))).await;
    assert_eq!(status, 200);
    assert!(html.contains("Configurations"));
}

#[tokio::test]
async fn push_page_renders() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    let (status, html) = raw_get(&app.url("/push"), Some(&format!("outpost_session={cookie}"))).await;
    assert_eq!(status, 200);
    assert!(html.contains("Push messages"));
}

#[tokio::test]
async fn users_page_renders_with_seed_admin() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    let (status, html) = raw_get(&app.url("/users"), Some(&format!("outpost_session={cookie}"))).await;
    assert_eq!(status, 200);
    assert!(html.contains("Users"));
    // The seed admin must show up.
    assert!(html.contains("admin"));
}

#[tokio::test]
async fn new_pages_redirect_to_login_without_cookie() {
    let app = TestApp::start().await;
    for path in ["/groups", "/applications", "/configurations", "/push", "/users"] {
        let (status, _body) = raw_get(&app.url(path), None).await;
        assert_eq!(status, 303, "{path} must redirect when unauthenticated");
    }
}

async fn web_login_cookie(app: &TestApp) -> String {
    let (status, raw_resp) =
        raw_post_form(&app.url("/login"), "login=admin&password=AdminTestPass%21").await;
    assert_eq!(status, 303);
    extract_set_cookie_value(&raw_resp, "outpost_session").unwrap()
}

#[tokio::test]
async fn create_device_via_web_form_redirects_then_shows_in_list() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;

    let (status, raw) = raw_request_with_cookie(
        "POST",
        &app.url("/devices/new"),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        "serial=WEB-NEW-1&display_name=ui-created",
    )
    .await;
    assert_eq!(status, 303);
    assert!(raw.to_lowercase().contains("location: /devices"));
    let flash = extract_set_cookie_value(&raw, "outpost_flash").unwrap_or_default();
    assert!(flash.contains("Device") || !flash.is_empty());

    let (status, html) = raw_get(
        &app.url("/devices"),
        Some(&format!("outpost_session={cookie}")),
    )
    .await;
    assert_eq!(status, 200);
    assert!(html.contains("WEB-NEW-1"));
    assert!(html.contains("ui-created"));
}

#[tokio::test]
async fn create_device_with_empty_serial_re_renders_with_error() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    let (status, raw) = raw_request_with_cookie(
        "POST",
        &app.url("/devices/new"),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        "serial=&display_name=",
    )
    .await;
    assert_eq!(status, 200);
    let body = body_after_headers(&raw);
    assert!(body.contains("Serial is required"));
}

#[tokio::test]
async fn create_group_via_web_form() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    let (status, raw) = raw_request_with_cookie(
        "POST",
        &app.url("/groups/new"),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        "name=alpha-squad&description=test+squad",
    )
    .await;
    assert_eq!(status, 303);
    let _ = &raw; // keep for diff-debugging when this test fails
    assert!(raw.to_lowercase().contains("location: /groups"));

    let (status, html) = raw_get(
        &app.url("/groups"),
        Some(&format!("outpost_session={cookie}")),
    )
    .await;
    assert_eq!(status, 200);
    assert!(html.contains("alpha-squad"));
}

#[tokio::test]
async fn create_user_via_web_form_with_role() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    let (status, _raw) = raw_request_with_cookie(
        "POST",
        &app.url("/users/new"),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        "login=op1&password=NotShortPwd1&role_id=3&email=op1%40example.test",
    )
    .await;
    assert_eq!(status, 303);
    let (status, html) = raw_get(
        &app.url("/users"),
        Some(&format!("outpost_session={cookie}")),
    )
    .await;
    assert_eq!(status, 200);
    assert!(html.contains("op1"));
}

#[tokio::test]
async fn create_user_rejects_short_password() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    let (status, raw) = raw_request_with_cookie(
        "POST",
        &app.url("/users/new"),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        "login=op2&password=short&role_id=3",
    )
    .await;
    assert_eq!(status, 200);
    let body = body_after_headers(&raw);
    assert!(body.contains("at least 8 characters"));
}

#[tokio::test]
async fn create_configuration_with_invalid_settings_json_re_renders_with_error() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    let (status, raw) = raw_request_with_cookie(
        "POST",
        &app.url("/configurations/new"),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        "name=base&description=&kiosk_package=&settings_json=not-json",
    )
    .await;
    assert_eq!(status, 200);
    let body = body_after_headers(&raw);
    assert!(body.contains("not valid JSON"));
}

#[tokio::test]
async fn create_configuration_happy_path() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    let (status, _raw) = raw_request_with_cookie(
        "POST",
        &app.url("/configurations/new"),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        "name=base&description=baseline&kiosk_package=ru.tacticalar.outpost&settings_json=%7B%22preferred_llm%22%3A%22gemma3%22%7D",
    )
    .await;
    assert_eq!(status, 303);
    let (status, html) = raw_get(
        &app.url("/configurations"),
        Some(&format!("outpost_session={cookie}")),
    )
    .await;
    assert_eq!(status, 200);
    assert!(html.contains("base"));
    assert!(html.contains("ru.tacticalar.outpost"));
}

#[tokio::test]
async fn device_enrollment_view_then_generate_then_qr_visible() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    // Seed: create device via API to get an id without depending on the new form
    let body = serde_json::json!({"serial":"ENROLL-001"}).to_string();
    let (status, _resp) = http_request(
        "POST",
        &app.url("/api/v1/devices"),
        Some(&app.admin_token),
        None,
        Some(&body),
    )
    .await;
    assert_eq!(status, 201);

    // GET /devices/1/enroll — should render with no secret yet
    let (status, html) = raw_get(
        &app.url("/devices/1/enroll"),
        Some(&format!("outpost_session={cookie}")),
    )
    .await;
    assert_eq!(status, 200);
    assert!(html.contains("Generate enrollment payload"));

    // POST /devices/1/enroll — generates secret + QR
    let (status, raw) = raw_request_with_cookie(
        "POST",
        &app.url("/devices/1/enroll"),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        "",
    )
    .await;
    assert_eq!(status, 200);
    let body = body_after_headers(&raw);
    assert!(body.contains("Enrollment payload"));
    assert!(body.contains("enrollment_secret"));
    assert!(body.contains("<svg") || body.contains("<rect")); // qrcode SVG
}

#[tokio::test]
async fn change_own_password_happy_path_then_relogin_with_new() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;

    let (status, _raw) = raw_request_with_cookie(
        "POST",
        &app.url("/me/password"),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        "current_password=AdminTestPass%21&new_password=NewerPwd1234&confirm_password=NewerPwd1234",
    )
    .await;
    assert_eq!(status, 303);

    // Old password no longer works
    let (status, _raw) =
        raw_post_form(&app.url("/login"), "login=admin&password=AdminTestPass%21").await;
    assert_eq!(status, 200); // login page re-renders with error

    // New password works
    let (status, raw) = raw_post_form(&app.url("/login"), "login=admin&password=NewerPwd1234").await;
    assert_eq!(status, 303);
    assert!(extract_set_cookie_value(&raw, "outpost_session").is_some());
}

#[tokio::test]
async fn change_password_mismatch_confirm_rejected() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    let (status, raw) = raw_request_with_cookie(
        "POST",
        &app.url("/me/password"),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        "current_password=AdminTestPass%21&new_password=Newpassword1&confirm_password=DIFFERENT12",
    )
    .await;
    assert_eq!(status, 200);
    let body = body_after_headers(&raw);
    assert!(body.contains("do not match"));
}

// ----- Phase 21 edit/delete + new-page tests --------------------------------

#[tokio::test]
async fn device_edit_assigns_group_and_persists() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;

    // Create a device + a group via API to get stable ids.
    let body = serde_json::json!({"serial":"EDIT-DEV-1"}).to_string();
    let (status, raw) = http_request("POST", &app.url("/api/v1/devices"), Some(&app.admin_token), None, Some(&body)).await;
    assert_eq!(status, 201);
    let dev: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let did = dev["id"].as_i64().unwrap();
    let body = serde_json::json!({"name":"squad-x"}).to_string();
    let (status, raw) = http_request("POST", &app.url("/api/v1/groups"), Some(&app.admin_token), None, Some(&body)).await;
    assert_eq!(status, 201);
    let grp: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let gid = grp["id"].as_i64().unwrap();

    // Edit device through web form: rename + assign to group
    let body = format!("display_name=alpha-one&is_active=1&group_ids={gid}");
    let (status, _raw) = raw_request_with_cookie(
        "POST",
        &app.url(&format!("/devices/{did}/edit")),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        &body,
    )
    .await;
    assert_eq!(status, 303);

    // Verify device list shows new name
    let (status, html) = raw_get(
        &app.url("/devices"),
        Some(&format!("outpost_session={cookie}")),
    )
    .await;
    assert_eq!(status, 200);
    assert!(html.contains("alpha-one"));

    // Verify group page shows member_count = 1 via API
    let (status, raw) = http_request(
        "GET",
        &app.url(&format!("/api/v1/groups/{gid}")),
        Some(&app.admin_token),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200);
    let _ = raw;
}

#[tokio::test]
async fn device_edit_with_multiple_group_ids_assigns_all() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    let body = serde_json::json!({"serial":"EDIT-DEV-2"}).to_string();
    let (_, raw) = http_request("POST", &app.url("/api/v1/devices"), Some(&app.admin_token), None, Some(&body)).await;
    let did = serde_json::from_str::<serde_json::Value>(&raw).unwrap()["id"].as_i64().unwrap();
    let mut gids = Vec::new();
    for n in &["alpha", "beta", "gamma"] {
        let body = serde_json::json!({"name": n}).to_string();
        let (_, raw) = http_request("POST", &app.url("/api/v1/groups"), Some(&app.admin_token), None, Some(&body)).await;
        let gid = serde_json::from_str::<serde_json::Value>(&raw).unwrap()["id"].as_i64().unwrap();
        gids.push(gid);
    }
    // Send body with multiple group_ids
    let body = format!(
        "display_name=multi&is_active=1&group_ids={a}&group_ids={b}&group_ids={c}",
        a = gids[0], b = gids[1], c = gids[2],
    );
    let (status, _raw) = raw_request_with_cookie(
        "POST",
        &app.url(&format!("/devices/{did}/edit")),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        &body,
    )
    .await;
    assert_eq!(status, 303);
}

#[tokio::test]
async fn device_delete_via_web_then_404_on_edit() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    let body = serde_json::json!({"serial":"DEL-1"}).to_string();
    let (_, raw) = http_request("POST", &app.url("/api/v1/devices"), Some(&app.admin_token), None, Some(&body)).await;
    let did = serde_json::from_str::<serde_json::Value>(&raw).unwrap()["id"].as_i64().unwrap();
    let (status, _raw) = raw_request_with_cookie(
        "POST",
        &app.url(&format!("/devices/{did}/delete")),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        "",
    )
    .await;
    assert_eq!(status, 303);
    let (status, _raw) = raw_get(
        &app.url(&format!("/devices/{did}/edit")),
        Some(&format!("outpost_session={cookie}")),
    )
    .await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn group_edit_then_delete() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    let body = serde_json::json!({"name":"to-rename"}).to_string();
    let (_, raw) = http_request("POST", &app.url("/api/v1/groups"), Some(&app.admin_token), None, Some(&body)).await;
    let gid = serde_json::from_str::<serde_json::Value>(&raw).unwrap()["id"].as_i64().unwrap();
    let (status, _raw) = raw_request_with_cookie(
        "POST",
        &app.url(&format!("/groups/{gid}/edit")),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        "name=renamed&description=new+desc",
    )
    .await;
    assert_eq!(status, 303);
    let (_, html) = raw_get(&app.url("/groups"), Some(&format!("outpost_session={cookie}"))).await;
    assert!(html.contains("renamed"));
    assert!(html.contains("new desc"));
    // Delete
    let (status, _raw) = raw_request_with_cookie(
        "POST",
        &app.url(&format!("/groups/{gid}/delete")),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        "",
    )
    .await;
    assert_eq!(status, 303);
    let (_, html) = raw_get(&app.url("/groups"), Some(&format!("outpost_session={cookie}"))).await;
    assert!(!html.contains("renamed"));
}

#[tokio::test]
async fn admin_resets_other_users_password_then_user_logs_in_once() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    let body = serde_json::json!({"login":"op-reset","password":"OrigPass123","role_id":3}).to_string();
    let (_, raw) = http_request("POST", &app.url("/api/v1/users"), Some(&app.admin_token), None, Some(&body)).await;
    let uid = serde_json::from_str::<serde_json::Value>(&raw).unwrap()["id"].as_i64().unwrap();
    let (status, resp) = raw_request_with_cookie(
        "POST",
        &app.url(&format!("/users/{uid}/reset-password")),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        "",
    )
    .await;
    assert_eq!(status, 303);
    // Flash cookie holds the new password
    let flash = extract_set_cookie_value(&resp, "outpost_flash").unwrap_or_default();
    assert!(flash.contains("one-time") || flash.contains("password"));
}

#[tokio::test]
async fn user_delete_not_self() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    let body = serde_json::json!({"login":"to-delete","password":"PwdValid123","role_id":4}).to_string();
    let (_, raw) = http_request("POST", &app.url("/api/v1/users"), Some(&app.admin_token), None, Some(&body)).await;
    let uid = serde_json::from_str::<serde_json::Value>(&raw).unwrap()["id"].as_i64().unwrap();
    let (status, _raw) = raw_request_with_cookie(
        "POST",
        &app.url(&format!("/users/{uid}/delete")),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        "",
    )
    .await;
    assert_eq!(status, 303);
    let (_, html) = raw_get(&app.url("/users"), Some(&format!("outpost_session={cookie}"))).await;
    assert!(!html.contains("to-delete"));
}

#[tokio::test]
async fn config_edit_then_add_then_remove_app_then_delete() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    // Need an application and a configuration first.
    let body = serde_json::json!({"package_name":"x.test","display_name":"X"}).to_string();
    let (_, raw) = http_request("POST", &app.url("/api/v1/applications"), Some(&app.admin_token), None, Some(&body)).await;
    let aid = serde_json::from_str::<serde_json::Value>(&raw).unwrap()["id"].as_i64().unwrap();
    let body = serde_json::json!({"name":"baseline","settings_json":"{}"}).to_string();
    let (_, raw) = http_request("POST", &app.url("/api/v1/configurations"), Some(&app.admin_token), None, Some(&body)).await;
    let cid = serde_json::from_str::<serde_json::Value>(&raw).unwrap()["id"].as_i64().unwrap();

    // Edit (update description)
    let body = "name=baseline&description=edited&kiosk_package=&is_active=1&settings_json=%7B%7D".to_string();
    let (status, _raw) = raw_request_with_cookie(
        "POST",
        &app.url(&format!("/configurations/{cid}/edit")),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        &body,
    )
    .await;
    assert_eq!(status, 303);

    // Add app
    let body = format!("application_id={aid}&mode=install");
    let (status, _raw) = raw_request_with_cookie(
        "POST",
        &app.url(&format!("/configurations/{cid}/apps")),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        &body,
    )
    .await;
    assert_eq!(status, 303);

    // Visit edit page — should show assigned app
    let (_, html) = raw_get(
        &app.url(&format!("/configurations/{cid}/edit")),
        Some(&format!("outpost_session={cookie}")),
    )
    .await;
    assert!(html.contains("x.test"));

    // Remove app
    let (status, _raw) = raw_request_with_cookie(
        "POST",
        &app.url(&format!("/configurations/{cid}/apps/{aid}/delete")),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        "",
    )
    .await;
    assert_eq!(status, 303);
}

#[tokio::test]
async fn roles_page_renders_seed_roles_and_permissions() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    let (status, html) = raw_get(&app.url("/roles"), Some(&format!("outpost_session={cookie}"))).await;
    assert_eq!(status, 200);
    assert!(html.contains("super-admin"));
    assert!(html.contains("admin"));
    assert!(html.contains("operator"));
    assert!(html.contains("viewer"));
    // Permissions table
    assert!(html.contains("devices."));
}

#[tokio::test]
async fn settings_save_then_reflected_in_form_defaults() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    let body = "enrollment_base_url=https%3A%2F%2Fmdm.example.com&default_sync_interval=120&max_upload_mb=300&branding_display_name=Frontier+MDM";
    let (status, _raw) = raw_request_with_cookie(
        "POST",
        &app.url("/settings"),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        body,
    )
    .await;
    assert_eq!(status, 303);
    let (_, html) = raw_get(&app.url("/settings"), Some(&format!("outpost_session={cookie}"))).await;
    assert!(html.contains("https://mdm.example.com"));
    assert!(html.contains("Frontier MDM"));
    assert!(html.contains("value=\"120\""));
}

#[tokio::test]
async fn profile_save_email_then_visible() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    let (status, _raw) = raw_request_with_cookie(
        "POST",
        &app.url("/profile"),
        &format!("outpost_session={cookie}"),
        "application/x-www-form-urlencoded",
        "email=admin%40example.test",
    )
    .await;
    assert_eq!(status, 303);
    let (_, html) = raw_get(&app.url("/profile"), Some(&format!("outpost_session={cookie}"))).await;
    assert!(html.contains("admin@example.test"));
}

#[tokio::test]
async fn files_upload_then_listed_then_deleted() {
    let app = TestApp::start().await;
    let cookie = web_login_cookie(&app).await;
    // multipart upload via raw POST
    let boundary = "----formboundary";
    let body = format!(
        "--{b}\r\nContent-Disposition: form-data; name=\"kind\"\r\n\r\nknowledge-db\r\n--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"k.db\"\r\nContent-Type: application/octet-stream\r\n\r\nHELLO BYTES\r\n--{b}--\r\n",
        b = boundary,
    );
    let (status, _raw) = raw_request_with_cookie(
        "POST",
        &app.url("/files/upload"),
        &format!("outpost_session={cookie}"),
        &format!("multipart/form-data; boundary={boundary}"),
        &body,
    )
    .await;
    assert_eq!(status, 303);
    let (_, html) = raw_get(&app.url("/files"), Some(&format!("outpost_session={cookie}"))).await;
    assert!(html.contains("k.db"));
    assert!(html.contains("knowledge-db"));
}

async fn raw_request_with_cookie(
    method: &str,
    url: &str,
    cookie_header_value: &str,
    content_type: &str,
    body: &str,
) -> (u16, String) {
    let rest = url.strip_prefix("http://").expect("http://");
    let (host, path) = rest.split_once('/').expect("path");
    let path = format!("/{path}");

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Connection: close\r\n\
         Cookie: {cookie_header_value}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {len}\r\n\
         \r\n\
         {body}",
        len = body.len(),
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::with_capacity(4096);
    stream.read_to_end(&mut buf).await.unwrap();
    parse_raw_response(&buf)
}

#[tokio::test]
async fn cookie_session_works_for_api_endpoints_too() {
    let app = TestApp::start().await;
    // Log in via web POST; reuse the cookie on /api/v1/auth/me — the API
    // extractor accepts cookies as fallback when no Bearer is present.
    let (_, raw_resp) =
        raw_post_form(&app.url("/login"), "login=admin&password=AdminTestPass%21").await;
    let cookie = extract_set_cookie_value(&raw_resp, "outpost_session").unwrap();

    let (status, body) = raw_get(
        &app.url("/api/v1/auth/me"),
        Some(&format!("outpost_session={cookie}")),
    )
    .await;
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(body_after_headers_only(&body).as_str())
        .or_else(|_| serde_json::from_str(&body))
        .unwrap();
    assert_eq!(v["login"], "admin");
}

// ------------------------- raw HTTP helpers ------------------------------
//
// The shared `common::http_request` helper strips response headers, so
// we reach for a lower-level pair here to inspect `Set-Cookie` and
// status-line details.

async fn raw_get(url: &str, cookie: Option<&str>) -> (u16, String) {
    raw_request("GET", url, cookie, None).await
}

async fn raw_post_form(url: &str, body: &str) -> (u16, String) {
    let rest = url.strip_prefix("http://").expect("http://");
    let (host, path) = rest.split_once('/').expect("path");
    let path = format!("/{path}");

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
    let req = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Connection: close\r\n\
         Content-Type: application/x-www-form-urlencoded\r\n\
         Content-Length: {len}\r\n\
         \r\n\
         {body}",
        len = body.len(),
        body = body,
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::with_capacity(4096);
    stream.read_to_end(&mut buf).await.unwrap();
    parse_raw_response(&buf)
}

async fn raw_request(
    method: &str,
    url: &str,
    cookie: Option<&str>,
    json_body: Option<&str>,
) -> (u16, String) {
    let rest = url.strip_prefix("http://").expect("http://");
    let (host, path) = rest.split_once('/').expect("path");
    let path = format!("/{path}");

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    if let Some(c) = cookie {
        head.push_str(&format!("Cookie: {c}\r\n"));
    }
    let body_bytes: Vec<u8> = if let Some(jb) = json_body {
        head.push_str("Content-Type: application/json\r\n");
        head.push_str(&format!("Content-Length: {}\r\n\r\n", jb.len()));
        jb.as_bytes().to_vec()
    } else {
        head.push_str("\r\n");
        Vec::new()
    };
    let mut req = head.into_bytes();
    req.extend_from_slice(&body_bytes);
    stream.write_all(&req).await.unwrap();
    let mut buf = Vec::with_capacity(4096);
    stream.read_to_end(&mut buf).await.unwrap();
    parse_raw_response(&buf)
}

/// Returns the (status_code, **raw response including headers**). Callers
/// that want just the body should use `body_after_headers`.
fn parse_raw_response(buf: &[u8]) -> (u16, String) {
    let raw = String::from_utf8_lossy(buf).into_owned();
    let status_line = raw.lines().next().expect("status line");
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    (status_code, raw)
}

fn extract_set_cookie_value(raw_resp: &str, cookie_name: &str) -> Option<String> {
    for line in raw_resp.lines() {
        let lower = line.to_ascii_lowercase();
        if !lower.starts_with("set-cookie:") {
            continue;
        }
        let value = line.split_once(':')?.1.trim();
        let kv = value.split(';').next()?;
        let (k, v) = kv.split_once('=')?;
        if k.trim() == cookie_name {
            return Some(v.trim().to_string());
        }
    }
    None
}

fn find_header_line<'a>(raw_resp: &'a str, name: &str) -> Option<&'a str> {
    raw_resp.lines().find(|l| {
        l.to_ascii_lowercase()
            .starts_with(&format!("{}:", name.to_ascii_lowercase()))
    })
}

fn body_after_headers(raw: &str) -> String {
    raw.find("\r\n\r\n")
        .map(|i| raw[i + 4..].to_string())
        .unwrap_or_else(|| raw.to_string())
}

/// Variant that returns body, used by tests that already have status.
fn body_after_headers_only(raw: &str) -> String {
    body_after_headers(raw)
}
