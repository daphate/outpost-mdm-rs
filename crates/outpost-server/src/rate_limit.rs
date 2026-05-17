//! Hand-rolled per-IP token-bucket rate limiter.
//!
//! Used to protect login endpoints from brute-force credential stuffing.
//! Lives in-process — for production deployments behind nginx, the
//! `limit_req_zone` directive provides a coarser first line of defence;
//! this in-app limiter is the second line that survives a misconfigured
//! reverse proxy and that also applies to direct-Docker-port traffic.
//!
//! Defaults: **10 attempts per 5 minutes per IP**, refilling at 1 token
//! per 30 s. A successful or failed login both consume one token.
//!
//! Per-`AppState` map: tests get fresh `AppState` instances so they
//! don't bleed buckets across cases.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Burst capacity — max attempts allowed in a short window.
pub const LOGIN_CAPACITY: f64 = 10.0;

/// Refill rate — tokens added per second.
pub const LOGIN_REFILL_PER_SEC: f64 = 1.0 / 30.0;

/// Drop a bucket from the map if it has not been touched for this long.
/// 1 hour is plenty for an admin panel; idle attackers age out.
const BUCKET_IDLE_TTL_SECS: u64 = 3600;

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

impl Bucket {
    fn fresh(capacity: f64) -> Self {
        Self {
            tokens: capacity,
            last_refill: Instant::now(),
        }
    }

    /// Refill based on elapsed wall-clock time, then attempt to take 1.
    /// Returns `true` if a token was available.
    fn try_take(&mut self, capacity: f64, refill_per_sec: f64) -> bool {
        let now = Instant::now();
        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        self.tokens = (self.tokens + elapsed * refill_per_sec).min(capacity);
        self.last_refill = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Shared per-process rate limiter for login attempts.
#[derive(Clone, Debug)]
pub struct LoginRateLimiter {
    inner: Arc<Mutex<HashMap<IpAddr, Bucket>>>,
    capacity: f64,
    refill_per_sec: f64,
}

impl LoginRateLimiter {
    pub fn new(capacity: f64, refill_per_sec: f64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            capacity,
            refill_per_sec,
        }
    }

    /// Default production-style limits: 10 attempts, ~1 per 30 s.
    pub fn default_login() -> Self {
        Self::new(LOGIN_CAPACITY, LOGIN_REFILL_PER_SEC)
    }

    /// Attempt to consume one token for `ip`. Returns `true` if allowed.
    pub fn try_take(&self, ip: IpAddr) -> bool {
        let mut map = self.inner.lock().expect("login limiter mutex poisoned");
        // Opportunistic GC: every ~100 lookups, drop idle buckets. Cheap.
        if map.len() > 64 {
            let now = Instant::now();
            map.retain(|_, b| {
                now.saturating_duration_since(b.last_refill).as_secs() < BUCKET_IDLE_TTL_SECS
            });
        }
        let bucket = map
            .entry(ip)
            .or_insert_with(|| Bucket::fresh(self.capacity));
        bucket.try_take(self.capacity, self.refill_per_sec)
    }
}

impl Default for LoginRateLimiter {
    fn default() -> Self {
        Self::default_login()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn ip() -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))
    }

    #[test]
    fn first_burst_allowed() {
        let rl = LoginRateLimiter::new(3.0, 0.0);
        assert!(rl.try_take(ip()));
        assert!(rl.try_take(ip()));
        assert!(rl.try_take(ip()));
        // Fourth attempt is rejected — bucket empty, no refill.
        assert!(!rl.try_take(ip()));
    }

    #[test]
    fn buckets_are_per_ip() {
        let rl = LoginRateLimiter::new(1.0, 0.0);
        let a = IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4));
        let b = IpAddr::V4(Ipv4Addr::new(5, 6, 7, 8));
        assert!(rl.try_take(a));
        assert!(rl.try_take(b)); // different IP, its own bucket
        assert!(!rl.try_take(a)); // a is empty
        assert!(!rl.try_take(b)); // b is empty
    }

    #[tokio::test]
    async fn refill_restores_capacity_over_time() {
        // 2 tokens capacity, very fast refill so we don't sleep long.
        let rl = LoginRateLimiter::new(2.0, 50.0); // 50 tokens/sec
        let a = ip();
        assert!(rl.try_take(a));
        assert!(rl.try_take(a));
        assert!(!rl.try_take(a));
        // Wait ~40 ms — should refill ~2 tokens.
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        assert!(rl.try_take(a));
    }
}
