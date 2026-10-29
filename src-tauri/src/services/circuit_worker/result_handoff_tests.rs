use super::*;
use crate::services::transcript_reader::report_snapshot::ReportReadError;

/// The submission that started the turn moved the lifecycle stamp at
/// `TURN_START_MS`; the process incarnation is older.
const TURN_START_MS: i64 = 1_793_300_000_000;

fn stamp() -> String {
    let changed = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(TURN_START_MS)
        .expect("valid time")
        .to_rfc3339();
    format!("{}:{changed}", TURN_START_MS - 60_000)
}

fn turn(stamp: &str) -> ResultTurn<'_> {
    ResultTurn {
        file_only: true,
        written_at_ms: TURN_START_MS + 120_000,
        now_ms: TURN_START_MS + 120_000 + RESULT_SETTLE_MS,
        quiet_ms: None,
        transcript: TranscriptActivity::Unavailable,
        stamp,
    }
}

#[test]
fn a_settled_file_written_during_the_turn_finishes_a_file_only_turn() {
    let stamp = stamp();
    assert!(finishes_turn(&turn(&stamp)));
}

#[test]
fn a_file_still_being_written_does_not_finish_the_turn() {
    let stamp = stamp();
    let mut unsettled = turn(&stamp);
    unsettled.now_ms = unsettled.written_at_ms + RESULT_SETTLE_MS - 1;
    assert!(!finishes_turn(&unsettled));
}

#[test]
fn a_file_older_than_the_current_turn_does_not_finish_it() {
    // A prompt submitted after the file was written moved the stamp past it:
    // that file answers an earlier turn.
    let stamp = stamp();
    let mut stale = turn(&stamp);
    stale.written_at_ms = TURN_START_MS - 1;
    stale.now_ms = TURN_START_MS + 60_000;
    assert!(!finishes_turn(&stale));
    assert!(!finishes_turn(&ResultTurn {
        stamp: "not-a-stamp",
        ..turn(&stamp)
    }));
}

#[test]
fn a_harness_with_its_own_lifecycle_also_needs_a_quiet_terminal() {
    // The file is a fallback for a lost hook, so the harness must look
    // finished on its other evidence too: no output for the lost-turn window.
    let stamp = stamp();
    let fallback = |quiet_ms| ResultTurn {
        file_only: false,
        quiet_ms,
        ..turn(&stamp)
    };
    assert!(!finishes_turn(&fallback(None)));
    assert!(!finishes_turn(&fallback(Some(
        super::super::turn_classify::LOST_TURN_QUIET_MS - 1
    ))));
    assert!(finishes_turn(&fallback(Some(
        super::super::turn_classify::LOST_TURN_QUIET_MS
    ))));
}

#[test]
fn transcript_activity_after_the_file_vetoes_the_fallback() {
    let stamp = stamp();
    let quiet = Some(super::super::turn_classify::LOST_TURN_QUIET_MS);
    let base = turn(&stamp);
    let fallback = |transcript| ResultTurn {
        file_only: false,
        quiet_ms: quiet,
        transcript,
        ..turn(&stamp)
    };
    assert!(finishes_turn(&fallback(TranscriptActivity::ReportedAt(
        base.written_at_ms
    ))));
    assert!(!finishes_turn(&fallback(TranscriptActivity::ReportedAt(
        base.written_at_ms + 1
    ))));
    assert!(!finishes_turn(&fallback(TranscriptActivity::Busy)));
    // A file-only harness has no transcript to consult.
    assert!(finishes_turn(&ResultTurn {
        transcript: TranscriptActivity::Busy,
        ..turn(&stamp)
    }));
}

#[test]
fn transcript_activity_distinguishes_unfinished_work_from_an_unreadable_record() {
    for busy in [
        ReportReadError::WorkInProgress,
        ReportReadError::PartialPublication,
        ReportReadError::ChangedDuringRead,
    ] {
        assert_eq!(
            TranscriptActivity::of(&Err(busy)),
            TranscriptActivity::Busy,
            "{busy:?}"
        );
    }
    for unavailable in [
        ReportReadError::Unsupported,
        ReportReadError::NoSession,
        ReportReadError::NoTranscript,
        ReportReadError::Unreadable,
        ReportReadError::NoReport,
    ] {
        assert_eq!(
            TranscriptActivity::of(&Err(unavailable)),
            TranscriptActivity::Unavailable,
            "{unavailable:?}"
        );
    }
}

#[test]
fn turn_revision_names_the_owed_step_attempt() {
    let path = std::path::Path::new("/data/circuits/runs/run-9/implementer-attempt2.result.md");
    assert_eq!(
        turn_revision(path),
        "result-handoff:implementer-attempt2.result.md"
    );
    assert_ne!(
        turn_revision(path),
        turn_revision(std::path::Path::new(
            "/data/circuits/runs/run-9/implementer-attempt3.result.md"
        ))
    );
}

#[test]
fn written_at_ignores_a_missing_or_blank_file() {
    let dir = std::env::temp_dir().join(format!(
        "buildmesh-result-handoff-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join("step-attempt1.result.md");
    assert_eq!(written_at_ms(&path), None);
    std::fs::write(&path, "  \n").expect("blank file");
    assert_eq!(written_at_ms(&path), None);
    std::fs::write(&path, "Done.\nBUILDMESH_HANDOFF_V1: READY\n").expect("report");
    let written = written_at_ms(&path).expect("written report");
    assert!((written - chrono::Utc::now().timestamp_millis()).abs() < 60_000);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn file_only_harnesses_are_the_ones_without_a_lifecycle() {
    for (selection, expected) in [
        ("freebuff", true),
        ("perch", true),
        ("dsh", true),
        ("freebuff:some-account", true),
        ("claude", false),
        ("codex", false),
        ("commandcode", false),
        ("muse", false),
        ("cline", false),
    ] {
        assert_eq!(
            selection_completes_by_result_file_only(selection),
            expected,
            "{selection}"
        );
    }
}
