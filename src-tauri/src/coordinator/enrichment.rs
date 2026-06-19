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
use crate::secret_scrubber::SecretScrubber;
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
    scrub_tail(transcript_reader::read_tail(
        node.cli_session_id.as_deref(),
        &transcript_dir(node),
        tail,
    ))
}

/// Mask any secrets the raw transcript echoed (tokens, passwords, private keys)
/// before it leaves the host for an external Coordinator (ADR-0012 §5). Both the
/// `GET /nodes/{id}/log` full tail and the `GET /nodes` digest's
/// `last_assistant_message` flow through here, so every coordinator-facing
/// transcript path is scrubbed at exactly one boundary. Scrubs the structured
/// content only — turn text, each tool call's raw `input`, and the last
/// assistant message — never the `{"status":…}` envelope, so the shape the
/// Coordinator parses is untouched. An `Unavailable` tail carries no content and
/// passes through verbatim.
///
/// Known residual (issue #499 follow-up): the `transcript_reader` truncates each
/// turn text to `MAX_TURN_TEXT` and each tool-string leaf to `MAX_TOOL_STRING`
/// *before* this runs, so a context-free token (one not in `key=value`/`Bearer`
/// form) landing within ~100 bytes of a truncation boundary can leave a prefix
/// shorter than the token rules' minimum length, which then isn't masked. The
/// leaked prefix is always a fragment — the remainder is truncated away and
/// never served — so it is not a usable credential. Closing it fully means
/// scrubbing inside the reader before truncation; deferred to keep the
/// JSONL-quarantine reader untouched in this slice.
fn scrub_tail(tail: TranscriptTail) -> TranscriptTail {
    match tail {
        TranscriptTail::Available {
            mut turns,
            last_assistant_message,
        } => {
            for turn in &mut turns {
                turn.text = SecretScrubber::scrub(&turn.text);
                for call in &mut turn.tool_calls {
                    SecretScrubber::scrub_json(&mut call.input);
                }
            }
            TranscriptTail::Available {
                turns,
                last_assistant_message: last_assistant_message
                    .map(|m| SecretScrubber::scrub(&m)),
            }
        }
        other => other,
    }
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
    Some(scrub_tail(transcript_reader::read_last_assistant_message(
        node.cli_session_id.as_deref(),
        &transcript_dir(node),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Provider;

    fn node(provider: Provider, cli_session_id: Option<&str>, use_worktree: bool) -> AgentNode {
        // `path` / `worktree_name` / `use_worktree` drive `transcript_dir`
        // (via `env::node_working_path`); `provider` gates transcript support;
        // `cli_session_id` is the on-disk session lookup key. The rest spread
        // through `..Default::default()` (issue #457).
        AgentNode {
            path: "X:\\src\\proj".to_string(),
            provider,
            cli_session_id: cli_session_id.map(str::to_string),
            worktree_name: Some("gentle-fox".to_string()),
            use_worktree,
            ..Default::default()
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

    /// Secrets the agent echoed in its transcript must be masked before the tail
    /// leaves the host for a Coordinator (ADR-0012 §5). Covers all three content
    /// surfaces: turn text, a tool call's raw `input`, and the last assistant
    /// message.
    #[test]
    fn scrub_tail_masks_secrets_in_all_content_surfaces() {
        use crate::services::transcript_reader::{ToolCall, Turn};
        let tail = TranscriptTail::Available {
            turns: vec![Turn {
                role: "assistant".to_string(),
                text: "exported GITHUB_TOKEN=ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345".to_string(),
                tool_calls: vec![ToolCall {
                    name: "Bash".to_string(),
                    input: serde_json::json!({
                        "command": "curl -H 'Authorization: Bearer abc123def456ghi789'"
                    }),
                }],
            }],
            last_assistant_message: Some("password=swordfish leaked".to_string()),
        };
        match scrub_tail(tail) {
            TranscriptTail::Available {
                turns,
                last_assistant_message,
            } => {
                assert_eq!(turns[0].text, "exported GITHUB_TOKEN=[REDACTED]");
                assert_eq!(
                    turns[0].tool_calls[0].input["command"].as_str().unwrap(),
                    "curl -H 'Authorization: Bearer [REDACTED]'"
                );
                assert_eq!(last_assistant_message.unwrap(), "password=[REDACTED] leaked");
            }
            other => panic!("expected Available, got {other:?}"),
        }
    }

    /// An `Unavailable` tail has no content to scrub and must pass through
    /// untouched — scrubbing must not change the typed degrade reason.
    #[test]
    fn scrub_tail_passes_unavailable_through_unchanged() {
        let tail = TranscriptTail::Unavailable {
            reason: UnavailableReason::NoSession,
        };
        assert_eq!(
            scrub_tail(tail),
            TranscriptTail::Unavailable {
                reason: UnavailableReason::NoSession
            }
        );
    }
}
