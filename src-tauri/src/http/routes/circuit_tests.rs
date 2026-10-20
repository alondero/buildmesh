//! Every piece of a route is driven on the test's own thread against a
//! private database (`db::test_support::isolated`), with runs minted by the
//! production writers, and what a `200` claims is checked in the ledger
//! afterwards. The only step not exercised here is the `run_blocking_typed`
//! hop between the pieces: an isolated test database is installed per thread,
//! so it cannot follow the work onto the blocking pool.
use super::*;
use crate::db::circuit::CircuitStepOp;
use crate::models::{EnvType, SessionStatus};

/// Issue #1533: a database that cannot answer right now is transient, so
/// the route answers `503` — the client retries — instead of `500`.
#[test]
fn a_busy_database_answers_503_and_a_broken_query_answers_500() {
    let busy = db_failure(crate::db::error::busy_error());
    assert!(
        matches!(busy, ActError::Unavailable(_)),
        "a busy database must not be reported as a broken query"
    );
    assert_eq!(
        busy.response().status,
        "503 Service Unavailable",
        "the client must be told to come back"
    );

    let broken = db_failure(rusqlite::Error::InvalidQuery);
    assert!(
        matches!(broken, ActError::Failed(_)),
        "a broken query is a fault in this build"
    );
    assert_eq!(broken.response().status, "500 Internal Server Error");
}

#[test]
fn review_choices_include_only_this_meshs_manual_reviews() {
    let _db = db::test_support::isolated();
    let (node, run) = review_run();
    let run = db::get_circuit_run(run).unwrap().unwrap();
    let copied = db::circuit::ledger::copy_review_blueprint(run.circuit_id, "Team review").unwrap();
    db::create_autopilot_circuit(
        run.mesh_id,
        "Not a review",
        "",
        &crate::circuit::model::CircuitGraph::walking_skeleton("hello")
            .to_json()
            .unwrap(),
    )
    .unwrap();
    let other_mesh = db::create_mesh("Other", "/other").unwrap();
    db::create_autopilot_circuit(
        other_mesh.id,
        "Other review",
        "",
        &crate::circuit::model::CircuitGraph::agent_review(None, None, 3)
            .to_json()
            .unwrap(),
    )
    .unwrap();
    let options = review_options_core(node).unwrap();
    assert_eq!(options.len(), 2);
    assert_eq!(options[0].circuit_id, None);
    assert_eq!(options[1].circuit_id, Some(copied.id));
    assert_eq!(options[1].name, "Team review");
    assert_eq!(
        review_options_core(99999).unwrap_err().response().status,
        "404 Not Found"
    );
}

#[test]
fn starting_a_review_parses_strictly_and_refuses_unavailable_sources() {
    let _db = db::test_support::isolated();
    for body in [
        br#"{}"#.as_slice(),
        br#"{"max_rounds":3,"extra":true}"#,
        b"bad",
    ] {
        assert_eq!(
            parse_body::<StartNodeReviewRequest>(&post("reviews", 1, body))
                .err()
                .unwrap()
                .status,
            "400 Bad Request"
        );
    }
    let (node, _) = review_run();
    let request = |id| StartNodeReviewRequest {
        circuit_id: id,
        max_rounds: 3,
    };
    assert_eq!(
        start_review_core(node, request(Some(99999)))
            .unwrap_err()
            .response()
            .status,
        "409 Conflict"
    );
    let refusal = start_review_core(node, request(None))
        .unwrap_err()
        .response();
    assert_eq!(refusal.status, "409 Conflict");
    assert!(text(&refusal).contains("Resume the agent"));
}

#[test]
fn starting_a_review_uses_the_desktop_run_and_deduplicates_retries() {
    let _db = db::test_support::isolated();
    let unique = 89_000_000 + i64::from(std::process::id());
    db::write_conn()
        .execute(
            "UPDATE sqlite_sequence SET seq = ?1 WHERE name = 'agent_nodes'",
            [unique - 1],
        )
        .unwrap();
    let (node, old_run) = review_run();
    assert_eq!(node, unique);
    db::commit_circuit_advance(old_run, Some("completed"), None, &[]).unwrap();
    let registry = &crate::agent::process::PROCESS_REGISTRY;
    crate::agent::process::testing::insert_standin(registry, node);
    struct Cleanup(i64);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            crate::agent::process::PROCESS_REGISTRY.kill_session(self.0);
        }
    }
    let _cleanup = Cleanup(node);
    let request = || StartNodeReviewRequest {
        circuit_id: None,
        max_rounds: 3,
    };
    let run = start_review_core(node, request()).unwrap();
    assert_ne!(run, old_run);
    assert_eq!(start_review_core(node, request()).unwrap(), run);
    let stored = db::get_circuit_run(run).unwrap().unwrap();
    assert_eq!(stored.source_agent_node_id, Some(node));
    assert_eq!(stored.state, "pending");
    assert_eq!(reply(run, Ok(())).status, "200 OK");
    cancel_core(run).unwrap();
    for provider in ["dsh", "freebuff"] {
        db::set_agent_node_provider(node, provider).unwrap();
        let refusal = start_review_core(node, request()).unwrap_err().response();
        assert_eq!(refusal.status, "409 Conflict");
        assert!(text(&refusal).contains("no turn-completion signal"));
    }
}

fn text(response: &Response) -> String {
    String::from_utf8_lossy(response.body()).into_owned()
}

fn json(response: &Response) -> serde_json::Value {
    serde_json::from_slice(response.body()).expect("route bodies are JSON")
}

fn post(route: &str, run_id: i64, body: &[u8]) -> ParsedRequest {
    ParsedRequest::test_post(&format!("/api/circuit-runs/{run_id}/{route}"), body)
        .with_ids(Some(run_id), None)
}

/// The approve route's pieces, joined the way the handler joins them.
fn drive_approve(run_id: i64, body: &[u8]) -> Response {
    match approve_request(&post("approve", run_id, body)) {
        Ok(body) => reply(run_id, approve_core(run_id, body.node_id)),
        Err(response) => response,
    }
}

fn drive_outcome(run_id: i64, body: &str) -> Response {
    match outcome_request(&post("outcome", run_id, body.as_bytes())) {
        Ok(request) => reply(request.run_id, outcome_core(&request)),
        Err(response) => response,
    }
}

fn drive_recover(run_id: i64, body: &str) -> Response {
    match recover_request(&post("recover", run_id, body.as_bytes())) {
        Ok(request) => reply(request.run_id, recover_core(&request)),
        Err(response) => response,
    }
}

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
    let mesh = db::create_mesh(
        "circuit-routes",
        &format!("/tmp/circuit-routes-{}", std::process::id()),
    )
    .unwrap();
    let node = db::create_agent_node(db::NewAgentNode {
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
    db::update_agent_node_status(node.id, SessionStatus::Ready).unwrap();
    let run = db::create_node_circuit_run(node.id, None, 3, None).unwrap();
    (node.id, run)
}

fn advance(run: i64, state: &str, steps: &[CircuitStepOp]) {
    db::commit_circuit_advance(run, Some(state), None, steps).unwrap();
}

fn listing() -> Response {
    list_reply(node_run::list_node_circuit_runs())
}

#[test]
fn ownership_read_shares_the_desktop_ledger_and_reports_failed_reads() {
    let _db = db::test_support::isolated();
    assert_eq!(
        text(&ownerships_reply(
            circuit_commands::list_circuit_agent_ownerships_now()
        )),
        "[]"
    );
    let (node, run) = review_run();
    let response = ownerships_reply(circuit_commands::list_circuit_agent_ownerships_now());
    assert_eq!(response.status, "200 OK");
    let body = json(&response);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["node_id"], node);
    assert_eq!(body[0]["run_id"], run);
    assert!(body[0]["parent_node_id"].is_null());
    let failure = ownerships_reply(Err(crate::db::DbError::Rendered("read failed".into())));
    assert_eq!(failure.status, "500 Internal Server Error");
    assert_eq!(json(&failure)["error"], "read failed");
}

#[test]
fn the_list_is_an_empty_array_when_no_node_is_in_a_run() {
    let _db = db::test_support::isolated();
    let response = listing();
    assert_eq!(response.status, "200 OK");
    assert_eq!(text(&response), "[]");
}

#[test]
fn the_list_reports_a_parked_gate_as_needing_approval() {
    let _db = db::test_support::isolated();
    let (node, run) = review_run();
    advance(
        run,
        "running",
        &[step("confirm_source", "blocked", 1, None)],
    );

    let response = listing();
    assert_eq!(response.status, "200 OK");
    let body = json(&response);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["node_id"], node);
    assert_eq!(body[0]["run_id"], run);
    assert_eq!(body[0]["needs_you"][0]["kind"], "approval");
    assert_eq!(body[0]["needs_you"][0]["step"]["role"], "Approval gate");
}

#[test]
fn a_failed_read_is_a_500_with_the_reason() {
    let response = list_reply(Err(crate::db::DbError::Rendered("pool exhausted".into())));
    assert_eq!(response.status, "500 Internal Server Error");
    assert_eq!(json(&response)["error"], "pool exhausted");
}

#[test]
fn approve_rejects_malformed_or_unexpected_bodies_before_touching_the_run() {
    for body in [&b"not json"[..], br#"{}"#, br#"{"node_id":"a","extra":1}"#] {
        let response = drive_approve(1, body);
        assert_eq!(response.status, "400 Bad Request", "{body:?}");
    }
}

#[test]
fn approve_answers_404_for_a_run_that_does_not_exist() {
    let _db = db::test_support::isolated();
    let response = drive_approve(999, br#"{"node_id":"confirm_source"}"#);
    assert_eq!(response.status, "404 Not Found");
}

#[test]
fn approve_refuses_a_step_that_is_not_waiting_without_logging_the_phone_out() {
    let _db = db::test_support::isolated();
    let (_, run) = review_run();
    advance(
        run,
        "running",
        &[step("confirm_source", "running", 1, None)],
    );
    let response = drive_approve(run, br#"{"node_id":"confirm_source"}"#);
    // 409, never 401/403: the phone reads those as a revoked device.
    assert_eq!(response.status, "409 Conflict");
    assert!(
        text(&response).contains("not waiting for approval"),
        "{}",
        text(&response)
    );
}

#[test]
fn approve_queues_the_approval_for_the_worker() {
    let _db = db::test_support::isolated();
    // The worker's approval queue is process-wide and keyed by run id, while
    // every test database numbers its runs from 1. Starting this database's
    // run numbering at a distinctive id keeps the test from draining another
    // test's approval.
    let unique = 7_000_000 + i64::from(std::process::id());
    db::write_conn()
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

    let response = drive_approve(run, br#"{"node_id":"confirm_source"}"#);
    assert_eq!(response.status, "200 OK", "{}", text(&response));
    assert_eq!(json(&response)["run_id"], run);
    assert_eq!(
        crate::services::circuit_worker::drain_approvals_for(run),
        vec!["confirm_source".to_string()]
    );
}

#[test]
fn outcome_refuses_a_body_that_names_a_different_run() {
    let body = r#"{"run_id":2,"node_id":"verdict","attempt":1,"expected_revision":0,"action":"recheck","reason":""}"#;
    let response = drive_outcome(1, body);
    assert_eq!(response.status, "400 Bad Request");
    assert!(text(&response).contains("different run"));
}

#[test]
fn outcome_rejects_unknown_actions_and_missing_fields() {
    for body in [
        r#"{"run_id":1,"node_id":"verdict","attempt":1,"expected_revision":0,"action":"approve_everything","reason":""}"#,
        r#"{"run_id":1,"node_id":"verdict"}"#,
    ] {
        assert_eq!(drive_outcome(1, body).status, "400 Bad Request", "{body}");
    }
}

#[test]
fn outcome_refuses_a_stale_revision() {
    let _db = db::test_support::isolated();
    let (_, run) = review_run();
    advance(run, "running", &[step("verdict", "unverified", 1, None)]);
    let body = format!(
        r#"{{"run_id":{run},"node_id":"verdict","attempt":1,"expected_revision":-5,"action":"recheck","reason":""}}"#
    );
    let response = drive_outcome(run, &body);
    assert_eq!(response.status, "409 Conflict");
    assert!(
        text(&response).contains("The run changed"),
        "{}",
        text(&response)
    );
}

#[test]
fn outcome_recheck_puts_the_unverified_step_back_to_work() {
    let _db = db::test_support::isolated();
    let (_, run) = review_run();
    advance(run, "running", &[step("verdict", "unverified", 1, None)]);
    let revision = db::circuit::evidence::attention(run).unwrap().revision;
    let body = format!(
        r#"{{"run_id":{run},"node_id":"verdict","attempt":1,"expected_revision":{revision},"action":"recheck","reason":"Checked again from the phone."}}"#
    );
    let response = drive_outcome(run, &body);
    assert_eq!(response.status, "200 OK", "{}", text(&response));
    let steps = db::list_circuit_run_steps(run).unwrap();
    assert_eq!(
        steps
            .iter()
            .find(|s| s.node_id == "verdict")
            .unwrap()
            .status,
        "running"
    );
}

#[test]
fn outcome_answers_404_for_a_run_that_does_not_exist() {
    let _db = db::test_support::isolated();
    let body = r#"{"run_id":404,"node_id":"verdict","attempt":1,"expected_revision":0,"action":"recheck","reason":""}"#;
    assert_eq!(drive_outcome(404, body).status, "404 Not Found");
}

#[test]
fn recover_is_refused_for_a_failed_review_approval() {
    let _db = db::test_support::isolated();
    let (_, run) = review_run();
    advance(
        run,
        "failed",
        &[step("verdict", "failed", 1, Some("reviewer vanished"))],
    );
    let revision = db::circuit::evidence::attention(run).unwrap().revision;
    let body = format!(
        r#"{{"run_id":{run},"node_id":"verdict","attempt":1,"expected_revision":{revision},"action":"continue","reason":""}}"#
    );
    let response = drive_recover(run, &body);
    assert_eq!(response.status, "409 Conflict", "{}", text(&response));
    assert_eq!(
        db::get_circuit_run(run).unwrap().unwrap().state,
        "failed",
        "a refused recovery must leave the run failed"
    );
}

#[test]
fn recover_retry_reopens_a_failed_publish_step() {
    let _db = db::test_support::isolated();
    let (_, run) = review_run();
    advance(
        run,
        "failed",
        &[step("publish", "failed", 1, Some("prompt delivery failed"))],
    );
    let revision = db::circuit::evidence::attention(run).unwrap().revision;
    let body = format!(
        r#"{{"run_id":{run},"node_id":"publish","attempt":1,"expected_revision":{revision},"action":"retry","reason":""}}"#
    );
    let response = drive_recover(run, &body);
    assert_eq!(response.status, "200 OK", "{}", text(&response));
    assert_eq!(db::get_circuit_run(run).unwrap().unwrap().state, "pending");
}

#[test]
fn cancel_answers_404_then_cancels_a_live_run() {
    let _db = db::test_support::isolated();
    assert_eq!(reply(404, cancel_core(404)).status, "404 Not Found");

    let (_, run) = review_run();
    let response = reply(run, cancel_core(run));
    assert_eq!(response.status, "200 OK", "{}", text(&response));
    assert_eq!(
        db::get_circuit_run(run).unwrap().unwrap().state,
        "cancelled"
    );
    assert_eq!(
        text(&listing()),
        "[]",
        "a cancelled run leaves the phone's list"
    );
}
