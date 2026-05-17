//! End-to-end test: multipart upload → signed URL → public download.

use std::time::Duration;
use tokio::net::TcpListener;

#[tokio::test]
async fn upload_signed_url_download_round_trip() {
    let (addr, _server_handle, token) = bring_up_admin_token().await;
    let payload: &[u8] = b"hello outpost world\n";

    // 1. Multipart upload via /api/v1/files/upload
    let boundary = "----outpost-test-boundary";
    let body = build_multipart(boundary, "test.txt", "text/plain", payload, Some("generic"));
    let (status, resp) = http_request(
        "POST",
        &format!("http://{addr}/api/v1/files/upload"),
        Some(&token),
        Some(MultipartBody {
            boundary,
            bytes: &body,
        }),
        None,
    )
    .await;
    assert_eq!(status, 201, "upload failed: {resp}");
    let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let file_id = v["id"].as_i64().expect("id");
    assert_eq!(v["original_name"], "test.txt");
    assert_eq!(v["file_size_bytes"], payload.len() as i64);
    let sha256 = v["sha256"].as_str().unwrap().to_string();
    assert_eq!(sha256.len(), 64);

    // 2. Get signed URL
    let signed = http_get(
        &format!("http://{addr}/api/v1/files/{file_id}/signed-url?expires_in=60"),
        &token,
        200,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&signed).unwrap();
    let url_path = v["url"].as_str().unwrap().to_string();
    assert!(url_path.starts_with("/files/signed/"));

    // 3. Public download (no Authorization header)
    let (status, body) =
        http_request("GET", &format!("http://{addr}{url_path}"), None, None, None).await;
    assert_eq!(status, 200);
    assert_eq!(body.as_bytes(), payload);

    // 4. Tampered token → 403
    let mut parts: Vec<&str> = url_path.split('/').collect();
    let token_idx = parts.len() - 1;
    let original_token = parts[token_idx];
    let mut tampered_chars: Vec<char> = original_token.chars().collect();
    let last_idx = tampered_chars.len() - 1;
    tampered_chars[last_idx] = if tampered_chars[last_idx] == '0' {
        '1'
    } else {
        '0'
    };
    let tampered_token: String = tampered_chars.into_iter().collect();
    parts[token_idx] = &tampered_token;
    let tampered_path = parts.join("/");
    let (status, _b) = http_request(
        "GET",
        &format!("http://{addr}{tampered_path}"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn upload_without_token_returns_401() {
    let (addr, _server_handle, _t) = bring_up_admin_token().await;
    let boundary = "----no-auth";
    let body = build_multipart(boundary, "x.txt", "text/plain", b"abc", None);
    let (status, _b) = http_request(
        "POST",
        &format!("http://{addr}/api/v1/files/upload"),
        None,
        Some(MultipartBody {
            boundary,
            bytes: &body,
        }),
        None,
    )
    .await;
    assert_eq!(status, 401);
}

// ------------------- test infra ----------------------

fn build_multipart(
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

struct MultipartBody<'a> {
    boundary: &'a str,
    bytes: &'a [u8],
}

async fn bring_up_admin_token() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>, String) {
    let state = outpost_server::state::test_state().await;
    let pool = state.db.clone();
    let phc = outpost_server::auth::hash_password("AdminFiles!").unwrap();
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
        None,
        Some(&serde_json::json!({ "login": "admin", "password": "AdminFiles!" }).to_string()),
    )
    .await
    .1;
    let v: serde_json::Value = serde_json::from_str(&login_body).unwrap();
    let token = v["access_token"].as_str().unwrap().to_string();
    (addr, server_handle, token)
}

async fn http_get(url: &str, token: &str, expected_status: u16) -> String {
    let (status, body) = http_request("GET", url, Some(token), None, None).await;
    assert_eq!(status, expected_status, "GET {url} -> {status}: {body}");
    body
}

async fn http_request(
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
        head.push_str(&format!("Content-Length: {}\r\n", mp.bytes.len()));
        head.push_str("\r\n");
        mp.bytes.to_vec()
    } else if let Some(jb) = json_body {
        head.push_str("Content-Type: application/json\r\n");
        head.push_str(&format!("Content-Length: {}\r\n", jb.len()));
        head.push_str("\r\n");
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
