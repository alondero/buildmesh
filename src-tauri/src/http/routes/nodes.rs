//! `GET /api/nodes` and `POST /api/nodes/create`, plus `POST /api/nodes/{id}/input`
//! (issue #1377) for the triage deck's one-tap Approve/Reject chips.

use crate::agent::process::{ProcessRegistryApi, PROCESS_REGISTRY};
use tauri::Emitter;

use crate::db;
use crate::http::read_status::{read_response, serialized, serialized_or_500};
use crate::http::response::Response;
use crate::http::router::ParsedRequest;
use crate::http::state;
use crate::read_outcome::{ReadOutcome, Unavailable};

pub async fn list(_req: &ParsedRequest) -> Response {
    match list_json().await {
        Ok(response) => response,
        // A list read is never "absent" — this message only names the
        // resource the list is *of*, for the (unreachable) absence branch.
        Err(failure) => read_response(&failure, "No nodes found"),
    }
}

/// Mobile close preserves the checkout. Like desktop, stop the process before
/// deleting its node; `delete(_, false)` alone does not stop a live process.
pub async fn close(req: &ParsedRequest) -> Response {
    let node_id = req.id0();
    if node_id <= 0 {
        return Response::json_error("400 Bad Request", "Invalid node id");
    }
    match crate::commands::run_blocking("http_close_node", move || {
        crate::agent::process::kill_agent_blocking(node_id)?;
        crate::services::agent_node::delete(node_id, false).map_err(|error| error.to_string())
    })
    .await
    {
        Ok(()) => Response::empty("204 No Content"),
        Err(error) => Response::json_error("500 Internal Server Error", &error),
    }
}

#[cfg(test)]
#[path = "nodes_close_tests.rs"]
mod close_tests;

/// The authoritative agent-node inventory as a JSON body.
///
/// Issue #2330: this ended in `Err(_) => "[]"`, and a failed query was
/// indistinguishable from an empty `agent_nodes` table — so a phone polling a
/// populated Work list during a storage fault was handed `200 []` and showed an
/// empty workspace. `200 []` now means "a successful query found zero rows",
/// and a failed read is a `503` the client can retry rather than a success it
/// must believe.
pub async fn list_json() -> Result<Response, Unavailable> {
    let nodes: Vec<crate::models::AgentNode> =
        crate::commands::run_blocking_typed::<Vec<crate::models::AgentNode>, Unavailable, _>(
            "http_list_nodes",
            || ReadOutcome::ok_or_unavailable(db::list_agent_nodes()).into_result(),
        )
        .await?;

    serialized("200 OK", &nodes).map_err(Unavailable::Read)
}

#[derive(serde::Deserialize, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CreateNodeRequest.ts")]
pub struct CreateNodeRequest {
    #[ts(as = "i32")]
    pub mesh_id: i64,
    #[serde(default)]
    #[ts(optional)]
    pub prompt: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration_id: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub rows: Option<u16>,
    #[serde(default)]
    #[ts(optional)]
    pub cols: Option<u16>,
    /// Client-generated identity for one submitted idea (issue #2331).
    ///
    /// Stable across retries of the *same* submission: a phone that lost the
    /// response reuses it and gets the same node back instead of a duplicate.
    /// Omitted by older clients, which keep the pre-#2331 behaviour (no
    /// ledger row, no replay protection) — the two shapes coexist so a
    /// rollout never strands an already-installed app.
    #[serde(default)]
    #[ts(optional)]
    pub operation_key: Option<String>,
}

/// Longest accepted client operation key. Bounds the ledger row and rejects
/// an obviously-not-a-uuid body before it reaches the transaction.
const OPERATION_KEY_MAX_BYTES: usize = 128;

/// Body for `POST /api/nodes/{id}/input`: the keystrokes the phone typed
/// into an agent's terminal.
///
/// Crate-visible so the canonical mobile-route fixture (issue #2318) can be
/// built from the same type the route parses.
#[derive(serde::Deserialize, serde::Serialize)]
pub(crate) struct InputRequest {
    pub(crate) seq: String,
}

/// Pre-commit failures raised while preparing a create.
///
/// Exists so the saved-configuration error keeps its `Invalid` (400) vs
/// `Storage` (500) distinction across the blocking-pool hop — `run_blocking`
/// flattens to `String` and would have blamed the client for a disk failure.
#[derive(Debug)]
enum CreateTransportError {
    Configuration(crate::preferences::spawn_configurations::SpawnConfigurationError),
    /// The blocking task itself failed.
    Offload(String),
}

impl From<String> for CreateTransportError {
    fn from(message: String) -> Self {
        Self::Offload(message)
    }
}

/// The mesh's default Spawn Option, with a missing mesh as the caller's 400
/// (the runner's `MeshNotFound` answers the same) and a failed read as ours.
///
/// A busy reader pool is transient (issue #1533): the answer is `503`, so the
/// client retries, not `500` — which reads as "this build is broken".
fn mesh_default_provider(mesh_id: i64) -> Result<String, (&'static str, String)> {
    crate::commands::mesh::get_default_provider_blocking(mesh_id).map_err(|error| match error {
        rusqlite::Error::QueryReturnedNoRows => {
            ("400 Bad Request", format!("Mesh not found: {mesh_id}"))
        }
        other if crate::db::error::is_busy_error(&other) => (
            "503 Service Unavailable",
            format!("Failed to read the mesh default: {other}"),
        ),
        other => (
            "500 Internal Server Error",
            format!("Failed to read the mesh default: {other}"),
        ),
    })
}

/// Resolve the saved configuration, then hand the whole create-and-start
/// sequence to the application service (issue #2331).
///
/// This function does transport concerns only — parse, resolve, invoke, map.
/// It must not decide whether a failure means "committed" or "nothing
/// committed"; that judgement belongs to `services::create_operation`.
async fn run_create(
    request: CreateNodeRequest,
    start: &dyn crate::services::create_operation::AgentStart,
) -> Response {
    let mesh_id = request.mesh_id;
    let prompt = match request.prompt.clone() {
        Some(text) if text.trim().is_empty() || text.len() > 16_000 => {
            return Response::json_error(
                "400 Bad Request",
                "Idea must contain 1 to 16000 bytes of text",
            );
        }
        other => other,
    };
    let provider = match request.provider.as_deref().filter(|p| !p.trim().is_empty()) {
        Some(provider) => provider.to_string(),
        None => {
            // Pre-#2331 contract, preserved: a saved configuration id doubles
            // as the provider lookup key, so a request carrying only
            // `configuration_id` still resolves. With neither, the phone asked
            // for the mesh default, resolved by the desktop's own chain.
            match request
                .configuration_id
                .as_deref()
                .filter(|id| !id.trim().is_empty())
            {
                Some(id) => id.to_string(),
                None => match crate::commands::run_blocking("http_create_node_default", move || {
                    Ok(mesh_default_provider(mesh_id))
                })
                .await
                {
                    Ok(Ok(provider)) => provider,
                    Ok(Err((status, message))) => return Response::json_error(status, &message),
                    Err(message) => {
                        return Response::json_error("500 Internal Server Error", &message)
                    }
                },
            }
        }
    };

    let operation_key = match request.operation_key.as_deref() {
        Some(key) if key.trim().is_empty() => {
            return Response::json_error(
                "400 Bad Request",
                "operation_key must not be empty when present",
            );
        }
        Some(key) if key.len() > OPERATION_KEY_MAX_BYTES => {
            return Response::json_error(
                "400 Bad Request",
                &format!("operation_key must be at most {OPERATION_KEY_MAX_BYTES} bytes"),
            );
        }
        other => other.map(str::to_string),
    };

    // Saved-configuration resolution reads preferences off the async worker,
    // so it runs on the blocking pool with the rest of the create work.
    let configuration_provider = provider.clone();
    let configuration_id = request.configuration_id.clone();
    let configuration = match crate::commands::run_blocking_typed::<_, CreateTransportError, _>(
        "http_create_node",
        move || {
            crate::preferences::spawn_configurations::resolve_saved(
                &configuration_provider,
                configuration_id
                    .as_deref()
                    .filter(|id| !id.trim().is_empty()),
            )
            .map_err(CreateTransportError::Configuration)
        },
    )
    .await
    {
        Ok(configuration) => configuration,
        // A stored recipe that no longer validates is the caller's 400.
        Err(CreateTransportError::Configuration(
            crate::preferences::spawn_configurations::SpawnConfigurationError::Invalid(message),
        )) => return Response::json_error("400 Bad Request", &message),
        // A preferences read that failed is ours, not the caller's.
        Err(CreateTransportError::Configuration(error)) => {
            return Response::json_error(
                "500 Internal Server Error",
                &format!("Failed to resolve spawn configuration: {error}"),
            )
        }
        Err(CreateTransportError::Offload(message)) => {
            return Response::json_error(
                "500 Internal Server Error",
                &format!("Failed to create node: {message}"),
            )
        }
    };

    let outcome = crate::services::create_operation::create_and_start(
        crate::services::create_operation::CreateAndStartRequest {
            mesh_id,
            provider,
            configuration_id: request.configuration_id.clone(),
            configuration,
            prompt,
            rows: request.rows.unwrap_or(24),
            cols: request.cols.unwrap_or(80),
            operation_key,
        },
        start,
        &crate::services::create_operation::BlockingPool,
    )
    .await;

    match outcome {
        Ok(outcome) => {
            // A committed node must be discoverable by other clients even when
            // its start failed (#2331) — the old handler emitted this event
            // only after a *complete* success, so a partially-created node was
            // invisible until a refresh.
            if let Some(app) = state::app_handle() {
                let _ = app.emit(
                    "node-created",
                    crate::commands::agent::NodeCreatedPayload {
                        id: outcome.node().id,
                    },
                );
            }
            outcome_response(&outcome)
        }
        Err(rejection) => map_create_rejection(rejection),
    }
}

/// Map a rejection (nothing committed) onto its status.
///
/// A same-key/different-content request is the one 409 here: it is distinct
/// from every 4xx the shared create runner can raise, because the key — not the
/// request — is what the client must change.
fn map_create_rejection(rejection: crate::services::create_operation::CreateRejection) -> Response {
    use crate::services::create_operation::CreateRejection;
    let status = match &rejection {
        CreateRejection::KeyConflict { .. } => "409 Conflict",
        // Issue #1533: a busy reader pool is transient — the client retries.
        CreateRejection::Create(crate::services::agent_node::AgentNodeError::Db(error))
            if crate::db::error::is_busy_error(error) =>
        {
            "503 Service Unavailable"
        }
        other if other.is_client_error() => "400 Bad Request",
        _ => "500 Internal Server Error",
    };
    Response::json_error(status, &rejection.message())
}

/// Serialize a committed outcome.
///
/// The status code carries the same signal as the `outcome` discriminant but
/// is *not* the contract: 202 and 207 are never produced for a running agent,
/// so a client that only reads the status still cannot mistake a
/// committed-but-unstarted node for a running one.
///
/// Issue #2330: a body that cannot be encoded used to become `"{}"` under the
/// outcome's own success status — a 200/202/207 carrying no node at all. There
/// is no honest body to substitute, so the failure is reported as a 500.
fn outcome_response(outcome: &crate::services::create_operation::CreateNodeOutcome) -> Response {
    use crate::services::create_operation::CreateNodeOutcome;

    let status = match outcome {
        CreateNodeOutcome::Active { .. } => "200 OK",
        // Accepted: the node exists and will be startable — the work is
        // committed, the process is not running yet.
        CreateNodeOutcome::StartPending { .. } => "202 Accepted",
        // Multi-Status is already this repo's "the row landed but a step
        // failed" signal (see the import-and-resume route).
        CreateNodeOutcome::StartFailed { .. } => "207 Multi-Status",
    };
    serialized_or_500(status, outcome)
}

pub async fn create(req: &ParsedRequest) -> Response {
    let parsed: CreateNodeRequest = match serde_json::from_slice(&req.body) {
        Ok(r) => r,
        Err(e) => {
            return Response::json_error("400 Bad Request", &format!("Invalid JSON: {}", e));
        }
    };

    // The App handle doubles as the "can we start right now" signal. Without
    // it the service still commits the node and reports `start_pending`, which
    // is the whole point of #2331 — the old handler answered a bare 503 here,
    // after the row was already committed.
    let Some(app) = state::app_handle() else {
        return run_create(parsed, &crate::services::create_operation::UnreadyStart).await;
    };

    run_create(
        parsed,
        &crate::services::create_operation::TauriStart::new(app),
    )
    .await
}

/// Body-shape cap for `/api/nodes/{id}/input`. The triage deck ships 2 bytes
/// (`"y\r"` / `"n\r"`); we allow up to 1 KiB so future call-sites (a
/// desktop-style "send a whole prompt" shortcut) keep working without a
/// schema bump. Anything larger is rejected with `413` before any DB or PTY
/// work runs.
pub(crate) const INPUT_BODY_MAX_BYTES: usize = 1024;

/// `POST /api/nodes/{id}/input` — fire a raw keystroke sequence into a
/// node's PTY (issue #1377, triage deck Approve/Reject chips).
///
/// This replaces the previous "open the full terminal WS, send two bytes,
/// close" RPC pattern. The terminal WS opens a per-connection broadcast
/// channel, allocates a snapshot RPC against the desktop, and spawns a write
/// task — every tap on Approve just to push `"y\r"` was executing all of
/// that heavyweight machinery, AND racing the server's read loop with the
/// client's immediate close (a successful `ws.send()` followed by an
/// immediate `ws.close()` could land before the server's read loop entered,
/// silently dropping the keystroke while the user saw a green "Sent ✓").
///
/// The HTTP path has none of those problems:
///   * one round-trip, no snapshot, no broadcast, no spawned task
///   * the 200 OK body is the delivery proof — the bytes are in the PTY by
///     the time we write the response
///   * `forward_mobile_input` is reused so the attention autoclear (a CR/LF
///     in the payload) runs through the same code path the WS does
///
/// That "200 OK is the delivery proof" claim stopped being true when a full
/// PTY input queue dropped the bytes and still reported success, so the body
/// now carries the typed disposition and a refusal is a distinct status
/// (issue #1530). A backpressured write is *transient*: the same bytes can be
/// re-sent, and `Retry-After` says so.
pub async fn post_input(req: &ParsedRequest) -> Response {
    let node_id = req.id0();

    let parsed: InputRequest = match serde_json::from_slice(&req.body) {
        Ok(r) => r,
        Err(e) => {
            return Response::json_error("400 Bad Request", &format!("Invalid JSON: {}", e));
        }
    };
    if parsed.seq.is_empty() {
        return Response::json_error("400 Bad Request", "seq must be non-empty");
    }

    // Verify the node exists in the DB before touching the PTY. A 404 here
    // matches the contract `get_agent_node_by_id` already enforces — without
    // it, a stale tap on a since-deleted node would 503 (PTY not running)
    // and the user would have to guess whether the agent died or the request
    // was malformed. Pin the failure shape.
    //
    // Issue #2330: this was `.is_ok()`, so *every* failure answered 404 — a
    // dropped or broken query read as "the node is gone" and would invite the
    // client to reconcile against a deletion that never happened. Only a
    // successful query matching no row is absence.
    let node: Result<crate::models::AgentNode, Unavailable> =
        crate::commands::run_blocking_typed::<_, Unavailable, _>(
            "http_input_node_lookup",
            move || {
                ReadOutcome::absent_or_unavailable(db::get_agent_node_by_id(node_id)).into_result()
            },
        )
        .await;
    match node {
        Ok(_) => {}
        Err(failure) => return read_response(&failure, "Node not found"),
    }

    // Write the bytes to the PTY. `write_mobile_input` runs the attention
    // autoclear side-effect when the payload contains CR/LF — same code path
    // the WS uses, so a `"y\r"` tap flips awaiting_input → running exactly
    // as a typed Enter would.
    //
    // `run_blocking`'s `F: FnOnce() -> Result<T, String>` bound forces the
    // closure to return `Result<(), String>` for some inferred T; the closure
    // body already returns one, so T is inferred as `()`. The full result is
    // a single `Result<(), String>` whose `Err` carries either the PTY-write
    // failure or (rarely) the offload-task failure — both surface as 5xx.
    let seq = parsed.seq.clone();
    let write_result = crate::commands::run_blocking(
        "http_input_write_bytes",
        move || -> Result<crate::agent::process::InputOutcome, String> {
            let registry: &dyn ProcessRegistryApi = &**PROCESS_REGISTRY;
            crate::http::ws::write_mobile_input(registry, node_id, &seq)
        },
    )
    .await;

    input_write_response(write_result)
}

/// Map a PTY input write to its HTTP response.
///
/// Split out from the handler so the status/body contract is testable without
/// a DB row, a live registry, or a socket — the handler's remaining job is
/// validation and the node lookup, neither of which is where the risk lives
/// (issue #1530).
///
/// The contract: only `Accepted` is a success. A `Backpressured` write was
/// never queued, so re-sending the identical `seq` is safe and the response
/// says so with `Retry-After`; a `Closed`/hard failure is terminal. Pre-fix
/// both of the first two answered `200 {"ok":true}`, which is how a dropped
/// prompt came back looking delivered.
fn input_write_response(write: Result<crate::agent::process::InputOutcome, String>) -> Response {
    use crate::agent::process::InputDisposition;
    match write {
        Ok(outcome) if outcome.disposition == InputDisposition::Accepted => {
            // `accepted` is explicit rather than a bare `{"ok":true}` so a
            // client can assert on the disposition it is relying on instead of
            // inferring delivery from a status code (issue #1530).
            Response::json("200 OK", r#"{"ok":true,"disposition":"accepted"}"#)
        }
        // Issue #1530: the queue refused the bytes. They were never queued, so
        // the tap is safe to repeat — 503 plus `Retry-After`, distinct from the
        // `{"ok":true}` the pre-fix route returned for a dropped write.
        Ok(outcome) if outcome.disposition == InputDisposition::Backpressured => Response::json(
            "503 Service Unavailable",
            r#"{"error":"input_backpressured: the agent is not reading its input; retry the same seq"}"#,
        )
        .with_header("Retry-After", "1"),
        // A closed queue is terminal — the writer thread is gone, so the same
        // seq can never be delivered however long the client waits. It gets the
        // same 503 but *without* `Retry-After`, so a client following that hint
        // does not spin on a dead process.
        Ok(_) => Response::json(
            "503 Service Unavailable",
            r#"{"error":"input_closed: the agent is no longer running; this tap cannot be delivered"}"#,
        ),
        Err(e) => {
            // PTY not running (process killed, spawn failed) or the offload
            // task itself failed — surface as 503 so the SPA knows the
            // keystroke never reached the agent. The WS path logs and
            // continues; a one-shot HTTP tap can't recover by retrying the
            // same socket.
            Response::json_error("503 Service Unavailable", &format!("PTY not running: {}", e))
        }
    }
}

#[cfg(test)]
mod tests {
    //! Issue #1377 — `POST /api/nodes/{id}/input` is the new triage-deck
    //! input endpoint. The handler must:
    //!   * reject malformed JSON with `400 Bad Request`
    //!   * reject an empty `seq` with `400 Bad Request`
    //!   * return `404` when the node id isn't in the DB
    //!
    //! Body-size 413 is the router's `max_body` policy (see router tests);
    //! these tests drive `post_input` through `ParsedRequest` so they do
    //! not open sockets. The PTY-down 503 path lives behind a real
    //! `ProcessRegistry` and is covered by `ws::tests::
    //! forward_mobile_input_handles_registry_error`.
    //!
    //! Issue #1530 adds the disposition → status contract, which is pinned
    //! through `input_write_response` — the pure mapping the handler delegates
    //! to — because that is where a dropped write used to masquerade as a 200.
    use super::*;
    use crate::http::router::ParsedRequest;

    fn outcome(
        disposition: crate::agent::process::InputDisposition,
    ) -> crate::agent::process::InputOutcome {
        crate::agent::process::InputOutcome {
            disposition,
            activity: Default::default(),
        }
    }

    /// The encoded wire bytes, so a header assertion proves the value actually
    /// reaches the client rather than sitting in an unasserted struct field.
    fn wire(resp: &Response) -> String {
        String::from_utf8_lossy(&resp.encode()).to_string()
    }

    #[test]
    fn an_accepted_input_write_is_a_200_carrying_its_disposition() {
        let resp = input_write_response(Ok(outcome(
            crate::agent::process::InputDisposition::Accepted,
        )));
        assert_eq!(resp.status_code(), 200);
        let text = String::from_utf8_lossy(resp.body());
        assert!(text.contains("\"disposition\":\"accepted\""), "got: {text}");
        assert!(
            !wire(&resp).contains("Retry-After"),
            "a delivered tap needs no retry hint"
        );
    }

    /// Issue #1530's mobile half: a full PTY input queue must not answer
    /// `{"ok":true}`. The pre-fix route returned exactly that for a dropped
    /// write, so the phone rendered a tap as delivered while the agent never
    /// saw it.
    #[test]
    fn a_backpressured_input_write_is_a_retryable_503_not_a_success() {
        let resp = input_write_response(Ok(outcome(
            crate::agent::process::InputDisposition::Backpressured,
        )));
        assert_eq!(resp.status_code(), 503, "a refused write must not be a 200");
        let text = String::from_utf8_lossy(resp.body());
        assert!(
            text.contains("input_backpressured"),
            "the client needs to know why: {text}"
        );
        assert!(
            !text.contains("\"ok\":true"),
            "a refusal must never claim delivery: {text}"
        );
        assert!(
            wire(&resp).contains("Retry-After: 1"),
            "the bytes were never queued, so repeating the same seq is safe: {}",
            wire(&resp)
        );
    }

    /// A closed queue is terminal — the writer thread is gone — so the response
    /// must not invite a retry the agent cannot honour.
    #[test]
    fn a_closed_input_write_is_a_503_without_a_retry_hint() {
        let resp =
            input_write_response(Ok(outcome(crate::agent::process::InputDisposition::Closed)));
        assert_eq!(resp.status_code(), 503);
        let text = String::from_utf8_lossy(resp.body());
        assert!(text.contains("input_closed"), "got: {text}");
        assert!(
            !wire(&resp).contains("Retry-After"),
            "a dead PTY is not a retryable stall: {}",
            wire(&resp)
        );
    }

    #[test]
    fn a_hard_write_failure_is_a_503_naming_the_pty() {
        let resp = input_write_response(Err("Agent not running".to_string()));
        assert_eq!(resp.status_code(), 503);
        let text = String::from_utf8_lossy(resp.body());
        assert!(text.contains("PTY not running"), "got: {text}");
    }

    #[test]
    fn configuration_resolution_keeps_validation_and_storage_errors_typed() {
        let invalid = crate::preferences::spawn_configurations::SpawnConfigurationError::Invalid(
            "Saved configuration no longer exists".into(),
        );
        let storage = crate::preferences::spawn_configurations::SpawnConfigurationError::Storage(
            "failed to read preferences".into(),
        );
        assert!(matches!(
            invalid,
            crate::preferences::spawn_configurations::SpawnConfigurationError::Invalid(_)
        ));
        assert!(matches!(
            storage,
            crate::preferences::spawn_configurations::SpawnConfigurationError::Storage(_)
        ));
    }

    fn req(body: &[u8], node_id: i64) -> ParsedRequest {
        ParsedRequest::test_post("/api/nodes/0/input", body).with_ids(Some(node_id), None)
    }

    #[tokio::test]
    async fn http_and_desktop_reject_a_prompt_the_harness_cannot_take() {
        // Issue #2331: the route now delegates to `services::create_operation`,
        // and the service must not turn a client error into a *committed*
        // outcome. The desktop half still runs on this thread.
        //
        // The HTTP half cannot be driven from here: both the database and the
        // preferences app-data dir are `thread_local!` under `cfg(test)`, and
        // the route reaches them from `spawn_blocking`, where neither install
        // is visible. The equivalent HTTP assertion therefore lives in
        // `services::create_operation::tests`, which injects `InlineWork` and
        // runs the same code path on this thread.
        let _db = crate::db::test_support::shared_global_db();
        let root = tempfile::tempdir().unwrap();
        crate::preferences::init_for_tests(root.path().to_path_buf());
        let mesh_id = crate::db::create_mesh(
            "prompt-gate",
            &format!("/tmp/buildmesh_prompt_gate_{}", std::process::id()),
        )
        .unwrap()
        .id;

        let desktop = crate::commands::agent_node::create_agent_node_blocking(
            mesh_id,
            "main",
            Some("terminal"),
            None,
            None,
            Some("ship the prompt"),
        )
        .expect_err("desktop create must reject Terminal");
        assert_eq!(
            desktop,
            crate::services::agent_node::INITIAL_PROMPT_UNSUPPORTED
        );
        assert!(crate::db::list_agent_nodes_by_mesh(mesh_id)
            .unwrap()
            .is_empty());
    }

    /// The route rejects every bad request *before* it reaches the blocking
    /// pool, so these run end-to-end without a database or preferences install.
    #[tokio::test]
    async fn create_rejects_unusable_requests_before_any_node_is_created() {
        for (body, expected) in [
            (
                serde_json::json!({"mesh_id": 0, "prompt": " \n "}),
                "Idea must contain",
            ),
            (
                serde_json::json!({"mesh_id": 0, "prompt": "a".repeat(16_001)}),
                "Idea must contain",
            ),
            (
                serde_json::json!({"mesh_id": 0, "provider": "anthropic", "operation_key": "   "}),
                "operation_key",
            ),
            (
                serde_json::json!({
                    "mesh_id": 0,
                    "provider": "anthropic",
                    "operation_key": "k".repeat(super::OPERATION_KEY_MAX_BYTES + 1),
                }),
                "operation_key",
            ),
        ] {
            let response = create(&ParsedRequest::test_post(
                "/api/nodes/create",
                body.to_string().as_bytes(),
            ))
            .await;
            assert_eq!(response.status_code(), 400, "for {body}");
            assert!(
                String::from_utf8_lossy(response.body()).contains(expected),
                "expected {expected:?} in {}",
                String::from_utf8_lossy(response.body())
            );
        }
    }

    /// "Mesh default" on the phone omits both `provider` and
    /// `configuration_id`; the desktop's own default chain (explicit > mesh >
    /// app-wide > Claude Code) decides, as it does for issue and PR spawns.
    #[test]
    fn an_omitted_provider_resolves_the_mesh_default() {
        let _db = crate::db::test_support::isolated();
        let root = tempfile::tempdir().unwrap();
        crate::preferences::init_for_tests(root.path().to_path_buf());
        let path = |name: &str| format!("/tmp/buildmesh_default_{}_{name}", std::process::id());
        let plain = crate::db::create_mesh("plain", &path("plain")).unwrap();
        let pinned = crate::db::create_mesh("pinned", &path("pinned")).unwrap();
        crate::db::write_conn()
            .execute(
                "UPDATE meshes SET default_provider = 'codex' WHERE id = ?1",
                [pinned.id],
            )
            .unwrap();

        assert_eq!(super::mesh_default_provider(pinned.id).unwrap(), "codex");
        assert_eq!(super::mesh_default_provider(plain.id).unwrap(), "claude");
        let (status, message) = super::mesh_default_provider(plain.id + pinned.id + 1).unwrap_err();
        assert_eq!(status, "400 Bad Request");
        assert!(message.contains("Mesh not found"), "{message}");
    }

    /// Issue #2331: a committed outcome never answers 200, so a client that
    /// only reads the status cannot mistake it for a running agent.
    #[tokio::test]
    async fn committed_outcomes_never_answer_plain_ok() {
        use crate::services::create_operation::CreateNodeOutcome;

        let node = crate::models::AgentNode::default();
        assert_eq!(
            super::outcome_response(&CreateNodeOutcome::StartPending {
                node: node.clone(),
                operation_key: Some("op-1".into()),
                reason: "App not ready".into(),
            })
            .status_code(),
            202
        );
        assert_eq!(
            super::outcome_response(&CreateNodeOutcome::StartFailed {
                node,
                operation_key: Some("op-1".into()),
                start_error: "boom".into(),
            })
            .status_code(),
            207
        );
    }

    #[tokio::test]
    async fn create_rejects_blank_and_oversized_ideas_before_creating_nodes() {
        for prompt in [" \n ".to_string(), "a".repeat(16_001)] {
            let body =
                serde_json::to_vec(&serde_json::json!({"mesh_id": 0, "prompt": prompt})).unwrap();
            let response = create(&ParsedRequest::test_post("/api/nodes/create", &body)).await;
            assert_eq!(response.status_code(), 400);
            assert!(String::from_utf8_lossy(response.body()).contains("Idea must contain"));
        }
    }

    /// Issue #1377 (post-review): malformed JSON must reject with 400
    /// BEFORE any DB lookup — the body is the only thing we know about
    /// the caller's intent, so a parse failure is a client error.
    #[tokio::test]
    async fn rejects_malformed_json() {
        let resp = post_input(&req(b"not json at all", 0)).await;
        assert_eq!(resp.status_code(), 400);
        let text = String::from_utf8_lossy(resp.body());
        assert!(
            text.contains("Invalid JSON"),
            "expected JSON-parse error envelope; got: {text:?}"
        );
    }

    /// A `seq` of zero bytes would be a no-op against the PTY (the
    /// attention autoclear only fires on \r or \n) and would silently
    /// leave the card stuck on "Approved ✓" while the agent saw nothing.
    /// The handler must reject so the SPA never sees a "200 OK" for a
    /// tap that didn't deliver anything.
    #[tokio::test]
    async fn rejects_empty_seq() {
        let resp = post_input(&req(br#"{"seq":""}"#, 0)).await;
        assert_eq!(resp.status_code(), 400, "expected 400 for empty seq");
    }

    /// `node_id = 0` doesn't exist in the per-test DB, so the
    /// `db::get_agent_node_by_id` lookup returns `Err` and the route
    /// short-circuits with 404 BEFORE any PTY work runs. The SPA can
    /// distinguish this from the PTY-down 503 — a deleted node gets a
    /// different status code than a killed agent, so the triage card
    /// can show different user-facing copy.
    #[tokio::test]
    async fn returns_404_for_unknown_node() {
        let _db = crate::db::test_support::isolated();
        let resp = post_input(&req(br#"{"seq":"y\r"}"#, 0)).await;
        assert_eq!(resp.status_code(), 404, "expected 404 for missing node");
        let text = String::from_utf8_lossy(resp.body());
        assert!(
            text.contains("Node not found"),
            "expected 'Node not found' envelope; got: {text:?}"
        );
    }

    // --- Issue #2330: the node inventory must not invent an empty workspace ---

    async fn list_nodes() -> Response {
        list(&ParsedRequest::test_get("/api/nodes")).await
    }

    /// The acceptance check in both directions: a successful query against an
    /// empty database is `200 []`, because "there are no nodes" is a real
    /// observation a client is entitled to act on.
    #[tokio::test]
    async fn an_empty_database_still_returns_a_successful_empty_array() {
        let _db = crate::db::test_support::isolated();
        let resp = list_nodes().await;
        assert_eq!(resp.status_code(), 200);
        assert_eq!(String::from_utf8_lossy(resp.body()), "[]");
    }

    /// The production failure this issue is about: a phone polling a populated
    /// Work list while a query fails. Pre-fix the handler answered `200 []`, so
    /// the client replaced a populated workspace with an empty one and a
    /// Coordinator would have scheduled against an invented empty fleet.
    #[tokio::test]
    async fn a_failed_query_is_503_and_never_an_empty_success() {
        let _db = crate::db::test_support::isolated();
        let mesh = crate::db::create_mesh("demo", "/tmp/bm-2330-nodes").unwrap();
        crate::db::create_agent_node(crate::db::NewAgentNode {
            mesh_id: mesh.id,
            name: "agent-1",
            path: "/tmp/bm-2330-nodes",
            branch: "main",
            env: crate::models::EnvType::Windows,
            provider: "terminal",
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
        // Break the query the route depends on. The isolated database is
        // carried onto the blocking pool by `blocking::adopt_test_db`, so this
        // is the test's own database and no other test can observe it.
        crate::db::write_conn()
            .execute_batch("DROP TABLE agent_nodes;")
            .unwrap();

        let resp = list_nodes().await;
        assert_eq!(
            resp.status_code(),
            503,
            "an unavailable read is retryable, never an empty success"
        );
        let text = String::from_utf8_lossy(resp.body());
        assert!(
            !text.contains("[]"),
            "the failure must not read as an empty workspace: {text:?}"
        );
    }

    /// The triage route's node lookup. Pre-fix `.is_ok()` turned every failure
    /// — including a failed query — into a 404, which told the client a live
    /// node had been deleted.
    #[tokio::test]
    async fn a_failed_node_lookup_is_503_not_a_false_404() {
        let _db = crate::db::test_support::isolated();
        crate::db::write_conn()
            .execute_batch("DROP TABLE agent_nodes;")
            .unwrap();

        let resp = post_input(&req(br#"{"seq":"y\r"}"#, 0)).await;
        assert_ne!(
            resp.status_code(),
            404,
            "an unreadable node row must not be reported as a deleted node"
        );
        assert_eq!(resp.status_code(), 503);
    }
}
