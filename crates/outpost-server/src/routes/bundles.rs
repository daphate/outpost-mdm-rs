//! `/api/v1/bundles` — MDM bootstrap bundle assignment endpoints.
//!
//! Контракт: см. `F:\projects\tactical-ar-hud\tools\CONTENT-DISTRIBUTION-CONTRACT.md`
//! §«Канал 2: bundles[]» + INSIGHT-054 (soldier-v31 bundle 2026-06-03).
//!
//! Endpoints:
//!   - POST   /api/v1/bundles/{bundle_id}/assign            — назначить
//!   - GET    /api/v1/bundles/assignments                   — список (с фильтрами)
//!   - DELETE /api/v1/bundles/assignments/{id}              — отозвать
//!   - GET    /api/v1/devices/{device_id}/bundles           — эффективные bundle'ы
//!
//! Permission gates: `bundles.read` / `bundles.write`.

use crate::auth_extract::AuthUser;
use crate::error::ApiError;
use crate::page::{Page, PageParams};
use crate::permission::require_permission;
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{delete as axum_delete, get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/bundles/{bundle_id}/assign", post(create_assignment))
        .route("/api/v1/bundles/assignments", get(list_assignments))
        .route(
            "/api/v1/bundles/assignments/{id}",
            axum_delete(delete_assignment),
        )
        .route(
            "/api/v1/devices/{device_id}/bundles",
            get(list_effective_for_device),
        )
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct BundleAssignment {
    pub id: i64,
    pub customer_id: i64,
    pub bundle_id: String,
    pub target_type: String,
    pub target_id: i64,
    pub priority: i64,
    pub assigned_by_user_id: Option<i64>,
    pub assigned_at: DateTime<Utc>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateAssignmentReq {
    /// "device" | "group" | "customer".
    pub target_type: String,
    /// id из таблицы devices/groups/customers (зависит от target_type).
    pub target_id: i64,
    #[serde(default = "default_priority")]
    pub priority: i64,
    pub notes: Option<String>,
}

fn default_priority() -> i64 {
    100
}

#[derive(Debug, Deserialize)]
pub struct ListFilter {
    pub bundle_id: Option<String>,
    pub target_type: Option<String>,
    pub target_id: Option<i64>,
}

async fn create_assignment(
    user: AuthUser,
    State(state): State<AppState>,
    Path(bundle_id): Path<String>,
    Json(req): Json<CreateAssignmentReq>,
) -> Result<(StatusCode, Json<BundleAssignment>), ApiError> {
    require_permission(&state.db, user.role_id, "bundles.write").await?;

    if !["device", "group", "customer"].contains(&req.target_type.as_str()) {
        return Err(ApiError::BadRequest(
            "target_type must be 'device', 'group', or 'customer'".into(),
        ));
    }
    if bundle_id.is_empty() || bundle_id.len() > 128 {
        return Err(ApiError::BadRequest(
            "bundle_id length must be 1..128".into(),
        ));
    }

    // Idempotent upsert via UNIQUE(customer_id, bundle_id, target_type, target_id).
    // Updates priority/notes/assigned_at on conflict — assigned_by_user_id
    // тоже refresh'ится чтобы аудит видел кто последний раз менял.
    let row: BundleAssignment = sqlx::query_as::<_, BundleAssignment>(
        "INSERT INTO bundle_assignments(customer_id, bundle_id, target_type, target_id, \
                                        priority, assigned_by_user_id, notes) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(customer_id, bundle_id, target_type, target_id) DO UPDATE SET \
             priority = excluded.priority, \
             assigned_by_user_id = excluded.assigned_by_user_id, \
             assigned_at = datetime('now'), \
             notes = excluded.notes \
         RETURNING id, customer_id, bundle_id, target_type, target_id, priority, \
                   assigned_by_user_id, assigned_at, notes",
    )
    .bind(user.customer_id)
    .bind(&bundle_id)
    .bind(&req.target_type)
    .bind(req.target_id)
    .bind(req.priority)
    .bind(user.id)
    .bind(req.notes.as_deref())
    .fetch_one(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(row)))
}

async fn list_assignments(
    user: AuthUser,
    State(state): State<AppState>,
    Query(page): Query<PageParams>,
    Query(f): Query<ListFilter>,
) -> Result<Json<Page<BundleAssignment>>, ApiError> {
    require_permission(&state.db, user.role_id, "bundles.read").await?;
    let (limit, offset) = page.clamp();

    let mut sql = String::from(
        "SELECT id, customer_id, bundle_id, target_type, target_id, priority, \
                assigned_by_user_id, assigned_at, notes \
         FROM bundle_assignments WHERE customer_id = ?",
    );
    if f.bundle_id.is_some() {
        sql.push_str(" AND bundle_id = ?");
    }
    if f.target_type.is_some() {
        sql.push_str(" AND target_type = ?");
    }
    if f.target_id.is_some() {
        sql.push_str(" AND target_id = ?");
    }
    sql.push_str(" ORDER BY assigned_at DESC LIMIT ? OFFSET ?");

    let mut q = sqlx::query_as::<_, BundleAssignment>(&sql).bind(user.customer_id);
    if let Some(ref s) = f.bundle_id {
        q = q.bind(s);
    }
    if let Some(ref s) = f.target_type {
        q = q.bind(s);
    }
    if let Some(s) = f.target_id {
        q = q.bind(s);
    }
    q = q.bind(limit).bind(offset);

    let items = q.fetch_all(&state.db).await?;

    let mut sql_count =
        String::from("SELECT COUNT(*) FROM bundle_assignments WHERE customer_id = ?");
    if f.bundle_id.is_some() {
        sql_count.push_str(" AND bundle_id = ?");
    }
    if f.target_type.is_some() {
        sql_count.push_str(" AND target_type = ?");
    }
    if f.target_id.is_some() {
        sql_count.push_str(" AND target_id = ?");
    }
    let mut qc = sqlx::query_scalar::<_, i64>(&sql_count).bind(user.customer_id);
    if let Some(ref s) = f.bundle_id {
        qc = qc.bind(s);
    }
    if let Some(ref s) = f.target_type {
        qc = qc.bind(s);
    }
    if let Some(s) = f.target_id {
        qc = qc.bind(s);
    }
    let total = qc.fetch_one(&state.db).await?;

    Ok(Json(Page {
        items,
        total,
        limit,
        offset,
    }))
}

async fn delete_assignment(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    require_permission(&state.db, user.role_id, "bundles.write").await?;

    let rows = sqlx::query(
        "DELETE FROM bundle_assignments WHERE id = ? AND customer_id = ?",
    )
    .bind(id)
    .bind(user.customer_id)
    .execute(&state.db)
    .await?
    .rows_affected();

    if rows == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize)]
pub struct EffectiveBundle {
    pub bundle_id: String,
    pub source: String,
    pub priority: i64,
    pub assigned_at: DateTime<Utc>,
}

/// Resolve **effective** bundle assignments for a device, walking through
/// device → group(s) → customer chain. Higher specificity wins; within
/// same specificity, higher `priority` wins.
async fn list_effective_for_device(
    user: AuthUser,
    State(state): State<AppState>,
    Path(device_id): Path<i64>,
) -> Result<Json<Vec<EffectiveBundle>>, ApiError> {
    require_permission(&state.db, user.role_id, "bundles.read").await?;

    // Verify device is in customer's scope (avoid leaking across tenants).
    let device_customer: Option<i64> = sqlx::query_scalar(
        "SELECT customer_id FROM devices WHERE id = ?",
    )
    .bind(device_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(dc) = device_customer else {
        return Err(ApiError::NotFound);
    };
    if dc != user.customer_id {
        return Err(ApiError::Forbidden);
    }

    // Union: device + groups + customer. SQLite has no CTE-friendly OR
    // resolver, fetch each tier and merge in Rust — preserves explicit
    // specificity ordering (device > group > customer).
    let device_rows: Vec<(String, i64, DateTime<Utc>)> = sqlx::query_as(
        "SELECT bundle_id, priority, assigned_at FROM bundle_assignments \
         WHERE customer_id = ? AND target_type = 'device' AND target_id = ?",
    )
    .bind(user.customer_id)
    .bind(device_id)
    .fetch_all(&state.db)
    .await?;

    let group_rows: Vec<(String, i64, DateTime<Utc>)> = sqlx::query_as(
        "SELECT ba.bundle_id, ba.priority, ba.assigned_at \
         FROM bundle_assignments ba \
         WHERE ba.customer_id = ? \
           AND ba.target_type = 'group' \
           AND ba.target_id IN (SELECT group_id FROM device_groups WHERE device_id = ?)",
    )
    .bind(user.customer_id)
    .bind(device_id)
    .fetch_all(&state.db)
    .await?;

    let customer_rows: Vec<(String, i64, DateTime<Utc>)> = sqlx::query_as(
        "SELECT bundle_id, priority, assigned_at FROM bundle_assignments \
         WHERE customer_id = ? AND target_type = 'customer' AND target_id = ?",
    )
    .bind(user.customer_id)
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;

    // Merge with specificity-priority — keep highest-tier source per bundle_id.
    let mut out: std::collections::HashMap<String, EffectiveBundle> =
        std::collections::HashMap::new();
    let push = |out: &mut std::collections::HashMap<String, EffectiveBundle>,
                rows: Vec<(String, i64, DateTime<Utc>)>,
                source: &str| {
        for (bid, prio, ts) in rows {
            // Insert if not present (higher-tier-iterated-first means we keep first).
            out.entry(bid.clone()).or_insert(EffectiveBundle {
                bundle_id: bid,
                source: source.to_string(),
                priority: prio,
                assigned_at: ts,
            });
        }
    };
    push(&mut out, device_rows, "device");
    push(&mut out, group_rows, "group");
    push(&mut out, customer_rows, "customer");

    let mut result: Vec<EffectiveBundle> = out.into_values().collect();
    result.sort_by(|a, b| {
        // Higher priority first; tie-break by assigned_at desc.
        b.priority
            .cmp(&a.priority)
            .then_with(|| b.assigned_at.cmp(&a.assigned_at))
    });
    Ok(Json(result))
}
