use super::*;
use crate::autopilot::circuit::context::CircuitContext;
use crate::autopilot::circuit::model::{
    CircuitGraph, CircuitNode, CircuitNodeKind, GithubActionKind,
};
use crate::autopilot::circuit::stepper::{
    advance, CircuitEvent, RunState, RunView, StepStatus, StepView,
};
use crate::db::circuit::evidence::{EffectIntent, EvidenceWrite};
use crate::db::CircuitStepOp;
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
    let path = {
        let db = crate::db::read_conn();
        db.query_row("PRAGMA database_list", [], |row| row.get::<_, String>(2))
            .unwrap()
    };
    let reopened = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        crate::db::circuit::ledger::list_circuit_run_steps_inner(&reopened, fixture.run_id)
            .unwrap()[0]
            .status,
        "completed"
    );
    assert_eq!(
        reopened
            .query_row(
                "SELECT state FROM circuit_effects WHERE run_id=?1 AND node_id='open_pr' AND attempt=1 AND kind='github'",
                [fixture.run_id],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "acknowledged"
    );
    assert_eq!(
        reopened
            .query_row(
                "SELECT COUNT(*) FROM circuit_run_history WHERE run_id=?1 AND kind='effect_reconciled'",
                [fixture.run_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
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
    let path = {
        let db = crate::db::read_conn();
        db.query_row("PRAGMA database_list", [], |row| row.get::<_, String>(2))
            .unwrap()
    };
    let reopened = rusqlite::Connection::open(path).unwrap();
    let context = CircuitContext::from_json(&stored.context_json).unwrap();
    assert_eq!(stored.state, "cancelled");
    assert_eq!(context.get("pr.number"), None);
    assert_eq!(
        crate::db::circuit::ledger::list_circuit_run_steps_inner(&reopened, fixture.run_id)
            .unwrap()[0]
            .status,
        "cancelled"
    );
    assert_eq!(
        reopened
            .query_row(
                "SELECT state FROM circuit_effects WHERE run_id=?1 AND node_id='open_pr' AND attempt=1 AND kind='github'",
                [fixture.run_id],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "uncertain"
    );
    assert_eq!(
        reopened
            .query_row(
                "SELECT COUNT(*) FROM circuit_run_history WHERE run_id=?1 AND kind='effect_reconciled'",
                [fixture.run_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
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
        let path = {
            let db = crate::db::read_conn();
            db.query_row("PRAGMA database_list", [], |row| row.get::<_, String>(2))
                .unwrap()
        };
        let reopened = rusqlite::Connection::open(path).unwrap();
        let stored = crate::db::circuit::ledger::get_circuit_run_inner(&reopened, fixture.run_id)
            .unwrap()
            .unwrap();
        let context = CircuitContext::from_json(&stored.context_json).unwrap();
        assert_eq!(stored.state, "running");
        assert_eq!(context.get("pr.number"), None);
        assert_eq!(
            reopened
                .query_row(
                    "SELECT state FROM circuit_effects WHERE run_id=?1 AND node_id='open_pr' AND attempt=1 AND kind='github'",
                    [fixture.run_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "uncertain",
            "a failed read-only lookup never acknowledges or replays the effect"
        );
    }
}
