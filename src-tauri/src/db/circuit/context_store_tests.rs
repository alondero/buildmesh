//! Tests for `db::circuit::context_store` — the logical context seam of a
//! circuit run (issue #2672).
//!
//! The seam hides the split between the decision-relevant `context_json`
//! column and the bulky `circuit_run_artifacts` side table (issue #2601).
//! These tests read and write the run's context through the seam only: a
//! caller's set, get and remove mean the same thing on either side of the
//! storage threshold.
//!
//! Run with: cd src-tauri && cargo test db::circuit::context_store::tests

use std::collections::BTreeMap;

use rusqlite::{params, Connection};

use crate::env::test_helpers::TestDir;

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

/// The run's column, as it is on disk.
fn column_of(conn: &Connection, run_id: i64) -> String {
    conn.query_row(
        "SELECT context_json FROM autopilot_circuit_runs WHERE id = ?1",
        [run_id],
        |row| row.get(0),
    )
    .unwrap()
}

/// Replace the run's whole context and write the column back.
fn replace_all(conn: &Connection, run_id: i64, values: BTreeMap<String, String>) -> String {
    let tx = conn.unchecked_transaction().unwrap();
    let change = ContextChange::from(ContextReplacement::with_values(values));
    let applied = apply(&tx, run_id, &change).unwrap();
    let column = applied.column_json().to_string();
    tx.execute(
        "UPDATE autopilot_circuit_runs SET context_json = ?2 WHERE id = ?1",
        params![run_id, column],
    )
    .unwrap();
    tx.commit().unwrap();
    column
}

/// Apply a mutation and write the column back.
fn mutate(conn: &Connection, run_id: i64, mutation: &ContextMutation) -> String {
    let tx = conn.unchecked_transaction().unwrap();
    let applied = apply_mutation(&tx, run_id, mutation).unwrap();
    let column = applied.column_json().to_string();
    tx.execute(
        "UPDATE autopilot_circuit_runs SET context_json = ?2 WHERE id = ?1",
        params![run_id, column],
    )
    .unwrap();
    tx.commit().unwrap();
    column
}

/// How many stored bulky values the fixture left behind.
fn artifact_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM circuit_run_artifacts", [], |row| {
        row.get(0)
    })
    .unwrap()
}

/// How many rows a table holds.
fn row_count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

/// The fingerprint of the stored bulky set, read out of the column.
fn digest_of(column_json: &str) -> Option<String> {
    serde_json::from_str::<BTreeMap<String, String>>(column_json)
        .unwrap()
        .get(super::storage::DIGEST_KEY)
        .cloned()
}

#[test]
fn a_bulky_value_moves_out_of_the_column_and_a_small_one_stays() {
    let conn = schema();
    seed(&conn);
    let column = replace_all(&conn, 1, review_context());

    let stored: BTreeMap<String, String> = serde_json::from_str(&column).unwrap();
    assert!(stored.contains_key("review.provider"));
    assert!(stored.contains_key("circuit.id"));
    for key in [
        "source.output",
        "node.reviewer.output",
        "node.reviewer.evaluated_output",
        "node.await_source.evidence.1",
    ] {
        assert!(
            !stored.contains_key(key),
            "{key} should have moved to the side table"
        );
    }
    // The column keeps only the decision-relevant half.
    assert!(column.len() < 4 * 1024, "column was {} bytes", column.len());
}

#[test]
fn a_bulky_value_names_its_node_and_attempt() {
    // The side table records which node a stored value belongs to and which
    // attempt it belongs to, so a stored value can be addressed without
    // parsing its key.
    let conn = schema();
    seed(&conn);
    replace_all(&conn, 1, review_context());

    let evidence: (String, i64) = conn
        .query_row(
            "SELECT node_id, attempt FROM circuit_run_artifacts
             WHERE run_id = 1 AND key = 'node.await_source.evidence.1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(evidence, ("await_source".into(), 1));

    let output: (String, i64) = conn
        .query_row(
            "SELECT node_id, attempt FROM circuit_run_artifacts
             WHERE run_id = 1 AND key = 'node.reviewer.output'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(output, ("reviewer".into(), 0));

    let source: String = conn
        .query_row(
            "SELECT node_id FROM circuit_run_artifacts
             WHERE run_id = 1 AND key = 'source.output'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(source, "", "a run-level value was filed under a node");
}

#[test]
fn the_operator_projection_does_not_carry_a_lifecycle_blocker() {
    // A lifecycle blocker is runtime state, not context. The store never
    // persists one, and the projection does not resurrect one from a
    // historical row: the operator sees the run's context, not a veto that
    // outlived the report that raised it.
    let conn = schema();
    seed(&conn);
    conn.execute(
        "UPDATE autopilot_circuit_runs SET context_json = ?1 WHERE id = 1",
        params![r#"{"node.reviewer.lifecycle_blocker":"blocked"}"#],
    )
    .unwrap();

    let served = load_served_json(&conn, 1).unwrap();
    let served: BTreeMap<String, String> = serde_json::from_str(&served).unwrap();
    assert!(
        !served.contains_key("node.reviewer.lifecycle_blocker"),
        "the projection carried a runtime blocker"
    );
}

#[test]
fn a_commit_splits_the_context_and_a_read_joins_it_again() {
    let conn = schema();
    seed(&conn);
    let context = review_context();
    replace_all(&conn, 1, context.clone());

    let column = column_of(&conn, 1);
    assert!(column.len() < 4 * 1024, "column was {} bytes", column.len());
    assert!(!column.contains("node.reviewer.output"));

    // The joined read is the context the worker had.
    let joined = load_logical(&conn, 1).unwrap();
    for (key, value) in &context {
        assert_eq!(joined.get(key), Some(value.as_str()), "{key} was lost");
    }
}

#[test]
fn set_get_and_remove_mean_the_same_thing_on_either_side_of_the_threshold() {
    for size in [1_023usize, 1_024, 1_025] {
        let conn = schema();
        seed(&conn);
        let value = "v".repeat(size);
        let mut context = BTreeMap::new();
        context.insert("node.reviewer.output".into(), value.clone());
        replace_all(&conn, 1, context);

        // The value reads back the same, whichever half holds it.
        let logical = load_logical(&conn, 1).unwrap();
        assert_eq!(logical.get("node.reviewer.output"), Some(value.as_str()));

        // And removing it clears it, whichever half held it.
        let mut mutation = ContextMutation::new();
        mutation.remove("node.reviewer.output");
        mutate(&conn, 1, &mutation);

        let logical = load_logical(&conn, 1).unwrap();
        assert_eq!(
            logical.get("node.reviewer.output"),
            None,
            "a value of {size} bytes survived its removal"
        );
    }
}

#[test]
fn an_explicit_removal_clears_a_bulky_value() {
    // The contract the seam exists for: a removal means the value is gone,
    // whichever half of the storage held it.
    let conn = schema();
    seed(&conn);
    let context = review_context();
    replace_all(&conn, 1, context.clone());
    assert_eq!(artifact_count(&conn), 4);

    let mut mutation = ContextMutation::new();
    mutation.remove("node.reviewer.output");
    mutate(&conn, 1, &mutation);

    let joined = load_logical(&conn, 1).unwrap();
    assert_eq!(joined.get("node.reviewer.output"), None);
    assert_eq!(
        joined.get("node.reviewer.evaluated_output").map(str::len),
        Some(5_000),
        "a sibling value was lost"
    );
}

#[test]
fn a_partial_mutation_that_omits_an_unloaded_artifact_leaves_it_alone() {
    let conn = schema();
    seed(&conn);
    replace_all(&conn, 1, review_context());

    // A change that mentions only the flags leaves the bulky values alone: a
    // key the mutation does not mention is unchanged.
    let mut mutation = ContextMutation::new();
    mutation.put("evidence.revision", "2");
    mutate(&conn, 1, &mutation);

    let joined = load_logical(&conn, 1).unwrap();
    assert_eq!(
        joined.get("node.reviewer.output").map(str::len),
        Some(4_400),
        "a stored value was dropped by a mutation that did not mention it"
    );
}

#[test]
fn a_complete_replacement_deletes_the_key_it_omits() {
    let conn = schema();
    seed(&conn);
    replace_all(&conn, 1, review_context());

    // A replacement is the whole context: a key it does not mention is
    // gone, in both halves of the storage.
    let mut replacement = BTreeMap::new();
    replacement.insert("circuit.id".into(), "7".into());
    replace_all(&conn, 1, replacement);

    let joined = load_logical(&conn, 1).unwrap();
    assert_eq!(joined.get("circuit.id"), Some("7"));
    assert_eq!(joined.get("node.reviewer.output"), None);
    assert_eq!(joined.get("source.output"), None);
    assert_eq!(
        artifact_count(&conn),
        0,
        "the side table kept a value the replacement dropped"
    );
}

#[test]
fn a_scoped_reset_clears_one_nodes_working_state_and_leaves_its_siblings() {
    let conn = schema();
    seed(&conn);
    replace_all(&conn, 1, review_context());
    // The run's history is an audit record: clearing a node's working state
    // does not rewrite it.
    conn.execute(
        "INSERT INTO circuit_run_history(run_id,node_id,attempt,kind,detail) VALUES(1,'reviewer',1,'observation','{\"observations\":[]}')",
        [],
    )
    .unwrap();

    let mut mutation = ContextMutation::new();
    mutation.reset(ContextScope::node("reviewer"));
    mutate(&conn, 1, &mutation);

    let joined = load_logical(&conn, 1).unwrap();
    assert_eq!(joined.get("node.reviewer.output"), None);
    assert_eq!(joined.get("node.reviewer.evaluated_output"), None);
    assert!(joined.get("node.await_source.evidence.1").is_some());
    assert!(joined.get("source.output").is_some());
    assert!(joined.get("circuit.id").is_some());

    let history: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM circuit_run_history WHERE run_id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(history, 1, "the reset rewrote the run's history");
}

#[test]
fn a_put_is_the_last_word_on_the_key_it_names() {
    let conn = schema();
    seed(&conn);
    replace_all(&conn, 1, review_context());

    // The resets and the removals are applied before the puts, so a value
    // the change sets survives a reset of the same scope.
    let mut mutation = ContextMutation::new();
    mutation.reset(ContextScope::node("reviewer"));
    mutation.put("node.reviewer.status", "completed");
    mutate(&conn, 1, &mutation);

    let joined = load_logical(&conn, 1).unwrap();
    assert_eq!(joined.get("node.reviewer.status"), Some("completed"));
    assert_eq!(joined.get("node.reviewer.output"), None);
}

#[test]
fn an_unchanged_bulky_value_is_written_once() {
    let conn = schema();
    seed(&conn);
    let context = review_context();

    replace_all(&conn, 1, context.clone());
    // Pin the row's timestamp, so a rewrite cannot hide behind the clock's
    // one-second resolution.
    conn.execute(
        "UPDATE circuit_run_artifacts SET updated_at = '2000-01-01 00:00:00' WHERE run_id = 1",
        [],
    )
    .unwrap();

    replace_all(&conn, 1, context);

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
    let mut context = review_context();
    replace_all(&conn, 1, context.clone());

    context.insert("source.output".into(), "short".into());
    replace_all(&conn, 1, context);

    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM circuit_run_artifacts WHERE run_id = 1 AND key = 'source.output'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0, "the superseded copy survived");

    let joined = load_logical(&conn, 1).unwrap();
    assert_eq!(joined.get("source.output"), Some("short"));
}

#[test]
fn the_digest_changes_when_a_bulky_value_changes() {
    let conn = schema();
    seed(&conn);
    let mut context = review_context();
    let column = replace_all(&conn, 1, context.clone());
    let before = digest_of(&column).expect("the fixture stored a bulky value");

    context.insert("source.output".into(), "a different report".into());
    let column = replace_all(&conn, 1, context);

    assert_ne!(
        before,
        digest_of(&column).expect("the stored set has a fingerprint")
    );
}

#[test]
fn a_run_with_nothing_bulky_carries_no_digest() {
    let conn = schema();
    seed(&conn);
    let mut context: BTreeMap<String, String> = BTreeMap::new();
    context.insert("circuit.id".into(), "7".into());
    context.insert("review.provider".into(), "personal".into());

    let column = replace_all(&conn, 1, context.clone());

    // A run with nothing bulky in it has nothing to fingerprint, so its
    // context stays exactly what it was.
    let stored: BTreeMap<String, String> = serde_json::from_str(&column).unwrap();
    assert!(digest_of(&column).is_none());
    assert_eq!(stored, context);
}

#[test]
fn the_digest_covers_the_settled_set() {
    let conn = schema();
    seed(&conn);
    let mut context = review_context();
    let column = replace_all(&conn, 1, context.clone());
    let before = digest_of(&column).expect("the fixture stored a bulky value");

    // A value that becomes small leaves the stored set, so the fingerprint of
    // that set changes even though no value was rewritten.
    context.insert("source.output".into(), "short".into());
    let column = replace_all(&conn, 1, context);

    assert_ne!(
        before,
        digest_of(&column).expect("the stored set has a fingerprint")
    );
}

#[test]
fn a_mutation_cannot_store_the_reserved_fingerprint_key() {
    let conn = schema();
    seed(&conn);
    let mut mutation = ContextMutation::new();
    mutation.put(
        super::storage::DIGEST_KEY,
        "a fingerprint the caller made up",
    );
    mutation.put("circuit.id", "7");
    let column = mutate(&conn, 1, &mutation);

    let stored: BTreeMap<String, String> = serde_json::from_str(&column).unwrap();
    assert_eq!(stored.get("circuit.id").map(String::as_str), Some("7"));
    assert!(
        !stored.contains_key(super::storage::DIGEST_KEY),
        "the fingerprint is storage state, so a value that claims to be one is dropped"
    );
}

#[test]
fn the_strict_stored_read_refuses_a_payload_it_cannot_read() {
    let flat = StoredContext::strict_of_column(r#"{"circuit.id":"7"}"#).unwrap();
    assert_eq!(flat.get("circuit.id"), Some("7"));

    let error = StoredContext::strict_of_column(r#"{"x":{"y":"z"}}"#).unwrap_err();
    assert!(
        matches!(&error, DbError::Rendered(message) if message.contains("invalid context_json")),
        "a payload that is not a flat map has no decision-relevant values to read: {error:?}"
    );
}

#[test]
fn the_run_card_projection_serves_the_bulky_half() {
    let conn = schema();
    seed(&conn);
    let context = review_context();
    replace_all(&conn, 1, context.clone());

    // The column holds the small half only.
    let column = column_of(&conn, 1);
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
    // The fingerprint is a storage detail: the operator sees the run's
    // context.
    assert!(digest_of(&runs[0].context_json).is_none());
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
    replace_all(&conn, 1, context.clone());

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
fn a_batch_read_issues_a_bounded_number_of_queries() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use rusqlite::trace::{TraceEvent, TraceEventCodes};

    let mut conn = schema();
    seed(&conn);
    // Every run carries a bulky value of its own, the seeded one included.
    let mut first = BTreeMap::new();
    first.insert("source.output".to_string(), "report 1".repeat(400));
    replace_all(&conn, 1, first);
    // Eleven more runs, each with a bulky value of its own.
    for run_id in 2..=12i64 {
        conn.execute(
            "INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,trigger_identity,state) VALUES(?1,1,1,?2,'running')",
            params![run_id, format!("manual:{run_id}")],
        )
        .unwrap();
        let mut values = BTreeMap::new();
        values.insert(
            "source.output".to_string(),
            format!("report {run_id}").repeat(400),
        );
        replace_all(&conn, run_id, values);
    }

    // The statement trace counts what the connection actually runs.
    let statements = Arc::new(AtomicUsize::new(0));
    let counted = statements.clone();
    conn.trace_v2(
        TraceEventCodes::all(),
        Some(move |event: TraceEvent<'_>| {
            if matches!(event, TraceEvent::Stmt(..)) {
                counted.fetch_add(1, Ordering::SeqCst);
            }
        }),
    )
    .unwrap();
    let ids: Vec<i64> = (1..=12).collect();
    let served = load_served_json_for_runs(&conn, &ids).unwrap();

    assert_eq!(served.len(), 12);
    assert!(
        served.values().all(|json| json.contains("source.output")),
        "a batch read lost a run's bulky half"
    );
    // The run columns and the side table are read once each, whatever the
    // number of runs. A per-run read would issue one statement per run.
    assert!(
        statements.load(Ordering::SeqCst) <= 4,
        "the batch read issued {} statements for 12 runs",
        statements.load(Ordering::SeqCst)
    );
}

#[test]
fn a_logical_batch_read_issues_a_bounded_number_of_queries() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use rusqlite::trace::{TraceEvent, TraceEventCodes};

    let mut conn = schema();
    seed(&conn);
    // Every run carries a bulky value of its own, the seeded one included.
    let mut first = BTreeMap::new();
    first.insert("source.output".to_string(), "report 1".repeat(400));
    replace_all(&conn, 1, first);
    // Eleven more runs, each with a bulky value of its own.
    for run_id in 2..=12i64 {
        conn.execute(
            "INSERT INTO autopilot_circuit_runs(id,circuit_id,mesh_id,trigger_identity,state) VALUES(?1,1,1,?2,'running')",
            params![run_id, format!("manual:{run_id}")],
        )
        .unwrap();
        let mut values = BTreeMap::new();
        values.insert(
            "source.output".to_string(),
            format!("report {run_id}").repeat(400),
        );
        replace_all(&conn, run_id, values);
    }

    // The statement trace counts what the connection actually runs.
    let statements = Arc::new(AtomicUsize::new(0));
    let counted = statements.clone();
    conn.trace_v2(
        TraceEventCodes::all(),
        Some(move |event: TraceEvent<'_>| {
            if matches!(event, TraceEvent::Stmt(..)) {
                counted.fetch_add(1, Ordering::SeqCst);
            }
        }),
    )
    .unwrap();
    let ids: Vec<i64> = (1..=12).collect();
    // The strict read is the one the phone's run listing uses.
    let logical = load_logical_for_runs(&conn, &ids).unwrap();

    assert_eq!(logical.len(), 12);
    assert!(
        logical
            .values()
            .all(|context| context.get("source.output").is_some()),
        "a logical batch read lost a run's bulky half"
    );
    // The run columns and the side table are read once each, whatever the
    // number of runs. A per-run read would issue one statement per run.
    assert!(
        statements.load(Ordering::SeqCst) <= 4,
        "the logical batch read issued {} statements for 12 runs",
        statements.load(Ordering::SeqCst)
    );
}

#[test]
fn the_freshness_fence_reads_the_column_alone() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use rusqlite::trace::{TraceEvent, TraceEventCodes};

    let mut conn = schema();
    seed(&conn);
    replace_all(&conn, 1, review_context());
    let expected = ContextRevision::of_column(&column_of(&conn, 1));

    let statements = Arc::new(AtomicUsize::new(0));
    let counted = statements.clone();
    conn.trace_v2(
        TraceEventCodes::all(),
        Some(move |event: TraceEvent<'_>| {
            if matches!(event, TraceEvent::Stmt(..)) {
                counted.fetch_add(1, Ordering::SeqCst);
            }
        }),
    )
    .unwrap();
    assert!(revision_if_unchanged(&conn, 1, "running", &expected)
        .unwrap()
        .is_some());

    // One statement, and it is the column read. The bulky half is never
    // rehydrated to answer a freshness question.
    assert_eq!(
        statements.load(Ordering::SeqCst),
        1,
        "the freshness fence issued {} statements",
        statements.load(Ordering::SeqCst)
    );
}

#[test]
fn the_circuit_ledger_projection_serves_the_bulky_half() {
    let conn = schema();
    seed(&conn);
    let context = review_context();
    replace_all(&conn, 1, context.clone());

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
    replace_all(&conn, 1, context.clone());

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
    replace_all(&conn, 1, context.clone());

    // The worker resolves `{{node.reviewer.output}}` and reads a step's
    // evidence out of the context, so its read has to carry the bulky half.
    let joined = load_logical(&conn, 1).unwrap().runtime();
    for (key, value) in &context {
        assert_eq!(joined.get(key), Some(value.as_str()), "{key} was lost");
    }
}

#[test]
fn a_persisted_lifecycle_blocker_cannot_be_resurrected() {
    // A lifecycle blocker is runtime state, not context: it never travels in
    // a persisted context, and a historical one is cleaned up rather than
    // carried forward.
    let conn = schema();
    seed(&conn);
    let mut context: BTreeMap<String, String> = BTreeMap::new();
    context.insert("node.reviewer.lifecycle_blocker".into(), "b".repeat(2_000));
    replace_all(&conn, 1, context);

    let joined = load_logical(&conn, 1).unwrap();
    assert!(
        joined.get("node.reviewer.lifecycle_blocker").is_none(),
        "a persisted lifecycle blocker came back into the context"
    );
    let stored: BTreeMap<String, String> = serde_json::from_str(&column_of(&conn, 1)).unwrap();
    assert!(
        !stored.contains_key("node.reviewer.lifecycle_blocker"),
        "a lifecycle blocker was persisted"
    );
    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM circuit_run_artifacts WHERE run_id = 1 AND key = 'node.reviewer.lifecycle_blocker'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0, "a lifecycle blocker was stored");
}

#[test]
fn deleting_a_circuit_takes_its_artifacts_with_it() {
    let mut conn = schema();
    seed(&conn);
    replace_all(&conn, 1, review_context());
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
    replace_all(&conn, 1, review_context());
    assert!(artifact_count(&conn) > 0, "the fixture stored nothing");

    super::super::ledger::delete_circuits_for_mesh_inner(&conn, 1).unwrap();

    assert_eq!(
        artifact_count(&conn),
        0,
        "the mesh's artifacts survived the mesh"
    );
}

#[test]
fn a_failed_commit_leaves_neither_half_nor_the_run_and_step_history() {
    let conn = schema();
    seed(&conn);
    replace_all(&conn, 1, review_context());
    let before_column = column_of(&conn, 1);
    let before_artifacts = artifact_count(&conn);
    let before_steps = row_count(&conn, "autopilot_circuit_run_steps");
    let before_history = row_count(&conn, "circuit_run_history");

    // A trigger that refuses the step row, so the engine's commit point
    // fails after the context has been written and after the history rows
    // have been appended.
    conn.execute_batch(
        "CREATE TRIGGER refuse_the_step BEFORE INSERT ON autopilot_circuit_run_steps
         BEGIN SELECT RAISE(ABORT, 'the step is refused'); END;",
    )
    .unwrap();

    let tx = conn.unchecked_transaction().unwrap();
    let mut mutation = ContextMutation::new();
    mutation.put("source.output", "a brand new report");
    let change = ContextChange::from(mutation);
    let step = super::super::ledger::CircuitStepOp {
        node_id: "reviewer".into(),
        status: "working".into(),
        outcome: None,
        error: None,
        agent_node_id: None,
        attempt: 1,
        fresh_attempt: false,
    };
    let outcome =
        super::super::ledger::commit_circuit_advance_inner(&tx, 1, None, Some(&change), &[step]);
    assert!(
        outcome.is_err(),
        "the commit succeeded where it should have failed"
    );
    // The transaction is never committed, so the whole commit point rolls
    // back together.
    drop(tx);

    assert_eq!(
        column_of(&conn, 1),
        before_column,
        "a failed commit still wrote the column"
    );
    assert_eq!(
        artifact_count(&conn),
        before_artifacts,
        "a failed commit still wrote the side table"
    );
    assert_eq!(
        row_count(&conn, "autopilot_circuit_run_steps"),
        before_steps,
        "a failed commit still wrote a step row"
    );
    assert_eq!(
        row_count(&conn, "circuit_run_history"),
        before_history,
        "a failed commit still wrote a history row"
    );
}

#[test]
fn the_admission_and_retention_paths_do_not_rehydrate_bulky_values() {
    use std::sync::{Arc, Mutex};

    use rusqlite::trace::{TraceEvent, TraceEventCodes};

    let mut conn = schema();
    seed(&conn);
    replace_all(&conn, 1, review_context());
    assert!(artifact_count(&conn) > 0, "the fixture stored nothing");
    // A circuit whose graph carries a review retry gate, so the admission
    // probe has a budget to find.
    conn.execute(
        "UPDATE autopilot_circuits SET graph_json=?1 WHERE id = 1",
        [
            crate::circuit::model::CircuitGraph::agent_review(None, None, 3)
                .to_json()
                .unwrap(),
        ],
    )
    .unwrap();

    // Every statement the connection runs is recorded.
    let statements = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorded = statements.clone();
    conn.trace_v2(
        TraceEventCodes::all(),
        Some(move |event: TraceEvent<'_>| {
            if let TraceEvent::Stmt(_, sql) = event {
                recorded.lock().unwrap().push(sql.to_string());
            }
        }),
    )
    .unwrap();

    // Admission: the review budget is read from the run row's column alone.
    let run = super::super::ledger::get_circuit_run_inner(&conn, 1)
        .unwrap()
        .expect("the seeded run");
    let limit = super::super::review_rounds::current_limit(&conn, &run).unwrap();
    assert!(
        limit.is_some(),
        "the admission probe found no review budget"
    );

    // Retention: a settled run the sweep will compact. It reads no context
    // to decide, and it drops the run's stored values with it.
    conn.execute(
        "UPDATE autopilot_circuit_runs SET trigger_identity='github:42', state='failed',
                created_at='2020-01-01 00:00:00', updated_at='2020-01-01 00:00:00'
         WHERE id = 1",
        [],
    )
    .unwrap();
    super::super::leases::prune_terminal_circuit_runs_before_inner(&conn, "2999-01-01").unwrap();

    let recorded = statements.lock().unwrap().clone();
    for sql in recorded {
        if sql.to_ascii_lowercase().contains("circuit_run_artifacts") {
            assert!(
                sql.to_ascii_lowercase().starts_with("delete"),
                "a path read the bulky half back: {sql}"
            );
        }
    }
    // The sweep did its work: the run it compacted took its stored values
    // with it, without reading them.
    assert_eq!(
        artifact_count(&conn),
        0,
        "the retention sweep left the compacted run's stored values behind"
    );
}

#[test]
fn a_mutation_refuses_a_payload_it_cannot_read() {
    let conn = schema();
    seed(&conn);
    // A historical payload that is not a flat map: the seam cannot read its
    // values, so a mutation that would keep them cannot be applied.
    let raw = r#"{"vars":{"node.reviewer.output":"a report","node.verdict.status":"failed"}}"#;
    conn.execute(
        "UPDATE autopilot_circuit_runs SET context_json = ?2 WHERE id = ?1",
        params![1, raw],
    )
    .unwrap();

    let mut mutation = ContextMutation::new();
    mutation.put("review.round_limit", "3".to_string());
    let refused = apply_mutation(&conn, 1, &mutation);

    let message = match refused {
        Ok(applied) => panic!(
            "a mutation wrote over a payload it could not read: {}",
            applied.column_json()
        ),
        Err(error) => error.to_string(),
    };
    assert!(
        message.contains("invalid context_json"),
        "the refusal does not name the unreadable payload: {message}"
    );
    // The historical payload is untouched: the write refused it rather than
    // replacing it with the mutation's keys.
    assert_eq!(
        column_of(&conn, 1),
        raw,
        "the refused mutation still overwrote the historical payload"
    );
}

#[test]
fn a_key_that_lives_in_both_halves_keeps_the_columns_copy() {
    let conn = schema();
    seed(&conn);
    // A legacy overlap: the column holds a small copy of a key the side
    // table also holds.
    conn.execute(
        "INSERT INTO circuit_run_artifacts(run_id, key, node_id, attempt, value, bytes)
         VALUES (1,'source.output','source',0,?1,?2)",
        params!["x".repeat(2_000), 2_000],
    )
    .unwrap();
    conn.execute(
        "UPDATE autopilot_circuit_runs SET context_json = ?2 WHERE id = ?1",
        params![1, r#"{"source.output":"small"}"#],
    )
    .unwrap();

    let mut mutation = ContextMutation::new();
    mutation.put("review.round_limit", "3".to_string());
    let column = mutate(&conn, 1, &mutation);

    assert_eq!(
        artifact_count(&conn),
        0,
        "the stored copy of a key the column holds survived the write"
    );
    let values: BTreeMap<String, String> = serde_json::from_str(&column).unwrap();
    assert_eq!(
        values.get("source.output").map(String::as_str),
        Some("small")
    );
    assert_eq!(
        values.get("review.round_limit").map(String::as_str),
        Some("3")
    );
}

#[test]
fn the_fence_reads_the_context_and_the_history_revision_together() {
    let conn = schema();
    seed(&conn);
    replace_all(&conn, 1, review_context());
    let expected = ContextRevision::of_column(&column_of(&conn, 1));

    conn.execute(
        "INSERT INTO circuit_run_history(run_id, node_id, attempt, kind, detail)
         VALUES (1,'reviewer',1,'observation','{}')",
        [],
    )
    .unwrap();
    let history: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(id),0) FROM circuit_run_history WHERE run_id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();

    // The fence reports the history revision the context was read with, so
    // the two cannot come from different moments.
    assert_eq!(
        revision_if_unchanged(&conn, 1, "running", &expected).unwrap(),
        Some(history)
    );
    // A run that has moved to another state is stale in the same way.
    assert_eq!(
        revision_if_unchanged(&conn, 1, "paused", &expected).unwrap(),
        None
    );
}

#[test]
fn the_seam_deletes_the_stored_values_of_the_runs_a_query_names() {
    let conn = schema();
    seed(&conn);
    replace_all(&conn, 1, review_context());
    assert!(artifact_count(&conn) > 0, "the fixture stored nothing");

    let deleted = delete_stored_values_for_runs(
        &conn,
        "SELECT id FROM autopilot_circuit_runs WHERE id = 1",
        params![],
    )
    .unwrap();

    assert!(deleted > 0, "the seam deleted nothing");
    assert_eq!(
        artifact_count(&conn),
        0,
        "the runs' stored values survived the sweep"
    );
}

#[test]
fn adopting_the_split_storage_moves_a_runs_context_into_it() {
    let conn = schema();
    seed(&conn);
    // A context written before the split: one flat payload holding a bulky
    // value.
    let raw = serde_json::to_string(&review_context()).unwrap();
    conn.execute(
        "UPDATE autopilot_circuit_runs SET context_json = ?2 WHERE id = ?1",
        params![1, raw],
    )
    .unwrap();
    assert_eq!(artifact_count(&conn), 0);

    let column = adopt_split_storage(&conn, 1, &raw)
        .unwrap()
        .expect("a bulky context has something to move");
    assert!(
        column.len() < raw.len(),
        "the split left the column as large as the payload it split"
    );
    conn.execute(
        "UPDATE autopilot_circuit_runs SET context_json = ?2 WHERE id = ?1",
        params![1, column],
    )
    .unwrap();
    assert!(artifact_count(&conn) > 0, "the values never moved");

    // A context with nothing to move keeps the column it has.
    assert!(
        adopt_split_storage(&conn, 1, r#"{"review.provider":"personal"}"#)
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_bulky_only_change_moves_the_revision_the_fence_reads() {
    let conn = schema();
    seed(&conn);
    let mut context = review_context();
    replace_all(&conn, 1, context.clone());
    let before = column_of(&conn, 1);
    let expected = ContextRevision::of_column(&before);
    assert!(revision_if_unchanged(&conn, 1, "running", &expected)
        .unwrap()
        .is_some());

    // Only the bulky value changes. The small flags are untouched.
    context.insert("source.output".into(), "a different report".into());
    replace_all(&conn, 1, context);
    let after = column_of(&conn, 1);

    assert_ne!(
        before, after,
        "a bulky-only change left the column identical, so the freshness fence would not fire"
    );
    // The fence's own predicate: the revision the caller loaded no longer
    // matches the stored one, so the older expected revision is rejected.
    assert!(
        revision_if_unchanged(&conn, 1, "running", &expected)
            .unwrap()
            .is_none(),
        "the fence still holds after a bulky change"
    );
    assert_ne!(
        StoredContext::of_column(&column_of(&conn, 1)).revision(),
        &expected
    );
}

#[test]
fn a_failed_transaction_commits_neither_half() {
    let conn = schema();
    seed(&conn);
    replace_all(&conn, 1, review_context());
    let before_column = column_of(&conn, 1);
    let before_artifacts = artifact_count(&conn);

    // The change lands in the transaction, and the transaction fails.
    let fail_after_mutation = || -> Result<(), String> {
        let tx = conn.unchecked_transaction().unwrap();
        let mut mutation = ContextMutation::new();
        mutation.put("source.output", "a brand new report");
        apply_mutation(&tx, 1, &mutation).unwrap();
        Err("the transaction fails after the artifact mutation".to_string())
    };
    let outcome: Result<(), String> = fail_after_mutation();
    assert!(outcome.is_err());

    // The transaction rolled back, so neither half of the context moved.
    assert_eq!(column_of(&conn, 1), before_column);
    assert_eq!(artifact_count(&conn), before_artifacts);
    let joined = load_logical(&conn, 1).unwrap();
    assert_eq!(
        joined.get("source.output").map(str::len),
        Some(2_600),
        "a rolled-back change left its value behind"
    );
}

#[test]
fn a_node_id_with_an_underscore_does_not_match_a_sibling() {
    let conn = schema();
    seed(&conn);
    let mut context: BTreeMap<String, String> = BTreeMap::new();
    context.insert("node.await_source.output".into(), "s".repeat(2_000));
    context.insert("node.awaitXsource.output".into(), "x".repeat(2_000));
    replace_all(&conn, 1, context);

    let mut mutation = ContextMutation::new();
    mutation.reset(ContextScope::node("await_source"));
    mutate(&conn, 1, &mutation);

    let joined = load_logical(&conn, 1).unwrap();
    assert_eq!(joined.get("node.await_source.output"), None);
    assert!(joined.get("node.awaitXsource.output").is_some());
}

#[test]
fn a_wait_path_stays_in_the_column_however_long_it_is() {
    // The wait history compares the previous and the next context, so a long
    // blocker message has to stay where that comparison can see it.
    let conn = schema();
    seed(&conn);
    let mut context: BTreeMap<String, String> = BTreeMap::new();
    context.insert(
        "node.reviewer.observation_blocker".into(),
        "b".repeat(4_000),
    );
    let column = replace_all(&conn, 1, context);

    let stored: BTreeMap<String, String> = serde_json::from_str(&column).unwrap();
    assert!(stored.contains_key("node.reviewer.observation_blocker"));
    assert_eq!(artifact_count(&conn), 0);
}

#[test]
fn a_malformed_payload_is_served_as_it_stands_and_a_logical_read_fails() {
    let conn = schema();
    seed(&conn);
    // A historical payload that is not a flat string map: the split is a
    // storage optimisation, not a validation step, so the store keeps it as
    // it stands rather than normalising or erasing it.
    let payload = "{\"not\":{\"a\":\"flat map\"}}";
    conn.execute(
        "UPDATE autopilot_circuit_runs SET context_json = ?1 WHERE id = 1",
        params![payload],
    )
    .unwrap();

    // A projection serves the payload back unchanged: a read neither
    // normalises nor erases a historical payload.
    let served = load_served_json(&conn, 1).unwrap();
    assert_eq!(served, payload);

    // A logical read that needs a map reports the payload it cannot read.
    assert!(load_logical(&conn, 1).is_err());
    // The lenient read says what it can read, which is nothing of the
    // payload itself.
    assert!(load_logical_best_effort(&conn, 1)
        .unwrap()
        .values()
        .is_empty());
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

    let column = column_of(&conn, 1);
    assert!(column.len() < 4 * 1024, "column was {} bytes", column.len());
    let joined = load_logical(&conn, 1).unwrap();
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
/// the small flags on and one that brings a fresh report.
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
    // One claimed scratch directory for the whole measurement: it is removed
    // when this test ends, so nothing is left behind in `%TEMP%`.
    let dir = TestDir::new("circuit-context-store-bytes");

    // Baseline: the pre-change shape — the whole context rewritten as one
    // blob, through the same statement the engine's commit uses.
    let baseline =
        measure_on_a_fresh_database("baseline", &dir, advancing_context, |conn, json| {
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
    let split = measure_on_a_fresh_database("split", &dir, advancing_context, |conn, json| {
        let values: BTreeMap<String, String> = serde_json::from_str(json).unwrap();
        let change = ContextChange::from(ContextReplacement::with_values(values));
        super::super::ledger::commit_circuit_advance_locked(
            conn,
            1,
            Some("running"),
            Some(&change),
            &[],
        )
        .unwrap();
    });

    // The same comparison for an advance that brings a fresh report: the
    // reviewer's output is replaced, so the bulky half changes too.
    let baseline_with_report = measure_on_a_fresh_database(
        "baseline-report",
        &dir,
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
        &dir,
        advancing_context_with_report,
        |conn, json| {
            let values: BTreeMap<String, String> = serde_json::from_str(json).unwrap();
            let change = ContextChange::from(ContextReplacement::with_values(values));
            super::super::ledger::commit_circuit_advance_locked(
                conn,
                1,
                Some("running"),
                Some(&change),
                &[],
            )
            .unwrap();
        },
    );

    let conn = fresh_database("column", &dir);
    let column = replace_all(&conn, 1, review_context());
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
    let small_only = measure_on_a_fresh_database("small", &dir, small_context, |conn, json| {
        let values: BTreeMap<String, String> = serde_json::from_str(json).unwrap();
        let change = ContextChange::from(ContextReplacement::with_values(values));
        super::super::ledger::commit_circuit_advance_locked(
            conn,
            1,
            Some("running"),
            Some(&change),
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
    let artifact_pages: Vec<i64> = table_roots(&fresh_database("roots", &dir))
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
///
/// The directory comes from a [`TestDir`], which claims it and removes it on
/// drop. A hand-built `%TEMP%` directory leaks: Windows recycles process ids,
/// so the next run finds the last run's files and the accumulation is
/// unbounded.
fn fresh_database(tag: &str, dir: &TestDir) -> Connection {
    let path = dir.path().join(format!("{tag}.db"));
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
    dir: &TestDir,
    build: impl Fn(usize) -> String,
    advance: impl Fn(&mut Connection, &str),
) -> AdvanceCost {
    let advances = 10usize;
    let mut conn = fresh_database(tag, dir);
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
