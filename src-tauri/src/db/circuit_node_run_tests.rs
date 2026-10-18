//! The phone's per-node Circuit Run read model (issue #2314).
//!
//! These tests drive `list_node_circuit_runs_inner` against a private
//! in-memory database. Runs are minted by the production writer
//! (`create_node_circuit_run_locked`), so the context keys the read model
//! interprets (`retry.max_retries`, `node.<id>.review_verdict*`) are the ones the
//! stepper really writes.

use super::circuit::evidence::{attention_inner, CheckpointAction};
use super::circuit::node_run::{
    list_node_circuit_runs_inner, shorten_summary, NodeCircuitNeedKind, NodeCircuitRun,
    MAX_SUMMARY_CHARS,
};
use super::*;
use crate::circuit::context::CircuitContext;
use crate::circuit::observation::{HumanWait, HumanWaitKind, ObservationIdentity, WorkEvidence};
use crate::models::{EnvType, SessionStatus};
use rusqlite::Connection;

fn conn() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    super::init_schema(&conn).unwrap();
    conn
}

/// A mesh with one Ready implementation node.
fn source(conn: &Connection, label: &str) -> (i64, i64) {
    let mesh = create_mesh_inner(conn, label, &format!("/tmp/{label}")).unwrap();
    let node = create_agent_node_inner(
        conn,
        NewAgentNode {
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
        },
    )
    .unwrap();
    update_agent_node_status_inner(conn, node.id, SessionStatus::Ready).unwrap();
    (mesh.id, node.id)
}

/// A review started from the node's title bar, through the writer the command uses.
fn review(conn: &mut Connection, node: i64, rounds: i32) -> i64 {
    create_node_circuit_run_locked(conn, node, None, rounds, None).unwrap()
}

fn step_op(node_id: &str, status: &str, attempt: i32, error: Option<&str>) -> CircuitStepOp {
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

fn set_context(conn: &mut Connection, run: i64, pairs: &[(&str, &str)]) {
    let mut context = CircuitContext::from_json(
        &get_circuit_run_inner(conn, run)
            .unwrap()
            .unwrap()
            .context_json,
    )
    .unwrap();
    for (key, value) in pairs {
        context.set(key, *value);
    }
    commit_circuit_advance_locked(conn, run, None, Some(&context.to_json().unwrap()), &[]).unwrap();
}

fn only(conn: &Connection) -> NodeCircuitRun {
    let mut runs = list_node_circuit_runs_inner(conn).unwrap();
    assert_eq!(runs.len(), 1, "expected exactly one node run: {runs:?}");
    runs.remove(0)
}

#[test]
fn a_running_review_reports_its_round_limit_and_current_step() {
    let mut conn = conn();
    let (_, node) = source(&conn, "round");
    let run = review(&mut conn, node, 3);
    commit_circuit_advance_locked(
        &mut conn,
        run,
        Some("running"),
        None,
        &[step_op("verdict", "running", 1, None)],
    )
    .unwrap();

    let listed = only(&conn);
    assert_eq!(listed.node_id, node);
    assert_eq!(listed.run_id, run);
    assert_eq!(listed.state, "running");
    let step = listed.step.expect("the running step is the current step");
    assert_eq!(
        (
            step.node_id.as_str(),
            step.role.as_str(),
            step.status.as_str()
        ),
        ("verdict", "Review", "running")
    );
    let review = listed.review.expect("a review run reports review progress");
    assert_eq!((review.round, review.max_rounds), (1, Some(3)));
    assert_eq!((review.verdict, review.summary), (None, None));
    assert!(listed.needs_you.is_empty());
}

#[test]
fn the_latest_verdict_belongs_to_the_current_pass_only() {
    let mut conn = conn();
    let (_, node) = source(&conn, "verdict");
    let run = review(&mut conn, node, 3);
    set_context(
        &mut conn,
        run,
        &[
            ("node.verdict.review_verdict", "changes_requested"),
            ("node.verdict.review_verdict_attempt", "1"),
            ("node.verdict.evaluated_output", "Round one found a race."),
        ],
    );
    commit_circuit_advance_locked(
        &mut conn,
        run,
        Some("running"),
        None,
        &[step_op("verdict", "running", 2, None)],
    )
    .unwrap();

    let review = only(&conn).review.unwrap();
    assert_eq!(review.round, 2);
    assert_eq!(
        (review.verdict, review.summary),
        (None, None),
        "pass 1's verdict must not be shown as pass 2's"
    );

    set_context(
        &mut conn,
        run,
        &[
            ("node.verdict.review_verdict", "approved"),
            ("node.verdict.review_verdict_attempt", "2"),
            ("node.verdict.evaluated_output", "Looks good to merge."),
        ],
    );
    let review = only(&conn).review.unwrap();
    assert_eq!(review.verdict.as_deref(), Some("approved"));
    assert_eq!(review.summary.as_deref(), Some("Looks good to merge."));
}

#[test]
fn a_blocked_approval_gate_needs_approval_and_carries_no_checkpoint_actions() {
    let mut conn = conn();
    let (_, node) = source(&conn, "approval");
    let run = review(&mut conn, node, 3);
    commit_circuit_advance_locked(
        &mut conn,
        run,
        Some("running"),
        None,
        &[step_op("confirm_source", "blocked", 1, None)],
    )
    .unwrap();

    let listed = only(&conn);
    assert_eq!(listed.needs_you.len(), 1);
    let need = &listed.needs_you[0];
    assert_eq!(need.kind, NodeCircuitNeedKind::Approval);
    assert_eq!(need.step.node_id, "confirm_source");
    assert_eq!(need.step.role, "Approval gate");
    assert!(need.actions.is_empty());
    assert_eq!(
        listed.step.unwrap().node_id,
        "confirm_source",
        "the run is parked on the gate, so that is the current step"
    );
}

#[test]
fn an_unverified_step_offers_exactly_the_actions_the_desktop_run_card_offers() {
    let mut conn = conn();
    let (_, node) = source(&conn, "unverified");
    let run = review(&mut conn, node, 3);
    commit_circuit_advance_locked(
        &mut conn,
        run,
        Some("running"),
        None,
        &[step_op("verdict", "unverified", 1, None)],
    )
    .unwrap();

    let desktop = attention_inner(&conn, run).unwrap();
    assert_eq!(desktop.checkpoints.len(), 1, "fixture must reach the gate");
    assert!(
        desktop.checkpoints[0]
            .actions
            .iter()
            .any(|action| matches!(action, CheckpointAction::Recheck)),
        "a review gate can always be rechecked"
    );
    assert!(
        !desktop.checkpoints[0].starts_next_review,
        "a review verdict is not the next review pass"
    );
    assert!(!desktop.checkpoints[0].ends_review);

    let listed = only(&conn);
    assert_eq!(listed.revision, desktop.revision);
    let need = &listed.needs_you[0];
    assert_eq!(need.kind, NodeCircuitNeedKind::Unverified);
    assert_eq!(
        format!("{:?}", need.actions),
        format!("{:?}", desktop.checkpoints[0].actions),
        "the phone reads the desktop's eligibility, it does not recompute it"
    );
    assert_eq!(
        need.starts_next_review,
        desktop.checkpoints[0].starts_next_review
    );
    assert_eq!(need.ends_review, desktop.checkpoints[0].ends_review);
    assert_eq!(need.review_again, desktop.checkpoints[0].review_again);
}

#[test]
fn the_phone_offers_the_next_review_when_the_fix_wait_is_unverified() {
    let mut conn = conn();
    let (_, node) = source(&conn, "fixes");
    let run = review(&mut conn, node, 5);
    commit_circuit_advance_locked(
        &mut conn,
        run,
        Some("running"),
        None,
        &[step_op("await_fixes", "unverified", 2, None)],
    )
    .unwrap();

    let desktop = attention_inner(&conn, run).unwrap();
    let checkpoint = desktop
        .checkpoints
        .iter()
        .find(|checkpoint| checkpoint.node_id == "await_fixes")
        .expect("the fix wait is a checkpoint");
    assert!(checkpoint.starts_next_review);
    assert!(!checkpoint.ends_review);
    let listed = only(&conn);
    let need = listed
        .needs_you
        .iter()
        .find(|need| need.step.node_id == "await_fixes")
        .expect("the phone lists the fix wait");
    assert!(need.starts_next_review);
    assert!(!need.ends_review);
    assert!(!need.review_again);
    assert_eq!(need.review_again, checkpoint.review_again);
    assert_eq!(
        format!("{:?}", need.actions),
        format!("{:?}", checkpoint.actions)
    );
}

#[test]
fn the_phone_offers_review_again_on_the_last_allowed_round() {
    let mut conn = conn();
    let (_, node) = source(&conn, "last");
    let run = review(&mut conn, node, 5);
    commit_circuit_advance_locked(
        &mut conn,
        run,
        Some("running"),
        None,
        &[
            step_op("await_fixes", "unverified", 2, None),
            step_op("re_review", "completed", 5, None),
        ],
    )
    .unwrap();

    let desktop = attention_inner(&conn, run).unwrap();
    let checkpoint = desktop
        .checkpoints
        .iter()
        .find(|checkpoint| checkpoint.node_id == "await_fixes")
        .expect("the fix wait is a checkpoint");
    assert!(!checkpoint.starts_next_review);
    assert!(checkpoint.ends_review);
    let listed = only(&conn);
    let need = listed
        .needs_you
        .iter()
        .find(|need| need.step.node_id == "await_fixes")
        .expect("the phone lists the fix wait");
    assert!(!need.starts_next_review);
    assert!(need.ends_review);
    assert!(need.review_again);
    assert_eq!(need.review_again, checkpoint.review_again);
}

#[test]
fn a_failed_review_gate_is_not_offered_run_recovery_that_the_desktop_withholds() {
    let mut conn = conn();
    let (_, node) = source(&conn, "failed");
    let run = review(&mut conn, node, 3);
    commit_circuit_advance_locked(
        &mut conn,
        run,
        Some("failed"),
        None,
        &[step_op("verdict", "failed", 1, Some("reviewer vanished"))],
    )
    .unwrap();

    let listed = only(&conn);
    assert_eq!(listed.state, "failed");
    let need = &listed.needs_you[0];
    assert_eq!(need.kind, NodeCircuitNeedKind::Failed);
    assert_eq!(need.detail.as_deref(), Some("reviewer vanished"));
    let recovery = listed
        .recovery
        .expect("a failed run reports its recovery options");
    assert!(
        recovery.options.iter().all(|option| !option.available),
        "a review approval can never be retried or attested: {:?}",
        recovery.options
    );
    let desktop = attention_inner(&conn, run).unwrap().recovery.unwrap();
    assert_eq!(
        format!("{recovery:?}"),
        format!("{desktop:?}"),
        "same eligibility as the desktop"
    );
}

#[test]
fn a_failed_publish_step_passes_through_the_recovery_the_desktop_offers() {
    let mut conn = conn();
    let (_, node) = source(&conn, "recoverable");
    let run = review(&mut conn, node, 3);
    commit_circuit_advance_locked(
        &mut conn,
        run,
        Some("failed"),
        None,
        &[step_op(
            "publish",
            "failed",
            1,
            Some("prompt delivery failed"),
        )],
    )
    .unwrap();

    let listed = only(&conn);
    let recovery = listed
        .recovery
        .as_ref()
        .expect("a failed step is recoverable");
    assert_eq!(recovery.node_id, "publish");
    assert!(
        recovery.options.iter().all(|option| option.available),
        "a prompt to a live agent can be retried or attested: {:?}",
        recovery.options
    );
    let need = &listed.needs_you[0];
    assert_eq!(need.kind, NodeCircuitNeedKind::Failed);
    assert_eq!(need.step.role, "Send feedback");
    assert_eq!(listed.step.unwrap().node_id, "publish");
}

#[test]
fn an_unresolved_agent_question_is_reported_as_needing_input() {
    let mut conn = conn();
    let (_, node) = source(&conn, "input");
    let run = review(&mut conn, node, 3);
    commit_circuit_advance_locked(
        &mut conn,
        run,
        Some("running"),
        None,
        &[step_op("await_source", "running", 1, None)],
    )
    .unwrap();
    let wait = |request: &str, resolved_at_ms: Option<i64>| HumanWait {
        wait_kind: HumanWaitKind::Permission,
        request_id: Some(request.into()),
        identity: ObservationIdentity {
            run_id: run,
            step_id: "await_source".into(),
            attempt: 1,
            agent_node_id: node,
            session_incarnation: None,
            session_id: Some("session".into()),
            turn_id: None,
            report_revision: None,
        },
        source: "native_hook".into(),
        source_id: None,
        observed_at_ms: 1,
        authoritative: true,
        resolved_at_ms,
    };
    let evidence = |waits: Vec<HumanWait>| {
        serde_json::to_string(&WorkEvidence {
            human_waits: waits,
            ..Default::default()
        })
        .unwrap()
    };
    let key = "node.await_source.evidence.1";
    set_context(
        &mut conn,
        run,
        &[(key, &evidence(vec![wait("answered", Some(2))]))],
    );
    assert!(
        only(&conn).needs_you.is_empty(),
        "a resolved request no longer needs the person"
    );
    set_context(
        &mut conn,
        run,
        &[(
            key,
            &evidence(vec![wait("answered", Some(2)), wait("open", None)]),
        )],
    );

    let listed = only(&conn);
    let need = listed
        .needs_you
        .iter()
        .find(|need| need.kind == NodeCircuitNeedKind::Input)
        .expect("an unresolved permission request needs the person");
    assert_eq!(need.step.node_id, "await_source");
    assert_eq!(need.agent_node_id, Some(node));
    assert!(need.actions.is_empty());
}

#[test]
fn only_the_latest_live_or_unacknowledged_run_per_node_is_listed() {
    let mut conn = conn();
    let (mesh, finished) = source(&conn, "scope");
    let done = review(&mut conn, finished, 3);
    commit_circuit_advance_locked(&mut conn, done, Some("completed"), None, &[]).unwrap();
    assert!(
        list_node_circuit_runs_inner(&conn).unwrap().is_empty(),
        "a completed run has nothing left to show"
    );

    let rerun = review(&mut conn, finished, 3);
    assert_ne!(rerun, done);
    let listed = only(&conn);
    assert_eq!(
        listed.run_id, rerun,
        "the newer run supersedes the finished one"
    );

    commit_circuit_advance_locked(&mut conn, rerun, Some("failed"), None, &[]).unwrap();
    assert_eq!(
        only(&conn).state,
        "failed",
        "an unacknowledged failure stays visible"
    );
    dismiss_all_circuit_run_attention_inner(&conn, mesh).unwrap();
    assert!(
        list_node_circuit_runs_inner(&conn).unwrap().is_empty(),
        "dismissing attention on the desktop clears it from the phone too"
    );
}

#[test]
fn an_archived_node_is_not_listed() {
    let mut conn = conn();
    let (_, node) = source(&conn, "archived");
    review(&mut conn, node, 3);
    update_agent_node_status_inner(&conn, node, SessionStatus::Archived).unwrap();
    assert!(list_node_circuit_runs_inner(&conn).unwrap().is_empty());
}

#[test]
fn a_reviewer_helper_node_is_not_listed_as_an_implementation_node() {
    let mut conn = conn();
    let (mesh, node) = source(&conn, "helper");
    let run = review(&mut conn, node, 3);
    let reviewer = create_agent_node_inner(
        &conn,
        NewAgentNode {
            mesh_id: mesh,
            name: "Code reviewer",
            path: "/tmp/helper",
            branch: "review",
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
        },
    )
    .unwrap();
    commit_circuit_advance_locked(
        &mut conn,
        run,
        Some("running"),
        None,
        &[CircuitStepOp {
            agent_node_id: Some(reviewer.id),
            ..step_op("reviewer", "running", 1, None)
        }],
    )
    .unwrap();
    set_circuit_step_agent_node_with_parent_inner(&conn, run, "reviewer", reviewer.id, Some(node))
        .unwrap();

    let listed = list_node_circuit_runs_inner(&conn).unwrap();
    assert_eq!(
        listed.iter().map(|row| row.node_id).collect::<Vec<_>>(),
        vec![node],
        "the reviewer is the run's helper, so the status line belongs to the implementer"
    );
}

#[test]
fn a_long_reviewer_report_is_shortened_on_a_character_boundary() {
    let report = "é".repeat(MAX_SUMMARY_CHARS + 50);
    let shortened = shorten_summary(&report);
    assert_eq!(shortened.chars().count(), MAX_SUMMARY_CHARS + 1);
    assert!(shortened.ends_with('…'));
    assert_eq!(shorten_summary("  short report \n"), "short report");
}
