//! `/api/v1/users` — admin-level user account management.

use crate::auth as crypto;
use crate::auth_extract::AuthUser;
use crate::error::ApiError;
use crate::page::{Page, PageParams};
use crate::permission::require_permission;
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/users", get(list).post(create))
        .route(
            "/api/v1/users/{id}",
            get(get_one).put(update).delete(delete),
        )
        .route(
            "/api/v1/users/{id}/password",
            axum::routing::put(set_password),
        )
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct UserOut {
    pub id: i64,
    pub customer_id: i64,
    pub role_id: i64,
    pub login: String,
    pub email: Option<String>,
    pub is_active: bool,
    pub must_change_password: bool,
    pub last_login_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

async fn list(
    user: AuthUser,
    State(state): State<AppState>,
    Query(page): Query<PageParams>,
) -> Result<Json<Page<UserOut>>, ApiError> {
    require_permission(&state.db, user.role_id, "users.read").await?;
    let (limit, offset) = page.clamp();
    let items: Vec<UserOut> = sqlx::query_as::<_, UserOut>(
        "SELECT id, customer_id, role_id, login, email, is_active, \
                must_change_password, last_login_at, created_at, updated_at \
         FROM users WHERE customer_id = ? ORDER BY login LIMIT ? OFFSET ?",
    )
    .bind(user.customer_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE customer_id = ?")
        .bind(user.customer_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(Page {
        items,
        total,
        limit,
        offset,
    }))
}

async fn get_one(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<UserOut>, ApiError> {
    require_permission(&state.db, user.role_id, "users.read").await?;
    let u: Option<UserOut> = sqlx::query_as::<_, UserOut>(
        "SELECT id, customer_id, role_id, login, email, is_active, \
                must_change_password, last_login_at, created_at, updated_at \
         FROM users WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?;
    u.map(Json).ok_or(ApiError::NotFound)
}

#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    pub login: String,
    pub email: Option<String>,
    pub role_id: i64,
    pub password: String,
}

async fn create(
    user: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<UserOut>), ApiError> {
    require_permission(&state.db, user.role_id, "users.write").await?;
    if req.login.trim().is_empty() || req.password.len() < 8 {
        return Err(ApiError::BadRequest(
            "login is required and password must be at least 8 chars".into(),
        ));
    }
    // Role must exist (lookup tables are global, not per-tenant).
    let role_ok: Option<i64> = sqlx::query_scalar("SELECT 1 FROM user_roles WHERE id = ?")
        .bind(req.role_id)
        .fetch_optional(&state.db)
        .await?;
    if role_ok.is_none() {
        return Err(ApiError::BadRequest("unknown role_id".into()));
    }
    let phc = crypto::hash_password(&req.password).map_err(|_| ApiError::Internal)?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO users (customer_id, role_id, login, email, password_hash, is_active) \
         VALUES (?, ?, ?, ?, ?, 1) RETURNING id",
    )
    .bind(user.customer_id)
    .bind(req.role_id)
    .bind(&req.login)
    .bind(&req.email)
    .bind(&phc)
    .fetch_one(&state.db)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            ApiError::BadRequest(format!("login '{}' already exists", req.login))
        }
        _ => ApiError::from(e),
    })?;
    let u: UserOut = sqlx::query_as::<_, UserOut>(
        "SELECT id, customer_id, role_id, login, email, is_active, \
                must_change_password, last_login_at, created_at, updated_at \
         FROM users WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, Json(u)))
}

#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    pub email: Option<String>,
    pub role_id: Option<i64>,
    pub is_active: Option<bool>,
}

async fn update(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateUserRequest>,
) -> Result<Json<UserOut>, ApiError> {
    require_permission(&state.db, user.role_id, "users.write").await?;
    let _existing: UserOut = sqlx::query_as::<_, UserOut>(
        "SELECT id, customer_id, role_id, login, email, is_active, \
                must_change_password, last_login_at, created_at, updated_at \
         FROM users WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    if let Some(role) = req.role_id
        && sqlx::query_scalar::<_, i64>("SELECT 1 FROM user_roles WHERE id = ?")
            .bind(role)
            .fetch_optional(&state.db)
            .await?
            .is_none()
    {
        return Err(ApiError::BadRequest("unknown role_id".into()));
    }
    sqlx::query(
        "UPDATE users SET \
            email      = COALESCE(?, email), \
            role_id    = COALESCE(?, role_id), \
            is_active  = COALESCE(?, is_active), \
            updated_at = datetime('now') \
         WHERE id = ?",
    )
    .bind(&req.email)
    .bind(req.role_id)
    .bind(req.is_active)
    .bind(id)
    .execute(&state.db)
    .await?;
    let u: UserOut = sqlx::query_as::<_, UserOut>(
        "SELECT id, customer_id, role_id, login, email, is_active, \
                must_change_password, last_login_at, created_at, updated_at \
         FROM users WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(u))
}

async fn delete(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    require_permission(&state.db, user.role_id, "users.write").await?;
    if id == user.id {
        return Err(ApiError::BadRequest("cannot delete yourself".into()));
    }
    let res = sqlx::query("DELETE FROM users WHERE id = ? AND customer_id = ?")
        .bind(id)
        .bind(user.customer_id)
        .execute(&state.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct SetPasswordRequest {
    pub new_password: String,
}

async fn set_password(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<SetPasswordRequest>,
) -> Result<StatusCode, ApiError> {
    // Users may always change their own password; otherwise needs users.write.
    if id != user.id {
        require_permission(&state.db, user.role_id, "users.write").await?;
    }
    if req.new_password.len() < 8 {
        return Err(ApiError::BadRequest(
            "password must be at least 8 characters".into(),
        ));
    }
    let phc = crypto::hash_password(&req.new_password).map_err(|_| ApiError::Internal)?;
    let res = sqlx::query(
        "UPDATE users SET password_hash = ?, must_change_password = 0, updated_at = datetime('now') \
         WHERE id = ? AND customer_id = ?",
    )
    .bind(&phc)
    .bind(id)
    .bind(user.customer_id)
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}
