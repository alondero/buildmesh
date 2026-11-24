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

use crate::agent::provider::muse::telemetry::ObservedMuseSessionTelemetry;
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
    /// Observed MSP session telemetry for Muse nodes (issue #1680). Absent
    /// when this node has no observations. This is **not** a Usage Meter:
    /// token counts are local session facts, never remaining account quota.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_session_telemetry: Option<ObservedMuseSessionTelemetry>,
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
pub fn spine(node: &AgentNode, mesh_name: &str, status_changed_at: DateTime<Utc>) -> NodeDigest {
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
        observed_session_telemetry: None,
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
#[path = "node_digest_tests.rs"]
mod node_digest_tests;
