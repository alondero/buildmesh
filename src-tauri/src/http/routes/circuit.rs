//! Mobile HTTP routes for Circuit Runs on Agent Nodes (issue #2314).
//!
//! `GET /api/circuit-agent-ownerships` shares the desktop's pairing ledger.
//! `GET /api/circuit-runs` lists the implementation nodes that are
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

#[derive(Debug, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "NodeReviewOption.ts")]
pub struct NodeReviewOption {
    #[ts(as = "Option<i32>")]
    pub circuit_id: Option<i64>,
    pub name: String,
}

#[derive(serde::Deserialize, ts_rs::TS)]
#[ts(export, export_to = "StartNodeReviewRequest.ts")]
#[serde(deny_unknown_fields)]
pub struct StartNodeReviewRequest {
    #[ts(as = "Option<i32>")]
    pub circuit_id: Option<i64>,
    pub max_rounds: i32,
    /// Spawn Option or saved Launch Configuration for the built-in review only.
    #[serde(default)]
    #[ts(optional)]
    pub reviewer_provider: Option<String>,
}

fn review_options_core(node_id: i64) -> Result<Vec<NodeReviewOption>, ActError> {
    let node = db::get_agent_node_by_id(node_id).map_err(|error| match error {
        rusqlite::Error::QueryReturnedNoRows => ActError::NodeNotFound(node_id),
        other => db_failure(other),
    })?;
    let circuits = db::list_autopilot_circuits(node.mesh_id).map_err(db_failure)?;
    let mut options = vec![NodeReviewOption {
        circuit_id: None,
        name: "Built-in Review Blueprint".into(),
    }];
    for circuit in circuits {
        if circuit.is_preset {
            continue;
        }
        let Ok(graph) = crate::circuit::model::CircuitGraph::from_json(&circuit.graph_json) else {
            continue;
        };
        use crate::circuit::model::CircuitNodeKind;
        if graph.validate().is_ok()
            && !graph.roots().is_empty()
            && graph
                .roots()
                .iter()
                .all(|n| matches!(n.kind, CircuitNodeKind::Manual))
            && graph
                .nodes
                .iter()
                .any(|n| matches!(n.kind, CircuitNodeKind::ReviewVerdict { .. }))
        {
            options.push(NodeReviewOption {
                circuit_id: Some(circuit.id),
                name: circuit.name,
            });
        }
    }
    Ok(options)
}

pub async fn review_options(req: &ParsedRequest) -> Response {
    let node_id = req.id0();
    match crate::blocking::run_blocking_typed("http_node_review_options", move || {
        review_options_core(node_id)
    })
    .await
    {
        Ok(options) => Response::json(
            "200 OK",
            serde_json::to_string(&options).expect("serializable options"),
        ),
        Err(error) => error.response(),
    }
}

fn start_review_core(node_id: i64, request: StartNodeReviewRequest) -> Result<i64, ActError> {
    if !review_options_core(node_id)?
        .iter()
        .any(|o| o.circuit_id == request.circuit_id)
    {
        return Err(ActError::Refused(
            "Select a Review Circuit from this agent's Mesh.".into(),
        ));
    }
    circuit_commands::trigger_circuit_from_node_now(
        state::app_handle(),
        node_id,
        request.circuit_id,
        request.max_rounds,
        request.reviewer_provider,
    )
    .map_err(ActError::Refused)
}

pub async fn start_review(req: &ParsedRequest) -> Response {
    let request = match parse_body::<StartNodeReviewRequest>(req) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let node_id = req.id0();
    match crate::blocking::run_blocking_typed("http_start_node_review", move || {
        start_review_core(node_id, request)
    })
    .await
    {
        Ok(run_id) => reply(run_id, Ok(())),
        Err(error) => error.response(),
    }
}

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
    NodeNotFound(i64),
    NotFound(i64),
    /// The run is not in a state that accepts this action (stale revision,
    /// step already moved on, recovery not offered).
    Refused(String),
    /// The database could not answer, because it was momentarily busy
    /// (issue #1533). Transient, so the client retries rather than treating
    /// the request as broken. The error is kept, not flattened to text, so
    /// the shared status policy can decide between `503` and `500`.
    Unavailable(crate::db::DbError),
    Failed(String),
}

/// A failed database read is either "the row is not there" or "the database
/// could not answer". Only the second is transient: a busy reader pool earns
/// a `503`, not the `500` a genuinely broken query gets.
fn db_failure(error: rusqlite::Error) -> ActError {
    action_failure(crate::db::DbError::from(error))
}

/// The same rule for a failure that already carries the database layer's own
/// error type.
fn action_failure(error: crate::db::DbError) -> ActError {
    if error.is_busy() {
        ActError::Unavailable(error)
    } else {
        ActError::Failed(error.to_string())
    }
}

impl From<String> for ActError {
    fn from(message: String) -> Self {
        ActError::Failed(message)
    }
}

impl ActError {
    fn response(&self) -> Response {
        match self {
            ActError::NodeNotFound(node_id) => Response::json_error(
                "404 Not Found",
                &format!("Agent node {node_id} no longer exists."),
            ),
            ActError::NotFound(run_id) => {
                Response::json_error("404 Not Found", &format!("Run {run_id} no longer exists."))
            }
            ActError::Refused(message) => Response::json_error("409 Conflict", message),
            ActError::Unavailable(error) => crate::http::read_status::db_error_response(error),
            ActError::Failed(message) => Response::json_error("500 Internal Server Error", message),
        }
    }
}

fn ensure_run(run_id: i64) -> Result<(), ActError> {
    match db::get_circuit_run(run_id) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(ActError::NotFound(run_id)),
        Err(error) => Err(db_failure(error)),
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

fn list_reply(runs: Result<Vec<NodeCircuitRun>, crate::db::DbError>) -> Response {
    match runs {
        Ok(runs) => match serde_json::to_string(&runs) {
            Ok(body) => Response::json("200 OK", body),
            Err(error) => Response::json_error("500 Internal Server Error", &error.to_string()),
        },
        // A busy reader pool is transient (issue #1533): the client retries.
        Err(error) => crate::http::read_status::db_error_response(&error),
    }
}

/// `GET /api/circuit-runs` — see [`NodeCircuitRun`].
pub async fn list(_req: &ParsedRequest) -> Response {
    list_reply(
        crate::commands::run_blocking_typed::<_, crate::db::DbError, _>(
            "http_list_node_circuit_runs",
            node_run::list_node_circuit_runs,
        )
        .await,
    )
}

fn ownerships_reply(
    rows: Result<Vec<circuit_commands::CircuitAgentOwnership>, crate::db::DbError>,
) -> Response {
    match rows {
        Ok(rows) => match serde_json::to_string(&rows) {
            Ok(body) => Response::json("200 OK", body),
            Err(error) => Response::json_error("500 Internal Server Error", &error.to_string()),
        },
        // A busy reader pool is transient (issue #1533): the client retries.
        Err(error) => crate::http::read_status::db_error_response(&error),
    }
}

pub async fn ownerships(_req: &ParsedRequest) -> Response {
    ownerships_reply(
        crate::commands::run_blocking_typed::<_, crate::db::DbError, _>(
            "http_circuit_agent_ownerships",
            circuit_commands::list_circuit_agent_ownerships_now,
        )
        .await,
    )
}

fn approve_request(req: &ParsedRequest) -> Result<ApproveCircuitStepRequest, Response> {
    parse_body(req)
}

fn approve_core(run_id: i64, node_id: String) -> Result<(), ActError> {
    ensure_run(run_id)?;
    // A refusal stays a `409`; a busy database is transient and earns a `503`
    // (issue #1533). The typed error is what tells them apart.
    circuit_commands::approve_circuit_step_now(run_id, node_id).map_err(|error| {
        if error.is_busy() {
            ActError::Unavailable(error)
        } else {
            ActError::Refused(error.to_string())
        }
    })
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
    // A refusal stays a `409`; a busy database is transient and earns a `503`
    // (issue #1533).
    circuit_commands::record_circuit_outcome_now(request).map_err(|error| {
        if error.is_busy() {
            ActError::Unavailable(error)
        } else {
            ActError::Refused(error.to_string())
        }
    })
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
    // A refusal stays a `409`; a busy database is transient and earns a `503`
    // (issue #1533).
    circuit_commands::recover_failed_circuit_run_now(request).map_err(|error| {
        if error.is_busy() {
            ActError::Unavailable(error)
        } else {
            ActError::Refused(error.to_string())
        }
    })
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
    // A cancel failure stays a `500`; a busy database is transient and earns
    // a `503` (issue #1533).
    circuit_commands::cancel_circuit_run_now(state::app_handle(), run_id).map_err(|error| {
        if error.is_busy() {
            ActError::Unavailable(error)
        } else {
            ActError::Failed(error.to_string())
        }
    })
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
#[path = "circuit_tests.rs"]
mod tests;
