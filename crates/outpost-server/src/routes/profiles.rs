//! `/api/v1/profile` — per-tenant customer profile (situational platform Ф6).
//!
//! One profile per customer (PK `customer_id`, migration 0035). `GET` returns
//! the caller's tenant profile; `PUT` upserts it. The profile is **data, not a
//! code fork** — it feeds both the multi-tenant runtime (by `customer_id`) and
//! the per-customer on-prem build (see `docs/ON-PREM-BUILD.md`): enabled device
//! classes, feature flags, domain, branding, enrollment/TLS params.
//!
//! JSON columns are validated for shape here (`enabled_classes` must be a JSON
//! array of strings; `feature_flags`/`branding`/`enrollment` must be JSON
//! objects) so a malformed profile can never reach the export tool or the
//! runtime readers.

use crate::auth_extract::AuthUser;
use crate::error::ApiError;
use crate::permission::require_permission;
use crate::state::AppState;
use axum::{Json, Router, extract::State, routing::get};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/profile", get(get_profile).put(put_profile))
}

const COLS: &str = "customer_id, single_tenant, enabled_classes, feature_flags, \
                    domain, branding_json, enrollment_json, created_at, updated_at";

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct CustomerProfile {
    pub customer_id: i64,
    pub single_tenant: bool,
    /// JSON array of allowed `device_class` values (string), or null.
    pub enabled_classes: Option<String>,
    /// JSON object of feature flags (ballistics/bearing/players/…), or null.
    pub feature_flags: Option<String>,
    pub domain: Option<String>,
    pub branding_json: Option<String>,
    pub enrollment_json: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

async fn get_profile(
    user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<CustomerProfile>, ApiError> {
    require_permission(&state.db, user.role_id, "configurations.read").await?;
    let p: Option<CustomerProfile> = sqlx::query_as::<_, CustomerProfile>(&format!(
        "SELECT {COLS} FROM customer_profiles WHERE customer_id = ?"
    ))
    .bind(user.customer_id)
    .fetch_optional(&state.db)
    .await?;
    p.map(Json).ok_or(ApiError::NotFound)
}

#[derive(Debug, Deserialize)]
pub struct ProfileUpsert {
    #[serde(default)]
    pub single_tenant: Option<bool>,
    #[serde(default)]
    pub enabled_classes: Option<Value>,
    #[serde(default)]
    pub feature_flags: Option<Value>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub branding: Option<Value>,
    #[serde(default)]
    pub enrollment: Option<Value>,
}

fn json_array_of_strings(v: &Value, field: &str) -> Result<String, ApiError> {
    let arr = v
        .as_array()
        .ok_or_else(|| ApiError::BadRequest(format!("{field} must be a JSON array")))?;
    if !arr.iter().all(Value::is_string) {
        return Err(ApiError::BadRequest(format!(
            "{field} must be an array of strings"
        )));
    }
    Ok(v.to_string())
}

fn json_object(v: &Value, field: &str) -> Result<String, ApiError> {
    if !v.is_object() {
        return Err(ApiError::BadRequest(format!(
            "{field} must be a JSON object"
        )));
    }
    Ok(v.to_string())
}

async fn put_profile(
    user: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<ProfileUpsert>,
) -> Result<Json<CustomerProfile>, ApiError> {
    require_permission(&state.db, user.role_id, "configurations.write").await?;

    let enabled_classes = req
        .enabled_classes
        .as_ref()
        .map(|v| json_array_of_strings(v, "enabled_classes"))
        .transpose()?;
    let feature_flags = req
        .feature_flags
        .as_ref()
        .map(|v| json_object(v, "feature_flags"))
        .transpose()?;
    let branding = req
        .branding
        .as_ref()
        .map(|v| json_object(v, "branding"))
        .transpose()?;
    let enrollment = req
        .enrollment
        .as_ref()
        .map(|v| json_object(v, "enrollment"))
        .transpose()?;
    let domain = req
        .domain
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);

    // Upsert. COALESCE keeps existing values for omitted fields; `single_tenant`
    // defaults to 0 on first insert. Each field is bound once for the INSERT and
    // once for the ON CONFLICT UPDATE (positional binds, in order).
    sqlx::query(
        "INSERT INTO customer_profiles \
            (customer_id, single_tenant, enabled_classes, feature_flags, domain, \
             branding_json, enrollment_json, updated_at) \
         VALUES (?, COALESCE(?, 0), ?, ?, ?, ?, ?, datetime('now')) \
         ON CONFLICT(customer_id) DO UPDATE SET \
            single_tenant   = COALESCE(?, single_tenant), \
            enabled_classes = COALESCE(?, enabled_classes), \
            feature_flags   = COALESCE(?, feature_flags), \
            domain          = COALESCE(?, domain), \
            branding_json   = COALESCE(?, branding_json), \
            enrollment_json = COALESCE(?, enrollment_json), \
            updated_at      = datetime('now')",
    )
    .bind(user.customer_id)
    .bind(req.single_tenant)
    .bind(&enabled_classes)
    .bind(&feature_flags)
    .bind(&domain)
    .bind(&branding)
    .bind(&enrollment)
    .bind(req.single_tenant)
    .bind(&enabled_classes)
    .bind(&feature_flags)
    .bind(&domain)
    .bind(&branding)
    .bind(&enrollment)
    .execute(&state.db)
    .await?;

    let p: CustomerProfile = sqlx::query_as::<_, CustomerProfile>(&format!(
        "SELECT {COLS} FROM customer_profiles WHERE customer_id = ?"
    ))
    .bind(user.customer_id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(p))
}
