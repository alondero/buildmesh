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

use crate::agent::provider::muse::telemetry::{self, ObservedMuseSessionTelemetry};
use crate::env;
use crate::models::AgentNode;
use crate::secret_scrubber::SecretScrubber;
use crate::services::transcript_reader::{
    self, TranscriptFormat, TranscriptTail, UnavailableReason,
};

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

/// The transcript format a node's native completion is read with. A node with a
/// launch snapshot is read as the harness it was launched with, whatever its
/// profile maps to today; an unknown harness has no reader (fail closed).
pub(crate) fn native_completion_format(node: &AgentNode) -> Option<TranscriptFormat> {
    native_completion_format_for(crate::circuit::strategy::provider_for_agent(node)?)
}

/// As [`native_completion_format`] for a harness the caller already resolved.
pub(crate) fn native_completion_format_for(
    provider: crate::models::Provider,
) -> Option<TranscriptFormat> {
    let adapter = provider.adapter();
    if !adapter.produces_readable_transcript() {
        return None;
    }
    // No reader wired (issue #1817) means no native completion to observe.
    TranscriptFormat::for_harness(adapter.id())
}

pub(crate) fn native_turn_completion(
    node: &AgentNode,
) -> Option<transcript_reader::NativeTurnSnapshot> {
    native_turn_completion_for(node, crate::circuit::strategy::provider_for_agent(node)?)
}

/// Read the completion record with the reader of a harness the caller already
/// resolved, so the strategy that chose the read and the reader that serves it
/// can never come from two separate resolutions.
pub(crate) fn native_turn_completion_for(
    node: &AgentNode,
    provider: crate::models::Provider,
) -> Option<transcript_reader::NativeTurnSnapshot> {
    transcript_reader::read_native_turn_completion(
        native_completion_format_for(provider)?,
        node.cli_session_id.as_deref(),
        &transcript_dir(node),
    )
}

/// Read a node's transcript tail, gated on its provider's capability. A provider
/// that produces no readable transcript degrades to `Unsupported` *without*
/// touching the filesystem — the same degrade-and-flag rule the digest applies,
/// so `/nodes` and `/nodes/{id}/log` agree on why a node has no rich layer (an
/// unsupported provider never masquerades as a supported one that merely hasn't
/// captured a session). Pure over the node + filesystem, so it is unit-testable
/// without a DB.
pub fn transcript_tail(node: &AgentNode, tail: usize) -> TranscriptTail {
    let adapter = crate::preferences::resolve_harness_provider(&node.provider).adapter();
    if !adapter.produces_readable_transcript() {
        return TranscriptTail::Unavailable {
            reason: UnavailableReason::Unsupported,
        };
    }
    // No reader wired for this harness (issue #1817): degrade to
    // `Unsupported` instead of reading another harness's directory.
    let Some(format) = TranscriptFormat::for_harness(adapter.id()) else {
        return TranscriptTail::Unavailable {
            reason: UnavailableReason::Unsupported,
        };
    };
    scrub_tail(transcript_reader::read_tail(
        format,
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
                last_assistant_message: last_assistant_message.map(|m| SecretScrubber::scrub(&m)),
            }
        }
        other => other,
    }
}

/// Observed Muse MSP session telemetry for a Node Digest. Non-Muse
/// harnesses return `None` without touching the telemetry store.
pub fn observed_session_telemetry(node: &AgentNode) -> Option<ObservedMuseSessionTelemetry> {
    telemetry::snapshot_if_muse(&node.provider, node.id)
}

/// The enrichment a Node Digest layers on, in the shape `node_digest::layered`
/// expects: `None` is the unsupported-provider signal (which keeps `node_digest`
/// provider-agnostic — it never learns the `Unsupported` reason), while a
/// supported provider that simply has no transcript yet comes back as
/// `Some(Unavailable{ NoSession | NoTranscript | … })`.
///
/// Uses the **bounded** reader (issue #341): a digest only needs the last
/// assistant message, so this parses just the tail bytes of the transcript
/// rather than the whole file. For a `GET /nodes` poll over many Claude Code nodes
/// with long histories that turns N full-file parses into N bounded ones. The
/// full-tail [`transcript_tail`] is reserved for the on-demand `/log` drill-in.
pub fn digest_enrichment(node: &AgentNode) -> Option<TranscriptTail> {
    let adapter = crate::preferences::resolve_harness_provider(&node.provider).adapter();
    if !adapter.produces_readable_transcript() {
        return None;
    }
    // No reader wired for this harness (issue #1817): surface `Unsupported`
    // rather than `None` so the digest flags a wiring gap instead of
    // masquerading as an unsupported provider.
    let Some(format) = TranscriptFormat::for_harness(adapter.id()) else {
        return Some(TranscriptTail::Unavailable {
            reason: UnavailableReason::Unsupported,
        });
    };
    Some(scrub_tail(transcript_reader::read_last_assistant_message(
        format,
        node.cli_session_id.as_deref(),
        &transcript_dir(node),
    )))
}

pub(crate) fn assistant_report(node: &AgentNode) -> Option<transcript_reader::AssistantReport> {
    let adapter = crate::preferences::resolve_harness_provider(&node.provider).adapter();
    if !adapter.produces_readable_transcript() {
        return None;
    }
    // No reader wired (issue #1817): fall back to live per-turn PTY
    // observation like any other transcript-less harness.
    let format = TranscriptFormat::for_harness(adapter.id())?;
    let mut report = transcript_reader::read_assistant_report(
        format,
        node.cli_session_id.as_deref(),
        &transcript_dir(node),
    )?;
    report.text = SecretScrubber::scrub(&report.text);
    Some(report)
}

pub(crate) fn circuit_report_snapshot(
    node: &AgentNode,
) -> Result<
    transcript_reader::report_snapshot::ReportSnapshot,
    transcript_reader::report_snapshot::ReportReadError,
> {
    use transcript_reader::report_snapshot::ReportReadError;
    let adapter = crate::preferences::resolve_harness_provider(&node.provider).adapter();
    let format = TranscriptFormat::for_harness(adapter.id()).ok_or(ReportReadError::Unsupported)?;
    transcript_reader::report_snapshot::read(
        format,
        node.cli_session_id
            .as_deref()
            .ok_or(ReportReadError::NoSession)?,
        &transcript_dir(node),
    )
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
            // `provider` is stored as its harness-id string (issue #535); the
            // resolver maps it back to an executor for the capability gate.
            provider: provider.to_string(),
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
        assert_ne!(
            dir, n.path,
            "must not search the mesh root for a worktree node"
        );
    }

    /// A Root Node's transcript is keyed under the Mesh root itself.
    #[test]
    fn root_node_searches_mesh_root() {
        let n = node(Provider::Anthropic, Some("sid"), false);
        assert!(!transcript_dir(&n).contains("worktrees"));
    }

    /// An unsupported provider degrades to `Unsupported` without reading disk —
    /// distinct from a supported-but-unstarted node, so a Coordinator can tell
    /// "never has a transcript" from "hasn't captured a session yet". Uses
    /// `Provider::Freebuff` here because it is still transcript-less (#1437);
    /// OpenCode was the prior example but flipped to true in #1296.
    #[test]
    fn unsupported_provider_degrades_without_disk() {
        let tail = transcript_tail(&node(Provider::Freebuff, None, true), 10);
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
    /// digest flags it distinctly. Uses `Provider::Freebuff` for the same
    /// reason as the `unsupported_provider_degrades_without_disk` test:
    /// OpenCode flipped to true in #1296.
    #[test]
    fn observed_session_telemetry_is_none_for_non_muse_nodes() {
        assert!(observed_session_telemetry(&node(Provider::Anthropic, None, true)).is_none());
        assert!(observed_session_telemetry(&node(Provider::Muse, None, true)).is_none());
    }

    #[test]
    fn digest_enrichment_maps_unsupported_to_none_but_keeps_no_session() {
        assert!(digest_enrichment(&node(Provider::Freebuff, None, true)).is_none());
        assert_eq!(
            digest_enrichment(&node(Provider::Anthropic, None, true)),
            Some(TranscriptTail::Unavailable {
                reason: UnavailableReason::NoSession
            })
        );
    }

    /// Codex produces a readable transcript now (issue #887): the gate must
    /// let a codex node through to the reader rather than degrading to
    /// `Unsupported`. With no captured session the typed reason is `NoSession`
    /// — proof the gate passed and the reader actually ran.
    #[test]
    fn codex_provider_passes_the_capability_gate() {
        let tail = transcript_tail(&node(Provider::Codex, None, true), 10);
        assert_eq!(
            tail,
            TranscriptTail::Unavailable {
                reason: UnavailableReason::NoSession
            }
        );
        assert_eq!(
            digest_enrichment(&node(Provider::Codex, None, true)),
            Some(TranscriptTail::Unavailable {
                reason: UnavailableReason::NoSession
            })
        );
    }

    /// Issue #1912: a harness with no wired report adapter must yield an
    /// explicit `Unsupported` — never another provider's parsed report. The
    /// gate is the `TranscriptFormat::for_harness` resolver, so no file read
    /// (and no fallback to the Claude Code directory) happens for these ids.
    #[test]
    fn unwired_harness_report_is_explicitly_unsupported_not_another_parser() {
        use crate::services::transcript_reader::report_snapshot::ReportReadError;
        for provider in [
            Provider::Kimi,
            Provider::Dsh,
            Provider::Freebuff,
            Provider::Terminal,
        ] {
            let label = format!("{provider:?}");
            assert_eq!(
                circuit_report_snapshot(&node(provider, Some("sid"), true)).err(),
                Some(ReportReadError::Unsupported),
                "{label} has no wired report adapter"
            );
        }
    }

    #[test]
    fn unwired_harness_dispatch_preserves_the_spine_and_flags_unavailable() {
        use crate::coordinator::node_digest::{layered, Enrichment, EnrichmentUnavailable};
        use crate::models::SessionStatus;

        for provider in [
            Provider::Kimi,
            Provider::Dsh,
            Provider::Freebuff,
            Provider::Terminal,
        ] {
            let mut node = node(provider, Some("a-claude-session-id"), true);
            node.id = 1877;
            node.name = "Waiting node".into();
            node.status = SessionStatus::AwaitingInput;
            assert_eq!(TranscriptFormat::for_harness(&node.provider), None);
            let rich = digest_enrichment(&node);
            let changed_at = chrono::DateTime::parse_from_rfc3339("2026-09-23T10:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc);
            let digest = layered(&node, "Transcript mesh", changed_at, rich.as_ref());
            assert_eq!(digest.id, 1877);
            assert_eq!(digest.name, "Waiting node");
            assert_eq!(digest.mesh, "Transcript mesh");
            assert_eq!(digest.status, "awaiting_input");
            assert!(digest.needs_feedback);
            assert_eq!(digest.waiting_since, Some(changed_at));
            assert_eq!(digest.last_activity, changed_at);
            assert_eq!(
                digest.enrichment,
                Enrichment::Unavailable {
                    reason: EnrichmentUnavailable::Unsupported,
                }
            );
            assert_eq!(
                transcript_tail(&node, 1),
                TranscriptTail::unavailable(UnavailableReason::Unsupported)
            );
        }
    }

    /// Issue #1776 — the asymmetric case, and the reason Cline must not be
    /// folded into the list above. Cline *does* have a transcript reader (the
    /// Node Digest is rich), but it reads a single JSON **document**, not a
    /// line-oriented record stream, so it has no report adapter. The circuit
    /// report must still answer `Unsupported` rather than misreporting a
    /// complete document as a partial publication or a missing report.
    #[test]
    fn cline_has_a_digest_reader_but_no_report_adapter() {
        use crate::services::transcript_reader::report_snapshot::ReportReadError;
        let digest_node = node(Provider::Cline, None, true);
        // The digest reader is live: an uncaptured session is `NoSession`, not
        // `Unsupported` (which would mean "no reader wired" and flag a gap).
        assert_eq!(
            digest_enrichment(&digest_node),
            Some(TranscriptTail::Unavailable {
                reason: UnavailableReason::NoSession
            })
        );
        // The report read stays `Unsupported` even with a session id in hand —
        // the circuit then continues on Cline's `agent_end` hook receipt
        // instead of being handed a bogus document read.
        assert_eq!(
            circuit_report_snapshot(&node(Provider::Cline, Some("sid"), true)).err(),
            Some(ReportReadError::Unsupported),
            "a Cline document must never be reported as a line-oriented report"
        );
    }

    /// Issue #1776: Cline produces a readable transcript, so the digest gate
    /// must let a cline node through to the reader rather than degrading to
    /// `Unsupported`. With no captured session the typed reason is `NoSession`
    /// — proof the gate passed and the reader actually ran (and that it did not
    /// silently fall back to another harness's parser). The second case
    /// carries a session id but no on-disk store: the `NoTranscript` rung.
    #[test]
    fn cline_provider_passes_the_capability_gate() {
        assert_eq!(
            transcript_tail(&node(Provider::Cline, None, true), 10),
            TranscriptTail::Unavailable {
                reason: UnavailableReason::NoSession
            }
        );
        // A session id with no matching Cline store degrades as a *missing
        // transcript*, never as an unsupported harness. Deterministic: `sid` is
        // not a Cline session id, so the locator rejects it whatever the
        // machine's `~/.cline` happens to hold.
        assert_eq!(
            transcript_tail(&node(Provider::Cline, Some("sid"), true), 10),
            TranscriptTail::Unavailable {
                reason: UnavailableReason::NoTranscript
            }
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
                assert_eq!(
                    last_assistant_message.unwrap(),
                    "password=[REDACTED] leaked"
                );
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
