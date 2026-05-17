//! Tests for transport-layer hardening: body size limit + security headers + rate limit.

mod common;

use common::{TestApp, http_request};

#[tokio::test]
async fn login_rate_limit_kicks_in_after_burst() {
    let app = TestApp::start().await;
    // `TestApp::start` consumed 1 token via the successful admin login.
    // The default bucket is 10 capacity; we have ~9 attempts before
    // the rate limiter fires. Send 9 more, all with wrong password, all
    // expected to return 401. The 10th additional attempt should be 429.
    for i in 0..9 {
        let (status, _body) = http_request(
            "POST",
            &app.url("/api/v1/auth/login"),
            None,
            None,
            Some(
                &serde_json::json!({
                    "login": "admin",
                    "password": format!("wrong-{i}"),
                })
                .to_string(),
            ),
        )
        .await;
        assert_eq!(status, 401, "attempt {i} should still be 401");
    }
    let (status, body) = http_request(
        "POST",
        &app.url("/api/v1/auth/login"),
        None,
        None,
        Some(&serde_json::json!({"login": "admin", "password": "wrong-10"}).to_string()),
    )
    .await;
    assert_eq!(status, 429, "expected 429, got {status}: {body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["error"]["code"], "too_many_requests");
}

#[tokio::test]
async fn oversized_body_is_rejected() {
    // Spin up the app with a very small body cap (8 KiB) for this test.
    let mut state = outpost_server::state::test_state().await;
    state.max_body_bytes = 8 * 1024;
    // Set a deterministic admin password.
    let pool = state.db.clone();
    let phc = outpost_server::auth::hash_password("AdminTestPass!").unwrap();
    sqlx::query(
        "UPDATE users SET password_hash = ?, must_change_password = 0 WHERE login = 'admin'",
    )
    .bind(&phc)
    .execute(&pool)
    .await
    .unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let _server = tokio::spawn(async move {
        axum::serve(listener, outpost_server::app::build_router(state))
            .await
            .ok();
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Get a token first (small body).
    let login = http_request(
        "POST",
        &format!("http://{addr}/api/v1/auth/login"),
        None,
        None,
        Some(&serde_json::json!({"login": "admin", "password": "AdminTestPass!"}).to_string()),
    )
    .await
    .1;
    let token = serde_json::from_str::<serde_json::Value>(&login).unwrap()["access_token"]
        .as_str()
        .unwrap()
        .to_string();

    // Now POST a body larger than 8 KiB. Server should reject with 413.
    let big = "x".repeat(16 * 1024);
    let body = serde_json::json!({"serial": big}).to_string();
    let (status, _) = http_request(
        "POST",
        &format!("http://{addr}/api/v1/devices"),
        Some(&token),
        None,
        Some(&body),
    )
    .await;
    assert_eq!(
        status, 413,
        "expected 413 Payload Too Large for oversized body"
    );
}

#[tokio::test]
async fn responses_carry_security_headers() {
    let app = TestApp::start().await;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(app.addr).await.unwrap();
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut buf = Vec::with_capacity(2048);
    stream.read_to_end(&mut buf).await.unwrap();
    let raw = String::from_utf8_lossy(&buf).into_owned();

    // Lowercase the entire header block — header names are case-insensitive
    // but axum emits them lowercase already; this guards against future
    // changes upstream.
    let lc = raw.to_ascii_lowercase();
    for expected in [
        "x-content-type-options: nosniff",
        "x-frame-options: deny",
        "referrer-policy: no-referrer",
        "strict-transport-security: max-age=",
        "x-robots-tag: noindex",
        "permissions-policy: camera=()",
        "x-request-id:",
    ] {
        assert!(
            lc.contains(expected),
            "missing security header '{expected}' in response:\n{raw}"
        );
    }
}
