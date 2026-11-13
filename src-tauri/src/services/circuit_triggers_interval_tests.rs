//! Interval tick reuse (issue #2762).
//!
//! The pass reads enabled circuits and cooldown anchors when a cooldown is
//! due, a wake asks it to, or the schedule is cold. A later tick whose
//! cooldowns are still ahead does not touch the database.

use std::time::{Duration, SystemTime};

use super::{
    clear_interval_schedule, note_interval_wake, parse_sqlite_datetime, run_interval_pass,
    run_interval_pass_at,
};
use crate::circuit::model::{graph_parse_calls, CircuitGraph, CircuitNodeKind};
use crate::db;

fn epoch(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

fn sqlite_utc(at: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(at)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

fn interval_identities(circuit_id: i64) -> Vec<String> {
    db::list_circuit_trigger_identities(circuit_id)
        .unwrap()
        .into_iter()
        .filter(|identity| identity.starts_with("interval:"))
        .collect()
}

struct Fixture {
    circuit_id: i64,
    mesh_id: i64,
}

fn enabled_interval(seconds: i64) -> (db::test_support::IsolatedDbGuard, Fixture) {
    let db = db::test_support::isolated();
    clear_interval_schedule();
    let mesh = db::create_mesh(
        "interval",
        &format!("C:/buildmesh-interval-{seconds}-{}", std::process::id()),
    )
    .unwrap();
    let graph = CircuitGraph::triggered_skeleton(
        "task",
        CircuitNodeKind::Interval {
            interval_seconds: seconds,
        },
    );
    let circuit =
        db::create_autopilot_circuit(mesh.id, "timed", "", &graph.to_json().unwrap()).unwrap();
    db::set_autopilot_circuit_enabled(circuit.id, true).unwrap();
    (
        db,
        Fixture {
            circuit_id: circuit.id,
            mesh_id: mesh.id,
        },
    )
}

fn pin_created_at(run_id: i64, at: SystemTime) {
    db::write_conn()
        .execute(
            "UPDATE autopilot_circuit_runs SET created_at = ?1 WHERE id = ?2",
            rusqlite::params![sqlite_utc(at), run_id],
        )
        .unwrap();
}

fn seed_run(fixture: &Fixture, at: SystemTime) {
    let run_id =
        db::create_circuit_run(fixture.circuit_id, fixture.mesh_id, "manual:seed", "{}").unwrap();
    pin_created_at(run_id, at);
}

#[test]
fn an_idle_tick_does_not_list_circuits_while_the_cooldown_is_ahead() {
    let (_db, fixture) = enabled_interval(3600);
    seed_run(&fixture, SystemTime::now());

    let lists_before = db::list_enabled_circuits_calls();
    let anchors_before = db::latest_circuit_run_created_at_calls();
    run_interval_pass();
    assert_eq!(
        db::list_enabled_circuits_calls() - lists_before,
        1,
        "the first tick reads the enabled circuits"
    );
    assert!(
        db::latest_circuit_run_created_at_calls() > anchors_before,
        "the first tick reads the cooldown anchor"
    );
    assert!(
        interval_identities(fixture.circuit_id).is_empty(),
        "an hour-long cooldown must not mint a run immediately after one was created"
    );

    let lists = db::list_enabled_circuits_calls();
    let anchors = db::latest_circuit_run_created_at_calls();
    run_interval_pass();
    assert_eq!(
        db::list_enabled_circuits_calls(),
        lists,
        "a cooldown still ahead must not list enabled circuits again"
    );
    assert_eq!(
        db::latest_circuit_run_created_at_calls(),
        anchors,
        "a cooldown still ahead must not re-read the anchor"
    );
    assert!(interval_identities(fixture.circuit_id).is_empty());
}

#[test]
fn an_interval_fires_on_the_tick_its_cooldown_elapses_and_not_one_second_early() {
    let (_db, fixture) = enabled_interval(300);
    let anchor = epoch(1_700_000_000);
    seed_run(&fixture, anchor);

    run_interval_pass_at(anchor + Duration::from_secs(299));
    assert!(
        interval_identities(fixture.circuit_id).is_empty(),
        "one second before the cooldown elapses is not a fire"
    );

    let lists = db::list_enabled_circuits_calls();
    let anchors = db::latest_circuit_run_created_at_calls();
    run_interval_pass_at(anchor + Duration::from_secs(299));
    assert_eq!(db::list_enabled_circuits_calls(), lists);
    assert_eq!(db::latest_circuit_run_created_at_calls(), anchors);

    run_interval_pass_at(anchor + Duration::from_secs(300));
    assert_eq!(
        interval_identities(fixture.circuit_id).len(),
        1,
        "the tick the cooldown elapses mints one interval run"
    );

    let lists = db::list_enabled_circuits_calls();
    run_interval_pass_at(anchor + Duration::from_secs(301));
    assert_eq!(
        db::list_enabled_circuits_calls(),
        lists,
        "the tick after a fire is still inside the new cooldown"
    );
    assert_eq!(interval_identities(fixture.circuit_id).len(), 1);
}

#[test]
fn a_wake_after_a_newer_run_restarts_the_cadence_from_that_run() {
    let (_db, fixture) = enabled_interval(300);
    let anchor = epoch(1_700_000_000);
    seed_run(&fixture, anchor);
    run_interval_pass_at(anchor + Duration::from_secs(10));

    let newer = anchor + Duration::from_secs(1_000);
    let run_id =
        db::create_circuit_run(fixture.circuit_id, fixture.mesh_id, "manual:later", "{}").unwrap();
    pin_created_at(run_id, newer);
    note_interval_wake(true);
    run_interval_pass_at(anchor + Duration::from_secs(300));
    assert!(
        interval_identities(fixture.circuit_id).is_empty(),
        "the old cooldown must not fire once a newer run has restarted it"
    );

    run_interval_pass_at(newer + Duration::from_secs(299));
    assert!(interval_identities(fixture.circuit_id).is_empty());
    run_interval_pass_at(newer + Duration::from_secs(300));
    assert_eq!(interval_identities(fixture.circuit_id).len(), 1);
}

#[test]
fn a_cleared_schedule_reads_the_anchor_once_and_then_waits() {
    let (_db, fixture) = enabled_interval(300);
    let anchor = epoch(1_700_000_000);
    seed_run(&fixture, anchor);
    run_interval_pass_at(anchor + Duration::from_secs(10));

    clear_interval_schedule();
    let lists = db::list_enabled_circuits_calls();
    run_interval_pass_at(anchor + Duration::from_secs(20));
    assert_eq!(
        db::list_enabled_circuits_calls(),
        lists + 1,
        "a restart reads the anchor again"
    );
    assert!(interval_identities(fixture.circuit_id).is_empty());

    let lists = db::list_enabled_circuits_calls();
    run_interval_pass_at(anchor + Duration::from_secs(21));
    assert_eq!(db::list_enabled_circuits_calls(), lists);
}

#[test]
fn a_wake_rereads_without_parsing_an_unchanged_graph() {
    let (_db, fixture) = enabled_interval(300);
    let anchor = epoch(1_700_000_000);
    seed_run(&fixture, anchor);

    let parses_before = graph_parse_calls();
    run_interval_pass_at(anchor + Duration::from_secs(10));
    let parsed = graph_parse_calls() - parses_before;
    assert_eq!(parsed, 1, "the cold pass parses the graph once");

    note_interval_wake(true);
    let parses = graph_parse_calls();
    run_interval_pass_at(anchor + Duration::from_secs(11));
    assert_eq!(
        graph_parse_calls(),
        parses,
        "an unchanged graph_json is reused"
    );
    assert!(interval_identities(fixture.circuit_id).is_empty());
}

#[test]
fn a_refresh_sees_a_circuit_enabled_after_the_schedule_was_loaded() {
    let _db = db::test_support::isolated();
    clear_interval_schedule();
    let mesh = db::create_mesh(
        "interval enable",
        &format!("C:/buildmesh-interval-enable-{}", std::process::id()),
    )
    .unwrap();
    let graph = CircuitGraph::triggered_skeleton(
        "task",
        CircuitNodeKind::Interval {
            interval_seconds: 300,
        },
    );
    let circuit =
        db::create_autopilot_circuit(mesh.id, "timed", "", &graph.to_json().unwrap()).unwrap();
    run_interval_pass_at(epoch(10));
    assert!(interval_identities(circuit.id).is_empty());

    db::set_autopilot_circuit_enabled(circuit.id, true).unwrap();
    run_interval_pass_at(epoch(10));
    assert!(
        interval_identities(circuit.id).is_empty(),
        "a loaded schedule does not notice an enable until the worker is woken"
    );
    note_interval_wake(true);
    run_interval_pass_at(epoch(10));
    assert_eq!(
        interval_identities(circuit.id).len(),
        1,
        "the worker's next tick after enable fires a circuit that has never run"
    );
}

#[test]
fn a_timeout_tick_does_not_refresh_and_a_wake_does() {
    let (_db, fixture) = enabled_interval(300);
    let anchor = epoch(1_700_000_000);
    seed_run(&fixture, anchor);
    run_interval_pass_at(anchor + Duration::from_secs(10));

    let lists = db::list_enabled_circuits_calls();
    note_interval_wake(false);
    run_interval_pass_at(anchor + Duration::from_secs(11));
    assert_eq!(db::list_enabled_circuits_calls(), lists);

    note_interval_wake(true);
    run_interval_pass_at(anchor + Duration::from_secs(12));
    assert_eq!(db::list_enabled_circuits_calls(), lists + 1);
    assert!(interval_identities(fixture.circuit_id).is_empty());
}

#[test]
fn a_non_positive_interval_does_not_mint_or_keep_listing() {
    let (_db, fixture) = enabled_interval(0);
    let lists = db::list_enabled_circuits_calls();
    run_interval_pass_at(epoch(10));
    assert_eq!(db::list_enabled_circuits_calls(), lists + 1);
    assert!(interval_identities(fixture.circuit_id).is_empty());

    let lists = db::list_enabled_circuits_calls();
    run_interval_pass_at(epoch(11));
    assert_eq!(db::list_enabled_circuits_calls(), lists);
    assert!(interval_identities(fixture.circuit_id).is_empty());
}

#[test]
fn a_pinned_created_at_round_trips_through_the_cooldown_parser() {
    let (_db, fixture) = enabled_interval(300);
    let anchor = epoch(1_700_000_000);
    seed_run(&fixture, anchor);
    let stored = db::latest_circuit_run_created_at(fixture.circuit_id)
        .unwrap()
        .unwrap();
    assert_eq!(parse_sqlite_datetime(&stored), Some(anchor));
}
