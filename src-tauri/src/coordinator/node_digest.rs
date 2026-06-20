//! Node Digest — the coordinator-facing read summary of a single Agent Node.
//!
//! See ADR-0008 and `CONTEXT.md`. A digest is layered: an always-available
//! **spine** from Buildmesh's own database (lifecycle `status`,
//! `needs_feedback` = `awaiting_input`, `waiting_since`, `last_activity`)
//! plus — for the Claude Code provider family only — semantic **enrichment**
//! read from the on-disk JSONL transcript.
//!
//! This module is deliberately **pure** over its inputs (a node row, its Mesh
//! name, the status-change timestamp, and — for the rich layer — an
//! already-read [`TranscriptTail`]) so every field is testable without a PTY,
//! a live database, or the filesystem. The HTTP route does the transcript file
//! read and hands the result here; the layering itself never touches disk.
//!
//! The enrichment layer (issue #317) is *degrade-and-flag*: a node whose
//! provider has no readable transcript, or whose transcript fails to parse,
//! gets a spine-only digest with [`Enrichment::Unavailable`] carrying a typed
//! reason — never a silently-omitted field, so a busy node can never read as
//! quiet (ADR-0008 §3).

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::models::{AgentNode, SessionStatus};
use crate::services::transcript_reader::{TranscriptTail, UnavailableReason};

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
    /// The rich layer read from the on-disk transcript (issue #317). Always
    /// present and always tagged: either the truncated last assistant message,
    /// or an explicit `unavailable` reason. Never silently absent — a busy node
    /// must never read as quiet.
    pub enrichment: Enrichment,
}

/// The transcript-derived rich layer of a digest. Serializes to a
/// `{"status": "available" | "unavailable", ...}` envelope so it is
/// `curl`-inspectable and shaped for a later MCP wrap.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Enrichment {
    Available {
        /// The most recent assistant text, truncated. When the node is
        /// `awaiting_input` this *is the question it is blocked on* (ADR-0008
        /// §4) — the highest-value field in the whole digest. `None` if the
        /// transcript carries no assistant text yet.
        last_assistant_message: Option<String>,
    },
    Unavailable {
        reason: EnrichmentUnavailable,
    },
}

/// Why a digest has no rich layer. Distinguishes a provider that *can't* produce
/// a readable transcript (`Unsupported`) from one that can but whose transcript
/// this time couldn't be read — so a Coordinator can tell "never enriched" from
/// "degraded right now". Mirrors [`UnavailableReason`] one-for-one; the
/// exhaustive [`From`] impl below makes adding a reader reason a compile error
/// here until it's handled.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentUnavailable {
    /// The provider does not produce a readable transcript (capability flag off).
    Unsupported,
    /// Supported provider, but the node has no captured CLI session id.
    NoSession,
    /// Supported provider, but no transcript file exists on disk yet.
    NoTranscript,
    /// The transcript file exists but could not be opened or read.
    Unreadable,
    /// The file was read and well-formed but carried no recognizable turns yet —
    /// a genuinely quiet/new session. Distinct from `ShapeChanged` so a busy
    /// node's broken rich layer ("page me") never reads the same as "nothing has
    /// happened yet" (issue #335).
    Empty,
    /// The file was read but a malformed message line proved the Claude Code
    /// JSONL shape has changed. Degrades loudly so a busy node never looks quiet.
    ShapeChanged,
}

impl From<UnavailableReason> for EnrichmentUnavailable {
    fn from(reason: UnavailableReason) -> Self {
        match reason {
            UnavailableReason::Unsupported => EnrichmentUnavailable::Unsupported,
            UnavailableReason::NoSession => EnrichmentUnavailable::NoSession,
            UnavailableReason::NoTranscript => EnrichmentUnavailable::NoTranscript,
            UnavailableReason::Unreadable => EnrichmentUnavailable::Unreadable,
            UnavailableReason::Empty => EnrichmentUnavailable::Empty,
            UnavailableReason::ShapeChanged => EnrichmentUnavailable::ShapeChanged,
        }
    }
}

/// Compute the enrichment layer. Pure: the caller did the file I/O. `tail` is
/// `Some` exactly when the provider produces a readable transcript (the caller
/// read it); `None` is the unsupported-provider signal — so this function never
/// needs to know which providers those are.
pub fn enrichment(tail: Option<&TranscriptTail>) -> Enrichment {
    match tail {
        None => Enrichment::Unavailable {
            reason: EnrichmentUnavailable::Unsupported,
        },
        Some(TranscriptTail::Available {
            last_assistant_message,
            ..
        }) => Enrichment::Available {
            last_assistant_message: last_assistant_message.clone(),
        },
        Some(TranscriptTail::Unavailable { reason }) => Enrichment::Unavailable {
            reason: (*reason).into(),
        },
    }
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
        provider: node.provider.clone(),
        status: node.status.to_db_str().to_string(),
        needs_feedback,
        // The moment a node became blocked IS its last status change, so reuse
        // the same timestamp — only surfaced while it is actually waiting.
        waiting_since: needs_feedback.then_some(status_changed_at),
        last_activity: status_changed_at,
        // A bare spine carries no rich layer yet. Default to the conservative,
        // loudest degrade (`Unsupported`); `layered()` overrides it with the
        // real enrichment for providers that produce a transcript.
        enrichment: Enrichment::Unavailable {
            reason: EnrichmentUnavailable::Unsupported,
        },
    }
}

/// The full layered digest: [`spine`] plus the transcript-derived [`enrichment`]
/// layer. Pure — the caller reads the transcript file (only for providers that
/// produce one) and passes the result as `tail`; `None` means the provider has
/// no readable transcript. This is the seam the HTTP route builds every digest
/// through.
pub fn layered(
    node: &AgentNode,
    mesh_name: &str,
    status_changed_at: DateTime<Utc>,
    tail: Option<&TranscriptTail>,
) -> NodeDigest {
    NodeDigest {
        enrichment: enrichment(tail),
        ..spine(node, mesh_name, status_changed_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Provider;

    fn node(status: SessionStatus, provider: Provider) -> AgentNode {
        // `spine()` only reads `id` / `name` / `status` / `provider`; the
        // rest spread through `..Default::default()` (issue #457).
        AgentNode {
            id: 7,
            name: "fix-login".to_string(),
            // `provider` is stored as its harness-id string (issue #535).
            provider: provider.to_string(),
            status,
            ..Default::default()
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
            "waiting_since", "last_activity", "enrichment",
        ] {
            assert!(json.get(key).is_some(), "digest JSON must carry `{key}`");
        }
        assert_eq!(json["needs_feedback"], serde_json::json!(true));
        assert_eq!(json["waiting_since"], serde_json::json!("2026-06-14T10:00:00Z"));
    }

    // --- Enrichment layering (issue #317): the four degrade-and-flag paths ---

    fn available(last: Option<&str>) -> TranscriptTail {
        TranscriptTail::Available {
            turns: vec![],
            last_assistant_message: last.map(str::to_string),
        }
    }

    #[test]
    fn supported_provider_with_good_transcript_is_enriched() {
        let tail = available(Some("Found it — shall I apply the fix?"));
        let digest = layered(
            &node(SessionStatus::Running, Provider::Anthropic),
            "core",
            Utc::now(),
            Some(&tail),
        );
        assert_eq!(
            digest.enrichment,
            Enrichment::Available {
                last_assistant_message: Some("Found it — shall I apply the fix?".to_string()),
            }
        );
    }

    #[test]
    fn unsupported_provider_degrades_to_unsupported_flag() {
        // `None` tail is the unsupported-provider signal the route passes.
        let digest = layered(
            &node(SessionStatus::Running, Provider::OpenCode),
            "core",
            Utc::now(),
            None,
        );
        assert_eq!(
            digest.enrichment,
            Enrichment::Unavailable {
                reason: EnrichmentUnavailable::Unsupported,
            },
            "an unsupported provider is flagged, never silently omitted"
        );
    }

    #[test]
    fn parse_failure_degrades_to_shape_changed_flag() {
        // A supported provider whose transcript won't parse must NOT look quiet.
        let tail = TranscriptTail::Unavailable {
            reason: UnavailableReason::ShapeChanged,
        };
        let digest = layered(
            &node(SessionStatus::Running, Provider::Anthropic),
            "core",
            Utc::now(),
            Some(&tail),
        );
        assert_eq!(
            digest.enrichment,
            Enrichment::Unavailable {
                reason: EnrichmentUnavailable::ShapeChanged,
            }
        );
    }

    #[test]
    fn awaiting_input_node_surfaces_the_blocking_question_as_its_message() {
        // The highest-value path: a blocked node's last assistant message IS the
        // question it is stuck on, and the spine still flags it needs feedback.
        let question = "Found it — the redirect drops the query string. Shall I apply the fix?";
        let tail = available(Some(question));
        let digest = layered(
            &node(SessionStatus::AwaitingInput, Provider::Anthropic),
            "core",
            Utc::now(),
            Some(&tail),
        );

        assert!(digest.needs_feedback, "spine still flags it as blocked");
        assert!(digest.waiting_since.is_some());
        assert_eq!(
            digest.enrichment,
            Enrichment::Available {
                last_assistant_message: Some(question.to_string()),
            },
            "the blocking question is surfaced as the last assistant message"
        );
    }

    #[test]
    fn every_reader_reason_maps_to_a_distinct_unavailable_flag() {
        // Lock the reader-reason → digest-flag bridge so a renamed reason can't
        // silently collapse into the wrong (or `Unsupported`) bucket.
        use EnrichmentUnavailable as E;
        let cases = [
            (UnavailableReason::Unsupported, E::Unsupported),
            (UnavailableReason::NoSession, E::NoSession),
            (UnavailableReason::NoTranscript, E::NoTranscript),
            (UnavailableReason::Unreadable, E::Unreadable),
            (UnavailableReason::Empty, E::Empty),
            (UnavailableReason::ShapeChanged, E::ShapeChanged),
        ];
        for (reader, expected) in cases {
            assert_eq!(EnrichmentUnavailable::from(reader), expected);
        }
    }

    #[test]
    fn enrichment_serializes_to_status_envelope() {
        let enriched: serde_json::Value = serde_json::to_value(Enrichment::Available {
            last_assistant_message: Some("hi".to_string()),
        })
        .unwrap();
        assert_eq!(enriched["status"], "available");
        assert_eq!(enriched["last_assistant_message"], "hi");

        let degraded: serde_json::Value = serde_json::to_value(Enrichment::Unavailable {
            reason: EnrichmentUnavailable::Unsupported,
        })
        .unwrap();
        assert_eq!(degraded["status"], "unavailable");
        assert_eq!(degraded["reason"], "unsupported");
    }
}
