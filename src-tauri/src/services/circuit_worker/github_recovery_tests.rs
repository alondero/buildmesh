use super::*;
use crate::circuit::context::CircuitContext;
use crate::circuit::model::{
    CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, EdgeCondition, GithubActionKind,
};
use crate::circuit::stepper::{advance, CircuitEvent, RunState, RunView, StepStatus, StepView};
use crate::db::circuit::evidence::{EffectIntent, EffectKind, EvidenceWrite};
use crate::db::CircuitStepOp;
use crate::services::github::tests::{fake_server, Scripted};
use crate::services::github::{CreatePrRequest, GitHubClient};
use std::sync::atomic::{AtomicU64, Ordering};

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[test]
fn inherited_retry_attempt_can_commit_github_result() {
    let fixture = dispatch_fixture();
    crate::db::commit_circuit_advance(
        fixture.run_id,
        None,
        None,
        &[
            CircuitStepOp {
                node_id: "implementer".into(),
                status: "completed".into(),
                outcome: Some(Some("completed".into())),
                error: None,
                agent_node_id: None,
                attempt: 2,
                fresh_attempt: true,
            },
            CircuitStepOp {
                node_id: "open_pr".into(),
                status: "failed".into(),
                outcome: Some(Some("failed".into())),
                error: Some(Some("branch not pushed".into())),
                agent_node_id: None,
                attempt: 1,
                fresh_attempt: false,
            },
        ],
    )
    .unwrap();
    let mut view = running_view(fixture.run_id);
    let transition = advance(
        &mut view,
        &CircuitEvent::Tick(crate::circuit::stepper::Capacity {
            agent_free_slots: 4,
        }),
    );
    persist_transition(fixture.run_id, &mut view, &transition).unwrap();
    assert_eq!(view.step("open_pr").unwrap().attempt, 2);
    assert_eq!(view.step("open_pr").unwrap().status, StepStatus::Running);
    let event = CircuitEvent::GithubActionResult {
        node_id: "open_pr".into(),
        success: true,
        pr_number: Some(314),
        pr_url: Some("https://github.com/example/buildmesh/pull/314".into()),
        pr_head_ref: Some(DISPATCH_HEAD.into()),
        pr_title: Some("Fixed".into()),
        error: None,
    };
    persist_effect_result(&mut view, &event)
        .expect("retry projection must match durable attempt and error");
    assert_eq!(reopened_run(fixture.run_id).state, "completed");
    assert_eq!(run_context(fixture.run_id).get("pr.number"), Some("314"));
    let stored = crate::db::list_circuit_run_steps(fixture.run_id).unwrap();
    let step = stored
        .iter()
        .find(|step| step.node_id == "open_pr")
        .unwrap();
    assert_eq!(
        step.outcome.as_deref(),
        Some("completed"),
        "restart must preserve outcome-based routing"
    );
}

#[test]
fn unsupported_github_mutations_stay_uncertain_without_replay_on_recovery() {
    for action in [
        GithubActionKind::AddLabel,
        GithubActionKind::RemoveLabel,
        GithubActionKind::PostComment,
        GithubActionKind::CloseIssue,
    ] {
        let fixture = open_pr_fixture();
        let client = GitHubClient::for_test("http://127.0.0.1:1", "fake-token").unwrap();
        let mut view = fixture.view.clone();
        view.context.set("node.open_pr.recheck_only", "1");

        let mut events = super::execute_call_github_effect(
            &fixture.active,
            &mut view,
            "open_pr",
            action,
            Some("triaged"),
            Some("Recovery audit"),
            Some(&client),
        )
        .unwrap();
        assert_eq!(events.len(), 1);
        assert!(
            matches!(
                &events[0],
                CircuitEvent::EffectUncertain { attempt: 1, reason, .. }
                    if reason == "Read-only external-action recheck is unavailable for this GitHub action."
            ),
            "{action:?} recovery must require an operator outcome"
        );

        let event = events.pop().unwrap();
        let mut projected = view.clone();
        let transition = advance(&mut projected, &event);
        assert!(
            transition.effects.is_empty(),
            "uncertain recovery cannot redispatch {action:?}"
        );
        assert_eq!(
            projected.step("open_pr").unwrap().status,
            StepStatus::Unverified
        );
        persist_effect_result(&mut view, &event).unwrap();
        assert_eq!(open_pr_step_status(fixture.run_id), "unverified");
        assert_eq!(effect_state(fixture.run_id), "uncertain");
        assert_eq!(history_count(fixture.run_id, "effect_possible_dispatch"), 1);
    }
}

/// A run whose OpenPr effect is already claimed but uncertain, read by the
/// recovery tests below. `_db` holds the private database `db::test_support`
/// installed for this test thread (issue #2048) instead of a process-global
/// one: every `db::` call in the test body then resolves to this test's own
/// rows. The fixture owns the guard rather than this helper because dropping a
/// local here would uninstall the database before the test body ran.
///
/// It is a *file-backed* database on purpose: `reopened_run` below simulates a
/// crash restart by reopening the database by path, which only means anything
/// if there is a file to reopen.
struct OpenPrFixture {
    _db: crate::db::test_support::IsolatedDbGuard,
    active: crate::db::ActiveCircuitRun,
    view: RunView,
    run_id: i64,
}

fn open_pr_fixture() -> OpenPrFixture {
    let _db = crate::db::test_support::isolated_file();
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
        kind: EffectKind::Github,
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
    // Seed the durable recheck mode production establishes through its
    // evidence-recheck path, so views rebuilt from the database (restarts)
    // observe the same dispatch intent as the live view.
    let mut seed = CircuitContext::default();
    seed.set("node.open_pr.recheck_only", "1");
    crate::db::circuit::evidence::commit_transition(
        run_id,
        Some("running"),
        &seed.to_json().unwrap(),
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
        _db,
        active,
        view,
        run_id,
    }
}

fn persist_effect_result(view: &mut RunView, event: &CircuitEvent) -> Result<bool, String> {
    persist_effect_outcome(view.run_id, view, event).map(|(_, changed)| changed)
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
                kind: EffectKind::Github
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
    // The lookup runs on its own thread, and a `thread_local!` database
    // install does not follow the thread: the worker has to adopt this test's
    // database or it resolves the process-global one (issue #2048).
    let lookup_db = fixture._db.handle();
    let lookup = std::thread::spawn(move || {
        let _adopted = crate::db::test_support::adopt(&lookup_db);
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
    assert_cancelled_open_pr_without_reconcile(fixture.run_id);
}

/// Fresh-read agreement shared by the cancellation-ordering tests: a
/// rejected late result leaves the run terminal with no `pr.*` context, a
/// cancelled step on its original attempt, an uncertain effect, and no
/// reconciled history.
fn assert_cancelled_open_pr_without_reconcile(run_id: i64) {
    let stored = reopened_run(run_id);
    let context = CircuitContext::from_json(&stored.context_json).unwrap();
    assert_eq!(stored.state, "cancelled");
    assert_eq!(context.get("pr.number"), None);
    assert_eq!(open_pr_step_status(run_id), "cancelled");
    assert_eq!(
        open_pr_step_attempt(run_id),
        1,
        "the rejected late result must not reopen the attempt"
    );
    assert_eq!(effect_state(run_id), "uncertain");
    assert_eq!(history_count(run_id, "effect_reconciled"), 0);
}

/// Rebuild a run view from durable state only, the way a restarted worker
/// pass loads it before dispatching.
fn restart_view_from_db(run_id: i64, circuit_graph_json: &str) -> RunView {
    let run = crate::db::get_circuit_run(run_id)
        .expect("read run")
        .expect("run row survives");
    let mut context = CircuitContext::from_json(&run.context_json).expect("context parses");
    let revision =
        crate::db::circuit::evidence::observation_revision(&run).expect("revision reads");
    context.set("evidence.revision", revision.to_string());
    context.with_run(run_id);
    RunView {
        run_id,
        graph: CircuitGraph::from_json(circuit_graph_json).expect("graph parses"),
        state: RunState::from_db_str(&run.state),
        context,
        steps: super::load_steps(run_id).expect("steps load"),
    }
}

/// Controllable OpenPr lookup endpoint for the cancellation-ordering tests.
/// One struct owns the base URL, request count, hold/release/done channels,
/// and server thread — no loose synchronization primitives cross the test.
/// The endpoint holds its single list-pulls response until released, then
/// keeps watching: a duplicate dispatch from a restart or retry is counted
/// (and fails the test) instead of refused or hung.
struct HeldOpenPrEndpoint {
    base_url: String,
    requests: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    arrived_rx: std::sync::mpsc::Receiver<String>,
    release_tx: std::sync::mpsc::Sender<()>,
    done_tx: std::sync::mpsc::Sender<()>,
    server: Option<std::thread::JoinHandle<()>>,
}

impl HeldOpenPrEndpoint {
    fn spawn(body: Vec<u8>, expected_head: &str) -> Self {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let requests = Arc::new(AtomicUsize::new(0));
        let (line_tx, arrived_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let expected = expected_head.to_string();
        let thread_requests = Arc::clone(&requests);
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept lookup");
            thread_requests.fetch_add(1, Ordering::SeqCst);
            let mut reader = BufReader::new(sock.try_clone().expect("clone socket"));
            let mut request_line = String::new();
            reader
                .read_line(&mut request_line)
                .expect("read request line");
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("read header");
                if line.trim().is_empty() {
                    break;
                }
            }
            assert!(
                request_line.starts_with("GET ")
                    && request_line.contains("/pulls?head=")
                    && request_line.contains(&expected),
                "OpenPr recheck must stay a read-only list lookup, got: {}",
                request_line.trim()
            );
            line_tx.send(request_line).expect("report lookup arrival");
            release_rx.recv().expect("lookup stays held until released");
            let http = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                String::from_utf8(body).expect("utf8 body")
            );
            sock.write_all(http.as_bytes())
                .expect("write held response");
            drop(sock);
            // Duplicate watch: the retry phase runs while this listener is
            // still up, so a second dispatch is observed here. The test
            // always sends `done`, so this loop cannot spin forever; a
            // panic before `done` fails the test first and the parked
            // thread dies with the process.
            listener.set_nonblocking(true).expect("watch nonblocking");
            loop {
                if done_rx.try_recv().is_ok() {
                    break;
                }
                match listener.accept() {
                    Ok((sock, _)) => {
                        thread_requests.fetch_add(1, Ordering::SeqCst);
                        drop(sock);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(error) => panic!("duplicate watch accept failed: {error}"),
                }
            }
        });
        Self {
            base_url: format!("http://{addr}"),
            requests,
            arrived_rx,
            release_tx,
            done_tx,
            server: Some(server),
        }
    }

    /// Block until the held lookup arrives; returns its request line.
    fn wait_for_lookup(&self) -> String {
        self.arrived_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("lookup reaches the controllable endpoint")
    }

    /// Release the held response.
    fn release(&self) {
        self.release_tx.send(()).expect("release the held lookup");
    }

    /// Report the retry phase finished; the duplicate watch exits.
    fn finish(&self) {
        self.done_tx.send(()).expect("stop the duplicate watch");
    }

    fn request_count(&self) -> usize {
        self.requests.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn join(mut self) {
        self.server
            .take()
            .expect("server thread")
            .join()
            .expect("server thread joins");
    }
}

/// Issue #1906: a late OpenPr lookup that returns after cancellation must be
/// rejected through the worker handoff. The lookup runs on a worker thread
/// through the worker's real `CallGithub` dispatch against a controllable
/// endpoint that holds its response; cancellation commits through the
/// production command ordering; only then is the stale success released into
/// the production outcome seam. A restart rebuild plus a gated retry then
/// prove no duplicate lookup or effect follows.
#[test]
fn open_pr_late_lookup_after_cancellation_is_rejected_through_worker_handoff() {
    use std::sync::{Arc, Mutex};

    use crate::circuit::stepper::Effect;

    let fixture = open_pr_fixture();
    let run_id = fixture.run_id;
    let pr_body = serde_json::to_vec(&serde_json::json!([{
        "number": 314,
        "html_url": "https://github.com/example/buildmesh/pull/314",
        "title": "Circuit recovery",
        "head": {"ref": "feature/circuit-recovery"}
    }]))
    .unwrap();
    let endpoint = HeldOpenPrEndpoint::spawn(pr_body, "head=example%3Afeature%2Fcircuit-recovery");

    let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let active = fixture.active.clone();
    let mut view = fixture.view.clone();
    let order_worker = Arc::clone(&order);
    let base_url = endpoint.base_url.clone();
    let thread_active = active.clone();
    // As above: the dispatch thread adopts this test's database, because the
    // per-thread install does not follow a spawned thread (issue #2048).
    let lookup_db = fixture._db.handle();
    let lookup = std::thread::spawn(move || {
        let _adopted = crate::db::test_support::adopt(&lookup_db);
        // Production orchestration only: `run_github_effect_pass` runs the
        // same batch, snapshot, gates, and dispatch the `execute_effects`
        // loop delegates to (`execute_effects` itself needs a Tauri
        // `AppHandle`, which unit tests cannot construct).
        let effect = Effect::CallGithub {
            node_id: "open_pr".to_string(),
            action: GithubActionKind::OpenPr,
            label: None,
            comment: None,
        };
        let client = crate::services::github::GitHubClient::for_test(&base_url, "fake-token")
            .expect("test client");
        let (outcomes, worker_saw_cancellation) =
            super::run_github_effect_pass(&thread_active, &mut view, &effect, Some(&client))
                .expect("dispatch runs");
        order_worker.lock().unwrap().push("lookup_returned");
        (view, outcomes, worker_saw_cancellation)
    });

    // The lookup is held inside the endpoint: cancellation must go terminal
    // before the delayed result returns.
    let request_line = endpoint.wait_for_lookup();
    assert!(
        request_line.contains("/pulls?head="),
        "recheck holds a read-only lookup, got: {}",
        request_line.trim()
    );
    order.lock().unwrap().push("lookup_held");

    // Production cancellation ordering: invalidate in-flight effects, commit
    // the durable terminal state, then release the marker.
    super::mark_circuit_run_cancelled(run_id);
    crate::db::cancel_circuit_run(run_id).expect("cancellation commits");
    super::finish_circuit_run_cancellation(run_id);
    order.lock().unwrap().push("cancellation_committed");

    let stored = crate::db::get_circuit_run(run_id)
        .expect("read run")
        .expect("run row survives cancellation");
    assert_eq!(stored.state, "cancelled");

    endpoint.release();
    let (mut view_with_result, outcomes, worker_saw_cancellation) =
        lookup.join().expect("worker thread joins");
    assert!(
        worker_saw_cancellation,
        "the in-flight worker batch observes the cancellation token"
    );
    assert_eq!(
        outcomes.len(),
        1,
        "the held lookup dispatches exactly one outcome event"
    );
    let event = outcomes.into_iter().next().unwrap();
    assert!(
        matches!(
            event,
            CircuitEvent::GithubActionResult {
                success: true,
                pr_number: Some(314),
                ..
            }
        ),
        "the held endpoint returns a success-shaped stale result"
    );
    assert_eq!(
        view_with_result
            .context
            .get("node.open_pr.effect_reconciled_attempt"),
        Some("1"),
        "the worker handoff mapping ran before the seam rejected it"
    );
    assert_eq!(
        order.lock().unwrap().as_slice(),
        &["lookup_held", "cancellation_committed", "lookup_returned"],
    );

    // The pending-outcome drain, shared with `drive_run`: the stale commit
    // must be rejected before any follow-on executes or any progress emits.
    // The stubs panic instead of silently passing, so a regression that
    // commits the stale result fails loudly right here.
    assert!(super::drain_effect_outcomes(
        view_with_result.run_id,
        &mut view_with_result,
        vec![event],
        &mut |_, _| -> Result<Vec<CircuitEvent>, String> {
            panic!("a rejected stale outcome must not execute follow-ons")
        },
        &mut |_, _, _| panic!("a rejected stale outcome must not emit progress"),
    )
    .is_err());

    // A fresh read and the append-only history agree: nothing from the late
    // result landed, and the attempt was not reopened.
    assert_cancelled_open_pr_without_reconcile(run_id);

    // Restart: the cancelled run leaves the worker's active set, so a fresh
    // pass never picks it up again.
    assert!(
        crate::db::list_active_circuit_runs()
            .expect("list active runs")
            .iter()
            .all(|active| active.run.id != run_id),
        "a cancelled run leaves the active set so a restart cannot redispatch it"
    );

    // Retry: the rebuilt run goes through production dispatch, which must
    // block it before any GitHub request. The endpoint is still watching,
    // so a stray dispatch would be counted.
    let mut restarted = restart_view_from_db(run_id, &fixture.active.circuit_graph_json);
    assert_eq!(
        restarted.context.get("node.open_pr.recheck_only"),
        Some("1"),
        "the rebuilt retry still intends a recheck, so the blocked dispatch below is not vacuous"
    );
    let effect = Effect::CallGithub {
        node_id: "open_pr".to_string(),
        action: GithubActionKind::OpenPr,
        label: None,
        comment: None,
    };
    let retry_client =
        crate::services::github::GitHubClient::for_test(&endpoint.base_url, "fake-token")
            .expect("test client");
    let (retry_outcomes, _) =
        super::run_github_effect_pass(&active, &mut restarted, &effect, Some(&retry_client))
            .expect("retry runs");
    assert!(
        retry_outcomes.is_empty(),
        "a retry on the cancelled run dispatches nothing"
    );
    assert!(
        !super::run_accepts_effects(run_id).expect("read run state"),
        "a cancelled run accepts no further effects on retry"
    );
    endpoint.finish();
    assert_eq!(
        endpoint.request_count(),
        1,
        "exactly one read-only lookup: no PR create, no duplicate dispatch by restart or retry"
    );
    endpoint.join();
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
    format!("{DISPATCH_OWNER}%3A{}", DISPATCH_HEAD.replace('/', "%2F"))
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
/// stays valid for the test's lifetime, and `_db` the private database
/// `db::test_support` installed for this test thread (issue #2048) so the test
/// body reads this test's own rows. The fixture owns that guard rather than
/// `dispatch_fixture`, because dropping it inside the helper would uninstall
/// the database before the test body ran.
struct DispatchFixture {
    _db: crate::db::test_support::IsolatedDbGuard,
    run_id: i64,
    view: RunView,
    _dir: tempfile::TempDir,
}

fn dispatch_fixture() -> DispatchFixture {
    let _db = crate::db::test_support::isolated_file();
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
        kind: EffectKind::Github,
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
        _db,
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
        kind: EffectKind::Github,
    };
    crate::db::circuit::evidence::claim_effect(fixture.run_id, &intent)
        .unwrap()
        .expect("a running OpenPr dispatch is claimed before any request");
    github::ensure_open_pr_with_target(
        &fixture.view,
        "open_pr",
        None,
        |_agent| {
            Ok(crate::circuit::verification::WrapupState {
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

fn open_pr_step_attempt(run_id: i64) -> i32 {
    crate::db::read_conn()
        .query_row(
            "SELECT attempt FROM autopilot_circuit_run_steps WHERE run_id=?1 AND node_id='open_pr'",
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
    crate::db::circuit::evidence::record_outcome(
        &crate::db::circuit::evidence::CheckpointRequest {
            run_id,
            node_id: "open_pr".into(),
            attempt: 1,
            expected_revision: revision,
            action: crate::db::circuit::evidence::CheckpointAction::Recheck,
            reason: "Reconcile the saved pull-request target without creating one.".into(),
        },
    )
    .unwrap();
}

#[test]
fn automatic_pr_reconciliation_is_read_only_durable_and_bounded() {
    let fixture = open_pr_fixture();
    let mut view = running_view(fixture.run_id);
    reconcile_stuck_github_step(&mut view);
    for index in 0..5 {
        let event = CircuitEvent::GithubRecheckDue {
            node_id: "open_pr".into(),
            attempt: 1,
            now_ms: 1000 + index * 60_000,
        };
        let transition = advance(&mut view, &event);
        assert_eq!(transition.effects.len(), 1);
        persist_transition(fixture.run_id, &mut view, &transition).unwrap();
        let active = active_run(fixture.run_id);
        let missing =
            github::reconcile_open_pr_for_worker(&active, &mut view, "open_pr", |_, _, _| Ok(None));
        persist_effect_result(&mut view, &missing).unwrap();
        // Reconstruct solely from the ledger, as after an app restart.
        view = running_view(fixture.run_id);
        let too_soon = advance(
            &mut view,
            &CircuitEvent::GithubRecheckDue {
                node_id: "open_pr".into(),
                attempt: 1,
                now_ms: 1001 + index * 60_000,
            },
        );
        assert!(too_soon.effects.is_empty());
    }
    let exhausted = advance(
        &mut view,
        &CircuitEvent::GithubRecheckDue {
            node_id: "open_pr".into(),
            attempt: 1,
            now_ms: 1_000_000,
        },
    );
    assert!(exhausted.effects.is_empty());
    assert_eq!(view.step("open_pr").unwrap().status, StepStatus::Unverified);
    assert_eq!(history_count(fixture.run_id, "effect_possible_dispatch"), 1);
    let history = crate::db::circuit::evidence::history(fixture.run_id).unwrap();
    assert!(history
        .entries
        .iter()
        .any(|entry| entry.kind == "checkpoint_reason"
            && entry.detail.contains("No open pull request was found")));
    operator_recheck(fixture.run_id);
    let mut view = resume_queued_recheck(fixture.run_id);
    let active = active_run(fixture.run_id);
    let found = github::reconcile_open_pr_for_worker(&active, &mut view, "open_pr", |_, _, _| {
        Ok(Some(pull_request("feature/circuit-recovery")))
    });
    persist_effect_result(&mut view, &found).unwrap();
    assert_eq!(reopened_run(fixture.run_id).state, "completed");
    let history = crate::db::circuit::evidence::history(fixture.run_id).unwrap();
    assert!(
        history
            .entries
            .iter()
            .any(|entry| entry.kind == "checkpoint_reason"
                && entry.detail.contains("No open pull request was found")),
        "recovery retains the original reason"
    );
}

/// The worker's next tick promotes the queued recheck back to `Running` and
/// re-emits the read-only GitHub call. Returns the view at that handoff, which
/// is where the deterministic reconciliation begins.
fn resume_queued_recheck(run_id: i64) -> RunView {
    let active = active_run(run_id);
    let mut view = view_from_active(&active);
    let transition = advance(
        &mut view,
        &CircuitEvent::Tick(crate::circuit::stepper::Capacity {
            agent_free_slots: 4,
        }),
    );
    assert!(
        transition.effects.iter().any(|effect| matches!(
            effect,
            crate::circuit::stepper::Effect::CallGithub { node_id, .. } if node_id == "open_pr"
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

    // Automatic read-only reconciliation uses the same durable attempt and
    // production effect dispatcher; the scripted endpoint rejects a POST.
    let mut resumed = running_view(fixture.run_id);
    let transition = advance(
        &mut resumed,
        &CircuitEvent::GithubRecheckDue {
            node_id: "open_pr".into(),
            attempt: 1,
            now_ms: 1000,
        },
    );
    assert_eq!(transition.effects.len(), 1);
    persist_transition(fixture.run_id, &mut resumed, &transition).unwrap();
    let active = active_run(fixture.run_id);
    let event = super::execute_call_github_effect(
        &active,
        &mut resumed,
        "open_pr",
        GithubActionKind::OpenPr,
        None,
        None,
        Some(&client),
    )
    .unwrap()
    .remove(0);
    assert!(matches!(
        event,
        CircuitEvent::GithubActionResult {
            success: true,
            pr_number: Some(314),
            ..
        }
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
                kind: EffectKind::Github
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
        CircuitEvent::GithubActionResult {
            success: true,
            pr_number: Some(314),
            ..
        }
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
