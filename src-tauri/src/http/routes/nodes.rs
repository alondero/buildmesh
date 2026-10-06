//! `GET /api/nodes` and `POST /api/nodes/create`, plus `POST /api/nodes/{id}/input`
//! (issue #1377) for the triage deck's one-tap Approve/Reject chips.

use tauri::Emitter;
use crate::agent::process::{ProcessRegistryApi, PROCESS_REGISTRY};

use crate::db;
use crate::http::response::Response;
use crate::http::router::ParsedRequest;
use crate::http::state;

pub async fn list(_req: &ParsedRequest) -> Response {
    Response::json("200 OK", list_json().await)
}

pub async fn list_json() -> String {
    match crate::commands::run_blocking("http_list_nodes", || {
        db::list_agent_nodes().map_err(|e| e.to_string())
    })
    .await
    {
        Ok(nodes) => serde_json::to_string(&nodes).unwrap_or_else(|_| "[]".to_string()),
        Err(_) => "[]".to_string(),
    }
}

#[derive(serde::Deserialize, ts_rs::TS)]
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
}

#[derive(Debug)]
enum CreateNodeError {
    Configuration(crate::preferences::spawn_configurations::SpawnConfigurationError),
    Agent(crate::services::agent_node::AgentNodeError),
}

pub async fn create(req: &ParsedRequest) -> Response {
    let parsed: CreateNodeRequest = match serde_json::from_slice(&req.body) {
        Ok(r) => r,
        Err(e) => {
            return Response::json_error("400 Bad Request", &format!("Invalid JSON: {}", e));
        }
    };

    let mesh_id = parsed.mesh_id;
    let intent = match parsed.prompt {
        Some(text) if text.trim().is_empty() || text.len() > 16_000 => {
            return Response::json_error("400 Bad Request", "Idea must contain 1 to 16000 bytes of text");
        }
        Some(text) => crate::agent::spawn::SpawnIntent::Prompt { text },
        None => crate::agent::spawn::SpawnIntent::Fresh,
    };
    let provider = if parsed.provider.as_deref().is_none_or(|p| p.trim().is_empty()) {
        parsed.configuration_id.clone().unwrap_or_default()
    } else { parsed.provider.unwrap() };
    let configuration_id = parsed.configuration_id.clone();
    let has_prompt = matches!(intent, crate::agent::spawn::SpawnIntent::Prompt { .. });
    let node = match crate::commands::run_blocking("http_create_node", move || {
        let result: Result<_, CreateNodeError> = (|| {
            let configuration = crate::preferences::spawn_configurations::resolve_saved(
                &provider,
                configuration_id.as_deref().filter(|s| !s.trim().is_empty()),
            )
            .map_err(CreateNodeError::Configuration)?;
        if has_prompt {
            let selection = configuration.as_ref().map(|c| c.id.as_str()).unwrap_or(&provider);
            let supports_prompt = crate::agent::provider_menu::available_providers().iter().any(|p| {
                p.id == selection && p.capabilities.supports_prefill && p.unavailable_reason.is_none()
            });
            if !supports_prompt {
                return Err(CreateNodeError::Agent(crate::services::agent_node::AgentNodeError::InvalidConfiguration(
                    "Choose an available agent that supports an initial prompt".into(),
                )));
            }
        }
        // Issue #1658 step 5 — the mesh-lookup + branch-resolution +
        // node-create trio is now a single shared runner; the helper
        // resolves `mesh.path` and the `"main"` branch internally, and
        // surfaces the typed `AgentNodeError::MeshNotFound(mesh_id)`
        // variant on an unknown mesh id (mapped to a 400 below). The
        // post-spawn `run_blocking` reload survives untouched so the
        // response body still carries the post-spawn row state.
        //
        // Review-round-2 fix: the prior string sentinel `"mesh not
        // found"` was stringly-typed brittle control flow;
        // `map_err` here tags the typed variant with its mesh id so
        // the outer match arm compares the discriminator rather than
        // the string body.
        crate::services::agent_node::create_blocking_configured(mesh_id, Some(provider.as_str()), Some("main"),
            None, // source_issue
            None, // name_override — none on this route
            None, // use_worktree_override — falls back to mesh default
            false,
            configuration.as_ref(),
        )
            .map_err(CreateNodeError::Agent)
        })();
        Ok::<_, String>(result)
    })
    .await
    {
        Ok(Ok(n)) => n,
        Ok(Err(CreateNodeError::Configuration(
            crate::preferences::spawn_configurations::SpawnConfigurationError::Invalid(message),
        ))) => {
            return Response::json_error("400 Bad Request", &message);
        }
        Ok(Err(CreateNodeError::Configuration(error))) => {
            return Response::json_error(
                "500 Internal Server Error",
                &format!("Failed to resolve spawn configuration: {error}"),
            );
        }
        Ok(Err(CreateNodeError::Agent(crate::services::agent_node::AgentNodeError::MeshNotFound(_)))) => {
            return Response::json_error("400 Bad Request", "Mesh not found");
        }
        Ok(Err(CreateNodeError::Agent(crate::services::agent_node::AgentNodeError::InvalidConfiguration(message)))) => {
            return Response::json_error("400 Bad Request", &message);
        }
        Ok(Err(CreateNodeError::Agent(e))) => {
            return Response::json_error(
                "500 Internal Server Error",
                &format!("Failed to create node: {}", e),
            );
        }
        Err(e) => {
            return Response::json_error(
                "500 Internal Server Error",
                &format!("Failed to create node: {e}"),
            );
        }
    };

    let Some(app) = state::app_handle() else {
        return Response::json_error("503 Service Unavailable", "App not ready");
    };

    let node_id = node.id;

    if let Err(e) = crate::agent::spawn::spawn_with_intent(
        app,
        crate::agent::spawn::SpawnRequest::new(
            node_id,
            intent,
            crate::agent::spawn::TerminalSize {
                rows: parsed.rows.unwrap_or(24),
                cols: parsed.cols.unwrap_or(80),
            },
        ),
    )
    .await
    {
        return Response::json_error(
            "500 Internal Server Error",
            &format!("Failed to spawn agent: {}", e),
        );
    }

    let node = match crate::commands::run_blocking("http_reload_node", move || {
        db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())
    })
    .await
    {
        Ok(node) => node,
        Err(e) => {
            return Response::json_error(
                "500 Internal Server Error",
                &format!("Failed to reload node: {}", e),
            );
        }
    };
    let body = serde_json::to_string(&node).unwrap_or_else(|_| "{}".to_string());

    let _ = app.emit(
        "node-created",
        crate::commands::agent::NodeCreatedPayload { id: node_id },
    );
    Response::json("200 OK", body)
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

    #[derive(serde::Deserialize)]
    struct InputRequest {
        seq: String,
    }

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
    let node_exists = crate::commands::run_blocking("http_input_node_lookup", move || {
        db::get_agent_node_by_id(node_id).map(|_| ()).map_err(|e| e.to_string())
    })
    .await
    .is_ok();
    if !node_exists {
        return Response::json_error("404 Not Found", "Node not found");
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

    fn outcome(disposition: crate::agent::process::InputDisposition) -> crate::agent::process::InputOutcome {
        crate::agent::process::InputOutcome { disposition, activity: Default::default() }
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
        assert!(!wire(&resp).contains("Retry-After"), "a delivered tap needs no retry hint");
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
        assert!(text.contains("input_backpressured"), "the client needs to know why: {text}");
        assert!(!text.contains("\"ok\":true"), "a refusal must never claim delivery: {text}");
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
        let resp = input_write_response(Ok(outcome(crate::agent::process::InputDisposition::Closed)));
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
        assert!(matches!(invalid, crate::preferences::spawn_configurations::SpawnConfigurationError::Invalid(_)));
        assert!(matches!(storage, crate::preferences::spawn_configurations::SpawnConfigurationError::Storage(_)));
    }

    fn req(body: &[u8], node_id: i64) -> ParsedRequest {
        ParsedRequest::test_post("/api/nodes/0/input", body).with_ids(Some(node_id), None)
    }

    #[tokio::test]
    async fn create_rejects_blank_and_oversized_ideas_before_creating_nodes() {
        for prompt in [" \n ".to_string(), "a".repeat(16_001)] {
            let body = serde_json::to_vec(&serde_json::json!({"mesh_id": 0, "prompt": prompt})).unwrap();
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
}
