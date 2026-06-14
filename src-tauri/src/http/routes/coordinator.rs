//! Coordinator read API routes (ADR-0008). Thin skins over the
//! `coordinator::node_digest` module — all the read-model logic lives there so
//! these handlers stay a transport adapter.
//!
//! This slice exposes `GET /nodes` (layered digests: spine + transcript
//! enrichment) and `GET /nodes/{id}/log?tail=N` (raw recent transcript turns).
//! Auth (the off-by-default master switch + read token) is enforced by the
//! dispatcher in `http::mod` via `coordinator::authenticate_read` before either
//! is reached.

use crate::coordinator::{enrichment, node_digest};
use crate::db;

/// `GET /nodes` → JSON array of layered Node Digests across every Mesh. Each
/// digest is the always-available spine plus a transcript-derived rich layer;
/// for a provider with no readable transcript the enrichment is explicitly
/// flagged `unsupported` (degrade-and-flag, ADR-0008 §3). Returns `[]` rather
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

/// `GET /nodes/{id}/log?tail=N` → the raw recent transcript turns for one node.
/// Returns `None` (→ 404 in the dispatcher) only when the node id is unknown;
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

