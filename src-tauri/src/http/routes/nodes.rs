//! `GET /api/nodes` and `POST /api/nodes/create`, plus `POST /api/nodes/{id}/input`
//! (issue #1377) for the triage deck's one-tap Approve/Reject chips.

use tauri::Emitter;
use crate::http::MaybeTls;
use crate::agent::process::{ProcessRegistryApi, PROCESS_REGISTRY};

use crate::db;
use crate::http::request;

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

pub async fn create(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    content_length: usize,
) {
    let Some(body_bytes) =
        request::read_body_or_send_error(lines, content_length, 64 * 1024).await
    else {
        return;
    };

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

    let req: CreateNodeRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("Invalid JSON: {}", e);
            request::send_json_error(lines, "400 Bad Request", &msg).await;
            return;
        }
    };

    let mesh_id = req.mesh_id;
    let provider = req.provider;
    let mesh = match crate::commands::run_blocking("http_create_node_mesh", move || {
        db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())
    })
    .await
    {
        Ok(m) => m,
        Err(_) => {
            request::send_json_error(lines, "400 Bad Request", "Mesh not found").await;
            return;
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
            let msg = format!("Failed to create node: {}", e);
            request::send_json_error(lines, "500 Internal Server Error", &msg).await;
            return;
        }
    };

    let Some(app) = crate::http::app_handle() else {
        request::send_json_error(lines, "503 Service Unavailable", "App not ready").await;
        return;
    };

    let node_id = node.id;

    if let Err(e) = crate::agent::spawn::spawn_with_intent(
        app,
        crate::agent::spawn::SpawnRequest::new(
            node_id,
            crate::agent::spawn::SpawnIntent::Fresh,
            crate::agent::spawn::TerminalSize {
                rows: req.rows,
                cols: req.cols,
            },
        ),
    )
    .await
    {
        let msg = format!("Failed to spawn agent: {}", e);
        request::send_json_error(lines, "500 Internal Server Error", &msg).await;
        return;
    }

    let node = match crate::commands::run_blocking("http_reload_node", move || {
        db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())
    })
    .await
    {
        Ok(node) => node,
        Err(e) => {
            request::send_json_error(
                lines,
                "500 Internal Server Error",
                &format!("Failed to reload node: {}", e),
            )
            .await;
            return;
        }
    };
    let body = serde_json::to_string(&node).unwrap_or_else(|_| "{}".to_string());

    let _ = app.emit(
        "node-created",
        crate::commands::agent::NodeCreatedPayload { id: node_id },
    );
    let _ = request::write_json(lines, "200 OK", &body).await;
}

/// Body-shape cap for `/api/nodes/{id}/input`. The triage deck ships 2 bytes
/// (`"y\r"` / `"n\r"`); we allow up to 1 KiB so future call-sites (a
/// desktop-style "send a whole prompt" shortcut) keep working without a
/// schema bump. Anything larger is rejected with `413` before any DB or PTY
/// work runs.
const INPUT_BODY_MAX_BYTES: usize = 1024;

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
pub async fn post_input(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    node_id: i64,
    content_length: usize,
) {
    let Some(body_bytes) =
        request::read_body_or_send_error(lines, content_length, INPUT_BODY_MAX_BYTES).await
    else {
        return;
    };

    #[derive(serde::Deserialize)]
    struct InputRequest {
        seq: String,
    }

    let req: InputRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("Invalid JSON: {}", e);
            request::send_json_error(lines, "400 Bad Request", &msg).await;
            return;
        }
    };
    if req.seq.is_empty() {
        request::send_json_error(lines, "400 Bad Request", "seq must be non-empty").await;
        return;
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
        request::send_json_error(lines, "404 Not Found", "Node not found").await;
        return;
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
    let seq = req.seq.clone();
    let write_result = crate::commands::run_blocking(
        "http_input_write_bytes",
        move || -> Result<(), String> {
            let registry: &dyn ProcessRegistryApi = &**PROCESS_REGISTRY;
            crate::http::ws::write_mobile_input(registry, node_id, &seq)
        },
    )
    .await;

    match write_result {
        Ok(()) => {
            let body = r#"{"ok":true}"#;
            let _ = request::write_json(lines, "200 OK", body).await;
        }
        Err(e) => {
            // PTY not running (process killed, spawn failed) or the offload
            // task itself failed — surface as 503 so the SPA knows the
            // keystroke never reached the agent. The WS path logs and
            // continues; a one-shot HTTP tap can't recover by retrying the
            // same socket.
            request::send_json_error(
                lines,
                "503 Service Unavailable",
                &format!("PTY not running: {}", e),
            )
            .await;
        }
    }
}
