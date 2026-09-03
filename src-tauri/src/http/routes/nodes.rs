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

#[cfg(test)]
mod tests {
    //! Issue #1377 — `POST /api/nodes/{id}/input` is the new triage-deck
    //! input endpoint (replaces the previous "open a terminal WS, send
    //! bytes, close" pattern). The handler must:
    //!   * reject malformed JSON with `400 Bad Request`
    //!   * reject an empty `seq` with `400 Bad Request` (otherwise we'd
    //!     push zero bytes and the agent would never see a CR/LF — the
    //!     triage chip's whole reason for existing)
    //!   * reject a body past `INPUT_BODY_MAX_BYTES` with `413` BEFORE
    //!     any DB call (the cap is the DoS bound)
    //!   * return `404` when the node id isn't in the DB (the SPA can
    //!     distinguish this from the PTY-down 503)
    //!
    //! These tests call `post_input` directly over a TCP socket so the
    //! body-read path is exercised end-to-end. They stop at the body/
    //! DB boundary — the 404 case uses `node_id = 0`, which `db::
    //! get_agent_node_by_id` resolves to "not found" in the per-test DB
    //! without us having to seed a row. The PTY-down 503 path lives
    //! behind a real `ProcessRegistry` and is covered by `ws::tests::
    //! forward_mobile_input_handles_registry_error` (the same code path
    //! `post_input` runs through `write_mobile_input`).
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Drive `post_input` over a real TCP socket and return the response
    /// bytes. `node_id = 0` exercises the 404 path without DB seeding
    /// (the per-test DB has no row 0).
    async fn drive(body: &[u8], node_id: i64) -> Vec<u8> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let content_length = body.len();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut lines = tokio::io::BufStream::new(crate::http::MaybeTls::Plain(stream));
            post_input(&mut lines, node_id, content_length).await;
        });
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(body).await.unwrap();
        stream.shutdown().await.unwrap();
        let mut resp = Vec::new();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream.read_to_end(&mut resp),
        )
        .await
        .expect("server hung");
        let _ = server.await;
        resp
    }

    fn status_line(resp: &[u8]) -> String {
        String::from_utf8_lossy(resp)
            .lines()
            .next()
            .unwrap_or("")
            .to_string()
    }

    fn body(resp: &[u8]) -> String {
        String::from_utf8_lossy(resp).into_owned()
    }

    /// Issue #1377 (post-review): malformed JSON must reject with 400
    /// BEFORE any DB lookup — the body is the only thing we know about
    /// the caller's intent, so a parse failure is a client error.
    #[tokio::test]
    async fn rejects_malformed_json() {
        let resp = drive(b"not json at all", 0).await;
        let s = status_line(&resp);
        assert!(s.starts_with("HTTP/1.1 400"), "expected 400; got: {s:?}");
        assert!(
            body(&resp).contains("Invalid JSON"),
            "expected JSON-parse error envelope; got: {:?}",
            body(&resp)
        );
    }

    /// A `seq` of zero bytes would be a no-op against the PTY (the
    /// attention autoclear only fires on \r or \n) and would silently
    /// leave the card stuck on "Approved ✓" while the agent saw nothing.
    /// The handler must reject so the SPA never sees a "200 OK" for a
    /// tap that didn't deliver anything.
    #[tokio::test]
    async fn rejects_empty_seq() {
        let resp = drive(br#"{"seq":""}"#, 0).await;
        let s = status_line(&resp);
        assert!(
            s.starts_with("HTTP/1.1 400"),
            "expected 400 for empty seq; got: {s:?}"
        );
    }

    /// Body past `INPUT_BODY_MAX_BYTES` (1024) — the cap is the DoS
    /// bound, and `request::read_body_or_send_error` short-circuits with
    /// 413 BEFORE the JSON parser runs. A regression that drops the cap
    /// (or moves it past the read) would let a malformed 10MB body
    /// pin a tokio worker for the full upload window.
    #[tokio::test]
    async fn rejects_oversized_body() {
        let mut body = br#"{"seq":""#.to_vec();
        // Pad past the 1024-byte cap with `a` chars — still parseable
        // JSON if it slipped through, but the cap rejects first.
        body.extend(std::iter::repeat(b'a').take(2048));
        body.extend_from_slice(br#""}"#);
        let resp = drive(&body, 0).await;
        let s = status_line(&resp);
        assert!(
            s.starts_with("HTTP/1.1 413"),
            "expected 413 for oversized body; got: {s:?}"
        );
    }

    /// `node_id = 0` doesn't exist in the per-test DB, so the
    /// `db::get_agent_node_by_id` lookup returns `Err` and the route
    /// short-circuits with 404 BEFORE any PTY work runs. The SPA can
    /// distinguish this from the PTY-down 503 — a deleted node gets a
    /// different status code than a killed agent, so the triage card
    /// can show different user-facing copy.
    #[tokio::test]
    async fn returns_404_for_unknown_node() {
        // NOTE: relies on `db::init` having been called by another test
        // first (the global OnceCell is process-shared). If this fails
        // with "Database not initialized", check the test ordering.
        let resp = drive(br#"{"seq":"y\r"}"#, 0).await;
        let s = status_line(&resp);
        assert!(
            s.starts_with("HTTP/1.1 404"),
            "expected 404 for missing node; got: {s:?}"
        );
        assert!(
            body(&resp).contains("Node not found"),
            "expected 'Node not found' envelope; got: {:?}",
            body(&resp)
        );
    }
}
