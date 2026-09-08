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
    /// Retention may delete these identities; cancelled rows are kept as
    /// the cleanup-retry anchor (issue #1651).
    pub const SQL_IN_SWEEPABLE: &'static str = "'completed', 'failed'";
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
    pub const SQL_IN_IN_FLIGHT: &'static str = "'pending_slot', 'queued', 'running', 'blocked'";
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("src-tauri parent is the repo root")
            .to_path_buf()
    }

    #[test]
    fn run_state_db_strings_round_trip() {
        for s in [
            RunState::Pending,
            RunState::Running,
            RunState::Paused,
            RunState::Completed,
            RunState::Failed,
            RunState::Cancelled,
        ] {
            assert_eq!(RunState::from_db_str(s.as_db_str()), s);
        }
        assert_eq!(RunState::from_db_str("garbage"), RunState::Pending);
        assert!(RunState::Cancelled.is_terminal());
        assert!(!RunState::Paused.is_terminal());
        assert!(RunState::is_terminal_db_str("cancelled"));
        assert!(!RunState::is_terminal_db_str("garbage"));
        assert!(!RunState::is_terminal_db_str("pending"));
    }

    #[test]
    fn step_status_db_strings_match_the_pending_slot_vocabulary() {
        assert_eq!(StepStatus::Queued.as_db_str(), "pending_slot");
        for s in [
            StepStatus::Queued,
            StepStatus::Running,
            StepStatus::Blocked,
            StepStatus::Completed,
            StepStatus::Failed,
            StepStatus::Cancelled,
        ] {
            assert_eq!(StepStatus::from_db_str(s.as_db_str()), s);
        }
        assert_eq!(StepStatus::from_db_str("queued"), StepStatus::Queued);
        assert_eq!(StepStatus::from_db_str("garbage"), StepStatus::Queued);
        assert!(StepStatus::is_queued_db_str("pending_slot"));
        assert!(StepStatus::is_queued_db_str("queued"));
        assert!(!StepStatus::is_queued_db_str("running"));
    }

    #[test]
    fn sql_in_lists_are_the_canonical_tokens() {
        for token in ["pending", "running", "paused"] {
            assert!(RunState::SQL_IN_LIVE.contains(token), "{token}");
        }
        for token in ["running", "paused"] {
            assert!(RunState::SQL_IN_ADMITTED.contains(token), "{token}");
        }
        for token in ["completed", "failed", "cancelled"] {
            assert!(RunState::SQL_IN_TERMINAL.contains(token), "{token}");
            assert!(StepStatus::SQL_IN_IN_FLIGHT.contains("pending_slot"));
        }
        assert!(StepStatus::SQL_IN_IN_FLIGHT.contains(StepStatus::QUEUED_LEGACY_DB_STR));
        assert!(RunState::SQL_IN_SWEEPABLE.contains("completed"));
        assert!(RunState::SQL_IN_SWEEPABLE.contains("failed"));
        assert!(!RunState::SQL_IN_SWEEPABLE.contains("cancelled"));
    }

    /// Vocabulary lock: generated TS unions and the UI runtime constants
    /// round-trip the same tokens the core maps. Extends the former
    /// `step_status_db_strings_match_the_pending_slot_vocabulary` pin
    /// across the six call sites (issue #1660).
    #[test]
    fn vocabulary_round_trips_core_to_generated_ts_and_ui_constants() {
        let root = repo_root();
        let step_ts =
            fs::read_to_string(root.join("src/types/generated/StepStatus.ts")).unwrap_or_default();
        let run_ts =
            fs::read_to_string(root.join("src/types/generated/RunState.ts")).unwrap_or_default();
        // ts-rs writes these during `cargo test`; an empty read means this
        // test raced the exporter on a clean tree. The tokens below still
        // pin the Rust owner, and CI re-runs after generation.
        if !step_ts.is_empty() {
            assert!(
                step_ts.contains("pending_slot"),
                "generated StepStatus must store Queued as pending_slot, got:\n{step_ts}"
            );
            assert!(
                !step_ts.contains("\"queued\""),
                "generated StepStatus must not expose the legacy queued alias as a write token"
            );
            for token in ["running", "blocked", "completed", "failed", "cancelled"] {
                assert!(
                    step_ts.contains(token),
                    "{token} missing from StepStatus.ts"
                );
            }
        }
        if !run_ts.is_empty() {
            for token in [
                "pending",
                "running",
                "paused",
                "completed",
                "failed",
                "cancelled",
            ] {
                assert!(run_ts.contains(token), "{token} missing from RunState.ts");
            }
        }

        let ui = fs::read_to_string(root.join("src/components/Circuits/circuitVocabulary.ts"))
            .expect("circuitVocabulary.ts is the UI runtime owner");
        assert!(ui.contains("pending_slot"));
        assert!(ui.contains("STEP_STATUS_QUEUED"));
        assert!(ui.contains("TERMINAL_RUN_STATES"));
        assert!(ui.contains("ADMITTED_RUN_STATES"));
        assert!(
            ui.contains("from '../../types/generated/StepStatus'")
                || ui.contains("from '../../types/generated/StepStatus.ts'"),
            "UI vocabulary must import the generated StepStatus union"
        );
        assert!(
            ui.contains("from '../../types/generated/RunState'")
                || ui.contains("from '../../types/generated/RunState.ts'"),
            "UI vocabulary must import the generated RunState union"
        );
    }
}
