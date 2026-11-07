//! The control module's own interface suite (issue #2673).
//!
//! The operations are driven here against an isolated database, with the same
//! records the desktop run card and the phone see. The point of the suite is
//! the *interface*: the operation is what persists the change, decides what
//! the worker is told, and refuses what must be refused — whichever transport
//! asked for it.

use crate::db::circuit::evidence::{CheckpointAction, CheckpointRequest};
use crate::db::circuit::step_recovery::{RecoveryAction, RecoveryRequest};
use crate::db::circuit::CircuitStepOp;
use crate::models::{EnvType, SessionStatus};

use super::{approve_step, cancel_run, record_outcome, recover_run, start_review};

/// A step write, in the shape the worker's commit takes.
fn step(node_id: &str, status: &str, attempt: i32, error: Option<&str>) -> CircuitStepOp {
    CircuitStepOp {
        node_id: node_id.into(),
        status: status.into(),
        outcome: None,
        error: error.map(|text| Some(text.to_string())),
        agent_node_id: None,
        attempt,
        fresh_attempt: false,
    }
}

/// An implementation node with a node-started review run.
fn review_run() -> (i64, i64) {
    let mesh = crate::db::create_mesh(
        "circuit-control",
        &format!("/tmp/circuit-control-{}", std::process::id()),
    )
    .unwrap();
    let node = crate::db::create_agent_node(crate::db::NewAgentNode {
        mesh_id: mesh.id,
        name: "Implementation",
        path: &mesh.path,
        branch: "work",
        env: EnvType::Windows,
        provider: "claude",
        worktree_name: None,
        source_issue: None,
        source_pr: None,
        source_pr_pinned_sha: None,
        use_worktree: false,
        head_repo_owner: None,
        head_repo_clone_url: None,
        worktree_path: None,
        spawn_configuration: None,
    })
    .unwrap();
    crate::db::update_agent_node_status(node.id, SessionStatus::Ready).unwrap();
    let run = crate::db::create_node_circuit_run(node.id, None, 3, None).unwrap();
    (node.id, run)
}

fn advance(run: i64, state: &str, steps: &[CircuitStepOp]) {
    crate::db::commit_circuit_advance(run, Some(state), None, steps).unwrap();
}

fn revision(run: i64) -> i64 {
    crate::db::circuit::evidence::attention(run)
        .unwrap()
        .revision
}

fn step_status(run: i64, node_id: &str) -> String {
    crate::db::list_circuit_run_steps(run)
        .unwrap()
        .into_iter()
        .find(|step| step.node_id == node_id)
        .unwrap()
        .status
}

#[test]
fn recording_an_outcome_persists_it_and_leaves_the_step_to_the_worker() {
    let _db = crate::db::test_support::isolated();
    let (_, run) = review_run();
    advance(run, "running", &[step("verdict", "unverified", 1, None)]);

    let request = CheckpointRequest {
        run_id: run,
        node_id: "verdict".into(),
        attempt: 1,
        expected_revision: revision(run),
        action: CheckpointAction::Recheck,
        reason: "Checked again.".into(),
    };
    record_outcome(None, &request).expect("the outcome is accepted");

    assert_eq!(step_status(run, "verdict"), "running");
}

#[test]
fn a_stale_revision_is_refused_and_writes_nothing() {
    let _db = crate::db::test_support::isolated();
    let (_, run) = review_run();
    advance(run, "running", &[step("verdict", "unverified", 1, None)]);

    let request = CheckpointRequest {
        run_id: run,
        node_id: "verdict".into(),
        attempt: 1,
        expected_revision: -5,
        action: CheckpointAction::Recheck,
        reason: "Checked again.".into(),
    };
    let error = record_outcome(None, &request).expect_err("a stale revision is refused");

    assert!(
        error.to_string().contains("changed"),
        "{}",
        error.to_string()
    );
    assert_eq!(
        step_status(run, "verdict"),
        "unverified",
        "a refused outcome must leave the step where it was"
    );
}

#[test]
fn recovering_a_failed_run_reopens_it() {
    let _db = crate::db::test_support::isolated();
    let (_, run) = review_run();
    advance(
        run,
        "failed",
        &[step("publish", "failed", 1, Some("prompt delivery failed"))],
    );

    let request = RecoveryRequest {
        run_id: run,
        node_id: "publish".into(),
        attempt: 1,
        expected_revision: revision(run),
        action: RecoveryAction::Retry,
        reason: "Tried again.".into(),
    };
    recover_run(None, &request).expect("the recovery is accepted");

    assert_eq!(
        crate::db::get_circuit_run(run).unwrap().unwrap().state,
        "pending"
    );
}

#[test]
fn approving_a_parked_gate_queues_the_approval_for_the_worker() {
    let _db = crate::db::test_support::isolated();
    // The worker's approval queue is process-wide and keyed by run id, while
    // every test database numbers its runs from 1. Starting this database's
    // run numbering at a distinctive id keeps the test from draining another
    // test's approval.
    let unique = 9_000_000 + i64::from(std::process::id());
    crate::db::write_conn()
        .execute(
            "INSERT INTO sqlite_sequence(name, seq) VALUES ('autopilot_circuit_runs', ?1)",
            [unique - 1],
        )
        .unwrap();
    let (_, run) = review_run();
    assert_eq!(run, unique, "the run must take the seeded id");
    advance(
        run,
        "running",
        &[step("confirm_source", "blocked", 1, None)],
    );

    approve_step(run, "confirm_source".into()).expect("the gate is parked");

    assert_eq!(
        crate::services::circuit_worker::drain_approvals_for(run),
        vec!["confirm_source".to_string()]
    );
}

#[test]
fn approving_a_step_that_is_not_waiting_is_refused() {
    let _db = crate::db::test_support::isolated();
    let (_, run) = review_run();
    advance(
        run,
        "running",
        &[step("confirm_source", "running", 1, None)],
    );

    let error = approve_step(run, "confirm_source".into())
        .expect_err("a running step is not waiting for approval");

    assert!(
        error.to_string().contains("not waiting for approval"),
        "{}",
        error.to_string()
    );
}

#[test]
fn cancelling_a_run_terminalises_it() {
    let _db = crate::db::test_support::isolated();
    let (_, run) = review_run();
    advance(run, "running", &[step("verdict", "running", 1, None)]);

    cancel_run(None, run).expect("the run is cancelled");

    assert_eq!(
        crate::db::get_circuit_run(run).unwrap().unwrap().state,
        "cancelled"
    );
}

#[test]
fn starting_a_review_refuses_an_agent_that_is_not_alive() {
    let _db = crate::db::test_support::isolated();
    let (node, _) = review_run();

    let error = start_review(None, None, node, None, 3, None, None)
        .expect_err("a stopped agent cannot start a review");

    assert!(error.contains("Resume the agent"), "{error}");
}
