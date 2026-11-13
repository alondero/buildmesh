//! Pins the per-test isolation seam `db::test_support` owns (issue #2663).
//!
//! The worker's cancellation registry is process-wide and keyed by run id, so
//! two databases that hand out the same run id share that registry entry: one
//! database's cancellation then invalidates the other database's effect batch,
//! and the dispatch path returns before it writes the prompt. A database that
//! reserves its run ids therefore has to hold an id that no other database
//! hands out.

use crate::circuit::model::CircuitGraph;
use crate::db::test_support;

/// The run ids a fresh database hands out, in the order it creates them.
fn run_ids(count: usize) -> Vec<i64> {
    let mesh = crate::db::create_mesh("reserved-run-ids", ".").unwrap();
    let graph = CircuitGraph::walking_skeleton("work");
    let circuit = crate::db::create_autopilot_circuit(
        mesh.id,
        "reserved-run-ids",
        "",
        &graph.to_json().unwrap(),
    )
    .unwrap();
    (0..count)
        .map(|index| {
            crate::db::create_circuit_run(
                circuit.id,
                mesh.id,
                &format!("manual:reserved-run-{index}"),
                "{}",
            )
            .unwrap()
        })
        .collect()
}

/// A database that reserves its run ids, and the floor it reserved them from.
fn reserved_database() -> (i64, i64) {
    let _db = test_support::isolated();
    test_support::seed_unique_circuit_run_ids();
    let floor = test_support::reserved_run_id_floor();
    (floor, run_ids(1)[0])
}

#[test]
fn a_reserved_run_id_starts_at_the_floor_its_database_reserved() {
    let (floor, reserved) = std::thread::spawn(reserved_database)
        .join()
        .expect("the reserved database joins");

    assert!(
        reserved >= floor,
        "a reserved run id starts at the floor its own database reserved: {reserved} < {floor}"
    );
    assert!(
        floor >= test_support::RESERVED_RUN_ID_FLOOR,
        "the reserved band sits above every id an unreserved database hands out: {floor}"
    );
}

#[test]
fn an_unreserved_database_hands_out_ids_below_the_reserved_band() {
    let floor = std::thread::spawn(reserved_database)
        .join()
        .expect("the reserved database joins")
        .0;
    let unreserved = std::thread::spawn(|| {
        let _db = test_support::isolated();
        run_ids(4)
    })
    .join()
    .expect("the unreserved database joins");

    assert!(
        unreserved.iter().all(|id| *id < floor),
        "an unreserved database hands out ids below the reserved band: {unreserved:?} >= {floor}"
    );
}

#[test]
fn two_reserved_databases_hold_different_run_ids() {
    let first = std::thread::spawn(reserved_database)
        .join()
        .expect("the first database joins")
        .1;
    let second = std::thread::spawn(reserved_database)
        .join()
        .expect("the second database joins")
        .1;

    assert_ne!(
        first, second,
        "two reserved databases must not share the run id the worker's cancellation registry is \
         keyed by"
    );
}
