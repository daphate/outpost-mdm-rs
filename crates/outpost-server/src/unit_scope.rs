//! Подразделение (org-unit) query scoping.
//!
//! A user assigned to a `unit_id` sees only devices in that unit and its
//! descendants; a super-admin, or a user with no unit, sees the whole tenant.
//! This is the enforcement layer behind the situational maps and the device
//! list — the "подразделение" axis added in migration 0031, alongside the
//! existing tenant(`customer_id`) and role axes.

use crate::auth_extract::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Resolved unit visibility for one request.
pub struct UnitScope {
    /// When false, the user sees every unit of the tenant (no unit filter is
    /// applied). When true, only devices whose `unit_id` is in `ids_json`.
    pub scoped: bool,
    /// JSON array of visible unit ids (the user's unit + all descendants).
    /// `"[]"` when unscoped. Bound into [`UnitScope::CLAUSE`] via `json_each`.
    pub ids_json: String,
}

impl UnitScope {
    /// SQL boolean fragment to AND into a device query's WHERE clause. The
    /// filtered column MUST be the device's `unit_id`. Consumes two binds, in
    /// order: [`UnitScope::scoped_flag`] then [`UnitScope::ids_json`].
    ///
    /// `scoped = 0` short-circuits to "visible" (tenant-wide). Otherwise the
    /// row's `unit_id` must appear in the JSON id set — a `NULL` unit_id
    /// (unassigned device) is therefore excluded from a scoped view.
    pub const CLAUSE: &'static str = "(? = 0 OR unit_id IN (SELECT value FROM json_each(?)))";

    pub fn scoped_flag(&self) -> i64 {
        i64::from(self.scoped)
    }
}

/// Compute the caller's unit visibility. Cheap for the common cases
/// (super-admin / no unit → no query); runs one recursive-CTE walk only when
/// the user is genuinely unit-scoped.
pub async fn resolve(state: &AppState, user: &AuthUser) -> Result<UnitScope, ApiError> {
    if user.is_super_admin() || user.unit_id.is_none() {
        return Ok(UnitScope {
            scoped: false,
            ids_json: "[]".to_string(),
        });
    }
    let root = user.unit_id.unwrap();
    // Root is tenant-checked in the base case; recursion only follows parent_id
    // within `units`, so descendants stay in the same tenant. A missing/foreign
    // root yields an empty set → the scoped user sees no devices (fail-closed).
    let ids: Vec<i64> = sqlx::query_scalar(
        "WITH RECURSIVE sub(id) AS ( \
             SELECT id FROM units WHERE id = ? AND customer_id = ? \
             UNION ALL \
             SELECT u.id FROM units u JOIN sub ON u.parent_id = sub.id \
         ) SELECT id FROM sub",
    )
    .bind(root)
    .bind(user.customer_id)
    .fetch_all(&state.db)
    .await?;
    let ids_json = serde_json::to_string(&ids).unwrap_or_else(|_| "[]".to_string());
    Ok(UnitScope {
        scoped: true,
        ids_json,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_extract::SUPER_ADMIN_ROLE_ID;
    use crate::state::test_state;

    fn user(customer_id: i64, role_id: i64, unit_id: Option<i64>) -> AuthUser {
        AuthUser {
            id: 1,
            customer_id,
            role_id,
            login: "t".into(),
            unit_id,
        }
    }

    fn ids(s: &UnitScope) -> Vec<i64> {
        let mut v: Vec<i64> = serde_json::from_str(&s.ids_json).unwrap();
        v.sort();
        v
    }

    #[tokio::test]
    async fn super_admin_and_no_unit_are_unscoped() {
        let st = test_state().await;
        assert!(
            !resolve(&st, &user(1, SUPER_ADMIN_ROLE_ID, Some(5)))
                .await
                .unwrap()
                .scoped,
            "super-admin bypasses unit scoping even when assigned a unit"
        );
        assert!(
            !resolve(&st, &user(1, 2, None)).await.unwrap().scoped,
            "a user with no unit sees the whole tenant"
        );
    }

    #[tokio::test]
    async fn subtree_includes_all_descendants_only() {
        let st = test_state().await;
        // battalion(1) → company A(2), company B(3); A → platoon(4).
        sqlx::query(
            "INSERT INTO units (id,customer_id,parent_id,name) VALUES \
                (1,1,NULL,'bat'),(2,1,1,'A'),(3,1,1,'B'),(4,1,2,'plA1')",
        )
        .execute(&st.db)
        .await
        .unwrap();

        // Scoped to the battalion → the whole subtree.
        let s = resolve(&st, &user(1, 2, Some(1))).await.unwrap();
        assert!(s.scoped);
        assert_eq!(ids(&s), vec![1, 2, 3, 4]);

        // Scoped to company A → itself + its platoon, NOT sibling B or the parent.
        let s2 = resolve(&st, &user(1, 2, Some(2))).await.unwrap();
        assert_eq!(ids(&s2), vec![2, 4]);
    }

    #[tokio::test]
    async fn foreign_or_missing_unit_fails_closed() {
        let st = test_state().await;
        // Unit 9 does not exist → scoped with an empty set → the user sees nothing.
        let s = resolve(&st, &user(1, 2, Some(9))).await.unwrap();
        assert!(s.scoped);
        assert!(ids(&s).is_empty());
    }
}
