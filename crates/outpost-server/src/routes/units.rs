//! `/api/v1/units` — org-unit (subdivision) tree CRUD.
//!
//! Units form a per-customer self-referencing hierarchy (`parent_id`). They add
//! the "подразделение" scoping axis alongside customer(tenant)/role; devices and
//! users carry a nullable `unit_id`. Deleting a unit cascades to its child units
//! and NULLs the `unit_id` on any devices/users assigned to it (see migration
//! 0031). Kept separate from the flat `groups` label set.

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
        .route("/api/v1/units", get(list).post(create))
        .route("/api/v1/units/{id}", get(get_one).put(update).delete(delete))
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Unit {
    pub id: i64,
    pub customer_id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const SELECT_COLS: &str =
    "id, customer_id, parent_id, name, description, created_at, updated_at";

async fn list(
    user: AuthUser,
    State(state): State<AppState>,
    Query(page): Query<PageParams>,
) -> Result<Json<Page<Unit>>, ApiError> {
    require_permission(&state.db, user.role_id, "units.read").await?;
    let (limit, offset) = page.clamp();
    let items: Vec<Unit> = sqlx::query_as::<_, Unit>(&format!(
        "SELECT {SELECT_COLS} FROM units WHERE customer_id = ? \
         ORDER BY COALESCE(parent_id, 0), name LIMIT ? OFFSET ?"
    ))
    .bind(user.customer_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM units WHERE customer_id = ?")
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
) -> Result<Json<Unit>, ApiError> {
    require_permission(&state.db, user.role_id, "units.read").await?;
    let u: Option<Unit> = sqlx::query_as::<_, Unit>(&format!(
        "SELECT {SELECT_COLS} FROM units WHERE id = ? AND customer_id = ?"
    ))
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?;
    u.map(Json).ok_or(ApiError::NotFound)
}

/// Verify a proposed parent unit exists inside the caller's tenant.
async fn require_parent_in_tenant(
    state: &AppState,
    customer_id: i64,
    parent_id: Option<i64>,
) -> Result<(), ApiError> {
    if let Some(pid) = parent_id {
        let ok: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM units WHERE id = ? AND customer_id = ?")
                .bind(pid)
                .bind(customer_id)
                .fetch_optional(&state.db)
                .await?;
        if ok.is_none() {
            return Err(ApiError::BadRequest("parent unit not found in tenant".into()));
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct CreateUnitRequest {
    pub name: String,
    #[serde(default)]
    pub parent_id: Option<i64>,
    #[serde(default)]
    pub description: Option<String>,
}

async fn create(
    user: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<CreateUnitRequest>,
) -> Result<(StatusCode, Json<Unit>), ApiError> {
    require_permission(&state.db, user.role_id, "units.write").await?;
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    require_parent_in_tenant(&state, user.customer_id, req.parent_id).await?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO units (customer_id, parent_id, name, description) \
         VALUES (?, ?, ?, ?) RETURNING id",
    )
    .bind(user.customer_id)
    .bind(req.parent_id)
    .bind(&req.name)
    .bind(&req.description)
    .fetch_one(&state.db)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            ApiError::BadRequest(format!("unit '{}' already exists under this parent", req.name))
        }
        _ => ApiError::from(e),
    })?;
    let u: Unit = sqlx::query_as::<_, Unit>(&format!("SELECT {SELECT_COLS} FROM units WHERE id = ?"))
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    Ok((StatusCode::CREATED, Json(u)))
}

#[derive(Debug, Deserialize)]
pub struct UpdateUnitRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    /// Reparent under another unit (must be in-tenant and not self). To move a
    /// unit to the root, delete + recreate — COALESCE cannot express "set NULL".
    #[serde(default)]
    pub parent_id: Option<i64>,
}

async fn update(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateUnitRequest>,
) -> Result<Json<Unit>, ApiError> {
    require_permission(&state.db, user.role_id, "units.write").await?;
    let _existing: Unit = sqlx::query_as::<_, Unit>(&format!(
        "SELECT {SELECT_COLS} FROM units WHERE id = ? AND customer_id = ?"
    ))
    .bind(id)
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    if req.parent_id == Some(id) {
        return Err(ApiError::BadRequest("a unit cannot be its own parent".into()));
    }
    require_parent_in_tenant(&state, user.customer_id, req.parent_id).await?;
    sqlx::query(
        "UPDATE units SET \
            name        = COALESCE(?, name), \
            description = COALESCE(?, description), \
            parent_id   = COALESCE(?, parent_id), \
            updated_at  = datetime('now') \
         WHERE id = ?",
    )
    .bind(&req.name)
    .bind(&req.description)
    .bind(req.parent_id)
    .bind(id)
    .execute(&state.db)
    .await?;
    let u: Unit = sqlx::query_as::<_, Unit>(&format!("SELECT {SELECT_COLS} FROM units WHERE id = ?"))
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
    require_permission(&state.db, user.role_id, "units.write").await?;
    let res = sqlx::query("DELETE FROM units WHERE id = ? AND customer_id = ?")
        .bind(id)
        .bind(user.customer_id)
        .execute(&state.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}
