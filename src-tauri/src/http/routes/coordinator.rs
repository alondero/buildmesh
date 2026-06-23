//! Coordinator read API routes (ADR-0008). Thin skins over the
//! `coordinator::node_digest` module - all the read-model logic lives there so
//! these handlers stay a transport adapter.
//!
//! This module exposes the read routes - `GET /nodes` (layered digests: spine +
//! transcript enrichment) and `GET /nodes/{id}/log?tail=N` (raw recent
//! transcript turns) - plus the drive route `POST /nodes/{id}/prompt` (issue
//! #319), which writes a prompt to a live node's PTY and returns an honest
//! verdict. Auth (the off-by-default master switch + the read- or drive-scoped
//! token) is enforced by the dispatcher in `http::mod` via
//! `auth::guard(.., CoordinatorRead | CoordinatorWrite)` (issue #500) before any
//! handler is reached.

use crate::coordinator::{drive, enrichment, node_digest};
use crate::db;
use crate::http::request;
use tokio::io::BufStream;
use crate::http::MaybeTls;

/// `GET /nodes` -> JSON array of layered Node Digests across every Mesh. Each
/// digest is the always-available spine plus a transcript-derived rich layer;
/// for a provider with no readable transcript the enrichment is explicitly
/// flagged `unsupported` (degrade-and-flag, ADR-0008 -3). Returns `[]` rather
/// than erroring if the DB read fails, so a Coordinator scan degrades to "no
/// nodes" instead of a 500.
pub fn list_nodes_json() -> String {
    match db::list_coordinator_node_rows() {
        Ok(rows) => {
            let digests: Vec<_> = rows
                .iter()
                .map(|(node, mesh, changed)| {
                    let tail = enrichment::digest_enrichment(node);
                    node_digest::layered(node, mesh, *changed, tail.as_ref())
                })
                .collect();
            serde_json::to_string(&digests).unwrap_or_else(|_| "[]".to_string())
        }
        Err(_) => "[]".to_string(),
    }
}

/// `GET /nodes/{id}/log?tail=N` -> the raw recent transcript turns for one node.
/// Returns `None` (-> 404 in the dispatcher) only when the node id is unknown;
/// every other degrade path (unsupported provider, no session,
/// missing/unreadable/shape-changed transcript) is a `200` carrying a typed
/// `{"status":"unavailable",...}` envelope, so the Coordinator always gets a
/// structured answer.
pub fn log_json(node_id: i64, tail: usize) -> Option<String> {
    let node = db::get_agent_node_by_id(node_id).ok()?;
    let result = enrichment::transcript_tail(&node, tail);
    Some(serde_json::to_string(&result).unwrap_or_else(|_| {
        // A serialization failure still answers structurally rather than 500ing.
        "{\"status\":\"unavailable\",\"reason\":\"unreadable\"}".to_string()
    }))
}

/// The drive request body: `{"prompt": "..."}`. Strict (no serde default) so a
/// malformed body is a 400, not a silent no-op - see the serde-default-fragility
/// lesson the read side follows.
#[derive(serde::Deserialize)]
struct PromptRequest {
    prompt: String,
}

/// `POST /nodes/{id}/prompt` - drive a live node by writing `prompt` to its PTY
/// (ADR-0008 -5, issue #319). Auth (the drive-scoped token + drive kill-switch)
/// is enforced by the dispatcher via `auth::guard(.., CoordinatorWrite)` (issue
/// #500) before this is reached.
///
/// Outcomes:
/// - unknown node id -> `404`
/// - empty prompt or malformed body -> `400`
/// - node not live (no agent process to write to) -> `409` with a clear error
/// - written -> `200 {"verdict":"delivered"|"unverified"}` (honest verdict)
pub async fn prompt(
    lines: &mut BufStream<MaybeTls>,
    node_id: i64,
    content_length: usize,
) {
    let req: PromptRequest = match request::read_json_body(lines, content_length, 256 * 1024).await {
        Ok(r) => r,
        Err(status) => {
            request::send_json_error(lines, &status, "Bad request").await;
            return;
        }
    };
    if req.prompt.trim().is_empty() {
        request::send_json_error(lines, "400 Bad Request", "Prompt must not be empty").await;
        return;
    }

    // Distinguish "no such node" (404) from "node exists but isn't drivable"
    // (409) - the latter comes back from the driver as `NotLive`.
    if db::get_agent_node_by_id(node_id).is_err() {
        request::send_json_error(lines, "404 Not Found", "Unknown node").await;
        return;
    }

    match drive::drive_node(node_id, &req.prompt) {
        Ok(verdict) => {
            let body = serde_json::json!({ "verdict": verdict }).to_string();
            let _ = request::write_json(lines, "200 OK", &body).await;
        }
        Err(drive::DriveError::NotLive) => {
            request::send_json_error(
                lines,
                "409 Conflict",
                "Node is not live - only a node with a running agent can be driven",
            )
            .await;
        }
        Err(drive::DriveError::WriteFailed(e)) => {
            request::send_json_error(lines, "500 Internal Server Error", &e).await;
        }
    }
}
