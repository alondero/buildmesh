//! Result-file turn completion: the lowest-fidelity Circuit turn signal.
//!
//! A Circuit prompt with a result contract asks the agent to write its final
//! report to a per-step result file (`circuit::handoff`, issue #2134), and only
//! once it has finished. A harness with no hook, watcher or native pull
//! (Freebuff, Perch, DeepSeek Harness) has no other way to say that its turn
//! ended, so it is asked for that file on every Circuit prompt and the settled
//! file becomes its turn completion. For a harness that does report its own
//! lifecycle, the same file recovers a turn whose completion signal was lost.
//!
//! The file proves a finished report and nothing more: no question, permission,
//! progress or owned background work. Those gaps stay explicit in the
//! diagnostics, and the shared report-admission policy still vetoes a handoff
//! that the last lifecycle report contradicts.

use std::path::Path;

use crate::models::AgentNode;
use crate::services::transcript_reader::report_snapshot::{ReportReadError, ReportSnapshot};

/// How long a result file must stay unchanged before it counts as written.
/// Agents write the file in one tool call, but a shell redirect or an editor
/// save can land in pieces; a file still changing is not a finished report.
pub(crate) const RESULT_SETTLE_MS: i64 = 5_000;

/// Whether this agent's harness reports no turn lifecycle of its own, so a
/// Circuit can only learn that its turn ended from a result file. An unknown
/// harness has no known lifecycle either. Resolving a stored provider reads
/// preferences, so this must not run while a database connection is held.
pub(crate) fn completes_by_result_file_only(agent: &AgentNode) -> bool {
    file_only(crate::circuit::strategy::provider_for_agent(agent))
}

/// [`completes_by_result_file_only`] for a launch selection that has no agent
/// row yet: a harness id, profile id or `harness:account` option.
pub(crate) fn selection_completes_by_result_file_only(selection: &str) -> bool {
    file_only(crate::circuit::strategy::provider_for_stored(selection))
}

fn file_only(provider: Option<crate::models::Provider>) -> bool {
    provider.is_none_or(|provider| {
        crate::circuit::compatibility::completes_by_result_file_only(
            &crate::agent::capabilities::capabilities_for(provider.adapter()),
        )
    })
}

/// The turn revision a report bound from `result` alone carries. It names the
/// step attempt the file belongs to, so a continuation is fenced to the same
/// owed file as the classification that requested it.
pub(crate) fn turn_revision(result: &Path) -> String {
    format!(
        "result-handoff:{}",
        result
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default()
    )
}

/// The revision of the agent's current turn: its transcript report when the
/// harness has one, otherwise the result-file turn it owes in this run. The
/// classification and the continuation delivery both read it through here, so
/// a file-only turn compares like with like.
pub(crate) fn current_turn_revision(run_id: i64, agent: &AgentNode) -> Option<String> {
    crate::coordinator::enrichment::assistant_report(agent)
        .map(|report| report.revision)
        .or_else(|| {
            crate::circuit::handoff::expected_result(run_id, agent.id)
                .map(|path| turn_revision(&path))
        })
}

/// When `path` was last written, if it exists and holds text.
pub(crate) fn written_at_ms(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    crate::circuit::handoff::read_result(path).ok()??;
    Some(chrono::DateTime::<chrono::Utc>::from(modified).timestamp_millis())
}

/// What a harness transcript says about activity around the result file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranscriptActivity {
    /// No transcript, or none this harness can read.
    Unavailable,
    /// The newest assistant report was published at this time.
    ReportedAt(i64),
    /// The transcript shows newer input or tool activity, or is mid-write.
    Busy,
}

impl TranscriptActivity {
    pub(crate) fn of(report: &Result<ReportSnapshot, ReportReadError>) -> Self {
        match report {
            Ok(report) => Self::ReportedAt(report.published_at_ms),
            Err(
                ReportReadError::WorkInProgress
                | ReportReadError::PartialPublication
                | ReportReadError::ChangedDuringRead,
            ) => Self::Busy,
            Err(_) => Self::Unavailable,
        }
    }
}

/// The evidence a result file must meet before it ends a Running turn.
pub(crate) struct ResultTurn<'a> {
    /// The harness has no lifecycle of its own.
    pub file_only: bool,
    pub written_at_ms: i64,
    pub now_ms: i64,
    /// Time since the agent's terminal last produced output.
    pub quiet_ms: Option<u128>,
    pub transcript: TranscriptActivity,
    /// The agent's lifecycle stamp: the file must postdate the turn's start.
    pub stamp: &'a str,
}

/// Whether the owed result file finishes the agent's current turn.
///
/// Every harness needs a settled file written after its current turn began
/// (the prompt submission moved the lifecycle stamp, and staging removed any
/// earlier file). A harness with its own lifecycle is only being recovered
/// from a lost signal, so it must also look finished on its other evidence:
/// a quiet terminal for the lost-turn window, and no transcript activity
/// after the file was written.
pub(crate) fn finishes_turn(turn: &ResultTurn<'_>) -> bool {
    let settled = turn.now_ms.saturating_sub(turn.written_at_ms) >= RESULT_SETTLE_MS;
    let current = crate::db::agent_turn_stamp_precedes(turn.stamp, turn.written_at_ms);
    settled
        && current
        && (turn.file_only
            || (turn
                .quiet_ms
                .is_some_and(|quiet| quiet >= super::turn_classify::LOST_TURN_QUIET_MS)
                && match turn.transcript {
                    TranscriptActivity::Unavailable => true,
                    TranscriptActivity::ReportedAt(at) => at <= turn.written_at_ms,
                    TranscriptActivity::Busy => false,
                }))
}

#[cfg(test)]
#[path = "result_handoff_tests.rs"]
mod tests;
