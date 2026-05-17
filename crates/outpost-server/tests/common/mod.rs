//! Shared test infrastructure for outpost-server integration tests.
//!
//! Each `tests/<name>.rs` file is its own binary; Cargo doesn't surface
//! `tests/common/mod.rs` as a binary (subdirectory, not top-level), so we
//! include it via `mod common;` from each test file and share helpers.
//!
//! `TestApp` owns the in-memory database, the bootstrapped admin
//! credentials, the bound listener, and the server task. Tests get a
//! ready-to-use admin token from `app.admin_token`. The server task
//! is aborted on `Drop` so each test cleans up after itself.

#![allow(dead_code)] // not every test file uses every helper

use std::time::Duration;
use tokio::net::TcpListener;

/// One running outpost-server, plus the admin JWT to drive it.
pub struct TestApp {
    pub addr: std::net::SocketAddr,
    pub admin_token: String,
    pub pool: sqlx::SqlitePool,
    server: tokio::task::JoinHandle<()>,
}

impl TestApp {
    /// Start with the default admin password.
    pub async fn start() -> Self {
        Self::start_with_password("AdminTestPass!").await
    }

    /// Start and authenticate with a specific admin password.
    pub async fn start_with_password(password: &str) -> Self {
        let state = outpost_server::state::test_state().await;
        let pool = state.db.clone();

        let phc = outpost_server::auth::hash_password(password).unwrap();
        sqlx::query(
            "UPDATE users SET password_hash = ?, must_change_password = 0 \
             WHERE login = 'admin'",
        )
        .bind(&phc)
        .execute(&pool)
        .await
        .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                outpost_server::app::build_router(state)
                    .into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let login_body = http_request(
            "POST",
            &format!("http://{addr}/api/v1/auth/login"),
            None,
            None,
            Some(&serde_json::json!({ "login": "admin", "password": password }).to_string()),
        )
        .await
        .1;
        let v: serde_json::Value = serde_json::from_str(&login_body).unwrap();
        let admin_token = v["access_token"].as_str().unwrap().to_string();

        Self {
            addr,
            admin_token,
            pool,
            server,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    /// Create a non-admin user with the given role_id (3=operator, 4=viewer),
    /// then return the bearer token for that user.
    pub async fn token_for_role(&self, login: &str, password: &str, role_id: i64) -> String {
        http_json(
            "POST",
            &self.url("/api/v1/users"),
            &self.admin_token,
            &serde_json::json!({
                "login": login,
                "password": password,
                "role_id": role_id,
            })
            .to_string(),
            201,
        )
        .await;
        let body = http_request(
            "POST",
            &self.url("/api/v1/auth/login"),
            None,
            None,
            Some(&serde_json::json!({"login": login, "password": password}).to_string()),
        )
        .await
        .1;
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        v["access_token"].as_str().unwrap().to_string()
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        self.server.abort();
    }
}

// --------------------------- HTTP helpers --------------------------------

pub async fn http_get(url: &str, token: &str, expected_status: u16) -> String {
    let (status, body) = http_request("GET", url, Some(token), None, None).await;
    assert_eq!(
        status, expected_status,
        "GET {url} -> {status} (expected {expected_status}): {body}"
    );
    body
}

pub async fn http_json(
    method: &str,
    url: &str,
    token: &str,
    body_json: &str,
    expected_status: u16,
) -> String {
    let (status, body) = http_request(method, url, Some(token), None, Some(body_json)).await;
    assert_eq!(
        status, expected_status,
        "{method} {url} -> {status} (expected {expected_status}): {body}"
    );
    body
}

pub async fn http_status(method: &str, url: &str, token: Option<&str>) -> u16 {
    http_request(method, url, token, None, None).await.0
}

pub struct MultipartBody<'a> {
    pub boundary: &'a str,
    pub bytes: &'a [u8],
}

pub async fn http_request(
    method: &str,
    url: &str,
    token: Option<&str>,
    multipart: Option<MultipartBody<'_>>,
    json_body: Option<&str>,
) -> (u16, String) {
    let rest = url.strip_prefix("http://").expect("http:// url");
    let (host, path) = rest.split_once('/').expect("path");
    let path = format!("/{path}");

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    if let Some(t) = token {
        head.push_str(&format!("Authorization: Bearer {t}\r\n"));
    }
    let body_bytes: Vec<u8> = if let Some(mp) = &multipart {
        head.push_str(&format!(
            "Content-Type: multipart/form-data; boundary={}\r\n",
            mp.boundary
        ));
        head.push_str(&format!("Content-Length: {}\r\n\r\n", mp.bytes.len()));
        mp.bytes.to_vec()
    } else if let Some(jb) = json_body {
        head.push_str("Content-Type: application/json\r\n");
        head.push_str(&format!("Content-Length: {}\r\n\r\n", jb.len()));
        jb.as_bytes().to_vec()
    } else {
        head.push_str("\r\n");
        Vec::new()
    };
    let mut req: Vec<u8> = head.into_bytes();
    req.extend_from_slice(&body_bytes);
    stream.write_all(&req).await.unwrap();

    let mut buf = Vec::with_capacity(4096);
    stream.read_to_end(&mut buf).await.unwrap();
    parse_response(&buf)
}

pub fn parse_response(buf: &[u8]) -> (u16, String) {
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

pub fn build_multipart(
    boundary: &str,
    filename: &str,
    content_type: &str,
    file_bytes: &[u8],
    kind: Option<&str>,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    out.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n")
            .as_bytes(),
    );
    out.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
    out.extend_from_slice(file_bytes);
    out.extend_from_slice(b"\r\n");
    if let Some(k) = kind {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        out.extend_from_slice(b"Content-Disposition: form-data; name=\"kind\"\r\n\r\n");
        out.extend_from_slice(k.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out
}
