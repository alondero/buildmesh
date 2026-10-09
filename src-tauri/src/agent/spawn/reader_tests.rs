use super::reader::*;

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
// Prepare's pre-write (Assign mode) and the
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
/// wrote a UUID in `prepare_context` (Assign mode), and the reader would
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

// -----------------------------------------------------------------------
// Reader epilogue — resume guard (issue #2137)
//
// The never-persisted-session respawn loop: `prepare_context` pre-writes a
// UUID to `cli_session_id`, so the frontend's spawn-intent resolver turns
// every subsequent relaunch into `--resume <id>`. The harness cannot resolve
// an id whose transcript was never written, exits immediately, and the node
// returns to a spawnable status — so it is relaunched with the same dead id
// every few seconds, forever. These tests pin the decision that stops it.
// -----------------------------------------------------------------------

use crate::agent::spawn::resume_guard::{self, UnusableResumeVerdict};

/// The loop's own shape: a resume launch that dies before its first
/// transcript must invalidate the stored identity, not merely go Idle.
/// Going Idle is exactly what re-armed the auto-spawn.
#[test]
fn unusable_resume_drops_the_session_identity_instead_of_going_idle() {
    let action = post_exit_action(false, false, std::time::Duration::from_millis(500));
    assert_eq!(
        action,
        PostExitAction::MarkErrorResumeFailed,
        "the CLI died inside the early-exit window"
    );
    assert_eq!(
        classify_post_exit(action, Some(UnusableResumeVerdict::DropIdentity)),
        EpilogueOutcome::DropUnusableSessionIdentity,
        "clearing `cli_session_id` is what makes the next spawn resolve to fresh"
    );
}

/// The same verdict applies past the early-exit window: a harness that boots
/// and only then discovers the conversation is gone still exits through the
/// `MarkIdle` arm, which is the arm the loop actually ran on.
#[test]
fn unusable_resume_after_the_early_exit_window_also_drops_the_identity() {
    let action = post_exit_action(
        false,
        false,
        resume_guard::UNUSABLE_RESUME_WINDOW - std::time::Duration::from_secs(1),
    );
    assert_eq!(action, PostExitAction::MarkIdle);
    assert_eq!(
        classify_post_exit(action, Some(UnusableResumeVerdict::DropIdentity)),
        EpilogueOutcome::DropUnusableSessionIdentity
    );
}

/// AC2 — the repeats are bounded and the node is surfaced instead of
/// relaunched. The give-up verdict outranks both status arms, so no later
/// `Idle`/`Error` write can re-arm the auto-spawn.
#[test]
fn exhausted_resume_is_surfaced_rather_than_respawned() {
    for action in [
        PostExitAction::MarkIdle,
        PostExitAction::MarkErrorResumeFailed,
    ] {
        assert_eq!(
            classify_post_exit(
                action,
                Some(UnusableResumeVerdict::Exhausted {
                    attempts: resume_guard::MAX_CONSECUTIVE_UNUSABLE_RESUMES
                }),
            ),
            EpilogueOutcome::SurfaceExhaustedResume,
            "the give-up verdict must supersede the {action:?} arm"
        );
    }
}

/// A deliberate kill is never a resume failure, so it keeps winning over the
/// guard — the kill initiator owns the next status (#654). Without this the
/// guard could stamp `Lost` over a node that a respawn is replacing.
#[test]
fn deliberate_kill_still_wins_over_the_resume_guard() {
    let action = post_exit_action(false, true, std::time::Duration::from_millis(500));
    assert_eq!(
        classify_post_exit(
            action,
            Some(UnusableResumeVerdict::Exhausted { attempts: 3 })
        ),
        EpilogueOutcome::LeaveStatusAlone,
        "the reader must not write any status after a kill_session teardown"
    );
}

/// The epilogue counts a strike only when this gate lets it, so closing a
/// node right after a resume spawn costs nothing. Without it, three
/// node-closes would give up on a node whose session was never bad.
#[test]
fn only_resume_failures_are_charged_against_the_budget() {
    let soon = std::time::Duration::from_millis(500);
    assert!(
        should_count_resume_failure(PostExitAction::MarkErrorResumeFailed, true, soon),
        "the #2137 shape: a --resume that died immediately"
    );
    assert!(
        should_count_resume_failure(PostExitAction::MarkIdle, true, soon),
        "a resume that outlived the early-exit window is the same defect"
    );
    assert!(
        !should_count_resume_failure(PostExitAction::LeaveStatusAlone, true, soon),
        "a deliberate kill must not spend the budget that would give the node up"
    );
    assert!(
        !should_count_resume_failure(PostExitAction::MarkErrorResumeFailed, false, soon),
        "a fresh spawn has no stored identity to invalidate"
    );
    assert!(
        !should_count_resume_failure(
            PostExitAction::MarkIdle,
            true,
            resume_guard::UNUSABLE_RESUME_WINDOW
        ),
        "a resume that ran long enough to be worked in is not a missing transcript"
    );
}

/// A fresh spawn's pre-assigned UUID is not evidence of a missing
/// conversation, so it must never trip the guard — only `--resume` launches
/// can invalidate an identity.
#[test]
fn fresh_spawns_are_unaffected_by_the_resume_guard() {
    let action = post_exit_action(false, false, std::time::Duration::from_millis(500));
    assert_eq!(
        classify_post_exit(action, None),
        EpilogueOutcome::MarkErrorResumeFailed,
        "no resume attempt, no verdict, no behaviour change"
    );
    let late = post_exit_action(false, false, std::time::Duration::from_secs(600));
    assert_eq!(classify_post_exit(late, None), EpilogueOutcome::MarkIdle);
}

/// End-to-end over the real guard: driving the never-persisted path for one
/// node stays recoverable for a couple of attempts and then gives up, so the
/// relaunch count is finite rather than the ~160 observed in #2137.
#[test]
fn the_relaunch_loop_is_finite_for_a_single_node() {
    let node = 98_213_777;
    let mut relaunched = 0;
    let mut surfaced = false;
    // Well past the bound: the loop must have stopped long before this.
    for _ in 0..50 {
        let action = post_exit_action(false, false, std::time::Duration::from_millis(500));
        match classify_post_exit(action, Some(resume_guard::note_unusable_resume(node))) {
            EpilogueOutcome::SurfaceExhaustedResume => {
                surfaced = true;
                break;
            }
            EpilogueOutcome::DropUnusableSessionIdentity => relaunched += 1,
            other => panic!("unexpected epilogue outcome: {other:?}"),
        }
    }
    assert!(surfaced, "the loop must reach the bound and stop");
    assert_eq!(
        relaunched,
        resume_guard::MAX_CONSECUTIVE_UNUSABLE_RESUMES as usize - 1,
        "only the tolerated attempts relaunch before the node is surfaced"
    );
}

/// A spawn that outlives the unusable-resume window starts the node over:
/// the budget must not carry across a working spawn.
#[test]
fn a_healthy_spawn_clears_the_budget_so_a_later_failure_starts_over() {
    let node = 98_213_778;
    assert_eq!(
        resume_guard::note_unusable_resume(node),
        UnusableResumeVerdict::DropIdentity
    );
    resume_guard::reset(node);
    assert_eq!(
        resume_guard::note_unusable_resume(node),
        UnusableResumeVerdict::DropIdentity,
        "one strike after a good spawn, not two"
    );
    resume_guard::reset(node);
}
