//! Injectable GitHub auth cache for `get_mesh_git_static`.
//!
//! Replaces the process-global `GH_AUTH_CACHE` / `GH_AUTH_CACHE_MISSES` statics
//! (issue #1483). Each `GhAuthCache` owns its TTL'd `(Instant, bool)` slot and
//! its miss counter, so tests can construct a per-fixture cache and drive it
//! without contending on global state.
//!
//! Production wires a single `GhAuthCache::new()` via Tauri `manage` and the
//! async command extracts it as `tauri::State<GhAuthCache>`. Tests construct
//! an isolated fixture with `for_test_with_auth` and a controllable clock via
//! `for_test_with_clock_and_auth`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a cached `check_gh_auth` result is reused across mesh mounts.
const GH_AUTH_CACHE_TTL: Duration = Duration::from_secs(30);

struct Inner {
    slot: Mutex<(Instant, bool)>,
    misses: AtomicU64,
    now_fn: Box<dyn Fn() -> Instant + Send + Sync>,
    auth_fn: Box<dyn Fn() -> bool + Send + Sync>,
}

/// Injectable cache for `check_gh_auth`.
///
/// `Clone` is cheap — one `Arc` bump — because all fields share the same
/// lifecycle. Required for moving the cache into `run_blocking` (`'static`).
#[derive(Clone)]
pub struct GhAuthCache(Arc<Inner>);

impl GhAuthCache {
    fn new_inner(
        expired: Instant,
        now_fn: Box<dyn Fn() -> Instant + Send + Sync>,
        auth_fn: Box<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        Self(Arc::new(Inner {
            slot: Mutex::new((expired, false)),
            misses: AtomicU64::new(0),
            now_fn,
            auth_fn,
        }))
    }

    /// Production constructor: expired entry (first call is a miss), zero
    /// misses, real clock and real GitHub auth checker.
    pub fn new() -> Self {
        let expired = Instant::now() - GH_AUTH_CACHE_TTL - Duration::from_secs(1);
        Self::new_inner(
            expired,
            Box::new(Instant::now),
            Box::new(|| {
                crate::services::github::GitHubClient::new()
                    .map(|c| c.check_auth())
                    .unwrap_or(false)
            }),
        )
    }

    /// Test fixture with a stubbed auth result (no network).
    #[cfg(test)]
    pub fn for_test_with_auth<F>(auth_fn: F) -> Self
    where
        F: Fn() -> bool + Send + Sync + 'static,
    {
        let expired = Instant::now() - GH_AUTH_CACHE_TTL - Duration::from_secs(1);
        Self::new_inner(expired, Box::new(Instant::now), Box::new(auth_fn))
    }

    /// Test fixture with controllable `now` and a stubbed auth result.
    #[cfg(test)]
    pub fn for_test_with_clock_and_auth<N, A>(now_fn: N, auth_fn: A) -> Self
    where
        N: Fn() -> Instant + Send + Sync + 'static,
        A: Fn() -> bool + Send + Sync + 'static,
    {
        let now = now_fn();
        let expired = now - GH_AUTH_CACHE_TTL - Duration::from_secs(1);
        Self::new_inner(expired, Box::new(now_fn), Box::new(auth_fn))
    }

    /// Number of times the underlying auth checker was invoked for this cache.
    pub fn misses(&self) -> u64 {
        self.0.misses.load(Ordering::Relaxed)
    }

    /// Cached wrapper around `auth_fn`. Holds the slot `Mutex` across the
    /// `auth_fn` call so concurrent callers serialize and only the first
    /// performs the HTTPS round-trip (second caller hits the fresh cache).
    /// Runs on `spawn_blocking`, so blocking the worker thread is the
    /// intended coalescing mechanism (see `commands::run_blocking`).
    pub fn check(&self) -> bool {
        let mut guard = self.0.slot.lock().expect("gh-auth cache mutex poisoned");
        let now = (self.0.now_fn)();
        if now.duration_since(guard.0) < GH_AUTH_CACHE_TTL {
            return guard.1;
        }
        let result = (self.0.auth_fn)();
        let stamp = (self.0.now_fn)();
        guard.0 = stamp;
        guard.1 = result;
        self.0.misses.fetch_add(1, Ordering::Relaxed);
        result
    }

    /// Expire the entry so the next `check()` is a miss.
    #[cfg(test)]
    pub fn expire_for_test(&self) {
        let mut guard = self.0.slot.lock().expect("gh-auth cache mutex poisoned");
        let now = (self.0.now_fn)();
        guard.0 = now - GH_AUTH_CACHE_TTL - Duration::from_secs(1);
    }
}

impl Default for GhAuthCache {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for GhAuthCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self
            .0
            .slot
            .lock()
            .map(|g| *g)
            .unwrap_or((Instant::now(), false));
        f.debug_struct("GhAuthCache")
            .field("cached_at", &guard.0)
            .field("value", &guard.1)
            .field("misses", &self.misses())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    #[test]
    fn new_starts_expired_with_zero_misses() {
        let cache = GhAuthCache::new();
        assert_eq!(cache.misses(), 0);
        let _ = cache.check();
        assert_eq!(cache.misses(), 1);
    }

    #[test]
    fn hits_within_ttl() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&calls);
        let cache = GhAuthCache::for_test_with_auth(move || {
            c.fetch_add(1, Ordering::SeqCst);
            true
        });
        for _ in 0..5 {
            assert!(cache.check());
        }
        assert_eq!(cache.misses(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn expires_via_backdate() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&calls);
        let cache = GhAuthCache::for_test_with_auth(move || {
            c.fetch_add(1, Ordering::SeqCst);
            true
        });
        assert!(cache.check());
        assert_eq!(cache.misses(), 1);
        cache.expire_for_test();
        assert!(cache.check());
        assert_eq!(cache.misses(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn controllable_now_advances_ttl() {
        let now_slot = Arc::new(Mutex::new(Instant::now()));
        let slot_clone = Arc::clone(&now_slot);
        let now_fn = move || *slot_clone.lock().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&calls);
        let cache = GhAuthCache::for_test_with_clock_and_auth(now_fn, move || {
            c.fetch_add(1, Ordering::SeqCst);
            false
        });
        let _ = cache.check();
        assert_eq!(cache.misses(), 1);
        let _ = cache.check();
        assert_eq!(cache.misses(), 1);
        {
            let mut guard = now_slot.lock().unwrap();
            *guard += GH_AUTH_CACHE_TTL + Duration::from_secs(1);
        }
        let _ = cache.check();
        assert_eq!(cache.misses(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn clone_shares_state() {
        let cache = GhAuthCache::for_test_with_auth(|| true);
        let cloned = cache.clone();
        assert!(cache.check());
        assert_eq!(cache.misses(), 1);
        assert!(cloned.check());
        assert_eq!(cloned.misses(), 1);
    }

    #[test]
    fn concurrent_callers_coalesce_to_single_miss() {
        let calls = Arc::new(AtomicUsize::new(0));
        let cache = GhAuthCache::for_test_with_auth({
            let calls = Arc::clone(&calls);
            move || {
                std::thread::sleep(Duration::from_millis(80));
                calls.fetch_add(1, Ordering::SeqCst);
                true
            }
        });
        let mut handles = Vec::new();
        for _ in 0..5 {
            let c = cache.clone();
            handles.push(std::thread::spawn(move || c.check()));
        }
        for h in handles {
            assert!(h.join().unwrap());
        }
        assert_eq!(
            cache.misses(),
            1,
            "5 concurrent callers on a cold cache must coalesce to 1 miss"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
