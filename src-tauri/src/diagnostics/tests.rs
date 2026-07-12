//! Tests for the diagnostics sample line and (on Windows) the vitals FFI.

use super::*;

fn sample_fixture() -> Sample {
    Sample {
        uptime_secs: 600,
        vitals: ProcessVitals {
            working_set_bytes: 812 * 1024 * 1024,
            private_bytes: 1340 * 1024 * 1024,
            handle_count: 4123,
            thread_count: 210,
            child_process_count: 6,
        },
        watchers: 6,
        pty_sessions: 6,
        git_changed_total: 142,
        git_changed_delta: 20,
        elapsed_secs: 5.0,
        bg_sync_attempts: 12,
        bg_sync_advanced: 2,
        bg_sync_failures: 0,
        bg_sync_last_ms: 430,
        git_timeouts: 0,
    }
}

/// The line is greppable: a stable `DIAG` prefix and `key=value` fields, with
/// bytes rendered as whole MiB and the GIT_CHANGED rate derived from the delta
/// and interval. Anchoring the exact fields means a field rename/reorder is a
/// visible test change, not a silent break of a log a user is handing back.
#[test]
fn format_line_renders_expected_fields() {
    let line = format_line(&sample_fixture());
    assert!(line.starts_with("DIAG "), "stable grep prefix, got: {line}");
    assert!(line.contains("uptime_s=600"));
    assert!(line.contains("rss_mb=812"), "working set in whole MiB: {line}");
    assert!(line.contains("priv_mb=1340"), "private bytes in whole MiB: {line}");
    assert!(line.contains("handles=4123"));
    assert!(line.contains("threads=210"));
    assert!(line.contains("child_procs=6"));
    assert!(line.contains("watchers=6"));
    assert!(line.contains("pty=6"));
    // 20 emits over a 5 s interval → 4.0 per second.
    assert!(
        line.contains("git_changed=142(+20/4.0ps)"),
        "git_changed total/delta/rate: {line}"
    );
    assert!(line.contains("bg_sync=att:12/adv:2/fail:0/last_ms:430"));
    assert!(line.contains("git_timeouts=0"));
}

/// A zero elapsed must not divide-by-zero into NaN/inf — the rate is 0.
#[test]
fn format_line_handles_zero_elapsed() {
    let mut s = sample_fixture();
    s.elapsed_secs = 0.0;
    let line = format_line(&s);
    assert!(line.contains("(+20/0.0ps)"), "zero elapsed → 0 rate, got: {line}");
}

/// On Windows the vitals FFI must return a plausibly-live process: the working
/// set and thread count of a running test process are always > 0. This guards
/// the struct layout / `cb` sizing — a wrong layout typically returns zeros or
/// garbage. On non-Windows `sample_vitals` is the zero stub, so the assertion
/// is Windows-gated.
#[cfg(target_os = "windows")]
#[test]
fn windows_vitals_report_a_live_process() {
    let v = sample_vitals();
    assert!(v.working_set_bytes > 0, "a live process has a non-zero working set");
    assert!(v.thread_count > 0, "a live process owns at least one thread");
    // Handle count is virtually always non-zero for a running process, but we
    // don't hard-assert it in case a stripped CI sandbox denies the query.
}
