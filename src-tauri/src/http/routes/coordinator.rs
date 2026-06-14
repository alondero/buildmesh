//! Coordinator read API routes (ADR-0008). Thin skins over the
//! `coordinator::node_digest` module — all the read-model logic lives there so
//! these handlers stay a transport adapter.
//!
//! This slice exposes `GET /nodes` (spine-only digests) and
//! `GET /nodes/{id}/log?tail=N` (raw recent transcript turns). Auth (the
//! off-by-default master switch + read token) is enforced by the dispatcher in
//! `http::mod` via `coordinator::authenticate_read` before either is reached.

use crate::coordinator::node_digest;
use crate::db;
use crate::services::transcript_reader;

/// `GET /nodes` → JSON array of spine-only Node Digests across every Mesh.
/// Returns `[]` rather than erroring if the DB read fails, so a Coordinator
/// scan degrades to "no nodes" instead of a 500.
pub fn list_nodes_json() -> String {
    match db::list_coordinator_node_rows() {
        Ok(rows) => {
            let digests: Vec<_> = rows
                .iter()
                .map(|(node, mesh, changed)| node_digest::spine(node, mesh, *changed))
                .collect();
            serde_json::to_string(&digests).unwrap_or_else(|_| "[]".to_string())
        }
        Err(_) => "[]".to_string(),
    }
}

/// `GET /nodes/{id}/log?tail=N` → the raw recent transcript turns for one node.
/// Returns `None` (→ 404 in the dispatcher) only when the node id is unknown;
/// every other degrade path (no session, missing/unreadable/shape-changed
/// transcript) is a `200` carrying a typed `{"status":"unavailable",...}`
/// envelope, so the Coordinator always gets a structured answer.
pub fn log_json(node_id: i64, tail: usize) -> Option<String> {
    let node = db::get_agent_node_by_id(node_id).ok()?;
    let result = transcript_reader::read_tail(node.cli_session_id.as_deref(), &node.path, tail);
    Some(serde_json::to_string(&result).unwrap_or_else(|_| {
        // A serialization failure still answers structurally rather than 500ing.
        "{\"status\":\"unavailable\",\"reason\":\"unreadable\"}".to_string()
    }))
}
