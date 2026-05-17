//! End-to-end auth flow: login with bootstrap admin → JWT → /api/v1/auth/me.

use std::time::Duration;
use tokio::net::TcpListener;

#[tokio::test]
async fn login_then_me_full_flow() {
    // 1. Boot test app with in-memory DB + seeded + bootstrapped admin.
    let state = outpost_server::state::test_state().await;
    let pool = state.db.clone();

    // 2. Discover the bootstrap password by hashing-verifying — we need a
    //    deterministic password for the test, so set it explicitly.
    let test_password = "TestPassword12345!";
    let phc = outpost_server::auth::hash_password(test_password).unwrap();
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

    // 3. POST /api/v1/auth/login
    let login_body = serde_json::json!({
        "login": "admin",
        "password": test_password,
    })
    .to_string();
    let body = http_post_json(&format!("http://{addr}/api/v1/auth/login"), &login_body).await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let token = v["access_token"]
        .as_str()
        .expect("missing access_token in response")
        .to_string();
    assert_eq!(v["token_type"], "Bearer");
    assert_eq!(v["must_change_password"], false);

    // 4. GET /api/v1/auth/me with the token → 200 with our login
    let me_body = http_get_with_bearer(&format!("http://{addr}/api/v1/auth/me"), &token).await;
    let me: serde_json::Value = serde_json::from_str(&me_body).unwrap();
    assert_eq!(me["login"], "admin");
    assert!(me["id"].as_i64().is_some());

    server_handle.abort();
}

#[tokio::test]
async fn login_rejects_wrong_password() {
    let state = outpost_server::state::test_state().await;
    let pool = state.db.clone();

    let phc = outpost_server::auth::hash_password("RightPass").unwrap();
    sqlx::query("UPDATE users SET password_hash = ? WHERE login = 'admin'")
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

    let body = http_post_json_expect_status(
        &format!("http://{addr}/api/v1/auth/login"),
        &serde_json::json!({"login": "admin", "password": "WrongPass"}).to_string(),
        401,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["error"]["code"], "invalid_credentials");

    server_handle.abort();
}

#[tokio::test]
async fn me_with_invalid_token_returns_401() {
    let state = outpost_server::state::test_state().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server_handle = tokio::spawn(async move {
        axum::serve(listener, outpost_server::app::build_router(state))
            .await
            .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (status, _body) =
        http_get_with_bearer_status(&format!("http://{addr}/api/v1/auth/me"), "garbage-token")
            .await;
    assert_eq!(status, 401);

    server_handle.abort();
}

// ----------------------------- HTTP helpers --------------------------------

async fn http_post_json(url: &str, body_json: &str) -> String {
    let (status, body) = http_post_json_with_status(url, body_json).await;
    assert!(
        (200..300).contains(&status),
        "expected 2xx, got {status}: {body}"
    );
    body
}

async fn http_post_json_expect_status(url: &str, body_json: &str, expected: u16) -> String {
    let (status, body) = http_post_json_with_status(url, body_json).await;
    assert_eq!(
        status, expected,
        "expected status {expected}, got {status}: {body}"
    );
    body
}

async fn http_post_json_with_status(url: &str, body_json: &str) -> (u16, String) {
    let rest = url.strip_prefix("http://").expect("http://");
    let (host, path) = rest.split_once('/').expect("path");
    let path = format!("/{path}");

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {len}\r\n\r\n{body}",
        len = body_json.len(),
        body = body_json,
    );
    stream.write_all(req.as_bytes()).await.unwrap();

    let mut buf = Vec::with_capacity(2048);
    stream.read_to_end(&mut buf).await.unwrap();
    parse_response(&buf)
}

async fn http_get_with_bearer(url: &str, token: &str) -> String {
    let (status, body) = http_get_with_bearer_status(url, token).await;
    assert!(
        (200..300).contains(&status),
        "expected 2xx, got {status}: {body}"
    );
    body
}

async fn http_get_with_bearer_status(url: &str, token: &str) -> (u16, String) {
    let rest = url.strip_prefix("http://").expect("http://");
    let (host, path) = rest.split_once('/').expect("path");
    let path = format!("/{path}");

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nAuthorization: Bearer {token}\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await.unwrap();

    let mut buf = Vec::with_capacity(2048);
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
