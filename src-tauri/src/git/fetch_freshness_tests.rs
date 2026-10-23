// Sibling test suite for the owning module. The suite lives in its own
// file so the production module keeps only the declaration below — the
// repo's rust-test-layout gate requires it.

use super::*;

/// Convenience wrapper for the spawn path: was `mesh_path` successfully
/// synced within [`SPAWN_FETCH_TTL`]?
fn spawn_can_skip_fetch(mesh_path: &str) -> bool {
    is_fresh_enough(time_since_success(mesh_path), SPAWN_FETCH_TTL)
}

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
