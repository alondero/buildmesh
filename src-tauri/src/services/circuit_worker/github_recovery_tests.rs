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
    assert_cancelled_open_pr_without_reconcile(fixture.run_id);
}

/// Fresh-read agreement shared by the cancellation-ordering tests: a
/// rejected late result leaves the run terminal with no `pr.*` context, a
/// cancelled step, an uncertain effect, and no reconciled history.
fn assert_cancelled_open_pr_without_reconcile(run_id: i64) {
    let stored = reopened_run(run_id);
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
        crate::db::circuit::ledger::list_circuit_run_steps_inner(&reopened, run_id).unwrap()[0]
            .status,
        "cancelled"
    );
    assert_eq!(
        reopened
            .query_row(
                "SELECT state FROM circuit_effects WHERE run_id=?1 AND node_id='open_pr' AND attempt=1 AND kind='github'",
                [run_id],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "uncertain"
    );
    assert_eq!(
        reopened
            .query_row(
                "SELECT COUNT(*) FROM circuit_run_history WHERE run_id=?1 AND kind='effect_reconciled'",
                [run_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

/// Rebuild a run view from durable state only, the way a restarted worker
/// pass loads it before dispatching.
fn restart_view_from_db(run_id: i64, circuit_graph_json: &str) -> RunView {
    let run = crate::db::get_circuit_run(run_id)
        .expect("read run")
        .expect("run row survives");
    let mut context =
        CircuitContext::from_json(&run.context_json).expect("context parses");
    let revision = crate::db::circuit::evidence::observation_revision(&run)
        .expect("revision reads");
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
            listener
                .set_nonblocking(true)
                .expect("watch nonblocking");
            loop {
                if done_rx.try_recv().is_ok() {
                    break;
                }
                match listener.accept() {
                    Ok((sock, _)) => {
                        thread_requests.fetch_add(1, Ordering::SeqCst);
                        drop(sock);
                    }
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock =>
                    {
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
        self.requests
            .load(std::sync::atomic::Ordering::SeqCst)
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

    use crate::autopilot::circuit::stepper::Effect;

    let fixture = open_pr_fixture();
    let run_id = fixture.run_id;
    let pr_body = serde_json::to_vec(&serde_json::json!([{
        "number": 314,
        "html_url": "https://github.com/example/buildmesh/pull/314",
        "title": "Circuit recovery",
        "head": {"ref": "feature/circuit-recovery"}
    }]))
    .unwrap();
    let endpoint =
        HeldOpenPrEndpoint::spawn(pr_body, "head=example%3Afeature%2Fcircuit-recovery");

    let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let active = fixture.active.clone();
    let mut view = fixture.view.clone();
    let order_worker = Arc::clone(&order);
    let base_url = endpoint.base_url.clone();
    let lookup = std::thread::spawn(move || {
        // `execute_effects` itself cannot run here: it takes a Tauri
        // `AppHandle`, which unit tests cannot construct. This thread
        // composes the same app-free production pieces in the loop's exact
        // order instead: batch permit, durable run-state snapshot,
        // per-effect gates, then the `CallGithub` dispatch the loop
        // delegates to.
        let batch = super::begin_circuit_effect_batch(run_id);
        let run_state = crate::db::get_circuit_run(run_id)
            .expect("read run state")
            .map(|run| run.state);
        let completing = matches!(view.state, RunState::Completed | RunState::Failed);
        let effect = Effect::CallGithub {
            node_id: "open_pr".to_string(),
            action: GithubActionKind::OpenPr,
            label: None,
            comment: None,
        };
        let mut outcomes = Vec::new();
        if !batch.is_cancelled()
            && run_state.as_deref().is_some_and(|state| {
                super::effect_allowed_in_state(state, completing, &effect)
            })
        {
            let client =
                crate::services::github::GitHubClient::for_test(&base_url, "fake-token")
                    .expect("test client");
            outcomes.extend(
                super::execute_call_github_effect(
                    &active,
                    &mut view,
                    "open_pr",
                    GithubActionKind::OpenPr,
                    None,
                    None,
                    Some(&client),
                )
                .expect("dispatch succeeds"),
            );
        }
        let worker_saw_cancellation = batch.is_cancelled();
        order_worker.lock().unwrap().push("lookup_returned");
        drop(batch);
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

    // The pending-outcome loop, exactly as `drive_run` composes it: advance
    // the stale event, then commit through the production seam, which must
    // reject the stale completion.
    let outcome = advance(&mut view_with_result, &event);
    assert!(
        persist_transition(
            view_with_result.run_id,
            &mut view_with_result,
            &outcome
        )
        .is_err()
    );

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

    // Retry: a view rebuilt from durable state still carries dispatch intent,
    // but the worker's own gates block it before any GitHub request. The
    // endpoint is still watching, so a stray dispatch would be counted.
    let restarted = restart_view_from_db(run_id, &fixture.active.circuit_graph_json);
    assert_eq!(
        restarted.context.get("node.open_pr.recheck_only"),
        Some("1"),
        "the rebuilt retry still intends a recheck, so the gate below is not vacuous"
    );
    let batch = super::begin_circuit_effect_batch(run_id);
    let run_state = crate::db::get_circuit_run(run_id)
        .expect("read run state")
        .map(|run| run.state);
    let completing = matches!(
        restarted.state,
        RunState::Completed | RunState::Failed
    );
    let effect = Effect::CallGithub {
        node_id: "open_pr".to_string(),
        action: GithubActionKind::OpenPr,
        label: None,
        comment: None,
    };
    let accepted = !batch.is_cancelled()
        && run_state.as_deref().is_some_and(|state| {
            super::effect_allowed_in_state(state, completing, &effect)
        });
    drop(batch);
    assert!(
        !accepted,
        "a retry on the cancelled run is gated before any GitHub request"
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
