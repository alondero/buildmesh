// Circuit worker admission tests. Included in the existing tests scope.
// -- draft-first drive gate (issue #1356) ----------------------------------

#[test]
fn disabled_circuits_still_drive_manual_trigger_now_runs() {
    assert!(should_drive_circuit_run(true, "interval:1", false));
    assert!(should_drive_circuit_run(true, "manual:1", false));
    assert!(
        should_drive_circuit_run(false, "manual:1724000000000", false),
        "Trigger Now is the dry-run seam on a draft circuit"
    );
    assert!(
        !should_drive_circuit_run(false, "interval:1", false),
        "background interval runs stay parked while disabled"
    );
    assert!(!should_drive_circuit_run(
        false,
        "issue:42:buildmesh:run",
        false
    ));
    assert!(should_drive_circuit_run(
        false,
        "issue:42:buildmesh:run",
        true
    ));
}

// Circuit-run admission gate (issue #1467).
//
// These tests pin the pure gate helper [`may_admit_run`] in three
// shapes — empty mesh / under-cap mesh / full mesh — against a
// database installed for their own thread (`test_support::isolated`,
// issue #2048). The DB-layer contracts
// (`count_active_circuit_runs` / terminal commit) are pinned
// separately in `db/circuit_tests.rs`; here we verify the gate
// composes correctly with state transitions on real rows.

/// `running` and `paused` runs always pass the gate — they already
/// hold a slot from their `pending` admission.
#[test]
fn may_admit_run_running_and_paused_unconditional_pass() {
    let mesh = crate::models::Mesh {
        id: 9_999_001,
        circuit_run_capacity: 1,
        ..zero_test_mesh()
    };
    let running = active_row_with_state(9_999_001, 9_999_010, "running");
    assert!(may_admit_run(&running, &mesh));
    let paused = active_row_with_state(9_999_001, 9_999_011, "paused");
    assert!(may_admit_run(&paused, &mesh));
}

/// The defer path: a `pending` run on a mesh whose admitted-run
/// count equals the configured `circuit_run_capacity` returns
/// `false`. Initialized against a real temp DB so the test
/// exercises the production read path through
/// `db::count_active_circuit_runs` (no shadow helpers).
#[test]
fn may_admit_run_pending_saturated_mesh_defers() {
    let _db = install_temp_db();
    let mesh = crate::db::create_mesh("may-admit-defer", "/tmp/may-admit-defer").unwrap();
    // Two admitted runs saturate the expected review-flow capacity.
    crate::db::set_mesh_circuit_run_capacity(mesh.id, 2).unwrap();
    let c1 = crate::db::create_autopilot_circuit(
        mesh.id,
        "c1",
        "",
        &crate::circuit::model::CircuitGraph::walking_skeleton("fixture")
            .to_json()
            .unwrap(),
    )
    .unwrap();
    let c2 = crate::db::create_autopilot_circuit(
        mesh.id,
        "c2",
        "",
        &crate::circuit::model::CircuitGraph::walking_skeleton("fixture")
            .to_json()
            .unwrap(),
    )
    .unwrap();
    let c3 = crate::db::create_autopilot_circuit(
        mesh.id,
        "c3",
        "",
        &crate::circuit::model::CircuitGraph::walking_skeleton("fixture")
            .to_json()
            .unwrap(),
    )
    .unwrap();

    let mesh_row = crate::db::get_mesh_by_id(mesh.id).unwrap();

    // Mesh starts below cap and admits the first run.
    let pending_run = crate::db::create_circuit_run(c1.id, mesh.id, "", "{}").unwrap();
    let pending_row = crate::db::list_active_circuit_runs()
        .unwrap()
        .into_iter()
        .find(|r| r.run.id == pending_run)
        .expect("pending run should be in list_active_circuit_runs");
    assert!(
        may_admit_run(&pending_row, &mesh_row),
        "below cap — must admit",
    );

    // Admit the second run, then verify the third stays pending.
    crate::db::set_circuit_run_state(pending_run, "running").unwrap();
    let pending_run_2 = crate::db::create_circuit_run(c2.id, mesh.id, "", "{}").unwrap();
    let pending_row_2 = crate::db::list_active_circuit_runs()
        .unwrap()
        .into_iter()
        .find(|r| r.run.id == pending_run_2)
        .expect("second pending run must be in the active list");

    assert!(
        may_admit_run(&pending_row_2, &mesh_row),
        "the second run must admit below cap 2"
    );
    crate::db::set_circuit_run_state(pending_run_2, "running").unwrap();
    let pending_run_3 = crate::db::create_circuit_run(c3.id, mesh.id, "", "{}").unwrap();
    let pending_row_3 = crate::db::list_active_circuit_runs()
        .unwrap()
        .into_iter()
        .find(|r| r.run.id == pending_run_3)
        .expect("third pending run must be in the active list");

    assert_eq!(
        crate::db::count_active_circuit_runs(mesh.id).unwrap(),
        2,
        "two running runs consume both run-admission slots"
    );
    assert_eq!(
        mesh_row.circuit_run_capacity, 2,
        "sanity: cap is 2, so two admitted runs fill it"
    );
    assert!(
        !may_admit_run(&pending_row_3, &mesh_row),
        "at cap — must defer to next pass (FIFO)",
    );

    // Terminal the running run via `commit_circuit_advance`'s
    // idempotent terminal-state branch — the third pending run
    // now admits on the next observation pass.
    crate::db::commit_circuit_advance(pending_run, Some("completed"), None, &[]).unwrap();
    assert_eq!(
        crate::db::count_active_circuit_runs(mesh.id).unwrap(),
        1,
        "terminal commit frees one admitted-run slot"
    );
    assert!(
        may_admit_run(&pending_row_3, &mesh_row),
        "after terminal — third pending must admit (FIFO promotion)",
    );
}

// -- admission hold logging (issue #2831) ---------------------------------
//
// The worker re-evaluates every pending run on every pass, so logging the
// hold on every pass wrote the same line again and again: on the hub the
// worker spun at ~90 passes a second and wrote 66 MB of that one sentence in
// 37 minutes. The line belongs to a state change — the run was first held,
// or the reason it is held changed — so it is written then and not on every
// pass that finds the same hold.

#[test]
fn the_held_line_is_written_once_per_run_per_hold_state() {
    let run_id = 9_101;
    let hold = super::admission::HeldAdmission {
        mesh_id: 42,
        active_runs: 2,
        capacity: 2,
    };
    assert!(
        super::admission::log_hold(run_id, hold),
        "the first hold of a run must be written"
    );
    assert!(
        !super::admission::log_hold(run_id, hold),
        "the same hold on the next pass must not be written again"
    );
    let fuller = super::admission::HeldAdmission {
        mesh_id: 42,
        active_runs: 3,
        capacity: 2,
    };
    assert!(
        super::admission::log_hold(run_id, fuller),
        "a changed hold must be written"
    );
    let wider = super::admission::HeldAdmission {
        mesh_id: 42,
        active_runs: 3,
        capacity: 3,
    };
    assert!(
        super::admission::log_hold(run_id, wider),
        "a changed capacity must be written"
    );
    // A run that stops waiting has its hold forgotten, so the next hold of
    // that run is a change again.
    super::admission::forget_hold(run_id);
    assert!(
        super::admission::log_hold(run_id, wider),
        "a hold after the run left the queue must be written again"
    );
}
