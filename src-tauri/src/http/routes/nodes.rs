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

pub async fn create(req: &ParsedRequest) -> Response {
    #[derive(serde::Deserialize)]
    struct CreateNodeRequest {
        mesh_id: i64,
        provider: String,
        #[serde(default = "default_rows")]
        rows: u16,
        #[serde(default = "default_cols")]
        cols: u16,
    }
    fn default_rows() -> u16 {
        24
    }
    fn default_cols() -> u16 {
        80
    }

    let parsed: CreateNodeRequest = match serde_json::from_slice(&req.body) {
        Ok(r) => r,
        Err(e) => {
            return Response::json_error("400 Bad Request", &format!("Invalid JSON: {}", e));
        }
    };

    let mesh_id = parsed.mesh_id;
    let provider = parsed.provider;
    let mesh = match crate::commands::run_blocking("http_create_node_mesh", move || {
        db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())
    })
    .await
    {
        Ok(m) => m,
        Err(_) => {
            return Response::json_error("400 Bad Request", "Mesh not found");
        }
    };

    let mesh_path = mesh.path.clone();
    let node = match crate::commands::run_blocking("http_create_node", move || {
        crate::services::agent_node::create(
            mesh_id,
            &mesh_path,
            "main",
            Some(provider.as_str()),
            None, // source_issue
            None, // source_pr — generic mobile spawn, not PR-spawn (issue #450)
            None, // source_pr_pinned_sha — generic mobile spawn, no pin (issue #444)
            None, // use_worktree_override — None falls back to mesh default
            None, // name_override — none supplied on this route
        )
        .map_err(|e| e.to_string())
    })
    .await
    {
        Ok(n) => n,
        Err(e) => {
            return Response::json_error(
                "500 Internal Server Error",
                &format!("Failed to create node: {}", e),
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
            crate::agent::spawn::SpawnIntent::Fresh,
            crate::agent::spawn::TerminalSize {
                rows: parsed.rows,
                cols: parsed.cols,
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
        move || -> Result<(), String> {
            let registry: &dyn ProcessRegistryApi = &**PROCESS_REGISTRY;
            crate::http::ws::write_mobile_input(registry, node_id, &seq)
        },
    )
    .await;

    match write_result {
        Ok(()) => Response::json("200 OK", r#"{"ok":true}"#),
        Err(e) => {
            // PTY not running (process killed, spawn failed) or the offload
            // task itself failed — surface as 503 so the SPA knows the
            // keystroke never reached the agent. The WS path logs and
            // continues; a one-shot HTTP tap can't recover by retrying the
            // same socket.
            Response::json_error(
                "503 Service Unavailable",
                &format!("PTY not running: {}", e),
            )
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
    use super::*;
    use crate::http::router::ParsedRequest;

    fn req(body: &[u8], node_id: i64) -> ParsedRequest {
        ParsedRequest::test_post("/api/nodes/0/input", body).with_ids(Some(node_id), None)
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
        crate::db::test_support::ensure_db_for_tests();
        let resp = post_input(&req(br#"{"seq":"y\r"}"#, 0)).await;
        assert_eq!(resp.status_code(), 404, "expected 404 for missing node");
        let text = String::from_utf8_lossy(resp.body());
        assert!(
            text.contains("Node not found"),
            "expected 'Node not found' envelope; got: {text:?}"
        );
    }
}
