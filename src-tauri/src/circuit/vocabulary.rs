//! Canonical Circuit Run / Circuit Step ledger vocabulary.
//!
//! [`RunState`] and [`StepStatus`] own the stored strings, the
//! `Queued` ↔ `pending_slot` mapping, and the terminal predicates.
//! Persistence, the worker, IPC, and the UI import these — they must
//! not re-spell the tokens (issue #1660).
//!
//! The one historical alias is [`StepStatus::QUEUED_LEGACY_DB_STR`]
//! (`queued`): old step rows may still carry it, so IN-lists and
//! [`StepStatus::from_db_str`] accept it, but new writes always use
//! [`StepStatus::as_db_str`] (`pending_slot`).

use serde::{Deserialize, Serialize};

/// Circuit Run row `state`. Generated to `RunState.ts` so the UI imports
/// the same union the ledger stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "RunState.ts")]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Run row exists, trigger not yet processed.
    Pending,
    Running,
    /// Graceful pause (#1207): the current step may finish but the graph
    /// does not advance until resumed.
    Paused,
    Completed,
    Failed,
    /// Terminal cancel (user or worker). Present in the DB vocabulary and
    /// the terminal predicate; previously missing here, which made
    /// `from_db_str("cancelled")` resurrect a cancelled row as Pending.
    Cancelled,
}

impl RunState {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_db_str(s: &str) -> Self {
        match s {
            "running" => Self::Running,
            "paused" => Self::Paused,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::Pending,
        }
    }

    /// Terminal states the worker never moves out of.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// String form of [`Self::is_terminal`]. Unknown tokens are live
    /// (not terminal) — unlike [`Self::from_db_str`], which maps garbage
    /// to [`Self::Pending`].
    pub fn is_terminal_db_str(s: &str) -> bool {
        matches!(s, "completed" | "failed" | "cancelled")
    }

    /// Already-admitted runs that occupy a `circuit_run_capacity` slot.
    pub const fn is_admitted(self) -> bool {
        matches!(self, Self::Running | Self::Paused)
    }

    pub fn is_admitted_db_str(s: &str) -> bool {
        matches!(s, "running" | "paused")
    }

    /// Not-yet-terminal run rows the worker still drives.
    pub const fn is_live(self) -> bool {
        matches!(self, Self::Pending | Self::Running | Self::Paused)
    }

    pub fn is_live_db_str(s: &str) -> bool {
        matches!(s, "pending" | "running" | "paused")
    }

    /// Trusted SQL `IN (...)` list for live run rows. Compile-time tokens
    /// only — never interpolate user input through this.
    pub const SQL_IN_LIVE: &'static str = "'pending', 'running', 'paused'";
    /// Admitted runs (`running` + `paused`) counted against mesh capacity.
    pub const SQL_IN_ADMITTED: &'static str = "'running', 'paused'";
    /// Terminal run states that release a capacity slot.
    pub const SQL_IN_TERMINAL: &'static str = "'completed', 'failed', 'cancelled'";
    /// Terminal run states whose rows the retention sweep may delete.
    ///
    /// A cancelled run is a terminal run, and its history is the same audit
    /// trail a failed run's is — so it is retained for the same window and
    /// swept on the same terms (issue #2600). Cancelling a run used to leave
    /// its history behind forever: the sweep skipped cancelled runs, so a
    /// cancelled run's observations accumulated without bound (one such run
    /// held 306k history rows).
    ///
    /// The cleanup retry is not what keeps a cancelled run alive. That anchor
    /// is the *node-scoped* `agent_node_lifecycle_leases` row, which the
    /// sweep already refuses to delete past: a run whose step still has a
    /// pending cleanup is not swept, whatever state it is in. Once the
    /// cleanup is done the lease releases and the run becomes sweepable like
    /// any other terminal run.
    pub const SQL_IN_SWEEPABLE: &'static str = "'completed', 'failed', 'cancelled'";
}

/// Circuit Step row `status`. `Queued` stores as `pending_slot`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "StepStatus.ts")]
pub enum StepStatus {
    /// Parked because a concurrency/agent-slot limit blocked it; promotes
    /// FIFO when capacity frees (stored as `pending_slot`).
    #[serde(rename = "pending_slot")]
    Queued,
    #[serde(rename = "running")]
    Running,
    /// Parked on a CollaboratorCheck RequireApproval gate (#1207):
    /// waiting for the user's Approve click. Not terminal.
    #[serde(rename = "blocked")]
    Blocked,
    /// Evidence is insufficient; neither failure nor permission to advance.
    #[serde(rename = "unverified")]
    Unverified,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "cancelled")]
    Cancelled,
}

impl StepStatus {
    /// Historical ledger spelling of [`Self::Queued`]. New writes use
    /// `pending_slot`; reads still accept this alias.
    pub const QUEUED_LEGACY_DB_STR: &'static str = "queued";

    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Queued => "pending_slot",
            Self::Running => "running",
            Self::Blocked => "blocked",
            Self::Unverified => "unverified",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_db_str(s: &str) -> Self {
        match s {
            "pending_slot" | "queued" => Self::Queued,
            "running" => Self::Running,
            "blocked" => Self::Blocked,
            "unverified" => Self::Unverified,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::Queued,
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn is_terminal_db_str(s: &str) -> bool {
        matches!(s, "completed" | "failed" | "cancelled")
    }

    pub fn is_queued_db_str(s: &str) -> bool {
        s == Self::Queued.as_db_str() || s == Self::QUEUED_LEGACY_DB_STR
    }

    /// In-flight (not terminal) step statuses, including the legacy
    /// `queued` alias so cancellation still covers old rows.
    pub const SQL_IN_IN_FLIGHT: &'static str =
        "'pending_slot', 'queued', 'running', 'blocked', 'unverified'";
}

#[cfg(test)]
#[path = "vocabulary_tests.rs"]
mod tests;
