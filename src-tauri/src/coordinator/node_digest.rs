//! Node Digest — the coordinator-facing read summary of a single Agent Node.
//!
//! See ADR-0008 and `CONTEXT.md`. A digest is layered: an always-available
//! **spine** from Buildmesh's own database (lifecycle `status`,
//! `needs_feedback` = `awaiting_input`, `waiting_since`, `last_activity`)
//! plus — for the Claude Code provider family only — semantic **enrichment**
//! read from the on-disk JSONL transcript.
//!
//! This slice (issue #315) ships the **spine only**: no transcript enrichment
//! yet. The module is deliberately pure over its inputs (a node row, its Mesh
//! name, and the status-change timestamp) so every field is testable without a
//! PTY or a live database — later slices graft enrichment on without disturbing
//! this seam.

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::models::{AgentNode, SessionStatus};

/// The spine-only Node Digest. Serialized as plain JSON over HTTP so it is
/// `curl`-inspectable and shaped for a later MCP resource wrap.
#[derive(Debug, Clone, Serialize)]
pub struct NodeDigest {
    pub id: i64,
    pub name: String,
    /// The owning Mesh's name (human-referable identity, per ADR-0008 §4).
    pub mesh: String,
    /// Provider id string (e.g. `anthropic`, `minimax`).
    pub provider: String,
    /// Lifecycle status string (e.g. `running`, `awaiting_input`, `idle`).
    pub status: String,
    /// True when the node is blocked on a human — i.e. `awaiting_input`. This
    /// is the single highest-value scan field: it answers "which nodes need me
    /// right now?" without the Coordinator interpreting raw status strings.
    pub needs_feedback: bool,
    /// When the node entered `awaiting_input`, so a Coordinator can prioritise
    /// the node that has been stuck longest. `None` whenever the node is not
    /// currently awaiting input.
    pub waiting_since: Option<DateTime<Utc>>,
    /// When the node last changed lifecycle status — the spine's best signal of
    /// "working vs gone quiet". Transcript enrichment will sharpen this in a
    /// later slice; for now it is the last lifecycle transition.
    pub last_activity: DateTime<Utc>,
}

/// Build the spine of a Node Digest from a node row plus the two fields that
/// aren't carried on `AgentNode`: its Mesh name and the timestamp of its last
/// status change. Pure — no I/O — so it is exhaustively unit-testable.
pub fn spine(
    node: &AgentNode,
    mesh_name: &str,
    status_changed_at: DateTime<Utc>,
) -> NodeDigest {
    let needs_feedback = node.status == SessionStatus::AwaitingInput;
    NodeDigest {
        id: node.id,
        name: node.name.clone(),
        mesh: mesh_name.to_string(),
        provider: node.provider.to_string(),
        status: node.status.to_db_str().to_string(),
        needs_feedback,
        // The moment a node became blocked IS its last status change, so reuse
        // the same timestamp — only surfaced while it is actually waiting.
        waiting_since: needs_feedback.then_some(status_changed_at),
        last_activity: status_changed_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{EnvType, Provider};

    fn node(status: SessionStatus, provider: Provider) -> AgentNode {
        AgentNode {
            id: 7,
            mesh_id: 1,
            name: "fix-login".to_string(),
            path: "/tmp/fix-login".to_string(),
            branch: "main".to_string(),
            env: EnvType::Windows,
            provider,
            status,
            cli_session_id: None,
            worktree_name: None,
            use_worktree: true,
            source_issue: None,
            position: 0,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn awaiting_input_node_needs_feedback_with_waiting_since() {
        let changed = Utc::now();
        let digest = spine(&node(SessionStatus::AwaitingInput, Provider::Anthropic), "core", changed);

        assert_eq!(digest.id, 7);
        assert_eq!(digest.name, "fix-login");
        assert_eq!(digest.mesh, "core");
        assert_eq!(digest.provider, "anthropic");
        assert_eq!(digest.status, "awaiting_input");
        assert!(digest.needs_feedback, "awaiting_input must set needs_feedback");
        assert_eq!(
            digest.waiting_since,
            Some(changed),
            "waiting_since is present and equals the status-change time"
        );
        assert_eq!(digest.last_activity, changed);
    }

    #[test]
    fn running_node_is_not_waiting() {
        let changed = Utc::now();
        let digest = spine(&node(SessionStatus::Running, Provider::Minimax), "core", changed);

        assert_eq!(digest.status, "running");
        assert_eq!(digest.provider, "minimax");
        assert!(!digest.needs_feedback);
        assert_eq!(digest.waiting_since, None, "a running node has no waiting_since");
        assert_eq!(digest.last_activity, changed);
    }

    #[test]
    fn idle_node_reports_spine_without_feedback() {
        let digest = spine(&node(SessionStatus::Idle, Provider::Anthropic), "core", Utc::now());
        assert_eq!(digest.status, "idle");
        assert!(!digest.needs_feedback);
        assert!(digest.waiting_since.is_none());
    }

    #[test]
    fn digest_serializes_to_expected_json_shape() {
        let changed = "2026-06-14T10:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let digest = spine(&node(SessionStatus::AwaitingInput, Provider::Anthropic), "core", changed);
        let json: serde_json::Value = serde_json::to_value(&digest).unwrap();

        // Lock the wire contract a later MCP wrap depends on.
        for key in [
            "id", "name", "mesh", "provider", "status", "needs_feedback",
            "waiting_since", "last_activity",
        ] {
            assert!(json.get(key).is_some(), "digest JSON must carry `{key}`");
        }
        assert_eq!(json["needs_feedback"], serde_json::json!(true));
        assert_eq!(json["waiting_since"], serde_json::json!("2026-06-14T10:00:00Z"));
    }
}
