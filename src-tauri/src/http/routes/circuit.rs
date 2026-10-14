//! Mobile HTTP routes for Circuit Runs on Agent Nodes (issue #2314).
//!
//! `GET /api/circuit-runs` is the one read: the implementation nodes that are
//! inside a live run, or whose failed run nobody has acknowledged, with the
//! actions a person can take on each. The `POST /api/circuit-runs/{id}/...`
//! routes call the same implementation as the desktop commands, so the phone is
//! held to the eligibility the desktop run card is: Run Recovery for a review
//! approval, a pull-request lookup or a spawn is refused by
//! `recover_failed_circuit_run`, not by this file remembering to.
//!
//! The run id lives in the path. A body that names a different run is a client
//! bug and is refused rather than silently resolved in favour of either id.
//!
//! Each route is three small pieces so each can be tested where its inputs live:
//! `*_request` parses and validates the body, `*_core` does the blocking work and
//! `reply` turns the typed result into a status. The handler only joins them
//! across the `run_blocking_typed` thread hop.

use crate::commands::circuit as circuit_commands;
use crate::db;
use crate::db::circuit::evidence::CheckpointRequest;
use crate::db::circuit::node_run::{self, NodeCircuitRun};
use crate::db::circuit::step_recovery::RecoveryRequest;
use crate::http::response::Response;
use crate::http::router::ParsedRequest;
use crate::http::state;

/// Body of `POST /api/circuit-runs/{id}/approve`.
#[derive(serde::Deserialize, ts_rs::TS)]
#[ts(export, export_to = "ApproveCircuitStepRequest.ts")]
#[serde(deny_unknown_fields)]
pub struct ApproveCircuitStepRequest {
    /// The circuit node (not Agent Node) id of the parked approval gate.
    pub node_id: String,
}

/// Why an action did not happen. The status comes from the variant, never from
/// the message text: a misread 401/403 makes the phone discard its session
/// (see `routes::pr::pr_source`).
#[derive(Debug)]
enum ActError {
    NotFound(i64),
    /// The run is not in a state that accepts this action (stale revision,
    /// step already moved on, recovery not offered).
    Refused(String),
    Failed(String),
}

impl From<String> for ActError {
    fn from(message: String) -> Self {
        ActError::Failed(message)
    }
}

impl ActError {
    fn response(&self) -> Response {
        match self {
            ActError::NotFound(run_id) => {
                Response::json_error("404 Not Found", &format!("Run {run_id} no longer exists."))
            }
            ActError::Refused(message) => Response::json_error("409 Conflict", message),
            ActError::Failed(message) => Response::json_error("500 Internal Server Error", message),
        }
    }
}

fn ensure_run(run_id: i64) -> Result<(), ActError> {
    match db::get_circuit_run(run_id) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(ActError::NotFound(run_id)),
        Err(error) => Err(ActError::Failed(error.to_string())),
    }
}

fn reply(run_id: i64, result: Result<(), ActError>) -> Response {
    match result {
        Ok(()) => Response::json(
            "200 OK",
            serde_json::json!({ "ok": true, "run_id": run_id }).to_string(),
        ),
        Err(error) => error.response(),
    }
}

fn parse_body<T: serde::de::DeserializeOwned>(req: &ParsedRequest) -> Result<T, Response> {
    serde_json::from_slice(&req.body)
        .map_err(|error| Response::json_error("400 Bad Request", &format!("Invalid JSON: {error}")))
}

fn require_same_run(path_run: i64, body_run: i64) -> Result<(), Response> {
    if path_run == body_run {
        return Ok(());
    }
    Err(Response::json_error(
        "400 Bad Request",
        "The request body names a different run than the URL.",
    ))
}

fn list_reply(runs: Result<Vec<NodeCircuitRun>, String>) -> Response {
    match runs.and_then(|runs| serde_json::to_string(&runs).map_err(|error| error.to_string())) {
        Ok(body) => Response::json("200 OK", body),
        Err(error) => Response::json_error("500 Internal Server Error", &error),
    }
}

/// `GET /api/circuit-runs` — see [`NodeCircuitRun`].
pub async fn list(_req: &ParsedRequest) -> Response {
    list_reply(
        crate::commands::run_blocking(
            "http_list_node_circuit_runs",
            node_run::list_node_circuit_runs,
        )
        .await,
    )
}

fn approve_request(req: &ParsedRequest) -> Result<ApproveCircuitStepRequest, Response> {
    parse_body(req)
}

fn approve_core(run_id: i64, node_id: String) -> Result<(), ActError> {
    ensure_run(run_id)?;
    circuit_commands::approve_circuit_step(run_id, node_id).map_err(ActError::Refused)
}

/// `POST /api/circuit-runs/{id}/approve` — approve the parked gate.
pub async fn approve(req: &ParsedRequest) -> Response {
    let run_id = req.id0();
    let body = match approve_request(req) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let result = crate::blocking::run_blocking_typed("http_approve_circuit_step", move || {
        approve_core(run_id, body.node_id)
    })
    .await;
    reply(run_id, result)
}

fn outcome_request(req: &ParsedRequest) -> Result<CheckpointRequest, Response> {
    let request: CheckpointRequest = parse_body(req)?;
    require_same_run(req.id0(), request.run_id)?;
    Ok(request)
}

fn outcome_core(request: &CheckpointRequest) -> Result<(), ActError> {
    ensure_run(request.run_id)?;
    circuit_commands::record_circuit_outcome_now(request).map_err(ActError::Refused)
}

/// `POST /api/circuit-runs/{id}/outcome` — recheck an Unverified Checkpoint or
/// record an Operator-recorded Outcome. The body is the desktop's
/// [`CheckpointRequest`].
pub async fn outcome(req: &ParsedRequest) -> Response {
    let request = match outcome_request(req) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let run_id = request.run_id;
    let result = crate::blocking::run_blocking_typed("http_record_circuit_outcome", move || {
        outcome_core(&request)
    })
    .await;
    if result.is_ok() {
        circuit_commands::emit_run_updated(state::app_handle(), run_id, "running");
    }
    reply(run_id, result)
}

fn recover_request(req: &ParsedRequest) -> Result<RecoveryRequest, Response> {
    let request: RecoveryRequest = parse_body(req)?;
    require_same_run(req.id0(), request.run_id)?;
    Ok(request)
}

fn recover_core(request: &RecoveryRequest) -> Result<(), ActError> {
    ensure_run(request.run_id)?;
    circuit_commands::recover_failed_circuit_run_now(request).map_err(ActError::Refused)
}

/// `POST /api/circuit-runs/{id}/recover` — Run Recovery on a failed run. The
/// body is the desktop's [`RecoveryRequest`].
pub async fn recover(req: &ParsedRequest) -> Response {
    let request = match recover_request(req) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let run_id = request.run_id;
    let result = crate::blocking::run_blocking_typed("http_recover_circuit_run", move || {
        recover_core(&request)
    })
    .await;
    if result.is_ok() {
        circuit_commands::emit_run_updated(state::app_handle(), run_id, "pending");
    }
    reply(run_id, result)
}

fn cancel_core(run_id: i64) -> Result<(), ActError> {
    ensure_run(run_id)?;
    circuit_commands::cancel_circuit_run_now(state::app_handle(), run_id).map_err(ActError::Failed)
}

/// `POST /api/circuit-runs/{id}/cancel` — cancel the run and retire its helper
/// agents, exactly as the desktop's Cancel does.
pub async fn cancel(req: &ParsedRequest) -> Response {
    let run_id = req.id0();
    let result =
        crate::blocking::run_blocking_typed("http_cancel_circuit_run", move || cancel_core(run_id))
            .await;
    reply(run_id, result)
}

#[cfg(test)]
mod tests {
    //! Every piece of a route is driven on the test's own thread against a
    //! private database (`db::test_support::isolated`), with runs minted by the
    //! production writers, and what a `200` claims is checked in the ledger
    //! afterwards. The only step not exercised here is the `run_blocking_typed`
    //! hop between the pieces: an isolated test database is installed per thread,
    //! so it cannot follow the work onto the blocking pool.
    use super::*;
    use crate::db::circuit::CircuitStepOp;
    use crate::models::{EnvType, SessionStatus};

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
        let node = db::create_agent_node(
            mesh.id,
            "Implementation",
            &mesh.path,
            "work",
            EnvType::Windows,
            "claude",
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
        )
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
        let response = list_reply(Err("pool exhausted".into()));
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
}
