//! Coordinator read API routes (ADR-0008). Thin skins over the
//! `coordinator::node_digest` module — all the read-model logic lives there so
//! these handlers stay a transport adapter.
//!
//! This slice exposes `GET /nodes` (layered digests: spine + transcript
//! enrichment) and `GET /nodes/{id}/log?tail=N` (raw recent transcript turns).
//! Auth (the off-by-default master switch + read token) is enforced by the
//! dispatcher in `http::mod` via `coordinator::authenticate_read` before either
//! is reached.

use crate::coordinator::node_digest;
use crate::db;
use crate::services::transcript_reader;

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
                    // Read the transcript only for providers that produce one;
                    // `None` is the unsupported signal `node_digest` degrades on.
                    // Issue #341: digest enrichment only needs `last_assistant_message`,
                    // so we use the cheap bounded reader instead of the full `read_tail`
                    // (which parses every line just to derive it). For `GET /nodes`
                    // over many cwrap nodes with long histories this turns N full-file
                    // parses into N bounded-tail parses — bounded to DIGEST_TAIL_BYTES
                    // (256 KiB) regardless of transcript size.
                    let tail = node
                        .provider
                        .adapter()
                        .produces_readable_transcript()
                        .then(|| {
                            transcript_reader::read_last_assistant_message(
                                node.cli_session_id.as_deref(),
                                &node.path,
                            )
                        });
                    node_digest::layered(node, mesh, *changed, tail.as_ref())
                })
                .collect();
            serde_json::to_string(&digests).unwrap_or_else(|_| "[]".to_string())
        }
        Err(_) => "[]".to_string(),
    }
}

/// The transcript tail for a node, gated on its provider's capability. A
/// provider that produces no readable transcript degrades to `Unsupported`
/// *without* touching the filesystem — the same degrade-and-flag rule the
/// digest applies, so `GET /nodes` and `GET /nodes/{id}/log` agree on why a
/// node has no rich layer (an unsupported provider never masquerades as a
/// supported one that merely hasn't captured a session). Pure over the node, so
/// it is unit-testable without a DB.
fn transcript_tail_for(node: &crate::models::AgentNode, tail: usize) -> transcript_reader::TranscriptTail {
    if node.provider.adapter().produces_readable_transcript() {
        transcript_reader::read_tail(node.cli_session_id.as_deref(), &node.path, tail)
    } else {
        transcript_reader::TranscriptTail::Unavailable {
            reason: transcript_reader::UnavailableReason::Unsupported,
        }
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
    let result = transcript_tail_for(&node, tail);
    Some(serde_json::to_string(&result).unwrap_or_else(|_| {
        // A serialization failure still answers structurally rather than 500ing.
        "{\"status\":\"unavailable\",\"reason\":\"unreadable\"}".to_string()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AgentNode, EnvType, Provider, SessionStatus};
    use crate::services::transcript_reader::{TranscriptTail, UnavailableReason};
    use chrono::Utc;

    fn node(provider: Provider, cli_session_id: Option<&str>) -> AgentNode {
        AgentNode {
            id: 1,
            mesh_id: 1,
            name: "n".to_string(),
            path: "X:\\nowhere\\does\\not\\exist".to_string(),
            branch: "main".to_string(),
            env: EnvType::Windows,
            provider,
            status: SessionStatus::Running,
            cli_session_id: cli_session_id.map(str::to_string),
            worktree_name: None,
            use_worktree: true,
            source_issue: None,
            position: 0,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn unsupported_provider_log_degrades_to_unsupported_without_reading_disk() {
        // An OpenCode node has no readable transcript; the /log endpoint must
        // say so explicitly rather than leaking a NoSession/NoTranscript reason
        // that a supported-but-not-started node would also produce.
        let tail = transcript_tail_for(&node(Provider::OpenCode, None), 10);
        assert_eq!(
            tail,
            TranscriptTail::Unavailable {
                reason: UnavailableReason::Unsupported
            }
        );
    }

    #[test]
    fn supported_provider_without_session_still_reports_no_session() {
        // A supported provider that hasn't captured a session id is a genuinely
        // different state from `Unsupported` — the capability gate must not
        // collapse the two.
        let tail = transcript_tail_for(&node(Provider::Anthropic, None), 10);
        assert_eq!(
            tail,
            TranscriptTail::Unavailable {
                reason: UnavailableReason::NoSession
            }
        );
    }
}
