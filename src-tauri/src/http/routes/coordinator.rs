//! Coordinator read API routes (ADR-0008). Thin skins over the
//! `coordinator::node_digest` module — all the read-model logic lives there so
//! these handlers stay a transport adapter.
//!
//! This slice exposes only `GET /nodes` (spine-only digests). Auth (the
//! off-by-default master switch + read token) is enforced by the dispatcher in
//! `http::mod` via `coordinator::authenticate_read` before this is reached.

use crate::coordinator::node_digest;
use crate::db;

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
