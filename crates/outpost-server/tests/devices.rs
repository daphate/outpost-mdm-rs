//! End-to-end test for `/api/v1/devices` — full CRUD + permission denial.
//!
//! Spins up the full router with an in-memory pool + bootstrapped admin,
//! exchanges credentials for a JWT, then walks the CRUD shape.

use std::time::Duration;
use tokio::net::TcpListener;

#[tokio::test]
async fn devices_crud_happy_path() {
    let (addr, _server_handle, token) = bring_up_admin_token().await;

    // CREATE
    let body = http_json(
        "POST",
        &format!("http://{addr}/api/v1/devices"),
        &token,
        &serde_json::json!({ "serial": "ULF-001", "display_name": "Test Device" }).to_string(),
        201,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = v["id"].as_i64().expect("id");
    assert_eq!(v["serial"], "ULF-001");
    assert_eq!(v["display_name"], "Test Device");

    // LIST
    let list_body = http_get(&format!("http://{addr}/api/v1/devices"), &token, 200).await;
    let lv: serde_json::Value = serde_json::from_str(&list_body).unwrap();
    assert_eq!(lv["total"], 1);
    assert_eq!(lv["items"].as_array().unwrap().len(), 1);
    assert_eq!(lv["items"][0]["serial"], "ULF-001");

    // GET
    let body = http_get(&format!("http://{addr}/api/v1/devices/{id}"), &token, 200).await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["id"], id);
    assert_eq!(v["serial"], "ULF-001");

    // UPDATE
    let body = http_json(
        "PUT",
        &format!("http://{addr}/api/v1/devices/{id}"),
        &token,
        &serde_json::json!({ "display_name": "Renamed", "is_active": false }).to_string(),
        200,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["display_name"], "Renamed");
    assert_eq!(v["is_active"], false);

    // DELETE
    let (status, _b) = http_request(
        "DELETE",
        &format!("http://{addr}/api/v1/devices/{id}"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, 204);

    // GET after DELETE → 404
    let (status, body) = http_request(
        "GET",
        &format!("http://{addr}/api/v1/devices/{id}"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, 404);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["error"]["code"], "not_found");
}

#[tokio::test]
async fn create_device_rejects_duplicate_serial() {
    let (addr, _server_handle, token) = bring_up_admin_token().await;

    http_json(
        "POST",
        &format!("http://{addr}/api/v1/devices"),
        &token,
        &serde_json::json!({ "serial": "DUP-001" }).to_string(),
        201,
    )
    .await;
    let body = http_json(
        "POST",
        &format!("http://{addr}/api/v1/devices"),
        &token,
        &serde_json::json!({ "serial": "DUP-001" }).to_string(),
        400,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["error"]["code"], "bad_request");
}

#[tokio::test]
async fn viewer_cannot_create_device() {
    let (addr, _server_handle, _admin_token) = bring_up_admin_token().await;

    // Create a viewer user via admin token, then log in as viewer.
    let admin_token = _admin_token.clone();
    http_json(
        "POST",
        &format!("http://{addr}/api/v1/users"),
        &admin_token,
        &serde_json::json!({
            "login": "viewer1",
            "password": "ViewerPass123",
            "role_id": 4,
        })
        .to_string(),
        201,
    )
    .await;
    let login_body = http_request(
        "POST",
        &format!("http://{addr}/api/v1/auth/login"),
        None,
        Some(&serde_json::json!({ "login": "viewer1", "password": "ViewerPass123" }).to_string()),
    )
    .await
    .1;
    let v: serde_json::Value = serde_json::from_str(&login_body).unwrap();
    let viewer_token = v["access_token"].as_str().unwrap().to_string();

    let (status, body) = http_request(
        "POST",
        &format!("http://{addr}/api/v1/devices"),
        Some(&viewer_token),
        Some(&serde_json::json!({ "serial": "VIEW-001" }).to_string()),
    )
    .await;
    assert_eq!(status, 403);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["error"]["code"], "forbidden");
}

#[tokio::test]
async fn fleet_stats_returns_zeros_on_empty_tenant() {
    let (addr, _server_handle, token) = bring_up_admin_token().await;
    let body = http_get(&format!("http://{addr}/api/v1/stats/fleet"), &token, 200).await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["devices_total"], 0);
    assert_eq!(v["push_pending"], 0);
}

// ------------------------- test infra ------------------------------------

async fn bring_up_admin_token() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>, String) {
    let state = outpost_server::state::test_state().await;
    let pool = state.db.clone();

    // Override the bootstrap-generated hash with a deterministic one.
    let phc = outpost_server::auth::hash_password("AdminTestPass!").unwrap();
    sqlx::query(
        "UPDATE users SET password_hash = ?, must_change_password = 0 WHERE login = 'admin'",
    )
    .bind(&phc)
    .execute(&pool)
    .await
    .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server_handle = tokio::spawn(async move {
        axum::serve(listener, outpost_server::app::build_router(state))
            .await
            .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let login_body = http_request(
        "POST",
        &format!("http://{addr}/api/v1/auth/login"),
        None,
        Some(&serde_json::json!({ "login": "admin", "password": "AdminTestPass!" }).to_string()),
    )
    .await
    .1;
    let v: serde_json::Value = serde_json::from_str(&login_body).unwrap();
    let token = v["access_token"].as_str().unwrap().to_string();
    (addr, server_handle, token)
}

async fn http_get(url: &str, token: &str, expected_status: u16) -> String {
    let (status, body) = http_request("GET", url, Some(token), None).await;
    assert_eq!(status, expected_status, "GET {url} -> {status}: {body}");
    body
}

async fn http_json(
    method: &str,
    url: &str,
    token: &str,
    body_json: &str,
    expected_status: u16,
) -> String {
    let (status, body) = http_request(method, url, Some(token), Some(body_json)).await;
    assert_eq!(
        status, expected_status,
        "{method} {url} -> {status} (expected {expected_status}): {body}"
    );
    body
}

async fn http_request(
    method: &str,
    url: &str,
    token: Option<&str>,
    body_json: Option<&str>,
) -> (u16, String) {
    let rest = url.strip_prefix("http://").expect("http:// url");
    let (host, path) = rest.split_once('/').expect("path");
    let path = format!("/{path}");

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    if let Some(token) = token {
        req.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    if let Some(body) = body_json {
        req.push_str("Content-Type: application/json\r\n");
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
        req.push_str("\r\n");
        req.push_str(body);
    } else {
        req.push_str("\r\n");
    }
    stream.write_all(req.as_bytes()).await.unwrap();

    let mut buf = Vec::with_capacity(4096);
    stream.read_to_end(&mut buf).await.unwrap();
    parse_response(&buf)
}

fn parse_response(buf: &[u8]) -> (u16, String) {
    let raw = String::from_utf8_lossy(buf).into_owned();
    let status_line = raw.lines().next().expect("status line");
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let body_start = raw.find("\r\n\r\n").expect("CRLFCRLF") + 4;
    (status_code, raw[body_start..].to_string())
}
