#![allow(unused_imports)]

use super::{reader::*, *};

/// The start_reader pattern: `pump_pty_output` inside `with_batcher`.
/// If the producer isn't dropped before join, this hangs on EOF.
#[test]
fn pump_inside_with_batcher_exits_cleanly_on_reader_eof() {
    let reader: Box<dyn std::io::Read + Send> = Box::new(std::io::Cursor::new(b"hello from pty\n"));
    let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let g = got.clone();
    let started = std::time::Instant::now();
    crate::pty::batch::with_batcher(
        move |batch| g.lock().unwrap().extend_from_slice(&batch),
        |tx| {
            pump_pty_output(reader, |data| {
                let _ = tx.send(data.to_vec());
            });
        },
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "reader+batcher hung after PTY EOF — producer was not dropped"
    );
    assert_eq!(&*got.lock().unwrap(), b"hello from pty\n");
}

// -----------------------------------------------------------------------
// Reader-epilogue decision matrix (false "failed to start" fix).
//
// The reader thread's post-exit status write used to apply the 3s
// early-exit Error heuristic unconditionally, so a process that
// `kill_session` tore down deliberately (spawn step-2 stale kill, node
// close, app shutdown) within 3s of its creation was stamped `Error`
// + toasted `resume-failed` — and that stale Error then blocked the
// replacing spawn's Spawning→Running promotion. These tests pin the
// full matrix of `post_exit_action`.
// -----------------------------------------------------------------------

#[test]
fn deliberate_kill_never_writes_status_even_within_early_exit_window() {
    // The heart of the fix: a deliberate kill 1s after process creation
    // must NOT be misread as a failed --resume.
    assert_eq!(
        post_exit_action(false, true, std::time::Duration::from_secs(1)),
        PostExitAction::LeaveStatusAlone,
    );
    // …nor may it write Idle over the replacing spawn's Spawning.
    assert_eq!(
        post_exit_action(false, true, std::time::Duration::from_secs(60)),
        PostExitAction::LeaveStatusAlone,
    );
    // Plain terminals too: the kill initiator owns the next status.
    assert_eq!(
        post_exit_action(true, true, std::time::Duration::from_secs(1)),
        PostExitAction::LeaveStatusAlone,
    );
}

#[test]
fn natural_early_exit_still_flags_resume_failure() {
    // The heuristic's true positive is preserved: an LLM process that
    // dies on its own within the window (typically `--resume` against
    // an expired session) still reads as a resume failure.
    assert_eq!(
        post_exit_action(false, false, std::time::Duration::from_secs(1)),
        PostExitAction::MarkErrorResumeFailed,
    );
}

#[test]
fn natural_exit_after_window_marks_idle() {
    assert_eq!(
        post_exit_action(false, false, EARLY_EXIT_WINDOW),
        PostExitAction::MarkIdle,
    );
}

#[test]
fn plain_terminal_natural_exit_is_idle_regardless_of_elapsed() {
    // A shell exiting fast is not a resume signal.
    assert_eq!(
        post_exit_action(true, false, std::time::Duration::from_millis(10)),
        PostExitAction::MarkIdle,
    );
}

// -----------------------------------------------------------------------
// Reader-thread session-id capture gate (issue #651)
//
// The orchestrator's pre-write at spawn_agent_inner (Assign mode) and the
// PTY reader thread's capture-from-output path both target the same
// `agent_nodes.cli_session_id` column. They are unsynchronised, so a
// last-writer-wins race left the row holding a UUID the agent never
// claimed — and auto-resume later invoked `claude --resume <wrong-uuid>`
// → "Conversation not found". The fix pins the gate to a single function
// of `session_id_mode` (the source of truth) so the two writers can never
// both target the same column. Each test pins one row of the truth table;
// the regression test is the `Assign(_)` row.
// -----------------------------------------------------------------------

/// Regression for issue #651. Even if a future adapter returns
/// `self_assigns_session_id() = true`, the reader thread MUST NOT capture
/// when the orchestrator is in Assign mode — the orchestrator already
/// wrote a UUID at `spawn_agent_inner` step 4, and the reader would
/// overwrite it with whatever UUID matched the regex on PTY output
/// (possibly a different log line, possibly never echoed back).
#[test]
fn reader_should_not_capture_in_assign_mode_even_if_provider_self_assigns() {
    assert!(
        !reader_should_capture_session_id(&SessionIdMode::Assign("orchestrator-uuid".into()), true,),
        "Assign mode is authoritative — reader MUST NOT overwrite the \
             orchestrator's pre-written UUID with a regex match from PTY output \
             (issue #651: 'a UUID the agent never claimed')"
    );
}

/// Resume already has the authoritative ID stored in `cli_session_id`
/// (or, for fresh `--resume` calls, the resume arg passed to the CLI).
/// Capture would race the in-flight `claude --resume <id>` with a
/// possibly-different UUID from the regex, so the reader must stay quiet.
#[test]
fn reader_should_not_capture_in_resume_mode() {
    assert!(
        !reader_should_capture_session_id(&SessionIdMode::Resume("resume-uuid".into()), true,),
        "Resume mode carries the authoritative ID; reader MUST NOT capture"
    );
}

/// `None` mode is the only mode where reader capture is allowed — and only
/// for providers that print a labeled UUID on the PTY (Codex, Agy).
/// OpenCode self-assigns `ses_…` IDs but captures them in
/// `after_fresh_spawn` (SQLite), so its PTY-capture flag is false.
#[test]
fn reader_should_capture_when_provider_self_assigns_and_mode_is_none() {
    assert!(
        reader_should_capture_session_id(&SessionIdMode::None, true),
        "Codex / Agy fresh spawns rely on the reader capturing the UUID \
             from PTY output (orchestrator has no pre-write in None mode)"
    );
}

/// Self-assigning capability is necessary but not sufficient — if the
/// provider accepts `--session-id` (Anthropic) or captures in
/// `after_fresh_spawn` (OpenCode), the PTY regex is not the source of
/// truth even when the orchestrator didn't pre-write.
#[test]
fn reader_should_not_capture_when_provider_does_not_self_assign() {
    assert!(
        !reader_should_capture_session_id(&SessionIdMode::None, false),
        "reader MUST NOT capture when provider does not self-assign; \
             any UUID match would overwrite the existing cli_session_id"
    );
}
