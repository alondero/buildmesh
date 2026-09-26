use super::*;
use crate::autopilot::circuit::context::CircuitContext;
use crate::autopilot::circuit::model::{
    CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, EdgeCondition, GithubActionKind,
};
use crate::autopilot::circuit::stepper::{
    advance, CircuitEvent, RunState, RunView, StepStatus, StepView,
};
use crate::db::circuit::evidence::{EffectIntent, EvidenceWrite};
use crate::db::CircuitStepOp;
use crate::services::github::tests::{fake_server, Scripted};
use crate::services::github::{CreatePrRequest, GitHubClient};
use std::sync::atomic::{AtomicU64, Ordering};

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

struct OpenPrFixture {
    active: crate::db::ActiveCircuitRun,
    view: RunView,
    run_id: i64,
}

fn open_pr_fixture() -> OpenPrFixture {
    crate::db::test_support::ensure_db_for_tests();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let dir = tempfile::tempdir().unwrap();
    let mesh = crate::db::create_mesh(
        &format!("open-pr-recovery-{sequence}"),
        dir.path().to_str().unwrap(),
    )
    .unwrap();
    let graph = CircuitGraph {
        version: 1,
        blueprint: None,
        nodes: vec![CircuitNode {
            id: "open_pr".into(),
            kind: CircuitNodeKind::GithubAction {
                action: GithubActionKind::OpenPr,
                open_pr_policy: None,
                label: None,
                comment: None,
            },
        }],
        edges: vec![],
    };
    let circuit = crate::db::create_autopilot_circuit(
        mesh.id,
        &format!("OpenPr recovery {sequence}"),
        "",
        1,
        &graph.to_json().unwrap(),
    )
    .unwrap();
    let run_id = crate::db::create_circuit_run(
        circuit.id,
        mesh.id,
        &format!("manual:open-pr-recovery-{sequence}"),
        "{}",
    )
    .unwrap();
    crate::db::set_circuit_run_state(run_id, "running").unwrap();

    let intent = EffectIntent {
        node_id: "open_pr".into(),
        attempt: 1,
        kind: "github".into(),
    };
    let step = CircuitStepOp {
        node_id: "open_pr".into(),
        status: "running".into(),
        outcome: None,
        error: None,
        agent_node_id: None,
        attempt: 1,
        fresh_attempt: false,
    };
    crate::db::circuit::evidence::commit_transition(
        run_id,
        Some("running"),
        "{}",
        &[step],
        EvidenceWrite {
            intents: std::slice::from_ref(&intent),
            ..Default::default()
        },
    )
    .unwrap();
    crate::db::circuit::evidence::claim_effect(run_id, &intent)
        .unwrap()
        .expect("running OpenPr dispatch should be claimed");
    let target = serde_json::json!({
        "owner":"example",
        "repo":"buildmesh",
        "head":"feature/circuit-recovery"
    })
    .to_string();
    crate::db::circuit::evidence::record_effect_target(run_id, "open_pr", 1, &target).unwrap();
    crate::db::write_conn()
        .execute(
            "UPDATE circuit_effects SET state='uncertain' WHERE run_id=?1 AND node_id='open_pr' AND attempt=1 AND kind='github'",
            [run_id],
        )
        .unwrap();

    let active = crate::db::list_active_circuit_runs()
        .unwrap()
        .into_iter()
        .find(|active| active.run.id == run_id)
        .unwrap();
    let revision = crate::db::circuit::evidence::observation_revision(&active.run).unwrap();
    let mut context = CircuitContext::default();
    context.with_run(run_id);
    context.set("node.open_pr.recheck_only", "1");
    context.set("evidence.revision", revision.to_string());
    let view = RunView {
        run_id,
        graph,
        state: RunState::Running,
        context,
        steps: vec![StepView {
            node_id: "open_pr".into(),
            attempt: 1,
            status: StepStatus::Running,
            outcome: None,
            error: None,
            agent_node_id: None,
        }],
    };
    OpenPrFixture {
        active,
        view,
        run_id,
    }
}

fn persist_effect_result(view: &mut RunView, event: &CircuitEvent) -> Result<bool, String> {
    let transition = advance(view, event);
    persist_transition(view.run_id, view, &transition)
}

fn pull_request(head_ref: &str) -> crate::services::github::PullRequest {
    serde_json::from_value(serde_json::json!({
        "number": 314,
        "html_url": "https://github.com/example/buildmesh/pull/314",
        "title": "Circuit recovery",
        "head": {"ref": head_ref}
    }))
    .unwrap()
}

fn reopened_run(run_id: i64) -> crate::models::AutopilotCircuitRun {
    let path = {
        let db = crate::db::read_conn();
        db.query_row("PRAGMA database_list", [], |row| row.get::<_, String>(2))
            .unwrap()
    };
    let reopened = rusqlite::Connection::open(path).unwrap();
    crate::db::circuit::ledger::get_circuit_run_inner(&reopened, run_id)
        .unwrap()
        .unwrap()
}

#[test]
fn open_pr_lookup_handoff_commits_acknowledged_result_and_survives_reopen() {
    let mut fixture = open_pr_fixture();
    let calls = std::cell::Cell::new(0);
    let event = github::reconcile_open_pr_for_worker(
        &fixture.active,
        &mut fixture.view,
        "open_pr",
        |owner, repo, head| {
            calls.set(calls.get() + 1);
            assert_eq!(
                (owner, repo, head),
                ("example", "buildmesh", "feature/circuit-recovery")
            );
            Ok(Some(pull_request(head)))
        },
    );
    assert_eq!(calls.get(), 1);
    assert!(matches!(
        event,
        CircuitEvent::GithubActionResult {
            success: true,
            pr_number: Some(314),
            ..
        }
    ));
    assert!(matches!(
        &event,
        CircuitEvent::GithubActionResult { pr_title: Some(title), .. } if title == "Circuit recovery"
    ));
    persist_effect_result(&mut fixture.view, &event).unwrap();

    let stored = reopened_run(fixture.run_id);
    let context = CircuitContext::from_json(&stored.context_json).unwrap();
    assert_eq!(stored.state, "completed");
    assert_eq!(context.get("pr.number"), Some("314"));
    assert_eq!(context.get("pr.head_ref"), Some("feature/circuit-recovery"));
    assert_eq!(open_pr_step_status(fixture.run_id), "completed");
    assert_eq!(effect_state(fixture.run_id), "acknowledged");
    assert_eq!(history_count(fixture.run_id, "effect_reconciled"), 1);
    assert!(
        crate::db::circuit::evidence::claim_effect(
            fixture.run_id,
            &EffectIntent {
                node_id: "open_pr".into(),
                attempt: 1,
                kind: "github".into()
            }
        )
        .unwrap()
        .is_none(),
        "an acknowledged effect cannot be claimed and dispatched again after reopen"
    );
}

#[test]
fn open_pr_blocked_lookup_cannot_commit_after_cancellation() {
    let fixture = open_pr_fixture();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    let active = fixture.active.clone();
    let mut view = fixture.view.clone();
    let lookup = std::thread::spawn(move || {
        let event =
            github::reconcile_open_pr_for_worker(&active, &mut view, "open_pr", |_, _, head| {
                started_tx.send(()).unwrap();
                finish_rx.recv().unwrap();
                Ok(Some(pull_request(head)))
            });
        (view, event)
    });
    started_rx.recv().unwrap();

    crate::db::commit_circuit_advance(
        fixture.run_id,
        Some("cancelled"),
        None,
        &[CircuitStepOp {
            node_id: "open_pr".into(),
            status: "cancelled".into(),
            outcome: Some(Some("cancelled".into())),
            error: None,
            agent_node_id: None,
            attempt: 1,
            fresh_attempt: false,
        }],
    )
    .unwrap();
    finish_tx.send(()).unwrap();
    let (mut view_with_result, event) = lookup.join().unwrap();
    assert!(matches!(
        event,
        CircuitEvent::GithubActionResult { success: true, .. }
    ));

    // This is the worker's post-effect handoff. The optimistic evidence
    // revision changed when cancellation committed, so the late result must
    // not update context, acknowledge the effect, or finish the step.
    assert!(persist_effect_result(&mut view_with_result, &event).is_err());
    let stored = reopened_run(fixture.run_id);
    let context = CircuitContext::from_json(&stored.context_json).unwrap();
    assert_eq!(stored.state, "cancelled");
    assert_eq!(context.get("pr.number"), None);
    assert_eq!(open_pr_step_status(fixture.run_id), "cancelled");
    assert_eq!(effect_state(fixture.run_id), "uncertain");
    assert_eq!(history_count(fixture.run_id, "effect_reconciled"), 0);
}

#[test]
fn open_pr_missing_or_mismatched_lookup_stays_uncertain_without_replay() {
    for (result, expected_reason) in [
        (None, "No open pull request was found"),
        (
            Some(pull_request("different-branch")),
            "returned branch different-branch",
        ),
    ] {
        let mut fixture = open_pr_fixture();
        let calls = std::cell::Cell::new(0);
        let event = github::reconcile_open_pr_for_worker(
            &fixture.active,
            &mut fixture.view,
            "open_pr",
            |_, _, _| {
                calls.set(calls.get() + 1);
                Ok(result)
            },
        );
        assert_eq!(calls.get(), 1, "recheck performs one read-only lookup");
        assert!(matches!(
            &event,
            CircuitEvent::EffectUncertain { reason, .. } if reason.contains(expected_reason)
        ));
        persist_effect_result(&mut fixture.view, &event).unwrap();
        let stored = reopened_run(fixture.run_id);
        let context = CircuitContext::from_json(&stored.context_json).unwrap();
        assert_eq!(stored.state, "running");
        assert_eq!(context.get("pr.number"), None);
        assert_eq!(
            effect_state(fixture.run_id),
            "uncertain",
            "a failed read-only lookup never acknowledges or replays the effect"
        );
    }
}

// ---------------------------------------------------------------------------
// Dispatch-to-crash recovery (issue #1907).
//
// The #1889 suite seeds an uncertain OpenPr effect directly. That proves the
// read-only handoff, but it skips the transition this issue targets: the create
// request may have reached GitHub before Buildmesh stopped. These tests drive
// the production dispatch (`ensure_open_pr_with_target`) against a deterministic
// loopback endpoint, then abandon its result the way a crash would, reopen the
// run from durable state, and reconcile. Only the network boundary is substituted.
// ---------------------------------------------------------------------------

const DISPATCH_OWNER: &str = "example";
const DISPATCH_REPO: &str = "buildmesh";
const DISPATCH_HEAD: &str = "feature/circuit-recovery";

/// The wire form the production `record_target` closure persists.
fn dispatch_target_detail(head: &str) -> String {
    serde_json::json!({
        "owner": DISPATCH_OWNER,
        "repo": DISPATCH_REPO,
        "head": head,
    })
    .to_string()
}

/// The percent-encoded `head=OWNER:BRANCH` value the client must emit
/// (the owner separator and any nested ref slash are both escaped).
fn dispatch_expected_head() -> String {
    format!(
        "{DISPATCH_OWNER}%3A{}",
        DISPATCH_HEAD.replace('/', "%2F")
    )
}

fn dispatch_pull_request_json() -> serde_json::Value {
    serde_json::json!({
        "number": 314,
        "html_url": "https://github.com/example/buildmesh/pull/314",
        "title": "Circuit recovery",
        "head": { "ref": DISPATCH_HEAD },
    })
}

/// A run whose OpenPr step is scheduled (implementer completed, open_pr running)
/// with a claimed GitHub effect — exactly the worker's state just before it
/// dispatches the create. Holds the mesh temp dir so the recorded mesh path
/// stays valid for the test's lifetime.
struct DispatchFixture {
    run_id: i64,
    view: RunView,
    _dir: tempfile::TempDir,
}

fn dispatch_fixture() -> DispatchFixture {
    crate::db::test_support::ensure_db_for_tests();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let dir = tempfile::tempdir().unwrap();
    let mesh = crate::db::create_mesh(
        &format!("open-pr-dispatch-{sequence}"),
        dir.path().to_str().unwrap(),
    )
    .unwrap();
    let graph = CircuitGraph {
        version: 1,
        blueprint: None,
        nodes: vec![
            CircuitNode {
                id: "implementer".into(),
                kind: CircuitNodeKind::SpawnAgentNode {
                    prompt: "implement the task".into(),
                    name: None,
                    provider: None,
                    model: None,
                    effort: None,
                    extra_args: None,
                    timeout_seconds: None,
                },
            },
            CircuitNode {
                id: "open_pr".into(),
                kind: CircuitNodeKind::GithubAction {
                    action: GithubActionKind::OpenPr,
                    open_pr_policy: None,
                    label: None,
                    comment: None,
                },
            },
        ],
        edges: vec![CircuitEdge {
            from: "implementer".into(),
            to: "open_pr".into(),
            condition: EdgeCondition::Always,
        }],
    };
    let circuit = crate::db::create_autopilot_circuit(
        mesh.id,
        &format!("OpenPr dispatch {sequence}"),
        "",
        1,
        &graph.to_json().unwrap(),
    )
    .unwrap();
    let run_id = crate::db::create_circuit_run(
        circuit.id,
        mesh.id,
        &format!("manual:open-pr-dispatch-{sequence}"),
        "{}",
    )
    .unwrap();
    crate::db::set_circuit_run_state(run_id, "running").unwrap();
    let intent = EffectIntent {
        node_id: "open_pr".into(),
        attempt: 1,
        kind: "github".into(),
    };
    crate::db::circuit::evidence::commit_transition(
        run_id,
        Some("running"),
        "{}",
        &[
            CircuitStepOp {
                node_id: "implementer".into(),
                status: "completed".into(),
                outcome: Some(Some("completed".into())),
                error: None,
                agent_node_id: Some(700),
                attempt: 1,
                fresh_attempt: false,
            },
            CircuitStepOp {
                node_id: "open_pr".into(),
                status: "running".into(),
                outcome: None,
                error: None,
                agent_node_id: None,
                attempt: 1,
                fresh_attempt: false,
            },
        ],
        EvidenceWrite {
            intents: std::slice::from_ref(&intent),
            ..Default::default()
        },
    )
    .unwrap();
    let view = running_view(run_id);
    DispatchFixture {
        run_id,
        view,
        _dir: dir,
    }
}

fn active_run(run_id: i64) -> crate::db::ActiveCircuitRun {
    crate::db::list_active_circuit_runs()
        .unwrap()
        .into_iter()
        .find(|active| active.run.id == run_id)
        .expect("the run is still active")
}

/// Rebuild a `RunView` from durable state alone — the app-restart read.
fn running_view(run_id: i64) -> RunView {
    view_from_active(&active_run(run_id))
}

fn view_from_active(active: &crate::db::ActiveCircuitRun) -> RunView {
    let graph = CircuitGraph::from_json(&active.circuit_graph_json).unwrap();
    let mut context = CircuitContext::from_json(&active.run.context_json).unwrap();
    let revision = crate::db::circuit::evidence::observation_revision(&active.run).unwrap();
    context.set("evidence.revision", revision.to_string());
    RunView {
        run_id: active.run.id,
        graph,
        state: RunState::from_db_str(&active.run.state),
        context,
        steps: super::load_steps(active.run.id).unwrap(),
    }
}

/// The production dispatch path with a real GitHub client pointed at a
/// deterministic loopback endpoint. The caller decides whether to commit the
/// result; a crash means it never does.
fn dispatch_open_pr(
    fixture: &DispatchFixture,
    client: &GitHubClient,
) -> Result<CircuitEvent, String> {
    let intent = EffectIntent {
        node_id: "open_pr".into(),
        attempt: 1,
        kind: "github".into(),
    };
    crate::db::circuit::evidence::claim_effect(fixture.run_id, &intent)
        .unwrap()
        .expect("a running OpenPr dispatch is claimed before any request");
    github::ensure_open_pr_with_target(
        &fixture.view,
        "open_pr",
        None,
        |_agent| {
            Ok(crate::autopilot::pipeline::WrapupState {
                dirty: false,
                pushed: true,
                branch: Some(DISPATCH_HEAD.into()),
                pr_url: None,
                pr_number: None,
                pr_required: false,
                repo_error: None,
            })
        },
        |head| {
            assert_eq!(head, DISPATCH_HEAD);
            crate::db::circuit::evidence::record_effect_target(
                fixture.run_id,
                "open_pr",
                1,
                &dispatch_target_detail(head),
            )
            .map(|_| ())
        },
        |head| {
            client
                .find_open_pr_for_branch(DISPATCH_OWNER, DISPATCH_REPO, head)
                .map_err(|error| error.to_string())
        },
        |head, title| {
            assert_eq!(head, DISPATCH_HEAD);
            client
                .create_pull_request_idempotent(CreatePrRequest {
                    owner: DISPATCH_OWNER,
                    repo: DISPATCH_REPO,
                    title,
                    body: "",
                    head,
                    base: "main",
                })
                .map_err(|error| error.to_string())
        },
    )
}

fn history_count(run_id: i64, kind: &str) -> i64 {
    crate::db::read_conn()
        .query_row(
            "SELECT COUNT(*) FROM circuit_run_history WHERE run_id=?1 AND kind=?2",
            rusqlite::params![run_id, kind],
            |row| row.get(0),
        )
        .unwrap()
}

fn effect_state(run_id: i64) -> String {
    crate::db::read_conn()
        .query_row(
            "SELECT state FROM circuit_effects WHERE run_id=?1 AND node_id='open_pr' AND attempt=1 AND kind='github'",
            [run_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn open_pr_step_status(run_id: i64) -> String {
    crate::db::read_conn()
        .query_row(
            "SELECT status FROM autopilot_circuit_run_steps WHERE run_id=?1 AND node_id='open_pr'",
            [run_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn run_context(run_id: i64) -> CircuitContext {
    CircuitContext::from_json(&reopened_run(run_id).context_json).unwrap()
}

/// The worker's restart reconciliation for a GitHub action whose result was
/// never observed: preserve the attempt as Unverified — the effect moves
/// `possible_dispatch` → `uncertain` — instead of replaying the effect.
fn reconcile_stuck_github_step(view: &mut RunView) {
    let event = CircuitEvent::GithubActionRetry {
        node_id: "open_pr".into(),
    };
    persist_effect_result(view, &event).unwrap();
}

/// The explicit, safe operator action: a read-only Recheck of the saved target
/// (it parks the step as `pending_slot` for the worker to reschedule).
fn operator_recheck(run_id: i64) {
    let active = active_run(run_id);
    let revision = crate::db::circuit::evidence::observation_revision(&active.run).unwrap();
    crate::db::circuit::evidence::record_outcome(&crate::db::circuit::evidence::CheckpointRequest {
        run_id,
        node_id: "open_pr".into(),
        attempt: 1,
        expected_revision: revision,
        action: crate::db::circuit::evidence::CheckpointAction::Recheck,
        reason: "Reconcile the saved pull-request target without creating one.".into(),
    })
    .unwrap();
}

/// The worker's next tick promotes the queued recheck back to `Running` and
/// re-emits the read-only GitHub call. Returns the view at that handoff, which
/// is where the deterministic reconciliation begins.
fn resume_queued_recheck(run_id: i64) -> RunView {
    let active = active_run(run_id);
    let mut view = view_from_active(&active);
    let transition = advance(
        &mut view,
        &CircuitEvent::Tick(crate::autopilot::circuit::stepper::Capacity {
            circuit_free_slots: 4,
            agent_free_slots: 4,
        }),
    );
    assert!(
        transition.effects.iter().any(|effect| matches!(
            effect,
            crate::autopilot::circuit::stepper::Effect::CallGithub { node_id, .. } if node_id == "open_pr"
        )),
        "the queued recheck is rescheduled as a read-only GitHub call"
    );
    assert!(view
        .step("open_pr")
        .is_some_and(|step| step.status == StepStatus::Running));
    persist_transition(run_id, &mut view, &transition).unwrap();
    view
}

/// The worker's read-only OpenPr handoff, backed by the real GitHub client at
/// the deterministic endpoint. It queries only the saved owner/repo/branch.
fn recheck_with_client(
    active: &crate::db::ActiveCircuitRun,
    view: &mut RunView,
    client: &GitHubClient,
) -> CircuitEvent {
    github::reconcile_open_pr_for_worker(active, view, "open_pr", |owner, repo, head| {
        assert_eq!(
            (owner, repo, head),
            (DISPATCH_OWNER, DISPATCH_REPO, DISPATCH_HEAD)
        );
        client
            .find_open_pr_for_branch(owner, repo, head)
            .map_err(|error| error.to_string())
    })
}

#[test]
fn open_pr_create_dispatch_crash_reconciles_found_pr_after_restart_without_second_create() {
    let fixture = dispatch_fixture();
    let (base, requests, endpoint) = fake_server(vec![
        Scripted::ListPulls {
            body: serde_json::json!([]),
            expected_head: dispatch_expected_head(),
        },
        Scripted::CreatePullRequest(dispatch_pull_request_json()),
        Scripted::ListPulls {
            body: serde_json::json!([dispatch_pull_request_json()]),
            expected_head: dispatch_expected_head(),
        },
    ]);
    let client = GitHubClient::for_test(&base, "fake-token").unwrap();

    // The create request is dispatched and answered, but the worker is stopped
    // before it can commit the result.
    let dispatched = dispatch_open_pr(&fixture, &client);
    assert!(
        matches!(
            dispatched,
            Ok(CircuitEvent::GithubActionResult {
                success: true,
                pr_number: Some(314),
                ..
            })
        ),
        "the dispatched create must reach the endpoint: {dispatched:?}"
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        2,
        "one find plus one create reached the endpoint"
    );

    // The durable boundary the crash leaves behind.
    assert_eq!(effect_state(fixture.run_id), "possible_dispatch");
    assert_eq!(open_pr_step_status(fixture.run_id), "running");
    assert_eq!(history_count(fixture.run_id, "effect_intent"), 1);
    assert_eq!(history_count(fixture.run_id, "effect_possible_dispatch"), 1);
    assert_eq!(history_count(fixture.run_id, "effect_target"), 1);
    assert_eq!(history_count(fixture.run_id, "effect_result"), 0);
    assert_eq!(history_count(fixture.run_id, "effect_reconciled"), 0);
    assert_eq!(run_context(fixture.run_id).get("pr.number"), None);

    // Restart: reconcile the durable state. The stuck GitHub step becomes an
    // actionable Unverified checkpoint; the ambiguous effect becomes uncertain.
    let active = active_run(fixture.run_id);
    let mut restarted = view_from_active(&active);
    reconcile_stuck_github_step(&mut restarted);
    assert_eq!(open_pr_step_status(fixture.run_id), "unverified");
    assert_eq!(effect_state(fixture.run_id), "uncertain");
    assert_eq!(history_count(fixture.run_id, "effect_possible_dispatch"), 1);

    // Explicit operator Recheck, then the worker reschedules the recheck.
    operator_recheck(fixture.run_id);
    assert_eq!(open_pr_step_status(fixture.run_id), "pending_slot");
    let mut resumed = resume_queued_recheck(fixture.run_id);
    assert_eq!(open_pr_step_status(fixture.run_id), "running");
    assert_eq!(effect_state(fixture.run_id), "uncertain");

    // The read-only lookup reconciles the found PR onto the same run.
    let active = active_run(fixture.run_id);
    let event = recheck_with_client(&active, &mut resumed, &client);
    assert!(matches!(
        event,
        CircuitEvent::GithubActionResult { success: true, pr_number: Some(314), .. }
    ));
    persist_effect_result(&mut resumed, &event).unwrap();

    let stored = reopened_run(fixture.run_id);
    let context = CircuitContext::from_json(&stored.context_json).unwrap();
    assert_eq!(stored.state, "completed");
    assert_eq!(context.get("pr.number"), Some("314"));
    assert_eq!(context.get("pr.head_ref"), Some(DISPATCH_HEAD));
    assert_eq!(open_pr_step_status(fixture.run_id), "completed");
    assert_eq!(effect_state(fixture.run_id), "acknowledged");
    assert_eq!(history_count(fixture.run_id, "effect_intent"), 1);
    assert_eq!(history_count(fixture.run_id, "effect_possible_dispatch"), 1);
    assert_eq!(history_count(fixture.run_id, "effect_target"), 1);
    assert_eq!(history_count(fixture.run_id, "effect_reconciled"), 1);
    assert!(history_count(fixture.run_id, "effect_result") >= 1);
    assert!(
        crate::db::circuit::evidence::claim_effect(
            fixture.run_id,
            &EffectIntent {
                node_id: "open_pr".into(),
                attempt: 1,
                kind: "github".into()
            }
        )
        .unwrap()
        .is_none(),
        "the reconciled effect is never claimed and dispatched again"
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        3,
        "one find plus one create during dispatch, one read-only find during recovery"
    );
    endpoint.join().expect("deterministic endpoint");
}

#[test]
fn open_pr_create_dispatch_crash_keeps_absent_pr_uncertain_without_create() {
    let fixture = dispatch_fixture();
    let (base, requests, endpoint) = fake_server(vec![
        Scripted::ListPulls {
            body: serde_json::json!([]),
            expected_head: dispatch_expected_head(),
        },
        Scripted::CreatePrError(502, r#"{"message":"Bad Gateway"}"#.to_string()),
        Scripted::ListPulls {
            body: serde_json::json!([]),
            expected_head: dispatch_expected_head(),
        },
    ]);
    let client = GitHubClient::for_test(&base, "fake-token").unwrap();

    // The create request reached the endpoint but failed ambiguously; the
    // worker stopped before it could record any outcome.
    let dispatched = dispatch_open_pr(&fixture, &client);
    assert!(
        dispatched.is_err(),
        "an ambiguous create produces no result: {dispatched:?}"
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        2,
        "one find plus one ambiguous create reached the endpoint"
    );
    assert_eq!(effect_state(fixture.run_id), "possible_dispatch");
    assert_eq!(history_count(fixture.run_id, "effect_target"), 1);

    // Restart, then the same production recovery chain as the found-PR case.
    let active = active_run(fixture.run_id);
    let mut restarted = view_from_active(&active);
    reconcile_stuck_github_step(&mut restarted);
    operator_recheck(fixture.run_id);
    let mut resumed = resume_queued_recheck(fixture.run_id);

    let active = active_run(fixture.run_id);
    let event = recheck_with_client(&active, &mut resumed, &client);
    assert!(
        matches!(&event, CircuitEvent::EffectUncertain { reason, .. } if reason.contains("No open pull request was found")),
        "an absent PR stays uncertain: {event:?}"
    );
    persist_effect_result(&mut resumed, &event).unwrap();

    assert_eq!(reopened_run(fixture.run_id).state, "running");
    assert_eq!(open_pr_step_status(fixture.run_id), "unverified");
    assert_eq!(run_context(fixture.run_id).get("pr.number"), None);
    assert_eq!(
        effect_state(fixture.run_id),
        "uncertain",
        "an absent PR leaves the ambiguous dispatch uncertain"
    );
    assert_eq!(history_count(fixture.run_id, "effect_intent"), 1);
    assert_eq!(history_count(fixture.run_id, "effect_possible_dispatch"), 1);
    assert_eq!(history_count(fixture.run_id, "effect_target"), 1);
    assert_eq!(history_count(fixture.run_id, "effect_reconciled"), 0);
    assert_eq!(
        requests.load(Ordering::SeqCst),
        3,
        "recovery issues one read-only find and never a create"
    );
    endpoint.join().expect("deterministic endpoint");
}

#[test]
fn open_pr_dispatch_crash_then_cancellation_fences_the_stale_recheck() {
    let fixture = dispatch_fixture();
    let (base, requests, endpoint) = fake_server(vec![
        Scripted::ListPulls {
            body: serde_json::json!([]),
            expected_head: dispatch_expected_head(),
        },
        Scripted::CreatePullRequest(dispatch_pull_request_json()),
        Scripted::ListPulls {
            body: serde_json::json!([dispatch_pull_request_json()]),
            expected_head: dispatch_expected_head(),
        },
    ]);
    let client = GitHubClient::for_test(&base, "fake-token").unwrap();

    // Real create dispatch, then a stop before its result commits.
    assert!(matches!(
        dispatch_open_pr(&fixture, &client),
        Ok(CircuitEvent::GithubActionResult { success: true, .. })
    ));
    let active = active_run(fixture.run_id);

    // Restart and reach the queued recheck exactly as production does.
    let mut restarted = view_from_active(&active);
    reconcile_stuck_github_step(&mut restarted);
    operator_recheck(fixture.run_id);

    // The worker reschedules the recheck and is about to run the read-only
    // lookup. This snapshot is what the in-flight lookup is computed against.
    let mut in_flight = resume_queued_recheck(fixture.run_id);
    assert_eq!(open_pr_step_status(fixture.run_id), "running");
    assert_eq!(effect_state(fixture.run_id), "uncertain");

    // Cancellation commits first, while that read-only lookup is in flight.
    crate::db::commit_circuit_advance(
        fixture.run_id,
        Some("cancelled"),
        None,
        &[CircuitStepOp {
            node_id: "open_pr".into(),
            status: "cancelled".into(),
            outcome: Some(Some("cancelled".into())),
            error: None,
            agent_node_id: None,
            attempt: 1,
            fresh_attempt: false,
        }],
    )
    .unwrap();

    // The lookup still finds the dispatched PR, but its result is stale.
    let event = recheck_with_client(&active, &mut in_flight, &client);
    assert!(matches!(
        event,
        CircuitEvent::GithubActionResult { success: true, pr_number: Some(314), .. }
    ));
    assert!(
        persist_effect_result(&mut in_flight, &event).is_err(),
        "a recheck computed before cancellation cannot commit after it"
    );

    assert_eq!(reopened_run(fixture.run_id).state, "cancelled");
    assert_eq!(open_pr_step_status(fixture.run_id), "cancelled");
    assert_eq!(run_context(fixture.run_id).get("pr.number"), None);
    assert_eq!(
        effect_state(fixture.run_id),
        "uncertain",
        "a fenced late recheck never acknowledges the uncertain effect"
    );
    assert_eq!(history_count(fixture.run_id, "effect_reconciled"), 0);
    assert_eq!(
        requests.load(Ordering::SeqCst),
        3,
        "one find plus one create during dispatch, one read-only find during the recheck"
    );
    endpoint.join().expect("deterministic endpoint");
}
