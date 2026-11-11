/// Snapshot the PTY output position before writing input. Unlike a turn
/// boundary, this does not change classifier state if a guarded write fails.
pub(crate) fn output_cursor(node_id: i64) -> Option<u64> {
    NODES
        .lock()
        .unwrap()
        .get(&node_id)
        .map(|state| state.output_bytes)
}

/// Read only output received after `cursor`, even when the bounded tail has
/// dropped earlier bytes. The cursor is captured before the PTY stdin write.
pub(crate) fn cleaned_output_since(node_id: i64, cursor: u64) -> String {
    let raw = NODES
        .lock()
        .unwrap()
        .get(&node_id)
        .map(|state| {
            let retained_from = state.output_bytes.saturating_sub(state.tail.len() as u64);
            let mut offset = cursor
                .saturating_sub(retained_from)
                .min(state.tail.len() as u64) as usize;
            while !state.tail.is_char_boundary(offset) {
                offset += 1;
            }
            state.tail[offset..].to_string()
        })
        .unwrap_or_default();
    crate::session_naming::ANSI_ESCAPE
        .replace_all(&raw, "")
        .to_string()
}

use super::*;

/// Outer hang guard for the two shell fixtures below — not a behaviour
/// bound. The drain/diagnostic contract is pinned on injected streams
/// (no child, no clock). `drains_output_larger_than_a_pipe_buffer`
/// still spawns a process because outrunning a real pipe buffer is the
/// property it guards, and `timeout_does_not_wait_for_stdin_consumption`
/// needs a real child that outlives the deadline without reading stdin.
/// Both children (`cmd.exe`/`ping` on Windows, `sh`/`sleep` elsewhere)
/// start in milliseconds, so CPU contention cannot false-fail them the
/// way a cold `powershell.exe` did (issue #2049). The 30s value matches
/// the budget production injects (`classify_with_prompt`); the verdict
/// comes from the content assertions, and only a genuinely wedged child
/// ever reaches the bound.
const FIXTURE_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

#[test]
fn circuit_classifier_failure_retains_authentication_diagnostics() {
    // No child, no clock: the diagnostics contract is a pure function
    // of the exit status plus the captured stdout/stderr (issue #2049).
    let error = classifier_command_error(
        Some("classifier exited with exit code: 1".to_string()),
        false,
        false,
        b"Failed to authenticate: OAuth session expired\n",
        b"Refresh rejected\n",
        false,
    )
    .unwrap_err();
    assert!(error.contains("OAuth session expired"), "{error}");
    assert!(error.contains("Refresh rejected"), "{error}");
}

#[test]
fn a_timed_out_classifier_is_told_apart_from_one_that_failed() {
    // Run 399: every `mcode exec` hung in start-up for 29 minutes. The
    // gate stores the error as `<provider>: <message>`, and only a
    // timeout may earn the longer retry back-off.
    let timed_out = classifier_command_error(None, true, false, b"", b"", false).unwrap_err();
    assert!(is_classifier_timeout(&format!("mcode: {timed_out}")));
    assert!(is_classifier_timeout(&timed_out));

    let exited = classifier_command_error(
        Some("classifier exited with exit code: 4".to_string()),
        false,
        false,
        b"",
        b"Model is not available for this route\n",
        false,
    )
    .unwrap_err();
    assert!(!is_classifier_timeout(&format!("mcode: {exited}")));
    assert!(!is_classifier_timeout(
        "mcode: classifier stdout exceeded 64 KiB"
    ));
    assert!(!is_classifier_timeout(
            "mcode: Classifier returned no recognised verdict: classifier exceeded its time budget soon"
        ));
    assert!(!is_classifier_timeout(""));
}

#[test]
fn circuit_classifier_stdout_drain_retains_bytes_and_flags_oversize() {
    // The retention/cap contract of `drain_classifier_stdout`, on
    // injected readers: small output passes through untouched, output
    // past the cap is cut at one byte over and raises the flag.
    let quiet = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let bytes = drain_classifier_stdout(
        &b"COMPLETED\n"[..],
        64 * 1024,
        std::sync::Arc::clone(&quiet),
    )
    .unwrap();
    assert_eq!(bytes, b"COMPLETED\n");
    assert!(!quiet.load(std::sync::atomic::Ordering::Acquire));

    let loud = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flood = vec![b'x'; 70 * 1024];
    let bytes =
        drain_classifier_stdout(&flood[..], 64 * 1024, std::sync::Arc::clone(&loud)).unwrap();
    assert!(loud.load(std::sync::atomic::Ordering::Acquire));
    assert_eq!(bytes.len(), 64 * 1024 + 1);
}

#[test]
fn circuit_classifier_drains_output_larger_than_a_pipe_buffer() {
    // The one property that needs a real OS pipe: the parent must keep
    // draining while the child emits more than a pipe buffer, or the
    // child blocks on write and the run wedges. `cmd.exe` starts in
    // milliseconds, so this measures draining, not shell start-up.
    let mut cmd = if cfg!(windows) {
        crate::process_util::command_no_window("cmd.exe")
    } else {
        crate::process_util::command_no_window("sh")
    };
    if cfg!(windows) {
        cmd.args([
            "/c",
            "(for /L %i in (1,1,1200) do @echo xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx) & echo COMPLETED",
        ]);
    } else {
        cmd.args([
            "-c",
            "cat >/dev/null; head -c 32000 /dev/zero | tr '\\0' x; printf '\\nCOMPLETED\\n'",
        ]);
    }
    let output = run_classifier_command(cmd, &"prompt".repeat(10_000), FIXTURE_BUDGET).unwrap();
    assert!(output.len() > 32_000);
    assert_eq!(
        parse_classification(&output),
        Some(Classification::Completed)
    );
}

#[test]
fn circuit_classifier_stderr_over_limit_is_truncated_without_killing_success() {
    // 70 KiB of stderr against a 64 KiB cap, all injected: the drain
    // must flag the overflow and cut retention, and the composer must
    // note the truncation while keeping the success output and a
    // bounded error message.
    let mut flood = vec![b'x'; 70 * 1024];
    flood.extend_from_slice(b"classification failed\n");
    let truncated = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let retained =
        drain_classifier_stderr(&flood[..], 64 * 1024, std::sync::Arc::clone(&truncated)).unwrap();
    assert!(truncated.load(std::sync::atomic::Ordering::Acquire));
    assert!(retained.len() <= 64 * 1024 + 1);

    let output =
        classifier_command_error(None, false, false, b"COMPLETED\n", &retained, true).unwrap();
    assert_eq!(
        parse_classification(&output),
        Some(Classification::Completed)
    );

    let error = classifier_command_error(
        Some("classifier exited with exit code: 1".to_string()),
        false,
        false,
        b"",
        &retained,
        true,
    )
    .unwrap_err();
    assert!(error.contains("stderr truncated after 64 KiB"), "{error}");
    assert!(error.contains("classifier exited"), "{error}");
    assert!(error.len() < 2200, "stderr diagnostics must remain bounded");
}

/// A child that outlives the deadline without ever reading stdin —
/// the same shape as `process_util::tests::hang_cmd`: `ping` on
/// Windows (`-n 10` runs ~9s; `timeout` was tried there and exits at
/// once without a console), `sleep` elsewhere. Both start in
/// milliseconds, so unlike a cold `powershell.exe` neither can eat the
/// hang guard below under CPU contention (issue #2049).
///
/// One function with the platform split *inside* the body: `cfg!` at
/// the call site does not prune the dead arm, so a `#[cfg]`-gated
/// item named from `cfg!` fails to compile on the other target.
fn hang_child_cmd(secs: u64) -> std::process::Command {
    #[cfg(target_os = "windows")]
    {
        let mut cmd = crate::process_util::command_no_window("ping");
        cmd.args(["-n", &secs.to_string(), "127.0.0.1"]);
        cmd
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut cmd = crate::process_util::command_no_window("sleep");
        cmd.arg(secs.to_string());
        cmd
    }
}

#[test]
fn circuit_classifier_timeout_does_not_wait_for_stdin_consumption() {
    let cmd = hang_child_cmd(10);
    let started = std::time::Instant::now();
    let result = run_classifier_command(
        cmd,
        &"prompt".repeat(10_000),
        std::time::Duration::from_millis(200),
    );
    assert!(result.is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[cfg(unix)]
#[test]
fn circuit_classifier_timeout_closes_inherited_output_handles() {
    let mut cmd = crate::process_util::command_no_window("sh");
    cmd.args(["-c", "sleep 60 & printf 'COMPLETED\\n'"]);
    let started = std::time::Instant::now();
    let result = run_classifier_command(cmd, "prompt", std::time::Duration::from_secs(2));
    assert_eq!(
        parse_classification(&result.unwrap()),
        Some(Classification::Completed)
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn circuit_wait_probe_does_not_read_transcripts_on_every_pty_redraw() {
    let id = -920_001;
    register_circuit(id);
    let generation = begin_circuit_wait_probe(id, "wait").unwrap();
    note_circuit_probe(id, "wait", generation);
    on_output(id, "redraw");
    assert!(begin_circuit_wait_probe(id, "wait").is_none());
    assert!(begin_circuit_wait_probe(id, "another-gate").is_some());
    unregister(id);
}

#[test]
fn circuit_attachment_starts_quiet_observation_without_fabricating_a_turn() {
    let id = -920_086;
    register_circuit(id);
    assert!(millis_since_last_output(id).is_some());
    assert!(!has_turn_start(id));
    let started = NODES.lock().unwrap().get(&id).unwrap().last_output;
    register_circuit(id);
    assert_eq!(NODES.lock().unwrap().get(&id).unwrap().last_output, started);
    unregister(id);
}

#[test]
fn circuit_continuation_requires_an_explicit_classification() {
    assert_eq!(
        parse_classification("CONTINUE"),
        Some(Classification::Continue)
    );
    assert_eq!(
        parse_classification("Verdict: CONTINUE."),
        Some(Classification::Continue)
    );
    assert_eq!(parse_classification("DISCONTINUE"), None);
    assert_eq!(parse_classification("Do not CONTINUE"), None);
    assert_eq!(parse_classification("NOT CONTINUE"), None);
    let prompt = circuit_classify_prompt("I will implement the remaining change next.");
    assert!(prompt.contains("Never classify these as CONTINUE"));
}

#[test]
fn deterministic_review_verdict_reads_the_explicit_report() {
    // Approvals resolve without a classifier call.
    for report in [
        "Approved.",
        "Round 3: Approved",
        "Approved. No remaining findings.",
        "Review complete: no remaining findings. Approved.",
        "LGTM",
        "Looks good to me, approving.",
        // A verified fix is done work, not a new change request.
        "Verified the fix. Approved.",
        "The fix looks great, approving.",
        "Good fix. Approved.",
        // Negated change requests are approval, not fresh work.
        "Approved. No changes needed.",
        "Approved. No further changes needed.",
        "Approved. Does not require any changes.",
        "All findings addressed; approving.",
    ] {
        assert_eq!(
            review_verdict_from_report(report),
            Classification::Completed,
            "{report}"
        );
    }
    // Findings and change requests loop back for fixes.
    for report in [
            "Changes requested: add regression tests",
            "Round 1: Changes requested: add regression tests",
            "Unresolved actionable findings in auth.rs:12.",
            "Please fix the off-by-one in retry.rs.",
            "I approve the approach but request changes to the error path.",
            "Approved, but please fix the error path.",
            "Findings: 1. Resolved the previous race condition. 2. Missing input validation on line 42.",
            "Review completion alone is not approval. Verdict: changes requested.",
        ] {
            assert_eq!(review_verdict_from_report(report), Classification::Working, "{report}");
        }
    // Negated approvals never complete, even with "approve" in the text.
    // Blockers and ambiguous reports need attention, never approval.
    for report in [
        "Do not approve. Critical security vulnerability in token validation.",
        "I don't approve this change.",
        "Will not approve until integration tests exist.",
        "Should not approve.",
        "Review completion alone is not approval.",
        "Cannot assess this diff: missing access to the base ref.",
        "Review is blocked on the failing provider call.",
        "Reviewed the diff; see notes above.",
        "Disapprove.",
    ] {
        assert_eq!(
            review_verdict_from_report(report),
            Classification::Blocked,
            "{report}"
        );
    }
    // The contract's own "non-blocking" guidance and code nouns like
    // "match block" are not blockers; an echoed template must not veto
    // a genuine verdict elsewhere in the report.
    for report in [
        "Treating style suggestions as non-blocking. Approved.",
        "Non-blocking comment: rename variable foo. Otherwise approved.",
        "No blocking issues found. LGTM.",
        "The match block looks solid. Approved.",
        "Review completion alone is not approval. Verdict: Approved.",
    ] {
        assert_eq!(
            review_verdict_from_report(report),
            Classification::Completed,
            "{report}"
        );
    }
}

// ── parse_classification against mock classifier outputs ───────────────

#[test]
fn parses_bare_single_word_answers() {
    assert_eq!(
        parse_classification("COMPLETED"),
        Some(Classification::Completed)
    );
    assert_eq!(
        parse_classification("BLOCKED\n"),
        Some(Classification::Blocked)
    );
    assert_eq!(
        parse_classification("  working  "),
        Some(Classification::Working)
    );
}

#[test]
fn parses_decorated_answer_on_last_line() {
    // Models often prepend reasoning despite instructions — the last
    // token-bearing line wins.
    let out = "Let me look at the transcript.\n\nThe agent finished.\nCOMPLETED\n";
    assert_eq!(parse_classification(out), Some(Classification::Completed));
}

#[test]
fn ambiguous_line_is_skipped_in_favor_of_an_unambiguous_one() {
    // A line echoing the whole instruction (all three tokens) must not
    // be treated as an answer; the real answer above it wins.
    let out = "BLOCKED\nOptions were: COMPLETED, BLOCKED or WORKING.";
    assert_eq!(parse_classification(out), Some(Classification::Blocked));
}

#[test]
fn tokenless_or_empty_output_degrades_to_none() {
    assert_eq!(parse_classification(""), None);
    assert_eq!(
        parse_classification("I am not sure what state this is."),
        None
    );
}

// ── buffering ───────────────────────────────────────────────────────────

#[test]
fn on_output_ignores_unregistered_nodes_and_buffers_registered_ones() {
    let id = 910_001;
    on_output(id, "dropped");
    assert_eq!(cleaned_tail(id), "", "unregistered node buffers nothing");

    register(id);
    on_output(id, "hello ");
    on_output(id, "\x1b[31mworld\x1b[0m");
    assert_eq!(cleaned_tail(id), "hello world", "ANSI codes are stripped");

    unregister(id);
    assert_eq!(cleaned_tail(id), "", "unregister drops the tail");
}

#[test]
fn note_turn_start_isolates_current_turn_output() {
    let id = 910_004;
    register(id);
    on_output(id, "turn 1 boot output\n");
    assert_eq!(cleaned_tail(id), "turn 1 boot output\n");

    note_turn_start(id);
    on_output(id, "turn 2 work: fixed issue #1357");
    assert_eq!(
        cleaned_turn_tail(id),
        "turn 2 work: fixed issue #1357",
        "cleaned_turn_tail excludes prior turn output"
    );
    assert!(cleaned_tail(id).contains("turn 1 boot output"));

    unregister(id);
}

#[test]
fn output_cursor_keeps_immediate_paste_output_without_moving_turn_start() {
    let id = 910_019;
    register(id);
    on_output(id, "old [Pasted Content 20 chars]");
    note_turn_start(id);
    let cursor = output_cursor(id).unwrap();
    on_output(id, "\x1b[32m[Pasted Content 30 chars]\x1b[0m");
    assert_eq!(
        cleaned_output_since(id, cursor),
        "[Pasted Content 30 chars]"
    );
    assert_eq!(cleaned_turn_tail(id), "[Pasted Content 30 chars]");
    on_output(id, &"x".repeat(MAX_TAIL_CHARS));
    assert_eq!(cleaned_output_since(id, cursor), "x".repeat(MAX_TAIL_CHARS));
    unregister(id);
}

#[test]
fn circuit_turn_tail_survives_buffer_rollover() {
    let id = 910_016;
    register_circuit(id);
    on_output(id, &"x".repeat(MAX_TAIL_CHARS - 10));
    note_turn_start(id);
    on_output(id, "Review complete: no remaining findings. Approved.");
    assert_eq!(
        cleaned_turn_tail(id),
        "Review complete: no remaining findings. Approved."
    );
    unregister(id);
}

#[test]
fn circuit_new_turn_without_output_does_not_reuse_previous_report() {
    let id = 910_017;
    register_circuit(id);
    on_output(id, "Previous implementation complete");
    note_turn_start(id);
    assert_eq!(cleaned_turn_tail(id), "");
    unregister(id);
}

#[test]
fn tail_is_capped_at_max_chars() {
    let id = 910_002;
    register(id);
    let chunk = "x".repeat(10_000);
    on_output(id, &chunk);
    on_output(id, &chunk);
    on_output(id, &chunk);
    let nodes = NODES.lock().unwrap();
    assert!(nodes.get(&id).unwrap().tail.len() <= MAX_TAIL_CHARS);
    drop(nodes);
    unregister(id);
}

#[test]
fn note_evaluation_is_tracked_and_unregister_clears_it() {
    let id = 910_003;
    register(id);
    assert_eq!(
        millis_since_last_evaluation(id),
        None,
        "no evaluation recorded yet"
    );
    note_evaluation(id);
    assert!(
        millis_since_last_evaluation(id).is_some(),
        "evaluation timestamp recorded"
    );
    unregister(id);
    assert_eq!(
        millis_since_last_evaluation(id),
        None,
        "unregister drops the evaluation timestamp"
    );
}

#[test]
fn probe_gate_only_reopens_for_fresh_output_or_due_retry() {
    let id = 910_006;
    register_circuit(id);
    let key = "run:gate:1";
    let generation =
        begin_circuit_probe(id, key, false).expect("registration permits one recovery probe");
    note_circuit_probe(id, key, generation);
    assert!(
        begin_circuit_probe(id, key, false).is_none(),
        "unchanged silent output stays cold"
    );
    assert!(
        begin_circuit_probe(id, key, true).is_some(),
        "durable backend retry bypasses freshness"
    );
    assert!(
        begin_circuit_probe(id, "run:sibling:1", false).is_some(),
        "one gate cannot suppress a sibling gate"
    );
    on_output(id, "new turn");
    let generation =
        begin_circuit_probe(id, key, false).expect("PTY output invalidates the probe clock");
    note_circuit_probe(id, key, generation);
    assert!(begin_circuit_probe(id, key, false).is_none());
    unregister(id);
}

#[test]
fn circuit_probe_cooldown_does_not_reset_classifier_retry_clock() {
    let id = 910_007;
    register_circuit(id);
    note_evaluation(id);
    let before = NODES
        .lock()
        .unwrap()
        .get(&id)
        .and_then(|state| state.last_evaluation);
    let generation = begin_circuit_probe(id, "run:gate:1", false).unwrap();
    note_circuit_probe(id, "run:gate:1", generation);
    let after = NODES
        .lock()
        .unwrap()
        .get(&id)
        .and_then(|state| state.last_evaluation);
    assert_eq!(
        after, before,
        "transcript probes must not postpone backend retry"
    );
    unregister(id);
}

#[test]
fn circuit_registration_buffers_output_and_unregister_drops_it() {
    let id = 910_005;
    register_circuit(id);
    assert!(is_circuit_piloted(id));
    on_output(id, "circuit output");
    assert_eq!(cleaned_tail(id), "circuit output");

    unregister(id);
    assert!(!is_circuit_piloted(id));
    assert_eq!(cleaned_tail(id), "");
}

// ── output wake floor (issue #2764) ─────────────────────────────────────
//
// The count moves only when `wake_circuit_worker` runs on this thread.
// The worker then runs its five passes once per wakeup and parks on the
// wake flag (`start_circuit_worker`). A flood that stays at one wake per
// 250 ms window keeps the pass rate on that cadence.
//
// Node ids are negative so they stay outside the positive 100_000-id
// blocks `db::test_support` hands to isolated databases.

#[test]
fn output_wake_flood_is_once_per_window_instead_of_once_per_read() {
    let id = -910_2764;
    register_circuit(id);
    let t0 = Instant::now();
    for i in 0..200 {
        on_output_at(id, "chunk", t0 + Duration::from_micros(i));
    }
    assert_eq!(
        output_wake_count(id),
        1,
        "200 reads in one window must notify the worker once"
    );
    assert_eq!(cleaned_tail(id).len(), 200 * "chunk".len());
    on_output_at(id, "boundary", t0 + OUTPUT_WAKE_FLOOR);
    assert_eq!(
        output_wake_count(id),
        2,
        "a chunk on the floor notifies immediately"
    );
    assert!(
        !deliver_due_output_wake(id, t0 + OUTPUT_WAKE_FLOOR),
        "that notify cancels the deferred wake for the same window"
    );
    unregister(id);
}

#[test]
fn output_wake_yield_inside_the_floor_is_delivered_when_the_window_ends() {
    let id = -910_2765;
    register_circuit(id);
    let t0 = Instant::now();
    on_output_at(id, "working", t0);
    on_output_at(id, "COMPLETED yield", t0 + Duration::from_millis(249));
    assert_eq!(
        output_wake_count(id),
        1,
        "the yield is inside the open window"
    );
    assert!(
        !deliver_due_output_wake(id, t0 + Duration::from_millis(249)),
        "the deferred wake waits until the floor"
    );
    assert!(
        deliver_due_output_wake(id, t0 + OUTPUT_WAKE_FLOOR),
        "a trailing yield is observed at the end of the 250 ms window"
    );
    assert_eq!(output_wake_count(id), 2);
    // The wake that closed the window starts the next one. A chunk on that
    // instant is deferred, and dropping the node cancels it.
    on_output_at(id, "again", t0 + OUTPUT_WAKE_FLOOR);
    assert_eq!(output_wake_count(id), 2);
    unregister(id);
    assert!(!deliver_due_output_wake(
        id,
        t0 + OUTPUT_WAKE_FLOOR + OUTPUT_WAKE_FLOOR
    ));
    assert_eq!(output_wake_count(id), 2);
}

#[test]
fn output_wake_quiet_yield_wakes_the_worker_immediately() {
    let id = -910_2766;
    register_circuit(id);
    let t0 = Instant::now();
    on_output_at(id, "COMPLETED", t0);
    assert_eq!(output_wake_count(id), 1);
    assert!(!deliver_due_output_wake(id, t0 + OUTPUT_WAKE_FLOOR));
    unregister(id);
}

#[test]
fn output_wake_one_nodes_flood_does_not_postpone_another_nodes_yield() {
    let busy = -910_2767;
    let quiet = -910_2768;
    register_circuit(busy);
    register_circuit(quiet);
    let t0 = Instant::now();
    on_output_at(busy, "chunk", t0);
    on_output_at(busy, "chunk", t0 + Duration::from_millis(1));
    on_output_at(quiet, "COMPLETED", t0 + Duration::from_millis(1));
    assert_eq!(output_wake_count(busy), 1);
    assert_eq!(
        output_wake_count(quiet),
        1,
        "another node's open window does not delay this yield"
    );
    unregister(busy);
    unregister(quiet);
}

#[test]
fn output_wake_delivery_keeps_a_deadline_armed_for_the_next_window() {
    let id = -910_2769;
    register_circuit(id);
    let t0 = Instant::now();
    on_output_at(id, "working", t0);
    on_output_at(id, "more", t0 + Duration::from_millis(1));
    let claimed = t0 + OUTPUT_WAKE_FLOOR;
    // The reader armed the next window after this delivery had already
    // decided the open one was due. The hub now holds that later deadline.
    let next = claimed + OUTPUT_WAKE_FLOOR;
    lock_output_wake_hub().deadlines.insert(id, next);
    assert!(
        deliver_due_output_wake(id, claimed),
        "the open window's wake is still delivered"
    );
    assert_eq!(
        lock_output_wake_hub().deadlines.get(&id).copied(),
        Some(next),
        "finishing that wake must leave the next window armed"
    );
    unregister(id);
}

#[test]
fn output_wake_hub_holds_the_deferred_deadline_until_it_is_due() {
    let id = -910_2770;
    register_circuit(id);
    let t0 = Instant::now();
    on_output_at(id, "working", t0);
    assert!(!lock_output_wake_hub().deadlines.contains_key(&id));
    on_output_at(id, "COMPLETED", t0 + Duration::from_millis(1));
    assert_eq!(
        lock_output_wake_hub().deadlines.get(&id).copied(),
        Some(t0 + OUTPUT_WAKE_FLOOR)
    );
    assert!(!deliver_due_output_wake(
        id,
        t0 + Duration::from_millis(249)
    ));
    assert_eq!(
        lock_output_wake_hub().deadlines.get(&id).copied(),
        Some(t0 + OUTPUT_WAKE_FLOOR),
        "an early look leaves the armed deadline in place"
    );
    assert!(deliver_due_output_wake(id, t0 + OUTPUT_WAKE_FLOOR));
    assert!(!lock_output_wake_hub().deadlines.contains_key(&id));
    assert_eq!(output_wake_count(id), 2);
    unregister(id);
}
