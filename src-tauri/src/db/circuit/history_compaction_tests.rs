//! Tests for `db::circuit::history_compaction` (issue #2600).
//!
//! Split out of `src-tauri/src/db/circuit/history_compaction.rs` by the
//! repo's sibling-test convention — see `docs/development/rust-conventions.md`,
//! under *Rust unit tests live in sibling `*_tests.rs` files*.
//!
//! Run with: cd src-tauri && cargo test db::circuit::history_compaction::tests

use rusqlite::params;

/// An isolated database with the real schema and one run to hang history off.
///
/// The guard is held for the length of the test: the database is
/// thread-local, and the sweep reaches it through the process-global
/// `db::read_conn()` / `db::write_conn()` helpers.
fn fixture() -> crate::db::test_support::IsolatedDbGuard {
    let guard = crate::db::test_support::isolated();
    {
        let db = crate::db::write_conn();
        db.execute_batch(
            "INSERT INTO meshes(id,name,path) VALUES(1,'test','/repo');
             INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'test');
             INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');",
        )
        .unwrap();
    }
    guard
}

/// Insert one observation row the way a pre-v51 database holds one: no hash,
/// because the column did not exist yet.
fn insert_legacy_observation(node_id: &str, attempt: i32, source: &str, detail: &str) -> i64 {
    let db = crate::db::write_conn();
    db.execute(
        "INSERT INTO circuit_run_history (run_id,node_id,attempt,kind,detail,source,disposition)
         VALUES (1,?1,?2,'observation',?3,?4,'accepted')",
        params![node_id, attempt, detail, source],
    )
    .unwrap();
    db.last_insert_rowid()
}

/// The ids of the observation rows, in the order they were written.
fn observation_ids() -> Vec<i64> {
    let db = crate::db::read_conn().unwrap();
    let mut stmt = db
        .prepare("SELECT id FROM circuit_run_history WHERE kind='observation' ORDER BY id")
        .unwrap();
    let rows = stmt.query_map([], |row| row.get(0)).unwrap();
    rows.collect::<Result<Vec<i64>, _>>().unwrap()
}

/// The copies of one observation collapse to its first and its last copy.
///
/// The hub this issue came from held 132k copies of a single rollout
/// completion. The sweep keeps the first copy (when Buildmesh saw the fact)
/// and the last (when it last saw it) and deletes the one in between.
#[test]
fn identical_observations_collapse_to_their_first_and_last_copy() {
    let _db = fixture();
    let first = insert_legacy_observation("build", 1, "codex_rollout", r#"{"a":1}"#);
    let middle = insert_legacy_observation("build", 1, "codex_rollout", r#"{"a":1}"#);
    let last = insert_legacy_observation("build", 1, "codex_rollout", r#"{"a":1}"#);

    let deleted = super::compact_duplicate_observations().unwrap();

    assert_eq!(deleted, 1, "one copy of three is a duplicate");
    assert_eq!(
        observation_ids(),
        vec![first, last],
        "the first and the last copy stay; the one in between goes"
    );
    assert!(
        !observation_ids().contains(&middle),
        "the copy in between is the one that goes"
    );
}

/// Two copies are two copies of one fact, and the sweep keeps both of them.
///
/// Keeping the first and the last is only a collapse when there is something
/// in between. A group of two loses no fact by keeping both.
#[test]
fn a_pair_of_identical_observations_keeps_both_copies() {
    let _db = fixture();
    let first = insert_legacy_observation("build", 1, "codex_rollout", r#"{"a":1}"#);
    let last = insert_legacy_observation("build", 1, "codex_rollout", r#"{"a":1}"#);

    let deleted = super::compact_duplicate_observations().unwrap();

    assert_eq!(deleted, 0);
    assert_eq!(observation_ids(), vec![first, last]);
}

/// The same words for a different node, attempt or source are different
/// observations.
///
/// The sweep collapses copies of one fact, not facts that happen to look
/// alike. The identity of an observation is its run, node, attempt and
/// source, so two rows that differ in any of those stay two rows.
#[test]
fn observations_with_a_different_identity_are_not_duplicates() {
    let _db = fixture();
    let a = insert_legacy_observation("build", 1, "codex_rollout", r#"{"a":1}"#);
    let other_node = insert_legacy_observation("test", 1, "codex_rollout", r#"{"a":1}"#);
    let other_attempt = insert_legacy_observation("build", 2, "codex_rollout", r#"{"a":1}"#);
    let other_source = insert_legacy_observation("build", 1, "agent_status", r#"{"a":1}"#);
    let other_payload = insert_legacy_observation("build", 1, "codex_rollout", r#"{"a":2}"#);

    let deleted = super::compact_duplicate_observations().unwrap();

    assert_eq!(deleted, 0, "five different facts are not duplicates");
    assert_eq!(
        observation_ids(),
        vec![a, other_node, other_attempt, other_source, other_payload]
    );
}

/// A transition is a timeline, not a fact.
///
/// Only observations are deduped. Two `step_transition` rows with the same
/// words are two moments in the run's story, and the sweep must not merge
/// them.
#[test]
fn the_sweep_leaves_other_history_kinds_alone() {
    let _db = fixture();
    {
        let db = crate::db::write_conn();
        for _ in 0..3 {
            db.execute(
                "INSERT INTO circuit_run_history (run_id,node_id,attempt,kind,detail,source,disposition)
                 VALUES (1,'build',1,'step_transition','unverified','circuit_worker','applied')",
                [],
            )
            .unwrap();
        }
    }

    let deleted = super::compact_duplicate_observations().unwrap();

    assert_eq!(deleted, 0);
    let db = crate::db::read_conn().unwrap();
    let transitions: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM circuit_run_history WHERE kind='step_transition'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(transitions, 3, "the timeline is intact");
}

/// The rows that stay get the hash the dedupe index is built on.
///
/// Without it a surviving row is invisible to the append-path dedupe, so the
/// same observation could still be written one more time after the sweep.
#[test]
fn the_sweep_hashes_the_rows_that_stay() {
    let _db = fixture();
    insert_legacy_observation("build", 1, "codex_rollout", r#"{"a":1}"#);
    insert_legacy_observation("build", 1, "codex_rollout", r#"{"a":1}"#);

    super::compact_duplicate_observations().unwrap();

    let db = crate::db::read_conn().unwrap();
    let unhashed: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM circuit_run_history WHERE kind='observation' AND detail_sha256 IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unhashed, 0, "every surviving observation carries its hash");
}

/// A second pass over a compacted table finds no row left to collapse.
#[test]
fn the_sweep_is_idempotent() {
    let _db = fixture();
    insert_legacy_observation("build", 1, "codex_rollout", r#"{"a":1}"#);
    insert_legacy_observation("build", 1, "codex_rollout", r#"{"a":1}"#);
    insert_legacy_observation("build", 1, "codex_rollout", r#"{"a":1}"#);

    assert_eq!(
        super::compact_duplicate_observations().unwrap(),
        1,
        "three copies keep the first and the last, so one goes"
    );
    assert_eq!(super::compact_duplicate_observations().unwrap(), 0);
}
