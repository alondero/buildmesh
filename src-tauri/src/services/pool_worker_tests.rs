// Sibling test suite for the owning module. The suite lives in its own
// file so the production module keeps only the declaration below — the
// repo's rust-test-layout gate requires it.

use super::*;

// ---- time-parameterised test seams (issue #751) ----
//
// Mirrors of `note_activity_for_mesh` / `idle_duration_for_mesh` that
// take an explicit `Instant` rather than calling `Instant::now()`
// internally. Lets the overwrite-prior-timestamp regression test
// drive a synthetic timeline — `base`, `base + 20ms`, `base + 70ms`
// — so the two reads are provably monotonic instead of depending on
// wall-clock arithmetic that Windows' ~15.6ms system timer tick
// collapses under full-suite `cargo test --lib` parallelism
// (issue #751). The actual storage code path is still exercised
// (same `LAST_ACTIVITY_BY_MESH`, same `mesh_id > 0` guard, same
// lock-or-recover policy) — only the clock source is pinned.

fn note_activity_for_mesh_at(mesh_id: i64, when: Instant) {
    if mesh_id <= 0 {
        return;
    }
    match LAST_ACTIVITY_BY_MESH.lock() {
        Ok(mut map) => {
            map.insert(mesh_id, when);
        }
        Err(poisoned) => {
            poisoned.into_inner().insert(mesh_id, when);
        }
    }
}

fn idle_duration_for_mesh_since(mesh_id: i64, when: Instant) -> Duration {
    let last = match LAST_ACTIVITY_BY_MESH.lock() {
        Ok(map) => map.get(&mesh_id).copied(),
        Err(poisoned) => poisoned.into_inner().get(&mesh_id).copied(),
    };
    match last {
        Some(t) => when.saturating_duration_since(t),
        None => Duration::MAX,
    }
}

// ---- is_idle_enough (the debounce gate) ----

#[test]
fn not_idle_below_the_silence_window() {
    assert!(
        !is_idle_enough(Duration::from_secs(3), IDLE_SILENCE),
        "3s of silence is below the 5s window — a refill must NOT fire"
    );
}

#[test]
fn idle_at_exactly_the_silence_window() {
    // Boundary: exactly `IDLE_SILENCE` counts as idle (>=) so a window that
    // lands precisely on the threshold still fires rather than waiting a
    // whole extra tick.
    assert!(
        is_idle_enough(IDLE_SILENCE, IDLE_SILENCE),
        "silence equal to the window must be treated as idle"
    );
}

#[test]
fn idle_past_the_silence_window() {
    assert!(
        is_idle_enough(Duration::from_secs(30), IDLE_SILENCE),
        "30s of silence is well past the window — a refill must fire"
    );
}

// ---- note_activity_for_mesh / idle_duration_for_mesh (issue #634) ----
//
// The per-mesh map is process-global, so concurrent tests would race on
// the same key set. Each test uses its OWN pair of mesh_ids (large
// unique sentinels per test) so the assertions don't depend on the
// order tests happen to run in. Tests pin the *direction* of the clock
// (record → low, skip → no change) rather than wall-clock arithmetic,
// so a slow CI machine doesn't flake them.

/// Pin the issue #634 starva regression: a chatty agent on MESH_A must
/// NOT make MESH_B look active. Before the fix, MESH_A's PTY output
/// reset a single global clock that the worker consulted for every
/// mesh, so MESH_B's pool would never get a refill while MESH_A was
/// busy. After the fix, only MESH_A's clock is fresh.
#[test]
fn one_meshs_activity_does_not_block_another_meshs_idle_check() {
    const MESH_A: i64 = 200_001;
    const MESH_B: i64 = 200_002;

    // MESH_A: just received a PTY chunk → activity recorded → idle
    // duration is tiny.
    note_activity_for_mesh(MESH_A);
    let mesh_a_idle = idle_duration_for_mesh(MESH_A);
    assert!(
        !is_idle_enough(mesh_a_idle, IDLE_SILENCE),
        "MESH_A's clock is fresh after note_activity — must read as not idle"
    );

    // MESH_B: no activity recorded → idle duration is the "infinity
    // idle" sentinel (Duration::MAX), which is ABOVE IDLE_SILENCE and
    // therefore IS idle. Pre-#634, this would have returned MESH_A's
    // tiny value (because the global clock was shared).
    let mesh_b_idle = idle_duration_for_mesh(MESH_B);
    assert!(
        is_idle_enough(mesh_b_idle, IDLE_SILENCE),
        "MESH_B's clock is independent of MESH_A — must read as idle even while MESH_A is busy (issue #634)"
    );
}

/// `note_activity_for_mesh(mesh_id)` only resets THAT mesh's clock.
/// Recording activity for MESH_B must leave MESH_A's clock untouched.
#[test]
fn note_activity_for_mesh_resets_only_that_meshs_clock() {
    const MESH_A: i64 = 300_001;
    const MESH_B: i64 = 300_002;

    note_activity_for_mesh(MESH_A);
    std::thread::sleep(Duration::from_millis(20));
    let mesh_a_before = idle_duration_for_mesh(MESH_A);
    // Update only MESH_B.
    note_activity_for_mesh(MESH_B);
    let mesh_a_after = idle_duration_for_mesh(MESH_A);
    assert!(
        mesh_a_after >= mesh_a_before,
        "recording activity for MESH_B must NOT roll back MESH_A's clock (got {} -> {})",
        mesh_a_before.as_millis(),
        mesh_a_after.as_millis()
    );
    // And MESH_B is fresh.
    assert!(
        !is_idle_enough(idle_duration_for_mesh(MESH_B), IDLE_SILENCE),
        "MESH_B's clock was just reset by note_activity_for_mesh"
    );
}

/// A mesh with no recorded activity returns `Duration::MAX` (the
/// "infinity-idle" sentinel). This is the desired behaviour for a
/// freshly-added mesh — its first idle tick should fill it rather
/// than waiting `IDLE_SILENCE` for some activity that won't come.
/// (`Duration::ZERO` would be the opposite — "just got activity",
/// which `is_idle_enough` reads as "not idle enough".)
#[test]
fn idle_duration_for_mesh_returns_max_when_no_activity_recorded() {
    const MESH_C: i64 = 400_001;
    assert_eq!(
        idle_duration_for_mesh(MESH_C),
        Duration::MAX,
        "a mesh with no recorded activity must read as 'infinity idle' so its first tick can fill it"
    );
    // Sanity: Duration::MAX >= IDLE_SILENCE, so the gate says idle.
    assert!(
        is_idle_enough(idle_duration_for_mesh(MESH_C), IDLE_SILENCE),
        "Duration::MAX is above the silence window — the gate must say idle (issue #634)"
    );
}

/// Boundary: `note_activity_for_mesh` seeds the FIRST entry to
/// `Instant::now()` rather than leaving it absent. Without this,
/// `idle_duration_for_mesh` would return `Duration::ZERO` for the
/// brief window between mesh creation and the first PTY chunk, and
/// the worker could fill a pool that's actively being warmed by the
/// spawn path. The seed-on-first-write is what guarantees
/// "freshly active" → low idle → "not idle" right after the call.
#[test]
fn note_activity_for_mesh_seeds_first_entry_to_now() {
    const MESH_C: i64 = 500_001;
    note_activity_for_mesh(MESH_C);
    let idle = idle_duration_for_mesh(MESH_C);
    // Right after seeding, idle must be very small — well below
    // IDLE_SILENCE. We allow up to 1 s of slack for slow CI machines.
    assert!(
        idle < Duration::from_secs(1),
        "first-write seed must set the clock to now (got {}ms idle)",
        idle.as_millis()
    );
    assert!(
        !is_idle_enough(idle, IDLE_SILENCE),
        "right after seeding, the gate must report 'not idle'"
    );
}

/// Load-bearing regression for issue #634 review finding #1: every
/// call to `note_activity_for_mesh` MUST overwrite the prior instant,
/// not no-op via `entry().or_insert()`. The pre-fix bug froze the
/// clock at the FIRST chunk's instant, so a chatty agent's mesh
/// would read as "idle" once `IDLE_SILENCE` elapsed past that
/// FIRST instant — letting the worker fill a pool that's actively
/// being warmed by the spawn path (defeating issue #613 AC2
/// suppression).
///
/// Issue #751: the previous version of this test called
/// `note_activity_for_mesh` / `idle_duration_for_mesh` (which both
/// read `Instant::now()` internally) and slept 20 ms + 50 ms between
/// the two reads. Under full-suite `cargo test --lib` parallelism
/// Windows' ~15.6 ms system-timer tick can collapse two consecutive
/// `Instant::now()` reads into the same instant, producing
/// `first_idle == second_idle == 0 ms` and tripping the
/// `second_idle < first_idle` assertion. We now drive a *synthetic*
/// timeline via the `cfg(test)` seams `note_activity_for_mesh_at`
/// and `idle_duration_for_mesh_since` — the two stored instants are
/// arithmetic (`base + 70 ms` vs `base`), not wall-clock, so the
/// test is provably deterministic regardless of OS timer resolution
/// or test-suite contention.
#[test]
fn note_activity_for_mesh_overwrites_prior_timestamp() {
    const MESH_D: i64 = 600_001;
    // Anchor — only used as a reference point for the synthetic deltas
    // below; the test never compares against wall-clock arithmetic.
    let base = Instant::now();
    // First note: stored instant = `base`.
    note_activity_for_mesh_at(MESH_D, base);
    // 20 ms later, read — stored instant is `base`, so idle is 20 ms.
    let first_idle = idle_duration_for_mesh_since(MESH_D, base + Duration::from_millis(20));
    // 50 ms later (total +70 ms from `base`), note again — stored
    // instant MUST be overwritten (pre-#634 bug used
    // `entry().or_insert(now)` which silently no-op'd here).
    note_activity_for_mesh_at(MESH_D, base + Duration::from_millis(70));
    // Immediate read — stored instant is `base + 70 ms`, so idle is 0 ms.
    let second_idle = idle_duration_for_mesh_since(MESH_D, base + Duration::from_millis(70));
    assert_eq!(
        first_idle,
        Duration::from_millis(20),
        "first read must see the originally stored instant (20 ms after `base`)"
    );
    assert_eq!(
        second_idle,
        Duration::ZERO,
        "second read must see the freshly overwritten instant (no time has elapsed since the +70 ms note)"
    );
    assert!(
        second_idle < first_idle,
        "second note_activity_for_mesh must reset the clock (first_idle={}ms, second_idle={}ms) — \
         the pre-#634 bug froze the clock at the FIRST chunk's instant",
        first_idle.as_millis(),
        second_idle.as_millis()
    );
}

/// Mesh-id sentinels (`<= 0`) must NOT pollute the activity map.
/// Production sources: spawn's `unwrap_or(-1)` fallback for an
/// orphaned-node spawn (spawn.rs:809), and test fixtures in
/// `tests/pty_spawn.rs` that use `mesh_id: 0`. The pre-fix bug
/// added phantom entries that the prune sweep would chase forever.
#[test]
fn note_activity_for_mesh_ignores_invalid_mesh_ids() {
    // Sentinel values must not change the map state.
    let pre_invalid = idle_duration_for_mesh(700_001); // 700_001 not yet recorded → MAX
    assert_eq!(pre_invalid, Duration::MAX);
    for invalid in [0i64, -1, -42] {
        note_activity_for_mesh(invalid);
    }
    // A never-seen mesh is still never-seen: the invalid sentinels
    // didn't pollute the map with phantom entries.
    let post_invalid = idle_duration_for_mesh(700_001);
    assert_eq!(
        post_invalid,
        Duration::MAX,
        "invalid mesh_id sentinels must not write to the activity map"
    );
    // And a real mesh_id after a sentinel still works as expected.
    note_activity_for_mesh(700_001);
    assert!(!is_idle_enough(
        idle_duration_for_mesh(700_001),
        IDLE_SILENCE
    ));
}

// ---- should_background_sync (ADR 0020 background mesh sync cadence) ----

#[test]
fn background_sync_fires_for_a_never_attempted_mesh() {
    // `fetch_freshness::time_since_attempt` returns Duration::MAX for a
    // mesh with no recorded attempt — the first idle tick must sync it.
    assert!(should_background_sync(Duration::MAX));
}

#[test]
fn background_sync_respects_the_interval() {
    assert!(
        !should_background_sync(Duration::from_secs(10)),
        "a mesh attempted 10s ago must NOT be re-fetched — one attempt per interval"
    );
    assert!(
        !should_background_sync(BACKGROUND_SYNC_INTERVAL - Duration::from_secs(1)),
        "just inside the interval still waits"
    );
    assert!(
        should_background_sync(BACKGROUND_SYNC_INTERVAL),
        "at the interval boundary the sync fires"
    );
}

/// The whole point of the background sync is that its cadence fits
/// inside the spawn path's freshness TTL with margin: if the interval
/// ever grew past the TTL, a steadily-idle mesh would oscillate between
/// fresh and stale and spawns would randomly pay the fetch again.
#[test]
fn background_sync_interval_fits_inside_the_spawn_fetch_ttl() {
    assert!(
        BACKGROUND_SYNC_INTERVAL * 3 / 2 <= crate::git::fetch_freshness::SPAWN_FETCH_TTL,
        "keep BACKGROUND_SYNC_INTERVAL well inside SPAWN_FETCH_TTL (currently {}s vs {}s) — \
         the worker needs slack (slow ticks, sync-lock waits) to re-stamp before the TTL lapses",
        BACKGROUND_SYNC_INTERVAL.as_secs(),
        crate::git::fetch_freshness::SPAWN_FETCH_TTL.as_secs()
    );
}

// ---- background_sync_allowed (issue #2348: an unpoolable Mesh must not
//      re-probe the filesystem on every tick) ----
//
// The bug this pins: `note_attempt` was only ever written inside the fetch
// that a non-repository mesh skips, so `time_since_attempt` stayed
// `Duration::MAX` and the probe ran every 2 s / 30 s tick forever.

/// Counters shared with the closures below; `Cell` is enough because each
/// test drives the decision synchronously on its own thread.
#[derive(Default)]
struct SyncGateCalls {
    probes: std::cell::Cell<u32>,
    stamps: std::cell::Cell<u32>,
    stamped_paths: std::cell::RefCell<Vec<String>>,
}

#[test]
fn background_sync_skips_a_mesh_that_is_inside_its_interval() {
    let calls = SyncGateCalls::default();
    let allowed = background_sync_allowed(
        "/probe/mesh/stale",
        |_| false, // attempt clock has not reached BACKGROUND_SYNC_INTERVAL
        || {
            calls.probes.set(calls.probes.get() + 1);
            true
        },
        |path| {
            calls.stamps.set(calls.stamps.get() + 1);
            calls.stamped_paths.borrow_mut().push(path.to_string());
        },
    );
    assert!(!allowed, "a mesh inside its interval must not be synced");
    assert_eq!(
        calls.probes.get(),
        0,
        "the repository probe is the expensive call — it must not run \
         before the interval is due"
    );
    assert_eq!(
        calls.stamps.get(),
        0,
        "a not-yet-due mesh must not stamp — the clock already says so"
    );
}

/// The regression the reviewer found: a due mesh whose folder cannot be
/// opened as a repository is suppressed AND stamped. Without the stamp the
/// attempt clock never advances, `should_background_sync` stays true, and
/// `mesh_repo_unusable` walks the filesystem on every worker tick.
#[test]
fn unusable_mesh_is_stamped_so_the_probe_waits_for_the_next_interval() {
    let calls = SyncGateCalls::default();
    let allowed = background_sync_allowed(
        "/probe/mesh/unusable",
        |_| true, // interval elapsed
        || {
            calls.probes.set(calls.probes.get() + 1);
            false // cannot be opened as a git working repository
        },
        |path| {
            calls.stamps.set(calls.stamps.get() + 1);
            calls.stamped_paths.borrow_mut().push(path.to_string());
        },
    );
    assert!(
        !allowed,
        "a mesh that cannot be fetched must not enter the fetch block"
    );
    assert_eq!(
        calls.probes.get(),
        1,
        "the probe runs exactly once per due tick"
    );
    assert_eq!(
        calls.stamps.get(),
        1,
        "the suppressed sync MUST stamp the attempt, or the probe repeats \
         every tick forever (issue #2348 review finding)"
    );
    assert_eq!(
        calls.stamped_paths.borrow().as_slice(),
        ["/probe/mesh/unusable".to_string()],
        "the stamp must land on the mesh whose clock is being advanced"
    );
}

/// The healthy path must not stamp here: `locked_fetch_origin_blocking`
/// writes the attempt itself, and a second stamp would silently reset the
/// interval mid-fetch.
#[test]
fn usable_due_mesh_runs_the_fetch_without_a_double_stamp() {
    let calls = SyncGateCalls::default();
    let allowed = background_sync_allowed(
        "/probe/mesh/usable",
        |_| true,
        || {
            calls.probes.set(calls.probes.get() + 1);
            true
        },
        |_| calls.stamps.set(calls.stamps.get() + 1),
    );
    assert!(allowed, "a due mesh with a usable repository must sync");
    assert_eq!(calls.probes.get(), 1);
    assert_eq!(
        calls.stamps.get(),
        0,
        "the fetch path stamps its own attempt — stamping here too would \
         restart the interval it just consumed"
    );
}

// ---- next_tick (adaptive backoff, issue #634) ----

/// Work → snap to FAST, reset counter.
#[test]
fn next_tick_resets_to_fast_on_work() {
    let (tick, noops) = next_tick(TICK_SLOW, true, 17);
    assert_eq!(tick, TICK_FAST);
    assert_eq!(noops, 0, "any work must reset the no-op counter");
}

/// No work, counter below threshold → stay on FAST, increment.
#[test]
fn next_tick_stays_fast_below_threshold() {
    // Pass 1 (counter 0 → 1): no switch yet.
    let (tick, noops) = next_tick(TICK_FAST, false, 0);
    assert_eq!(tick, TICK_FAST);
    assert_eq!(noops, 1);

    // Pass 2 (counter 1 → 2): still no switch.
    let (tick, noops) = next_tick(TICK_FAST, false, 1);
    assert_eq!(tick, TICK_FAST);
    assert_eq!(noops, 2);
}

/// No work, counter hits `NO_OP_PASSES_BEFORE_BACKOFF` (the third
/// no-op pass in a row) → switch to SLOW and keep incrementing. The
/// counter never resets while we're in the slow lane so a long idle
/// period doesn't lose the "we're slow" signal — only real work
/// (`did_work = true`) resets it.
#[test]
fn next_tick_switches_to_slow_at_threshold() {
    // The 3rd no-op triggers the switch.
    let (tick, noops) = next_tick(TICK_FAST, false, 2);
    assert_eq!(
        tick, TICK_SLOW,
        "the 3rd consecutive no-op pass (counter 2 → 3) must switch to TICK_SLOW"
    );
    assert_eq!(
        noops, 3,
        "the counter reaches the threshold value so we know we're in slow lane"
    );

    // Subsequent slow passes stay slow.
    let (tick, noops) = next_tick(TICK_SLOW, false, 3);
    assert_eq!(tick, TICK_SLOW);
    assert_eq!(noops, 4);

    let (tick, noops) = next_tick(TICK_SLOW, false, 17);
    assert_eq!(tick, TICK_SLOW);
    assert_eq!(noops, 18);
}

// ---- try_with_fill_lock (serialized execution, AC4) ----
//
// Both the "runs when free" and "skips a reentrant attempt" assertions live
// in ONE test: `FILL_LOCK` is process-global, so two separate `#[test]` fns
// touching it would race under cargo's parallel test runner (one holding
// the lock while the other asserts it acquired). Keeping them sequential in
// a single test removes that flakiness — no other test touches the lock.

#[test]
fn fill_lock_runs_when_free_and_skips_a_reentrant_attempt() {
    // 1. Uncontended: the body runs.
    let mut ran = false;
    let acquired = try_with_fill_lock(|| ran = true);
    assert!(acquired, "an uncontended lock must run the body");
    assert!(ran, "the body must have executed");

    // 2. Reentrant: while the outer closure holds the lock, a nested
    // attempt must be skipped (returns false, body not run) rather than
    // deadlocking or running a second concurrent fill — the exact shape of
    // "high-concurrency spawns" all racing to refill.
    let mut inner_ran = false;
    let outer = try_with_fill_lock(|| {
        let inner = try_with_fill_lock(|| inner_ran = true);
        assert!(
            !inner,
            "a nested attempt while the lock is held must be skipped"
        );
    });
    assert!(outer, "the outer attempt must have acquired the lock");
    assert!(
        !inner_ran,
        "the nested body must NOT run while a fill is in progress (AC4)"
    );
}
