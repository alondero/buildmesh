// The lost-turn watchdog reuses the drive pass's snapshot (issue #2762).
//
// A running run is stored so a watchdog that listed active runs again would
// parse that run's graph and context. The pass is given the snapshot and
// must not do either.

use crate::circuit::model::{graph_parse_calls, CircuitGraph, CircuitNodeKind};
use crate::db;

fn running_snapshot() -> ObservedRun {
    let mesh = db::create_mesh(
        "watchdog reuse",
        &format!("C:/buildmesh-watchdog-reuse-{}", std::process::id()),
    )
    .unwrap();
    let graph = CircuitGraph::triggered_skeleton("task", CircuitNodeKind::Manual);
    let graph_json = graph.to_json().unwrap();
    let circuit = db::create_autopilot_circuit(mesh.id, "watched", "", &graph_json).unwrap();
    db::set_autopilot_circuit_enabled(circuit.id, true).unwrap();
    let run_id = db::create_circuit_run(circuit.id, mesh.id, "manual:seed", "{}").unwrap();
    db::set_circuit_run_state(run_id, "running").unwrap();
    let run = db::get_circuit_run(run_id).unwrap().unwrap();
    ObservedRun {
        active: db::ActiveCircuitRun {
            run,
            circuit_enabled: true,
            circuit_graph_json: circuit.graph_json,
            circuit_name: circuit.name,
        },
        // Built directly. Parsing here would hide a later parse in the delta.
        graph: CircuitGraph::triggered_skeleton("task", CircuitNodeKind::Manual),
        context: crate::circuit::context::CircuitContext::new(),
        steps: Vec::new(),
    }
}

#[test]
fn the_watchdog_does_not_list_or_parse_what_the_drive_pass_already_loaded() {
    let _db = db::test_support::isolated();
    let observed = running_snapshot();
    let lists = db::list_active_circuit_runs_calls();
    let parses = graph_parse_calls();
    let contexts = db::circuit::context_store::load_logical_run_calls();

    let failures = lost_turn_watchdog_pass_for_test(vec![observed]);

    assert!(failures.is_empty());
    assert_eq!(
        db::list_active_circuit_runs_calls(),
        lists,
        "the watchdog must not list active runs again"
    );
    assert_eq!(
        graph_parse_calls(),
        parses,
        "the watchdog must not parse a graph the drive pass already loaded"
    );
    assert_eq!(
        db::circuit::context_store::load_logical_run_calls(),
        contexts,
        "the watchdog must not load a context the drive pass already loaded"
    );
}
