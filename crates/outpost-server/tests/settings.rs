//! Integration tests for `/api/v1/settings`.

mod common;

use common::{TestApp, http_get, http_json, http_request};

#[tokio::test]
async fn list_returns_seeded_settings() {
    let app = TestApp::start().await;
    let body = http_get(&app.url("/api/v1/settings"), &app.admin_token, 200).await;
    let arr: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert!(arr.len() >= 5);
    assert!(arr.iter().any(|s| s["key"] == "server.name"));
}

#[tokio::test]
async fn get_specific_seeded_setting() {
    let app = TestApp::start().await;
    let body = http_get(
        &app.url("/api/v1/settings/server.name"),
        &app.admin_token,
        200,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["key"], "server.name");
    let inner: serde_json::Value = serde_json::from_str(v["value_json"].as_str().unwrap()).unwrap();
    assert_eq!(inner, "Outpost MDM");
}

#[tokio::test]
async fn upsert_new_setting() {
    let app = TestApp::start().await;
    let (status, _) = http_request(
        "PUT",
        &app.url("/api/v1/settings/custom.feature_flag"),
        Some(&app.admin_token),
        None,
        Some(
            &serde_json::json!({
                "value_json": "true",
                "description": "Demo flag",
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(status, 204);
    let body = http_get(
        &app.url("/api/v1/settings/custom.feature_flag"),
        &app.admin_token,
        200,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let inner: serde_json::Value = serde_json::from_str(v["value_json"].as_str().unwrap()).unwrap();
    assert_eq!(inner, true);
}

#[tokio::test]
async fn upsert_invalid_json_rejected() {
    let app = TestApp::start().await;
    http_json(
        "PUT",
        &app.url("/api/v1/settings/bogus.key"),
        &app.admin_token,
        &serde_json::json!({"value_json": "{not json"}).to_string(),
        400,
    )
    .await;
}

#[tokio::test]
async fn unknown_setting_returns_404() {
    let app = TestApp::start().await;
    let (status, _) = http_request(
        "GET",
        &app.url("/api/v1/settings/does.not.exist"),
        Some(&app.admin_token),
        None,
        None,
    )
    .await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn viewer_can_read_but_not_write_settings() {
    let app = TestApp::start().await;
    let viewer = app.token_for_role("vsettings", "ViewerPass123", 4).await;

    http_get(&app.url("/api/v1/settings"), &viewer, 200).await;

    let (status, _) = http_request(
        "PUT",
        &app.url("/api/v1/settings/some.key"),
        Some(&viewer),
        None,
        Some(&serde_json::json!({"value_json": "1"}).to_string()),
    )
    .await;
    assert_eq!(status, 403);
}
