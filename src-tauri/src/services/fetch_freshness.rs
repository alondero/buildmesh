//! Per-Mesh fetch-recency registry — the memory behind the spawn-time
//! fetch skip and the background mesh sync (spawn-latency work, ADR 0020).
//!
//! Why this exists
//! ---------------
//! ADR 0001 put a synchronous `git fetch` + `git pull --ff-only` on every
//! fresh-worktree spawn so a new Agent Node always starts from the latest
//! upstream. That correctness goal doesn't actually require the fetch to
//! happen *at spawn time* — it requires the mesh to be *recently fetched*.
//! This module records, per Mesh, when a fetch last succeeded (and when one
//! was last attempted), so:
//!
//!   * the spawn path can skip its blocking network round-trip when the
//!     mesh was successfully synced within [`SPAWN_FETCH_TTL`] (the
//!     background worker or a previous spawn already did the work), and
//!   * the background worker (`services::pool_worker`) can rate-limit its
//!     own periodic sync to one attempt per
//!     [`pool_worker::BACKGROUND_SYNC_INTERVAL`] without hammering the
//!     remote when offline (failures stamp `last_attempt` but never
//!     `last_success`, so the spawn path stays conservative while the
//!     worker backs off).
//!
//! Keyed by the mesh's DB-stored path — the same key
//! `services::sync_lock::with_mesh_sync_lock` uses — so the freshness map
//! and the fetch serialization agree on what "one Mesh" means.
//!
//! In-memory only, on purpose: fetch recency is meaningless across an app
//! restart (the remote may have moved arbitrarily while the app was off),
//! so a fresh process starts with every mesh "never fetched" and the first
//! spawn/worker pass re-syncs.

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How recently a successful sync must have completed for the spawn path to
/// skip its own `fetch_origin`. Five minutes is the pragmatic balance: the
/// background worker re-syncs idle meshes every ~3 minutes
/// (`pool_worker::BACKGROUND_SYNC_INTERVAL`), so in steady state a spawn
/// virtually never fetches, and the worst-case staleness of a new worktree
/// is bounded at a few minutes of upstream commits — negligible for "start
/// an agent on this repo" and always recoverable with the manual Sync
/// button (which fetches unconditionally and refreshes this stamp).
pub const SPAWN_FETCH_TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy)]
struct FetchStamp {
    last_attempt: Instant,
    last_success: Option<Instant>,
}

/// Per-mesh fetch stamps. Small (one entry per mesh the app has synced this
/// session) and touched at most once per fetch, so a plain `Mutex<HashMap>`
/// is plenty — same shape as `pool_worker::LAST_ACTIVITY_BY_MESH`.
static FETCH_STAMPS: Lazy<Mutex<HashMap<String, FetchStamp>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn with_map<R>(f: impl FnOnce(&mut HashMap<String, FetchStamp>) -> R) -> R {
    match FETCH_STAMPS.lock() {
        Ok(mut map) => f(&mut map),
        // A poisoned lock just means a prior writer panicked mid-insert;
        // the map has no cross-entry invariant to corrupt, so recover.
        Err(poisoned) => f(&mut poisoned.into_inner()),
    }
}

/// Record that a fetch was *attempted* for `mesh_path` (regardless of
/// outcome). The background worker gates on this so a mesh that is offline
/// or has a broken remote is retried once per interval, not once per tick.
pub fn note_attempt(mesh_path: &str) {
    let now = Instant::now();
    with_map(|map| {
        map.entry(mesh_path.to_string())
            .and_modify(|s| s.last_attempt = now)
            .or_insert(FetchStamp {
                last_attempt: now,
                last_success: None,
            });
    });
}

/// Record that a fetch for `mesh_path` *succeeded* — the remote was reached
/// and the remote-tracking ref is now current (whether or not new commits
/// arrived, and whether or not the parent's checkout could fast-forward).
/// This is the stamp the spawn-time skip trusts.
pub fn note_success(mesh_path: &str) {
    let now = Instant::now();
    with_map(|map| {
        map.insert(
            mesh_path.to_string(),
            FetchStamp {
                last_attempt: now,
                last_success: Some(now),
            },
        );
    });
}

/// Time since the last *attempted* fetch for `mesh_path`.
/// `Duration::MAX` when never attempted this session.
pub fn time_since_attempt(mesh_path: &str) -> Duration {
    let now = Instant::now();
    with_map(|map| map.get(mesh_path).map(|s| s.last_attempt))
        .map(|t| now.saturating_duration_since(t))
        .unwrap_or(Duration::MAX)
}

/// Time since the last *successful* fetch for `mesh_path`.
/// `Duration::MAX` when no fetch has succeeded this session.
pub fn time_since_success(mesh_path: &str) -> Duration {
    let now = Instant::now();
    with_map(|map| map.get(mesh_path).and_then(|s| s.last_success))
        .map(|t| now.saturating_duration_since(t))
        .unwrap_or(Duration::MAX)
}

/// The spawn-path gate: `true` when a successful sync is recent enough that
/// the spawn can skip its own blocking `fetch_origin`. Pure on its inputs so
/// the TTL rule is unit-testable without sleeping.
pub fn is_fresh_enough(time_since_success: Duration, ttl: Duration) -> bool {
    time_since_success < ttl
}

/// Convenience wrapper for the spawn path: was `mesh_path` successfully
/// synced within [`SPAWN_FETCH_TTL`]?
pub fn spawn_can_skip_fetch(mesh_path: &str) -> bool {
    is_fresh_enough(time_since_success(mesh_path), SPAWN_FETCH_TTL)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The stamp map is process-global; each test uses its own unique
    // mesh-path keys so parallel tests can't interfere (same convention as
    // pool_worker's activity-map tests).

    #[test]
    fn never_fetched_mesh_reads_as_infinitely_stale() {
        assert_eq!(time_since_attempt("/never/seen/a"), Duration::MAX);
        assert_eq!(time_since_success("/never/seen/a"), Duration::MAX);
        assert!(
            !spawn_can_skip_fetch("/never/seen/a"),
            "a mesh with no recorded fetch must NOT skip the spawn-time fetch"
        );
    }

    #[test]
    fn note_success_makes_the_mesh_fresh() {
        note_success("/fresh/mesh/b");
        assert!(
            time_since_success("/fresh/mesh/b") < Duration::from_secs(1),
            "a just-recorded success must read as recent"
        );
        assert!(
            spawn_can_skip_fetch("/fresh/mesh/b"),
            "a just-synced mesh must allow the spawn-time skip"
        );
    }

    #[test]
    fn note_attempt_alone_does_not_satisfy_the_spawn_gate() {
        // A failed fetch (offline, auth error) stamps the attempt so the
        // background worker backs off — but the spawn path must still run
        // its own fetch, because no success ever landed.
        note_attempt("/attempted/mesh/c");
        assert!(
            time_since_attempt("/attempted/mesh/c") < Duration::from_secs(1),
            "the attempt stamp must be recorded"
        );
        assert_eq!(
            time_since_success("/attempted/mesh/c"),
            Duration::MAX,
            "an attempt is not a success"
        );
        assert!(!spawn_can_skip_fetch("/attempted/mesh/c"));
    }

    #[test]
    fn note_attempt_preserves_an_earlier_success() {
        // Sequence: successful sync, then a later failed attempt (e.g. the
        // network dropped). The success stamp must survive — it's still an
        // honest "the refs were current as of then" record; only its age
        // decides whether the spawn can skip.
        note_success("/mixed/mesh/d");
        note_attempt("/mixed/mesh/d");
        assert!(
            time_since_success("/mixed/mesh/d") < Duration::from_secs(1),
            "a later attempt must not erase the recorded success"
        );
    }

    #[test]
    fn freshness_gate_is_a_strict_ttl() {
        let ttl = Duration::from_secs(300);
        assert!(is_fresh_enough(Duration::from_secs(0), ttl));
        assert!(is_fresh_enough(Duration::from_secs(299), ttl));
        assert!(
            !is_fresh_enough(ttl, ttl),
            "exactly-at-TTL must NOT skip — the boundary errs toward fetching"
        );
        assert!(!is_fresh_enough(Duration::MAX, ttl));
    }

    #[test]
    fn stamps_are_scoped_per_mesh_path() {
        note_success("/scoped/mesh/e1");
        assert!(
            !spawn_can_skip_fetch("/scoped/mesh/e2"),
            "one mesh's success must not freshen another mesh"
        );
    }
}
