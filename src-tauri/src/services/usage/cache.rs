//! In-process 5-minute usage cache (issue #1657).
//!
//! The cache is the only state the `usage` god-module keeps after the
//! adapter split: wire types live in [`super::types`], fetch dispatch lives
//! behind [`super::adapter::UsageAdapter`], and this module owns the
//! TTL + identity-aware get/set/invalidate surface the catalog consults
//! before dispatch.

use super::adapter::UsageIdentityFingerprint;
use super::types::ProviderUsage;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CACHE_TTL: Duration = Duration::from_secs(300);

#[derive(Clone, Eq, Hash, PartialEq)]
struct CacheKey {
    provider: String,
    identity: UsageIdentityFingerprint,
}

type Cache = HashMap<CacheKey, (Instant, ProviderUsage)>;

pub(crate) struct UsageCache {
    entries: Mutex<Cache>,
}

impl UsageCache {
    pub(crate) fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
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
