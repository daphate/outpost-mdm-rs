//! Integration tests for `/api/v1/profile` (customer profile, Ф6).

mod common;

use common::{TestApp, http_get, http_request};

#[tokio::test]
async fn get_404_before_any_upsert() {
    let app = TestApp::start().await;
    let (status, _) = http_request(
        "GET",
        &app.url("/api/v1/profile"),
        Some(&app.admin_token),
        None,
        None,
    )
    .await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn upsert_then_get_roundtrip_and_coalesce() {
    let app = TestApp::start().await;
    let body = serde_json::json!({
        "single_tenant": true,
        "enabled_classes": ["android_tactical", "wearable"],
        "feature_flags": {"ballistics": false, "players": true},
        "domain": "acme.example.tech"
    })
    .to_string();
    let (status, _) = http_request(
        "PUT",
        &app.url("/api/v1/profile"),
        Some(&app.admin_token),
        None,
        Some(&body),
    )
    .await;
    assert_eq!(status, 200);

    let got = http_get(&app.url("/api/v1/profile"), &app.admin_token, 200).await;
    let v: serde_json::Value = serde_json::from_str(&got).unwrap();
    assert_eq!(v["single_tenant"], true);
    assert_eq!(v["domain"], "acme.example.tech");
    let classes: Vec<String> =
        serde_json::from_str(v["enabled_classes"].as_str().unwrap()).unwrap();
    assert!(classes.contains(&"wearable".to_string()));
    let flags: serde_json::Value =
        serde_json::from_str(v["feature_flags"].as_str().unwrap()).unwrap();
    assert_eq!(flags["players"], true);

    // A second PUT that omits fields must preserve them (COALESCE upsert).
    let body2 = serde_json::json!({"domain": "acme2.example.tech"}).to_string();
    let (status2, _) = http_request(
        "PUT",
        &app.url("/api/v1/profile"),
        Some(&app.admin_token),
        None,
        Some(&body2),
    )
    .await;
    assert_eq!(status2, 200);
    let got2 = http_get(&app.url("/api/v1/profile"), &app.admin_token, 200).await;
    let v2: serde_json::Value = serde_json::from_str(&got2).unwrap();
    assert_eq!(v2["domain"], "acme2.example.tech");
    assert_eq!(
        v2["single_tenant"], true,
        "omitted single_tenant must be preserved"
    );
    assert!(
        v2["enabled_classes"].is_string(),
        "omitted enabled_classes must be preserved"
    );
}

#[tokio::test]
async fn rejects_malformed_json_shapes() {
    let app = TestApp::start().await;
    for bad in [
        serde_json::json!({"enabled_classes": "notarray"}),
        serde_json::json!({"enabled_classes": [1, 2]}),
        serde_json::json!({"feature_flags": [1, 2, 3]}),
    ] {
        let (status, _) = http_request(
            "PUT",
            &app.url("/api/v1/profile"),
            Some(&app.admin_token),
            None,
            Some(&bad.to_string()),
        )
        .await;
        assert_eq!(status, 400, "expected 400 for {bad}");
    }
}

#[tokio::test]
async fn viewer_cannot_write_profile() {
    let app = TestApp::start().await;
    let viewer = app.token_for_role("vprofile", "ViewerPass123", 4).await;
    let (status, _) = http_request(
        "PUT",
        &app.url("/api/v1/profile"),
        Some(&viewer),
        None,
        Some(&serde_json::json!({"domain": "x"}).to_string()),
    )
    .await;
    assert_eq!(status, 403);
}
