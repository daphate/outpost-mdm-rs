//! Pagination envelope + query parameters shared across CRUD endpoints.

use serde::{Deserialize, Serialize};

/// Default page size returned when the client omits `?limit=`.
pub const DEFAULT_LIMIT: i64 = 50;

/// Hard cap on `?limit=`. Clients may not ask for more than this per page.
pub const MAX_LIMIT: i64 = 200;

/// Standard list-endpoint query string: `?limit=N&offset=M`.
#[derive(Debug, Deserialize)]
pub struct PageParams {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

impl PageParams {
    /// Clamp to safe ranges; never trust raw client input.
    pub fn clamp(&self) -> (i64, i64) {
        let limit = self.limit.clamp(1, MAX_LIMIT);
        let offset = self.offset.max(0);
        (limit, offset)
    }
}

impl Default for PageParams {
    fn default() -> Self {
        Self {
            limit: DEFAULT_LIMIT,
            offset: 0,
        }
    }
}

fn default_limit() -> i64 {
    DEFAULT_LIMIT
}

/// Common list response shape — `{items, total, limit, offset}`.
#[derive(Debug, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_caps_max_limit() {
        let p = PageParams {
            limit: 10_000,
            offset: -1,
        };
        let (limit, offset) = p.clamp();
        assert_eq!(limit, MAX_LIMIT);
        assert_eq!(offset, 0);
    }

    #[test]
    fn clamp_promotes_zero_limit() {
        let p = PageParams {
            limit: 0,
            offset: 5,
        };
        let (limit, offset) = p.clamp();
        assert_eq!(limit, 1);
        assert_eq!(offset, 5);
    }
}
