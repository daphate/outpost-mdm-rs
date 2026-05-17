//! Integration tests for `/api/v1/users` admin and `/users/{id}/password`.

mod common;

use common::{TestApp, http_get, http_json, http_request};

#[tokio::test]
async fn users_crud_happy_path() {
    let app = TestApp::start().await;

    // CREATE (operator role_id = 3)
    let body = http_json(
        "POST",
        &app.url("/api/v1/users"),
        &app.admin_token,
        &serde_json::json!({
            "login": "alice",
            "password": "AlicePass123",
            "role_id": 3,
            "email": "alice@example.com",
        })
        .to_string(),
        201,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = v["id"].as_i64().unwrap();
    assert_eq!(v["login"], "alice");
    assert_eq!(v["role_id"], 3);
    assert_eq!(v["email"], "alice@example.com");

    // LIST shows admin + alice
    let list = http_get(&app.url("/api/v1/users"), &app.admin_token, 200).await;
    let v: serde_json::Value = serde_json::from_str(&list).unwrap();
    assert_eq!(v["total"], 2);

    // UPDATE: change role to viewer (4) and deactivate
    let body = http_json(
        "PUT",
        &app.url(&format!("/api/v1/users/{id}")),
        &app.admin_token,
        &serde_json::json!({"role_id": 4, "is_active": false}).to_string(),
        200,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["role_id"], 4);
    assert_eq!(v["is_active"], false);

    // DELETE
    let (status, _) = http_request(
        "DELETE",
        &app.url(&format!("/api/v1/users/{id}")),
        Some(&app.admin_token),
        None,
        None,
    )
    .await;
    assert_eq!(status, 204);
}

#[tokio::test]
async fn duplicate_login_rejected() {
    let app = TestApp::start().await;
    http_json(
        "POST",
        &app.url("/api/v1/users"),
        &app.admin_token,
        &serde_json::json!({"login": "dup", "password": "DupPass123", "role_id": 4}).to_string(),
        201,
    )
    .await;
    let body = http_json(
        "POST",
        &app.url("/api/v1/users"),
        &app.admin_token,
        &serde_json::json!({"login": "dup", "password": "DupPass123", "role_id": 4}).to_string(),
        400,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["error"]["code"], "bad_request");
}

#[tokio::test]
async fn weak_password_rejected_on_create() {
    let app = TestApp::start().await;
    http_json(
        "POST",
        &app.url("/api/v1/users"),
        &app.admin_token,
        &serde_json::json!({"login": "weak", "password": "abc", "role_id": 4}).to_string(),
        400,
    )
    .await;
}

#[tokio::test]
async fn unknown_role_id_rejected() {
    let app = TestApp::start().await;
    http_json(
        "POST",
        &app.url("/api/v1/users"),
        &app.admin_token,
        &serde_json::json!({"login": "bob", "password": "BobPass123", "role_id": 999}).to_string(),
        400,
    )
    .await;
}

#[tokio::test]
async fn admin_cannot_delete_self() {
    let app = TestApp::start().await;
    // Admin's user id is 1 (seeded). Try to delete.
    let (status, body) = http_request(
        "DELETE",
        &app.url("/api/v1/users/1"),
        Some(&app.admin_token),
        None,
        None,
    )
    .await;
    assert_eq!(status, 400);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["error"]["code"], "bad_request");
}

#[tokio::test]
async fn user_can_change_own_password_without_users_write_permission() {
    let app = TestApp::start().await;
    // Operator (role_id=3) does NOT have users.write
    let operator_token = app.token_for_role("op1", "OpPass1234", 3).await;

    // Operator's own user id — recover via /me
    let body = http_get(&app.url("/api/v1/auth/me"), &operator_token, 200).await;
    let me: serde_json::Value = serde_json::from_str(&body).unwrap();
    let op_id = me["id"].as_i64().unwrap();

    // Operator changes OWN password — must succeed
    let (status, _) = http_request(
        "PUT",
        &app.url(&format!("/api/v1/users/{op_id}/password")),
        Some(&operator_token),
        None,
        Some(&serde_json::json!({"new_password": "NewOpPass987"}).to_string()),
    )
    .await;
    assert_eq!(status, 204);

    // Re-login with new password
    let body = http_request(
        "POST",
        &app.url("/api/v1/auth/login"),
        None,
        None,
        Some(&serde_json::json!({"login": "op1", "password": "NewOpPass987"}).to_string()),
    )
    .await
    .1;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(v["access_token"].as_str().is_some());
}

#[tokio::test]
async fn user_cannot_change_other_users_password_without_permission() {
    let app = TestApp::start().await;
    let viewer = app.token_for_role("v1", "ViewerPass123", 4).await;
    let (status, _) = http_request(
        "PUT",
        &app.url("/api/v1/users/1/password"), // admin's id
        Some(&viewer),
        None,
        Some(&serde_json::json!({"new_password": "HackPass123"}).to_string()),
    )
    .await;
    assert_eq!(status, 403);
}
