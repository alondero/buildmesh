//! Tests for `db::circuit::artifacts` — the split between a circuit run's
//! decision-relevant context and its bulky output/evidence text (issue #2601).
//!
//! Run with: cd src-tauri && cargo test db::circuit::artifacts::tests

use std::collections::BTreeMap;

use rusqlite::{params, Connection};

use super::*;

/// A review run's context as the worker holds it: the small decision flags
/// plus the bulky text the stepper reads on demand.
fn review_context() -> BTreeMap<String, String> {
    let mut context = BTreeMap::new();
    context.insert("circuit.id".into(), "7".into());
    context.insert("circuit.name".into(), "Review PR".into());
    context.insert("node.id".into(), "reviewer".into());
    context.insert("review.provider".into(), "personal".into());
    context.insert("source.output".into(), "s".repeat(2_600));
    context.insert("node.reviewer.output".into(), "r".repeat(4_400));
    context.insert("node.reviewer.evaluated_output".into(), "e".repeat(5_000));
    context.insert(
        "node.await_source.evidence.1".into(),
        format!("{{\"observations\":[\"{}\"]}}", "o".repeat(6_800)),
    );
    context
}

fn seed(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO meshes(id,name,path) VALUES(1,'m','/m');
         INSERT INTO autopilot_circuits(id,mesh_id,name) VALUES(1,1,'review');
         INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,state) VALUES(1,1,1,'running');",
    )
    .unwrap();
}

fn schema() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    conn
}

#[test]
fn a_bulky_value_moves_out_of_the_column_and_a_small_one_stays() {
    let context = review_context();
    let json = serde_json::to_string(&context).unwrap();
    let split = SplitContext::parse(&json);

    assert!(split.small.contains_key("review.provider"));
    assert!(split.small.contains_key("circuit.id"));
    for key in [
        "source.output",
        "node.reviewer.output",
        "node.reviewer.evaluated_output",
        "node.await_source.evidence.1",
    ] {
        assert!(
            split.bulky.iter().any(|value| value.key == key),
            "{key} should have moved to the side table"
        );
        assert!(!split.small.contains_key(key));
    }
    // The column keeps only the decision-relevant half.
    assert!(split.small_json().len() < 4 * 1024);
}

#[test]
fn a_bulky_value_names_its_node_and_attempt() {
    let split = SplitContext::parse(&serde_json::to_string(&review_context()).unwrap());
    let evidence = split
        .bulky
        .iter()
        .find(|value| value.key == "node.await_source.evidence.1")
        .expect("the evidence blob moved");
    assert_eq!(evidence.node_id, "await_source");
    assert_eq!(evidence.attempt, 1);

    let output = split
        .bulky
        .iter()
        .find(|value| value.key == "node.reviewer.output")
        .expect("the reviewer output moved");
    assert_eq!(output.node_id, "reviewer");
    assert_eq!(output.attempt, 0);

    let source = split
        .bulky
        .iter()
        .find(|value| value.key == "source.output")
        .expect("the source output moved");
    assert_eq!(source.node_id, "");
}

#[test]
fn a_commit_splits_the_context_and_a_read_joins_it_again() {
    let conn = schema();
    seed(&conn);
    let context = review_context();
    let json = serde_json::to_string(&context).unwrap();

    let tx = conn.unchecked_transaction().unwrap();
    let stored = write_context(&tx, 1, &json).unwrap();
    tx.execute(
        "UPDATE autopilot_circuit_runs SET context_json = ?2 WHERE id = ?1",
        params![1, stored],
    )
    .unwrap();
    tx.commit().unwrap();

    // The column holds the small half only.
    let column: String = conn
        .query_row(
            "SELECT context_json FROM autopilot_circuit_runs WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(column.len() < 4 * 1024, "column was {} bytes", column.len());
    assert!(!column.contains("node.reviewer.output"));

    // The joined read is the context the worker had.
    let joined = load_context(&conn, 1).unwrap();
    for (key, value) in &context {
        assert_eq!(joined.get(key), Some(value.as_str()), "{key} was lost");
    }
}

#[test]
fn an_unchanged_bulky_value_is_written_once() {
    let conn = schema();
    seed(&conn);
    let json = serde_json::to_string(&review_context()).unwrap();

    let tx = conn.unchecked_transaction().unwrap();
    write_context(&tx, 1, &json).unwrap();
    tx.commit().unwrap();
    // Pin the row's timestamp, so a rewrite cannot hide behind the clock's
    // one-second resolution.
    conn.execute(
        "UPDATE circuit_run_artifacts SET updated_at = '2000-01-01 00:00:00' WHERE run_id = 1",
        [],
    )
    .unwrap();

    let tx = conn.unchecked_transaction().unwrap();
    write_context(&tx, 1, &json).unwrap();
    tx.commit().unwrap();
    let updated: String = conn
        .query_row(
            "SELECT updated_at FROM circuit_run_artifacts WHERE run_id = 1 AND key = 'source.output'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(
        updated, "2000-01-01 00:00:00",
        "an unchanged value was rewritten"
    );
}

#[test]
fn a_value_that_becomes_small_supersedes_the_stored_copy() {
    let conn = schema();
    seed(&conn);
    let json = serde_json::to_string(&review_context()).unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    write_context(&tx, 1, &json).unwrap();
    tx.commit().unwrap();

    let mut context = review_context();
    context.insert("source.output".into(), "short".into());
    let tx = conn.unchecked_transaction().unwrap();
    let stored = write_context(&tx, 1, &serde_json::to_string(&context).unwrap()).unwrap();
    tx.execute(
        "UPDATE autopilot_circuit_runs SET context_json = ?2 WHERE id = ?1",
        params![1, stored],
    )
    .unwrap();
    tx.commit().unwrap();

    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM circuit_run_artifacts WHERE run_id = 1 AND key = 'source.output'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0, "the superseded copy survived");

    let joined = load_context(&conn, 1).unwrap();
    assert_eq!(joined.get("source.output"), Some("short"));
}

#[test]
fn the_digest_changes_when_a_bulky_value_changes() {
    let conn = schema();
    seed(&conn);
    let mut context = review_context();
    let tx = conn.unchecked_transaction().unwrap();
    let stored = write_context(&tx, 1, &serde_json::to_string(&context).unwrap()).unwrap();
    tx.commit().unwrap();
    let digest_of = |stored: &str| {
        serde_json::from_str::<BTreeMap<String, String>>(stored)
            .unwrap()
            .get(DIGEST_KEY)
            .cloned()
            .unwrap()
    };
    let before = digest_of(&stored);

    context.insert("source.output".into(), "a different report".into());
    let tx = conn.unchecked_transaction().unwrap();
    let stored = write_context(&tx, 1, &serde_json::to_string(&context).unwrap()).unwrap();
    tx.commit().unwrap();

    assert_ne!(before, digest_of(&stored));
}

#[test]
fn a_run_with_nothing_bulky_carries_no_digest() {
    let conn = schema();
    seed(&conn);
    let mut context: BTreeMap<String, String> = BTreeMap::new();
    context.insert("circuit.id".into(), "7".into());
    context.insert("review.provider".into(), "personal".into());

    let tx = conn.unchecked_transaction().unwrap();
    let stored = write_context(&tx, 1, &serde_json::to_string(&context).unwrap()).unwrap();
    tx.commit().unwrap();

    // A run with nothing bulky in it has nothing to fingerprint, so its
    // context stays exactly what it was.
    let stored: BTreeMap<String, String> = serde_json::from_str(&stored).unwrap();
    assert!(!stored.contains_key(DIGEST_KEY));
    assert_eq!(stored, context);
}

#[test]
fn the_digest_covers_the_settled_set() {
    let conn = schema();
    seed(&conn);
    let mut context = review_context();
    let tx = conn.unchecked_transaction().unwrap();
    let stored = write_context(&tx, 1, &serde_json::to_string(&context).unwrap()).unwrap();
    tx.commit().unwrap();
    let digest_of = |stored: &str| {
        serde_json::from_str::<BTreeMap<String, String>>(stored)
            .unwrap()
            .get(DIGEST_KEY)
            .cloned()
            .unwrap()
    };
    let before = digest_of(&stored);

    // A value that becomes small leaves the stored set, so the fingerprint of
    // that set changes even though no value was rewritten.
    context.insert("source.output".into(), "short".into());
    let tx = conn.unchecked_transaction().unwrap();
    let stored = write_context(&tx, 1, &serde_json::to_string(&context).unwrap()).unwrap();
    tx.commit().unwrap();

    assert_ne!(before, digest_of(&stored));
}

#[test]
fn a_stored_value_the_new_context_does_not_mention_survives() {
    let conn = schema();
    seed(&conn);
    let context = review_context();
    let tx = conn.unchecked_transaction().unwrap();
    write_context(&tx, 1, &serde_json::to_string(&context).unwrap()).unwrap();
    tx.commit().unwrap();

    // A write that mentions only the flags leaves the bulky values alone: the
    // context a writer submits is the whole context, so an omission means
    // "unchanged", not "gone". The two paths that really clear a node's
    // working state delete the stored values explicitly first.
    let mut flags: BTreeMap<String, String> = BTreeMap::new();
    flags.insert("evidence.revision".into(), "2".into());
    let tx = conn.unchecked_transaction().unwrap();
    write_context(&tx, 1, &serde_json::to_string(&flags).unwrap()).unwrap();
    tx.commit().unwrap();

    let joined = load_context(&conn, 1).unwrap();
    assert_eq!(
        joined.get("node.reviewer.output").map(str::len),
        Some(4_400),
        "a stored value was dropped by a write that did not mention it"
    );
}

#[test]
fn the_run_card_projection_serves_the_bulky_half() {
    let conn = schema();
    seed(&conn);
    let context = review_context();
    let tx = conn.unchecked_transaction().unwrap();
    let stored = write_context(&tx, 1, &serde_json::to_string(&context).unwrap()).unwrap();
    tx.execute(
        "UPDATE autopilot_circuit_runs SET context_json = ?2 WHERE id = ?1",
        params![1, stored],
    )
    .unwrap();
    tx.commit().unwrap();

    // The column holds the small half only.
    let column: String = conn
        .query_row(
            "SELECT context_json FROM autopilot_circuit_runs WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!column.contains("node.reviewer.evaluated_output"));

    // The run card reads the review report out of the projection, so the
    // projection has to carry the bulky half again.
    let runs = super::super::ledger::list_circuit_runs_inner(&conn, 1, 10).unwrap();
    assert_eq!(runs.len(), 1);
    let served: BTreeMap<String, String> = serde_json::from_str(&runs[0].context_json).unwrap();
    assert_eq!(
        served.get("node.reviewer.evaluated_output"),
        context.get("node.reviewer.evaluated_output"),
        "the run card lost the review report"
    );
    // The digest is a storage detail: the operator sees the run's context.
    assert!(!served.contains_key(DIGEST_KEY));
}

#[test]
fn the_queue_projection_serves_the_bulky_half() {
    let conn = schema();
    seed(&conn);
    conn.execute(
        "UPDATE autopilot_circuit_runs SET state = 'pending' WHERE id = 1",
        [],
    )
    .unwrap();
    let context = review_context();
    let tx = conn.unchecked_transaction().unwrap();
    let stored = write_context(&tx, 1, &serde_json::to_string(&context).unwrap()).unwrap();
    tx.execute(
        "UPDATE autopilot_circuit_runs SET context_json = ?2 WHERE id = ?1",
        params![1, stored],
    )
    .unwrap();
    tx.commit().unwrap();

    // The Probe searches the pending queue by the issue body, which is one of
    // the bulky values.
    let queued = super::super::queue::list_queued_circuit_runs_inner(&conn, 1).unwrap();
    assert_eq!(queued.len(), 1);
    let served: BTreeMap<String, String> = serde_json::from_str(&queued[0].0.context_json).unwrap();
    assert_eq!(
        served.get("source.output"),
        context.get("source.output"),
        "the queue lost the trigger body"
    );
}

#[test]
fn the_circuit_ledger_projection_serves_the_bulky_half() {
    let conn = schema();
    seed(&conn);
    let context = review_context();
    let tx = conn.unchecked_transaction().unwrap();
    let stored = write_context(&tx, 1, &serde_json::to_string(&context).unwrap()).unwrap();
    tx.execute(
        "UPDATE autopilot_circuit_runs SET context_json = ?2 WHERE id = ?1",
        params![1, stored],
    )
    .unwrap();
    tx.commit().unwrap();

    // The Probe's circuit ledger is where its run search and its run cards
    // read the context from.
    let rows = super::super::ledger::list_circuits_with_recent_runs_inner(&conn, 1, 10).unwrap();
    assert_eq!(rows.len(), 1);
    let ledgers = &rows[0].1;
    assert_eq!(ledgers.len(), 1);
    let served: BTreeMap<String, String> =
        serde_json::from_str(&ledgers[0].run.context_json).unwrap();
    assert_eq!(
        served.get("node.reviewer.evaluated_output"),
        context.get("node.reviewer.evaluated_output"),
        "the circuit ledger lost the review report"
    );
}

#[test]
fn the_overview_projection_serves_the_bulky_half() {
    let conn = schema();
    seed(&conn);
    conn.execute(
        "UPDATE autopilot_circuits SET graph_json = ?1",
        [crate::circuit::model::CircuitGraph::walking_skeleton("x")
            .to_json()
            .unwrap()],
    )
    .unwrap();
    let context = review_context();
    let tx = conn.unchecked_transaction().unwrap();
    let stored = write_context(&tx, 1, &serde_json::to_string(&context).unwrap()).unwrap();
    tx.execute(
        "UPDATE autopilot_circuit_runs SET context_json = ?2 WHERE id = ?1",
        params![1, stored],
    )
    .unwrap();
    tx.commit().unwrap();

    // The Agent Node card's Overview tab shows the reviewer's report.
    let overview = super::super::overview::overview_inner(&conn, 1).unwrap();
    let served: BTreeMap<String, String> =
        serde_json::from_str(&overview.run.context_json).unwrap();
    assert_eq!(
        served.get("node.reviewer.evaluated_output"),
        context.get("node.reviewer.evaluated_output"),
        "the Overview lost the review report"
    );
}

#[test]
fn the_worker_read_sees_the_bulky_half() {
    let conn = schema();
    seed(&conn);
    let context = review_context();
    let tx = conn.unchecked_transaction().unwrap();
    let stored = write_context(&tx, 1, &serde_json::to_string(&context).unwrap()).unwrap();
    tx.execute(
        "UPDATE autopilot_circuit_runs SET context_json = ?2 WHERE id = ?1",
        params![1, stored],
    )
    .unwrap();
    tx.commit().unwrap();

    // The worker resolves `{{node.reviewer.output}}` and reads a step's
    // evidence out of the context, so its read has to carry the bulky half.
    // `load_run_context` is this read through the pool.
    let joined = load_context(&conn, 1).unwrap();
    for (key, value) in &context {
        assert_eq!(joined.get(key), Some(value.as_str()), "{key} was lost");
    }
}

#[test]
fn a_stored_lifecycle_blocker_does_not_come_back_into_the_context() {
    // A lifecycle blocker is runtime state, not context: the context filter
    // drops it on the way in, so joining the stored bulky values back must
    // not resurrect a veto the filter removed.
    let conn = schema();
    seed(&conn);
    let mut context: BTreeMap<String, String> = BTreeMap::new();
    context.insert("node.reviewer.lifecycle_blocker".into(), "b".repeat(2_000));
    let tx = conn.unchecked_transaction().unwrap();
    write_context(&tx, 1, &serde_json::to_string(&context).unwrap()).unwrap();
    tx.commit().unwrap();

    let joined = load_context(&conn, 1).unwrap();
    assert!(
        joined.get("node.reviewer.lifecycle_blocker").is_none(),
        "a stored lifecycle blocker came back into the context"
    );
}

#[test]
fn deleting_a_circuit_takes_its_artifacts_with_it() {
    let mut conn = schema();
    seed(&conn);
    let tx = conn.unchecked_transaction().unwrap();
    write_context(&tx, 1, &serde_json::to_string(&review_context()).unwrap()).unwrap();
    tx.commit().unwrap();
    assert!(artifact_count(&conn) > 0, "the fixture stored nothing");

    // The schema declares `ON DELETE CASCADE`, but enforcement rides on the
    // connection's `foreign_keys` pragma — off by default for a system
    // libsqlite link — so the delete has to name the side table itself.
    super::super::ledger::delete_autopilot_circuit_locked(&mut conn, 1).unwrap();

    assert_eq!(
        artifact_count(&conn),
        0,
        "the circuit's artifacts survived the circuit"
    );
}

#[test]
fn deleting_a_mesh_takes_its_artifacts_with_it() {
    let conn = schema();
    seed(&conn);
    let tx = conn.unchecked_transaction().unwrap();
    write_context(&tx, 1, &serde_json::to_string(&review_context()).unwrap()).unwrap();
    tx.commit().unwrap();
    assert!(artifact_count(&conn) > 0, "the fixture stored nothing");

    super::super::ledger::delete_circuits_for_mesh_inner(&conn, 1).unwrap();

    assert_eq!(
        artifact_count(&conn),
        0,
        "the mesh's artifacts survived the mesh"
    );
}

/// How many stored bulky values the fixture left behind.
fn artifact_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM circuit_run_artifacts", [], |row| {
        row.get(0)
    })
    .unwrap()
}

#[test]
fn a_bulky_value_change_is_visible_in_the_column_the_fence_reads() {
    let conn = schema();
    seed(&conn);
    let mut context = review_context();
    let tx = conn.unchecked_transaction().unwrap();
    let stored = write_context(&tx, 1, &serde_json::to_string(&context).unwrap()).unwrap();
    tx.execute(
        "UPDATE autopilot_circuit_runs SET context_json = ?2 WHERE id = ?1",
        params![1, stored],
    )
    .unwrap();
    tx.commit().unwrap();
    let before: String = conn
        .query_row(
            "SELECT context_json FROM autopilot_circuit_runs WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();

    // Only the bulky value changes. The small flags are untouched.
    context.insert("source.output".into(), "a different report".into());
    let tx = conn.unchecked_transaction().unwrap();
    let stored = write_context(&tx, 1, &serde_json::to_string(&context).unwrap()).unwrap();
    tx.execute(
        "UPDATE autopilot_circuit_runs SET context_json = ?2 WHERE id = ?1",
        params![1, stored],
    )
    .unwrap();
    tx.commit().unwrap();
    let after: String = conn
        .query_row(
            "SELECT context_json FROM autopilot_circuit_runs WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_ne!(
        before, after,
        "a bulky-only change left the column identical, so the freshness fence would not fire"
    );
    // The fence's own predicate: the column the worker loaded no longer
    // matches the stored one.
    let fence_holds: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM autopilot_circuit_runs r WHERE r.id = 1 AND r.context_json = ?1",
            params![before],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(fence_holds, 0, "the fence still holds after a bulky change");
}

#[test]
fn a_node_s_own_values_can_be_dropped() {
    let conn = schema();
    seed(&conn);
    let json = serde_json::to_string(&review_context()).unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    write_context(&tx, 1, &json).unwrap();
    tx.commit().unwrap();

    let dropped = delete_node_values(&conn, 1, "reviewer").unwrap();
    assert_eq!(dropped, 2, "the reviewer's output and its evaluated copy");

    let joined = load_context(&conn, 1).unwrap();
    assert_eq!(joined.get("node.reviewer.output"), None);
    assert_eq!(joined.get("node.reviewer.evaluated_output"), None);
    assert!(joined.get("node.await_source.evidence.1").is_some());
    assert!(joined.get("source.output").is_some());
}

#[test]
fn a_node_id_with_an_underscore_does_not_match_a_sibling() {
    let conn = schema();
    seed(&conn);
    let mut context: BTreeMap<String, String> = BTreeMap::new();
    context.insert("node.await_source.output".into(), "s".repeat(2_000));
    context.insert("node.awaitXsource.output".into(), "x".repeat(2_000));
    let tx = conn.unchecked_transaction().unwrap();
    write_context(&tx, 1, &serde_json::to_string(&context).unwrap()).unwrap();
    tx.commit().unwrap();

    let dropped = delete_node_values(&conn, 1, "await_source").unwrap();
    assert_eq!(dropped, 1);
    let joined = load_context(&conn, 1).unwrap();
    assert_eq!(joined.get("node.await_source.output"), None);
    assert!(joined.get("node.awaitXsource.output").is_some());
}

#[test]
fn a_wait_path_stays_in_the_column_however_long_it_is() {
    // The wait history compares the previous and the next context, so a long
    // blocker message has to stay where that comparison can see it.
    let mut context: BTreeMap<String, String> = BTreeMap::new();
    context.insert(
        "node.reviewer.observation_blocker".into(),
        "b".repeat(4_000),
    );
    let split = SplitContext::parse(&serde_json::to_string(&context).unwrap());
    assert!(split
        .small
        .contains_key("node.reviewer.observation_blocker"));
    assert!(split.bulky.is_empty());
}

#[test]
fn the_migration_moves_an_existing_run_s_bulky_context() {
    let conn = schema();
    seed(&conn);
    // A pre-migration row: the whole context is one blob.
    conn.execute(
        "UPDATE autopilot_circuit_runs SET context_json = ?1 WHERE id = 1",
        params![serde_json::to_string(&review_context()).unwrap()],
    )
    .unwrap();
    // The backfill is one-shot; clear its flag so the step runs again.
    conn.execute(
        "DELETE FROM app_settings WHERE key = 'circuit_run_artifacts_backfill_v1'",
        [],
    )
    .unwrap();

    crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();

    let column: String = conn
        .query_row(
            "SELECT context_json FROM autopilot_circuit_runs WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(column.len() < 4 * 1024, "column was {} bytes", column.len());
    let joined = load_context(&conn, 1).unwrap();
    assert_eq!(
        joined.get("node.reviewer.output").map(str::len),
        Some(4_400),
        "the migrated run lost its reviewer output"
    );
}

/// The bytes one advance writes, measured on a file-backed database.
///
/// The old shape put the whole context in one blob and rewrote it on every
/// advance. The new shape writes the decision-relevant half to the column and
/// the bulky values to the side table, once, when they change.
///
/// Two advances are measured, because a run does both: one that only moves
/// the small flags on, and one that also brings a fresh report.
///
/// The baseline is a stand-in for the pre-change shape, not the old commit
/// path itself: the old path no longer exists to be measured. It writes the
/// whole context through the same statement the old commit wrote it through,
/// which is what an advance's write cost was. The "after" side goes through
/// the engine's real commit point, `commit_circuit_advance_locked`, so the
/// comparison is between the old write and the new one — not between two
/// approximations.
///
/// What the numbers say: the WAL frame count per advance does not change,
/// because SQLite rewrites the row's changed pages either way. What changes is
/// where the bytes live. A 19 KB context does not fit on one page, so the old
/// shape's run row overflowed — the first advance writes the overflow chain
/// (four extra pages) and every read of the run row has to follow it. The
/// split keeps the run row on one page and puts the bulky values in the side
/// table, where they are written once.
#[test]
fn an_advance_writes_the_small_context_instead_of_the_whole_blob() {
    let full = serde_json::to_string(&review_context()).unwrap();

    // Baseline: the pre-change shape — the whole context rewritten as one
    // blob, through the same statement the engine's commit uses.
    let baseline = measure_on_a_fresh_database("baseline", advancing_context, |conn, json| {
        let tx = conn.transaction().unwrap();
        tx.execute(
            "UPDATE autopilot_circuit_runs \
             SET state = 'running', context_json = ?2, updated_at = datetime('now') \
             WHERE id = ?1",
            params![1, json],
        )
        .unwrap();
        tx.commit().unwrap();
    });

    // The new shape: the same advance through the engine's commit point.
    let split = measure_on_a_fresh_database("split", advancing_context, |conn, json| {
        super::super::ledger::commit_circuit_advance_locked(
            conn,
            1,
            Some("running"),
            Some(json),
            &[],
        )
        .unwrap();
    });

    // The same comparison for an advance that brings a fresh report: the
    // reviewer's output is replaced, so the bulky half changes too.
    let baseline_with_report = measure_on_a_fresh_database(
        "baseline-report",
        advancing_context_with_report,
        |conn, json| {
            let tx = conn.transaction().unwrap();
            tx.execute(
                "UPDATE autopilot_circuit_runs \
             SET state = 'running', context_json = ?2, updated_at = datetime('now') \
             WHERE id = ?1",
                params![1, json],
            )
            .unwrap();
            tx.commit().unwrap();
        },
    );
    let split_with_report = measure_on_a_fresh_database(
        "split-report",
        advancing_context_with_report,
        |conn, json| {
            super::super::ledger::commit_circuit_advance_locked(
                conn,
                1,
                Some("running"),
                Some(json),
                &[],
            )
            .unwrap();
        },
    );

    let conn = fresh_database("column");
    let tx = conn.unchecked_transaction().unwrap();
    let stored = write_context(&tx, 1, &full).unwrap();
    tx.execute(
        "UPDATE autopilot_circuit_runs SET context_json = ?2 WHERE id = ?1",
        params![1, stored],
    )
    .unwrap();
    tx.commit().unwrap();
    let column: String = conn
        .query_row(
            "SELECT context_json FROM autopilot_circuit_runs WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let stored_artifacts: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(bytes), 0) FROM circuit_run_artifacts WHERE run_id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();

    println!(
        "context payload written per advance: before {} bytes, after {} bytes \
         (column {} bytes, side table {} bytes)",
        full.len(),
        column.len(),
        column.len(),
        stored_artifacts
    );
    println!(
        "WAL bytes per steady-state advance (median) — flags only: before {} bytes, after {} bytes; \
         with a fresh report: before {} bytes, after {} bytes",
        baseline.wal_bytes,
        split.wal_bytes,
        baseline_with_report.wal_bytes,
        split_with_report.wal_bytes
    );
    println!(
        "WAL bytes on the first advance (the one that fills the side table): before {} bytes, \
         after {} bytes",
        baseline.first_wal_bytes, split.first_wal_bytes
    );
    println!(
        "WAL after each advance — before: {:?}; after: {:?}",
        baseline.wal_samples, split.wal_samples
    );
    println!(
        "database pages once the advances are done — before: {}, after: {} \
         (a run whose context carries the report needs an overflow chain; the \
         split keeps the row on one page and the report in the side table)",
        baseline.db_pages, split.db_pages
    );

    // Which pages are these? A run with nothing bulky in it writes the same
    // column, so it isolates what the side table costs.
    let small_only = measure_on_a_fresh_database("small", small_context, |conn, json| {
        super::super::ledger::commit_circuit_advance_locked(
            conn,
            1,
            Some("running"),
            Some(json),
            &[],
        )
        .unwrap();
    });
    println!(
        "small context only — WAL bytes per advance: {}",
        small_only.wal_bytes
    );

    // The side table's own pages. A repeat advance must not touch them: the
    // bulky values are written once, when they change.
    let artifact_pages: Vec<i64> = table_roots(&fresh_database("roots"))
        .into_iter()
        .filter(|(name, _)| name.contains("circuit_run_artifacts"))
        .map(|(_, page)| page)
        .collect();
    for pages in split.pages.iter().skip(1) {
        for page in pages {
            assert!(
                !artifact_pages.contains(&(*page as i64)),
                "the side table was rewritten: page {page}"
            );
        }
    }

    // The acceptance criterion: the column's size on a new run.
    assert!(
        column.len() < 4 * 1024,
        "the column still holds {} bytes",
        column.len()
    );
    assert!(
        column.len() < full.len() / 4,
        "the column did not shrink: {} vs {}",
        column.len(),
        full.len()
    );
    assert!(
        split.wal_bytes <= baseline.wal_bytes,
        "a steady-state advance grew the WAL: {} vs {}",
        split.wal_bytes,
        baseline.wal_bytes
    );
    assert!(
        split_with_report.wal_bytes <= baseline_with_report.wal_bytes,
        "an advance with a fresh report grew the WAL: {} vs {}",
        split_with_report.wal_bytes,
        baseline_with_report.wal_bytes
    );
    // A run whose bulky values live in the side table writes exactly as many
    // pages per advance as a run with nothing bulky in it: the run row and
    // its two indexes. The old shape's run row overflowed, so reading it cost
    // the overflow chain as well — the first advance below writes four extra
    // pages for it.
    for (index, pages) in split.pages.iter().enumerate().skip(1) {
        assert_eq!(
            pages.len(),
            small_only.pages[index].len(),
            "the split's advance touched more pages than a run with nothing bulky in it"
        );
    }
    assert!(
        baseline.pages[0].len() > baseline.pages[1].len(),
        "the old shape's run row did not overflow, so the measurement says nothing"
    );
}

/// The context of the n-th advance: the same run, one revision further on.
fn advancing_context(index: usize) -> String {
    let mut context = review_context();
    context.insert("evidence.revision".into(), index.to_string());
    serde_json::to_string(&context).unwrap()
}

/// The context of the n-th advance of a run that also produced a fresh report.
fn advancing_context_with_report(index: usize) -> String {
    let mut context = review_context();
    context.insert("evidence.revision".into(), index.to_string());
    context.insert(
        "node.reviewer.output".into(),
        format!("{index}-{}", "r".repeat(4_400)),
    );
    serde_json::to_string(&context).unwrap()
}

/// The context of a run with nothing bulky in it.
fn small_context(index: usize) -> String {
    let mut context: BTreeMap<String, String> = BTreeMap::new();
    context.insert("circuit.id".into(), "7".into());
    context.insert("review.provider".into(), "personal".into());
    context.insert("evidence.revision".into(), index.to_string());
    serde_json::to_string(&context).unwrap()
}

/// What one advance writes: the bytes the write-ahead log grew by and the
/// pages it grew by.
struct AdvanceCost {
    /// The median bytes one steady-state advance writes. The first advance is
    /// left out: it is the one that fills the side table. The median rather
    /// than the mean because SQLite's page allocation occasionally emits one
    /// extra frame for a row that moved, and one extra frame must not decide
    /// the comparison.
    wal_bytes: usize,
    /// The bytes the first advance writes.
    first_wal_bytes: usize,
    wal_samples: Vec<usize>,
    pages: Vec<Vec<u32>>,
    /// The pages the database holds once the advances are done.
    ///
    /// This is where the split shows: a run whose context carries 19 KB of
    /// agent output needs an overflow chain, so its row costs five pages; the
    /// split keeps the row on one page and puts the bulky values in the side
    /// table.
    db_pages: usize,
}

/// A fresh file-backed database, so one measurement cannot inherit another's
/// pages.
fn fresh_database(tag: &str) -> Connection {
    let dir = std::env::temp_dir().join(format!(
        "buildmesh-circuit-artifacts-bytes-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{tag}.db"));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(dir.join(format!("{tag}.db-wal")));
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.pragma_update(None, "synchronous", "NORMAL").unwrap();
    crate::db::init_schema(&conn).unwrap();
    seed(&conn);
    conn
}

/// The root page of every table and index, so a WAL frame's page number can
/// be named.
fn table_roots(conn: &Connection) -> Vec<(String, i64)> {
    let mut statement = conn
        .prepare("SELECT name, rootpage FROM sqlite_master WHERE rootpage > 0 ORDER BY rootpage")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn measure_on_a_fresh_database(
    tag: &str,
    build: impl Fn(usize) -> String,
    advance: impl Fn(&mut Connection, &str),
) -> AdvanceCost {
    let advances = 10usize;
    let mut conn = fresh_database(tag);
    let wal = wal_path(&conn);
    // Start from a known state so the delta is the advances' own writes.
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    let mut samples = Vec::new();
    let mut changes = Vec::new();
    let mut pages = Vec::new();
    let mut previous = file_len(&wal);
    for index in 0..advances {
        let before_changes = conn.total_changes();
        advance(&mut conn, &build(index));
        changes.push((conn.total_changes() - before_changes) as usize);
        let current = file_len(&wal);
        samples.push(current.saturating_sub(previous));
        pages.push(wal_pages(&wal, previous));
        previous = current;
    }
    println!("[{tag}] rows changed per advance: {changes:?}");
    println!("[{tag}] pages written per advance: {pages:?}");
    let mut steady = samples[1..].to_vec();
    steady.sort_unstable();
    let db_pages: usize = conn
        .query_row("PRAGMA page_count", [], |row| row.get::<_, i64>(0))
        .unwrap() as usize;
    AdvanceCost {
        wal_bytes: steady.get(steady.len() / 2).copied().unwrap_or(0),
        first_wal_bytes: samples.first().copied().unwrap_or(0),
        wal_samples: samples,
        pages,
        db_pages,
    }
}

/// The page numbers of the WAL frames written from `from` onwards.
fn wal_pages(path: &std::path::Path, from: usize) -> Vec<u32> {
    let bytes = std::fs::read(path).unwrap_or_default();
    if bytes.len() < 32 {
        return Vec::new();
    }
    let page_size = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    let frame = 24 + page_size;
    let mut pages = Vec::new();
    // The WAL header is 32 bytes. A checkpoint that truncates the log leaves
    // it empty, so the first frame after it starts after the header — reading
    // from byte zero would read the header as a frame and name garbage pages.
    let mut offset = if from == 0 { 32 } else { from };
    while offset + frame <= bytes.len() {
        pages.push(u32::from_be_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]));
        offset += frame;
    }
    pages
}

fn wal_path(conn: &Connection) -> std::path::PathBuf {
    let path: String = conn
        .query_row("PRAGMA database_list", [], |row| row.get::<_, String>(2))
        .unwrap();
    std::path::PathBuf::from(format!("{path}-wal"))
}

fn file_len(path: &std::path::Path) -> usize {
    std::fs::metadata(path)
        .map(|meta| meta.len() as usize)
        .unwrap_or(0)
}
