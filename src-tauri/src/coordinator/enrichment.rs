//! Transcript enrichment builder (ADR-0008) — the one impure owner of "is this
//! Agent Node's transcript readable, where does it live, and read it". It wraps
//! the pure [`node_digest`](super::node_digest) core: the digest layering never
//! touches disk, while this module does the capability gate + path resolution +
//! file read so the HTTP route stays a thin transport skin.
//!
//! Both the `GET /nodes` digest enrichment and the `GET /nodes/{id}/log` raw
//! tail flow through here, so the provider-capability gate has exactly one home
//! (it used to be encoded two ways in the route — `Option<TranscriptTail>` for
//! the digest, a `TranscriptTail::Unavailable{Unsupported}` for the log).

use crate::env;
use crate::models::AgentNode;
use crate::services::transcript_reader::{self, TranscriptTail, UnavailableReason};

/// The directory the agent's transcript is keyed under — the
/// [Node Working Directory](../../../CONTEXT.md) in its *spawn* form, because
/// Claude Code encodes the path it actually ran in (the Worktree Node dir for a
/// worktree node, the Mesh root for a Root Node). Resolving `node.path` directly
/// here is the bug this builder fixes: a worktree node's transcript lives under
/// `.claude/worktrees/<name>`, not the mesh root, so the old route looked in the
/// wrong place and every worktree node silently degraded to a spine-only digest.
fn transcript_dir(node: &AgentNode) -> String {
    env::node_working_path(node).spawn_path
}

/// Read a node's transcript tail, gated on its provider's capability. A provider
/// that produces no readable transcript degrades to `Unsupported` *without*
/// touching the filesystem — the same degrade-and-flag rule the digest applies,
/// so `/nodes` and `/nodes/{id}/log` agree on why a node has no rich layer (an
/// unsupported provider never masquerades as a supported one that merely hasn't
/// captured a session). Pure over the node + filesystem, so it is unit-testable
/// without a DB.
pub fn transcript_tail(node: &AgentNode, tail: usize) -> TranscriptTail {
    if !node.provider.adapter().produces_readable_transcript() {
        return TranscriptTail::Unavailable {
            reason: UnavailableReason::Unsupported,
        };
    }
    transcript_reader::read_tail(node.cli_session_id.as_deref(), &transcript_dir(node), tail)
}

/// The enrichment a Node Digest layers on, in the shape `node_digest::layered`
/// expects: `None` is the unsupported-provider signal (which keeps `node_digest`
/// provider-agnostic — it never learns the `Unsupported` reason), while a
/// supported provider that simply has no transcript yet comes back as
/// `Some(Unavailable{ NoSession | NoTranscript | … })`.
///
/// Uses the **bounded** reader (issue #341): a digest only needs the last
/// assistant message, so this parses just the tail bytes of the transcript
/// rather than the whole file. For a `GET /nodes` poll over many cwrap nodes
/// with long histories that turns N full-file parses into N bounded ones. The
/// full-tail [`transcript_tail`] is reserved for the on-demand `/log` drill-in.
pub fn digest_enrichment(node: &AgentNode) -> Option<TranscriptTail> {
    if !node.provider.adapter().produces_readable_transcript() {
        return None;
    }
    Some(transcript_reader::read_last_assistant_message(
        node.cli_session_id.as_deref(),
        &transcript_dir(node),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{EnvType, Provider, SessionStatus};
    use chrono::Utc;

    fn node(provider: Provider, cli_session_id: Option<&str>, use_worktree: bool) -> AgentNode {
        AgentNode {
            id: 1,
            mesh_id: 1,
            name: "n".to_string(),
            path: "X:\\src\\proj".to_string(),
            branch: "main".to_string(),
            env: EnvType::Windows,
            provider,
            status: SessionStatus::Running,
            cli_session_id: cli_session_id.map(str::to_string),
            worktree_name: Some("gentle-fox".to_string()),
            use_worktree,
            source_issue: None,
            source_pr: None,
            position: 0,
            created_at: Utc::now(),
        }
    }

    /// Regression for the worktree transcript bug: a Worktree Node's transcript
    /// is keyed under its worktree dir, not the Mesh root. The route used to feed
    /// `node.path` (the mesh root) to the reader, so every worktree node — the
    /// default — found no transcript and degraded to spine. The builder must
    /// search the resolved Node Working Directory.
    #[test]
    fn worktree_node_searches_worktree_dir_not_mesh_root() {
        let n = node(Provider::Anthropic, Some("sid"), true);
        let dir = transcript_dir(&n);
        assert!(
            dir.contains("worktrees") && dir.contains("gentle-fox"),
            "expected the worktree dir, got: {dir}"
        );
        assert_ne!(dir, n.path, "must not search the mesh root for a worktree node");
    }

    /// A Root Node's transcript is keyed under the Mesh root itself.
    #[test]
    fn root_node_searches_mesh_root() {
        let n = node(Provider::Anthropic, Some("sid"), false);
        assert!(!transcript_dir(&n).contains("worktrees"));
    }

    /// An unsupported provider degrades to `Unsupported` without reading disk —
    /// distinct from a supported-but-unstarted node, so a Coordinator can tell
    /// "never has a transcript" from "hasn't captured a session yet".
    #[test]
    fn unsupported_provider_degrades_without_disk() {
        let tail = transcript_tail(&node(Provider::OpenCode, None, true), 10);
        assert_eq!(
            tail,
            TranscriptTail::Unavailable {
                reason: UnavailableReason::Unsupported
            }
        );
    }

    /// A supported provider with no captured session id is a genuinely different
    /// state from `Unsupported` — the gate must not collapse the two.
    #[test]
    fn supported_provider_without_session_reports_no_session() {
        let tail = transcript_tail(&node(Provider::Anthropic, None, true), 10);
        assert_eq!(
            tail,
            TranscriptTail::Unavailable {
                reason: UnavailableReason::NoSession
            }
        );
    }

    /// `digest_enrichment` maps an unsupported provider to `None` (the signal
    /// `node_digest::layered` reads as "unsupported"), keeping the digest core
    /// provider-agnostic — while a supported-no-session node stays `Some` so the
    /// digest flags it distinctly.
    #[test]
    fn digest_enrichment_maps_unsupported_to_none_but_keeps_no_session() {
        assert!(digest_enrichment(&node(Provider::OpenCode, None, true)).is_none());
        assert_eq!(
            digest_enrichment(&node(Provider::Anthropic, None, true)),
            Some(TranscriptTail::Unavailable {
                reason: UnavailableReason::NoSession
            })
        );
    }
}
