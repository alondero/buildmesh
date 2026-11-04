//! Node Glance — what an Agent Node is doing, read on demand.
//!
//! A glance is the quick answer a person wants before opening a terminal: what
//! the agent was asked, what it is doing about it, what it last said, and how
//! long it has been at it. It layers a transcript read over the node's own
//! timings, with the same degrade-and-flag rule as the Node Digest: a harness
//! with no readable transcript, or a transcript that cannot be read, still
//! answers with its timings and an explicit reason, never a silently empty
//! summary.
//!
//! It is read only when a client asks for one node, never on a list poll, and
//! the transcript layer is cached against the store's change stamp, so a phone
//! re-reading the node it shows costs a row read and a metadata check while the
//! agent is quiet.

use serde::Serialize;
use ts_rs::TS;

use crate::models::{AgentHistoryEntry, SessionStatus};
use crate::read_outcome::{ReadOutcome, Unavailable};
use crate::services::transcript_reader::{TranscriptGlance, UnavailableReason};

/// The at-a-glance summary of one Agent Node.
///
/// Times are epoch milliseconds on the desktop's clock. `observed_at_ms` is
/// that clock when the glance was read, so a client on another device measures
/// elapsed time against it rather than its own clock.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export, export_to = "NodeGlance.ts")]
pub struct NodeGlance {
    #[ts(as = "i32")]
    pub node_id: i64,
    /// The node's status when the glance was read.
    pub status: SessionStatus,
    #[ts(type = "number")]
    pub observed_at_ms: i64,
    /// When the node entered `status`.
    #[ts(type = "number | null")]
    pub status_since_ms: Option<i64>,
    /// When the agent process last started; `None` for a node that never ran.
    #[ts(type = "number | null")]
    pub session_started_at_ms: Option<i64>,
    #[ts(type = "number")]
    pub created_at_ms: i64,
    /// The transcript layer, when the harness's transcript could be read.
    pub transcript: Option<TranscriptGlance>,
    /// Why there is no transcript layer. Exactly one of `transcript` and this
    /// is present.
    pub transcript_unavailable: Option<UnavailableReason>,
}

/// Shape a glance from a node row and its transcript read. Pure, so the
/// layering is testable without a database or a transcript on disk.
pub fn compose(
    entry: &AgentHistoryEntry,
    transcript: Result<TranscriptGlance, UnavailableReason>,
    observed_at_ms: i64,
) -> NodeGlance {
    let (transcript, transcript_unavailable) = match transcript {
        Ok(glance) => (Some(glance), None),
        Err(reason) => (None, Some(reason)),
    };
    NodeGlance {
        node_id: entry.node.id,
        status: entry.node.status,
        observed_at_ms,
        status_since_ms: entry.status_changed_at.map(|at| at.timestamp_millis()),
        session_started_at_ms: entry.session_started_at,
        created_at_ms: entry.node.created_at.timestamp_millis(),
        transcript,
        transcript_unavailable,
    }
}

/// Read one node's glance. Blocking: a database row, then the transcript read
/// with the connection already returned, so file I/O never holds a reader.
/// A missing node is [`Unavailable::Absent`].
pub fn read_blocking(node_id: i64) -> Result<NodeGlance, Unavailable> {
    let entry = ReadOutcome::absent_or_unavailable(crate::db::get_agent_history_entry(node_id))
        .into_result()?;
    let transcript = crate::coordinator::enrichment::transcript_glance(&entry.node);
    Ok(compose(
        &entry,
        transcript,
        chrono::Utc::now().timestamp_millis(),
    ))
}

#[cfg(test)]
#[path = "node_glance_tests.rs"]
mod tests;
