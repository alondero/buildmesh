//! Tests for `circuit::vocabulary` (issue #1660, issue #2600).
//!
//! Split out of `src-tauri/src/circuit/vocabulary.rs` by the repo's
//! sibling-test convention — see `docs/development/rust-conventions.md`,
//! under *Rust unit tests live in sibling `*_tests.rs` files*. The module
//! keeps the name `tests`, so the test paths keep their shape.
//!
//! Run with: cd src-tauri && cargo test circuit::vocabulary::tests
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
    // A cancelled run is swept like a failed one (issue #2600): the
    // cleanup retry is anchored on the node-scoped lease, not the run.
    assert!(RunState::SQL_IN_SWEEPABLE.contains("cancelled"));
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
