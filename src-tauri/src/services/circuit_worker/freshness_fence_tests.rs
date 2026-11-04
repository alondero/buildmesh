//! The worker's freshness fence (issue #2672).
//!
//! The fence guards a pass against a run that moved on while the pass was
//! working. The expected revision comes from the run row the caller listed,
//! never from a context read a moment earlier: comparing the stored context
//! with itself would answer "is the run still what I saw?" with the value
//! the question was given, and the fence would never fire.
//!
//! Run with: cd src-tauri && cargo test --lib services::circuit_worker::freshness_fence_tests

use rusqlite::params;

use crate::circuit::model::CircuitGraph;

/// A run whose context moves on while the caller works.
///
/// The write below changes only the bulky half of the context. The column
/// still moves, because the column carries the fingerprint of the stored
/// bulky set — so the fence fires on a change a reader of the column alone
/// would otherwise miss.
#[test]
fn a_context_that_moves_on_while_the_pass_works_fires_the_fence() {
    let _db = crate::db::test_support::isolated();
    let dir = tempfile::tempdir().unwrap();
    let mesh = crate::db::create_mesh("fence", dir.path().to_str().unwrap()).unwrap();
    let graph = CircuitGraph::walking_skeleton("fence");
    let circuit =
        crate::db::create_autopilot_circuit(mesh.id, "Fence", "", &graph.to_json().unwrap())
            .unwrap();
    let run_id = crate::db::create_circuit_run(circuit.id, mesh.id, "manual:fence", "{}").unwrap();
    crate::db::set_circuit_run_state(run_id, "running").unwrap();

    // The pass's snapshot of the run.
    let snapshot = crate::db::list_active_circuit_runs()
        .unwrap()
        .into_iter()
        .find(|active| active.run.id == run_id)
        .expect("the run is active")
        .run;

    // The fence holds while nothing has moved.
    assert!(
        super::fenced_history_revision(&snapshot).is_ok(),
        "the fence fired on a run that had not moved"
    );

    // A write lands while the pass works: the bulky half of the context
    // changes, so the stored set changes and the column's fingerprint of it
    // moves with it.
    write_a_bulky_value(run_id);

    // The snapshot is stale now, so the fence fires.
    assert!(
        super::fenced_history_revision(&snapshot).is_err(),
        "the fence held after the run's context moved on"
    );

    // A row that carries the current context passes.
    let current = crate::db::get_circuit_run(run_id).unwrap().unwrap();
    assert!(
        super::fenced_history_revision(&current).is_ok(),
        "the fence fired on the run's current context"
    );
}

/// Write a bulky value through the context seam, the way an advance does.
///
/// The seam is what puts the value in the side table and fingerprints the
/// stored set in the column, so a write through it is the one a run's own
/// advance makes.
fn write_a_bulky_value(run_id: i64) {
    let db = crate::db::write_conn();
    let tx = db.unchecked_transaction().unwrap();
    let mut mutation = crate::db::circuit::context_store::ContextMutation::new();
    mutation.put("source.output", "r".repeat(2_000));
    let applied =
        crate::db::circuit::context_store::apply_mutation(&tx, run_id, &mutation).unwrap();
    tx.execute(
        "UPDATE autopilot_circuit_runs SET context_json = ?2 WHERE id = ?1",
        params![run_id, applied.column_json()],
    )
    .unwrap();
    tx.commit().unwrap();
}
