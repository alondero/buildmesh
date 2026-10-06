//! In-process 5-minute usage cache (issue #1657).
//!
//! The cache is the only state the `usage` god-module keeps after the
//! adapter split: wire types live in [`super::types`], fetch dispatch lives
//! behind [`super::adapter::UsageAdapter`], and this module owns the
//! TTL + identity-aware get/set/invalidate surface the catalog consults
//! before dispatch.

use super::adapter::UsageIdentityFingerprint;
use super::outcome::UsageOutcome;
use super::types::ProviderUsage;
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const CACHE_TTL: Duration = Duration::from_secs(300);

#[derive(Clone, Eq, Hash, PartialEq)]
struct CacheKey {
    provider: String,
    identity: UsageIdentityFingerprint,
}

type Cache = HashMap<CacheKey, (Instant, ProviderUsage)>;

// ── Miss coalescing (issue #2021) ────────────────────────────────────────
//
// The 5-minute TTL above collapses *sequential* reads; it cannot collapse a
// burst of *concurrent* cold reads. Desktop and mobile surfaces can both ask
// for a provider at the same instant, and every caller that missed the cache
// issued its own vendor request (profiled: 8 concurrent cold readers produced
// 8 vendor fetches for one credential identity). Vendors rate-limit, and a
// duplicated burst spends the user's quota twice.
//
// `Flight` is the single-flight slot for one `CacheKey`. The first caller for
// a key becomes the Leader and performs the fetch; concurrent callers for the
// SAME key become Followers and block until the leader publishes. Nothing
// waits across a different key, so unrelated providers (and different
// credentials for the same provider) are never serialized behind each other.
//
// The leader performs its fetch with NO cache mutex held — the in-flight
// registry is released before the fetch starts and only re-acquired to
// publish. A global lock across the vendor round-trip would defeat the point:
// it would serialize every provider behind the slowest one.

/// One in-progress vendor fetch. Holds the leader's result for followers.
///
/// `Condvar` rather than a shared future because the adapter seam is
/// synchronous (`fetch` returns `UsageOutcome`); the command layer runs it on
/// a blocking thread via `spawn_blocking`.
pub(crate) struct Flight {
    result: Mutex<Option<(UsageOutcome, ProviderUsage)>>,
    ready: Condvar,
}

impl Flight {
    fn new() -> Self {
        Self {
            result: Mutex::new(None),
            ready: Condvar::new(),
        }
    }

    /// Publish the leader's result and wake every follower. Idempotent: a
    /// second publish (the guard's `Drop` after an explicit `complete`) is a
    /// no-op, so the leader's real result is never overwritten by the panic
    /// fallback.
    fn publish(&self, value: (UsageOutcome, ProviderUsage)) {
        let mut guard = self.result.lock().unwrap();
        if guard.is_none() {
            *guard = Some(value);
            self.ready.notify_all();
        }
    }

    /// Block until the leader publishes. Spurious wakeups are handled by the
    /// `is_some` re-check inside the loop rather than by trusting one
    /// notification.
    pub(crate) fn wait(&self) -> (UsageOutcome, ProviderUsage) {
        let mut guard = self.result.lock().unwrap();
        loop {
            if let Some(value) = guard.as_ref() {
                return value.clone();
            }
            guard = self.ready.wait(guard).unwrap();
        }
    }
}

/// Which side of the single-flight a caller landed on.
pub(crate) enum FlightTicket {
    /// This caller owns the vendor fetch. It must complete (or drop) the
    /// returned guard exactly once.
    Leader(Arc<Flight>),
    /// Another caller is already fetching this key. Wait for its result.
    Follower(Arc<Flight>),
}

/// Releases a Leader's in-flight registration exactly once, on every exit
/// path including a panic, so a Follower can never block forever.
///
/// `Drop` publishes an `Unavailable` fallback when the leader never published
/// a real result. That keeps a failed fetch *visible* (the panel renders the
/// red error copy and the gate keeps the row) instead of hanging the caller,
/// and it mirrors the existing contract that failures are returned, not
/// swallowed.
pub(crate) struct LeaderGuard<'a> {
    cache: &'a UsageCache,
    provider: &'a str,
    identity: &'a UsageIdentityFingerprint,
    flight: Arc<Flight>,
    /// Set by [`LeaderGuard::complete`] so `Drop` skips the fallback. A
    /// successful leader already published the real result; re-publishing
    /// would be a no-op for correctness but still cost a heap `String`, a
    /// throwaway `into_usage` projection, and a mutex lock on every success.
    completed: bool,
}

impl<'a> LeaderGuard<'a> {
    pub(crate) fn new(
        cache: &'a UsageCache,
        provider: &'a str,
        identity: &'a UsageIdentityFingerprint,
        flight: Arc<Flight>,
    ) -> Self {
        Self {
            cache,
            provider,
            identity,
            flight,
            completed: false,
        }
    }

    pub(crate) fn complete(mut self, value: (UsageOutcome, ProviderUsage)) {
        // Publish first, then disarm: if `publish` itself unwinds, the
        // fallback still reaches the followers rather than leaving them
        // blocked on a flight nobody will ever complete.
        self.flight.publish(value);
        self.completed = true;
    }
}

impl Drop for LeaderGuard<'_> {
    fn drop(&mut self) {
        // Only on the panic path — a completed leader disarmed itself. The
        // slot release below runs on EVERY exit path and must not be guarded:
        // skipping it would strand the key in `in_flight` and make every later
        // caller wait on a flight nobody owns.
        if !self.completed {
            let reason = "usage fetch did not complete".to_string();
            self.flight.publish((
                UsageOutcome::Unavailable {
                    reason: reason.clone(),
                },
                UsageOutcome::Unavailable { reason }.into_usage(self.provider),
            ));
        }
        self.cache
            .end_flight(self.provider, self.identity, &self.flight);
    }
}

pub(crate) struct UsageCache {
    entries: Mutex<Cache>,
    in_flight: Mutex<HashMap<CacheKey, Arc<Flight>>>,
}

impl UsageCache {
    pub(crate) fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            in_flight: Mutex::new(HashMap::new()),
        }
    }

    /// Claim the single-flight slot for `(provider, identity)`, or join the
    /// one already in progress. The registry lock is held only for the map
    /// lookup — never across the fetch.
    ///
    /// Force-refresh callers join the same slot: the leader is performing a
    /// live vendor request, so its result is fresh by construction and
    /// satisfies an explicit refresh (a TTL cache hit would not).
    pub(crate) fn join_flight(
        &self,
        provider: &str,
        identity: &UsageIdentityFingerprint,
    ) -> FlightTicket {
        let key = CacheKey {
            provider: provider.to_string(),
            identity: identity.clone(),
        };
        let mut in_flight = self.in_flight.lock().unwrap();
        match in_flight.get(&key) {
            Some(flight) => FlightTicket::Follower(Arc::clone(flight)),
            None => {
                let flight = Arc::new(Flight::new());
                in_flight.insert(key, Arc::clone(&flight));
                FlightTicket::Leader(flight)
            }
        }
    }

    /// Drop the in-flight registration, but only if this exact flight still
    /// owns the key. Idempotent, so the explicit `complete` path and the
    /// guard's `Drop` can both call it without a double-remove race with a
    /// later fetch that already claimed the slot.
    fn end_flight(
        &self,
        provider: &str,
        identity: &UsageIdentityFingerprint,
        flight: &Arc<Flight>,
    ) {
        let key = CacheKey {
            provider: provider.to_string(),
            identity: identity.clone(),
        };
        let mut in_flight = self.in_flight.lock().unwrap();
        if in_flight
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, flight))
        {
            in_flight.remove(&key);
        }
    }

    pub(crate) fn get(
        &self,
        provider: &str,
        identity: &UsageIdentityFingerprint,
    ) -> Option<ProviderUsage> {
        let guard = self.entries.lock().unwrap();
        let key = CacheKey {
            provider: provider.to_string(),
            identity: identity.clone(),
        };
        guard.get(&key).and_then(|(instant, usage)| {
            if instant.elapsed() < CACHE_TTL {
                Some(usage.clone())
            } else {
                None
            }
        })
    }

    pub(crate) fn set(
        &self,
        provider: &str,
        identity: UsageIdentityFingerprint,
        usage: ProviderUsage,
    ) {
        let mut guard = self.entries.lock().unwrap();
        guard.insert(
            CacheKey {
                provider: provider.to_string(),
                identity,
            },
            (Instant::now(), usage),
        );
    }

    fn clear(&self) {
        self.entries.lock().unwrap().clear();
    }

    /// Test-only: drop the TTL entries so the next read exercises a real miss
    /// (and the in-flight slot) rather than a cache hit.
    #[cfg(test)]
    pub(crate) fn clear_for_test(&self) {
        self.clear();
    }

    fn remove_provider(&self, provider: &str) {
        self.entries
            .lock()
            .unwrap()
            .retain(|key, _| key.provider != provider);
    }

    fn provider_age(&self, provider: &str) -> Option<Duration> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .filter(|(key, _)| key.provider == provider)
            .map(|(_, (instant, _))| instant.elapsed())
            .min()
    }
}

pub(crate) static USAGE_CACHE: once_cell::sync::Lazy<UsageCache> =
    once_cell::sync::Lazy::new(UsageCache::new);

// ── Cache-age wire gap (issue #857 follow-up — deferred) ─────────────────
//
// The UI's "Refreshed X ago" indicator is currently stamped on the React side
// at the moment `loadMeters` resolves, NOT at the moment each provider's vendor
// endpoint returned. Because [`UsageCache::get`] may short-circuit before any
// HTTP round-trip, the indicator mislabels a pure cache hit as a fresh fetch.
//
// The clean fix is a wire-shape change, deliberately deferred to its own PR
// (issue #857 body flags the cross-cutting consequences — Rust struct +
// ts-rs regen + new React-side cache-vs-fresh semantics — as warranting a
// separate commit). When picked up:
//
//   1. Add `cached_at: Option<i64>` (epoch ms) to [`super::types::ProviderMeters`] with
//      `#[ts(rename = "cachedAt")]`. `None` means "freshly fetched on this
//      call"; `Some(_)` means "served from the in-process cache at that instant".
//   2. Change [`UsageCache::get`] to also expose the cache instant, e.g.
//      `Option<(ProviderUsage, Instant)>`, so callers can stamp `cached_at`.
//   3. Have [`super::catalog::cached_or_fetch`] thread the Optional instant through
//      the command's `assemble_meters`, which sets
//      `cached_at` per row in the returned [`super::types::ProviderMeters`].
//   4. Run `cargo test` to regenerate `src/types/generated/ProviderMeters.ts`
//      (the project's ts-rs gate; CLAUDE.md hard rule on wire-type drift).
//   5. The React side (`src/components/Probe/UsageTab.tsx`) then picks the
//      display timestamp: if every row carries `cachedAt`, the oldest one
//      drives a "Cached Xs ago" label; otherwise `Date.now()` keeps the
//      existing "Refreshed Xs ago" semantics for the fresh-row case.

pub fn invalidate_cache() {
    USAGE_CACHE.clear();
}

/// Targeted single-provider cache invalidation (issue #970). The refresh
/// seam in the OpenCode adapter calls this on a successful `try_refresh()`
/// so the next cache lookup cannot return a stale envelope
/// minted before the bearer was rotated. Distinct from [`invalidate_cache`]
/// (which clears every provider) so a refresh on one provider doesn't force
/// re-fetching unrelated providers on the next usage-panel poll.
pub fn invalidate_provider_cache(provider: &str) {
    USAGE_CACHE.remove_provider(provider);
}

/// Test-only: cache age for the OpenCode pre-emptive refresh check.
/// Exposed so the OpenCode adapter (which lives outside this module) can
/// decide whether its live credential is older than `REFRESH_TTL` without
/// reaching into the cache internals.
pub(crate) fn cached_age(provider: &str) -> Option<Duration> {
    USAGE_CACHE.provider_age(provider)
}
