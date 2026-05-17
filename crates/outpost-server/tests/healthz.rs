//! End-to-end test: boot the real TCP listener, hit /healthz with a TCP client.
//!
//! Complements the in-crate unit tests in `app::tests` which use `oneshot`
//! against the router directly. This file proves the WHOLE stack works
//! (TcpListener + axum::serve + tower-http middleware + handler).

use std::time::Duration;
use tokio::net::TcpListener;

#[tokio::test]
async fn healthz_e2e_over_real_tcp() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_handle = tokio::spawn(async move {
        axum::serve(listener, outpost_server::app::build_router())
            .await
            .unwrap();
    });

    // Give the server a moment to start accepting.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let url = format!("http://{addr}/healthz");
    let body = simple_get(&url).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["status"], "ok");
    assert!(json["version"].as_str().is_some());

    server_handle.abort();
}

/// Tiny inline HTTP/1.1 GET client to avoid a reqwest dependency in tests.
async fn simple_get(url: &str) -> String {
    let prefix = "http://";
    let rest = url.strip_prefix(prefix).expect("http:// url");
    let (host, path) = rest.split_once('/').expect("path");
    let path = format!("/{path}");

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();

    let mut buf = Vec::with_capacity(1024);
    stream.read_to_end(&mut buf).await.unwrap();
    let response = String::from_utf8_lossy(&buf).into_owned();

    // Strip headers, return body. Body starts after first blank line.
    let body_start = response
        .find("\r\n\r\n")
        .expect("missing CRLFCRLF in response")
        + 4;
    response[body_start..].to_string()
}
