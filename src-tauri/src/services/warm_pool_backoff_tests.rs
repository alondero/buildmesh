//! The fill-failure backoff's own contract (issue #2602).
//!
//! The backoff is the module the pool's retry policy lives in, so its
//! arithmetic is pinned here rather than through the fill orchestration:
//! a mesh whose `git worktree add` always fails must wait longer and longer
//! between attempts, and must recover at once when the situation changes.

use super::*;
use std::time::Duration;

/// The schedule the issue asks for: 30 s doubling to 30 min.
#[test]
fn interval_doubles_from_thirty_seconds_to_a_thirty_minute_cap() {
    assert_eq!(interval_for(1), Duration::from_secs(30));
    assert_eq!(interval_for(2), Duration::from_secs(60));
    assert_eq!(interval_for(3), Duration::from_secs(120));
    assert_eq!(interval_for(4), Duration::from_secs(240));
    assert_eq!(interval_for(5), Duration::from_secs(480));
    assert_eq!(interval_for(6), Duration::from_secs(960));
    // 30 min is 1800 s, so the seventh doubling (1920 s) is clamped.
    assert_eq!(interval_for(7), MAX_INTERVAL);
    assert_eq!(interval_for(8), MAX_INTERVAL);
    assert_eq!(interval_for(1000), MAX_INTERVAL);
}

/// A mesh with no recorded failure may attempt a fill.
#[test]
fn an_unrecorded_mesh_may_attempt_at_once() {
    let backoff = FillBackoff::new();
    assert!(backoff.allows_attempt(7, "fingerprint", Instant::now()));
}

/// The second failure waits twice as long as the first.
#[test]
fn consecutive_failures_grow_the_interval() {
    let backoff = FillBackoff::new();
    let now = Instant::now();

    let first = backoff.record_failure(7, "fingerprint", "boom", now);
    assert!(
        first.first,
        "the first failure is the one that reports the break"
    );
    assert_eq!(first.failures, 1);
    assert_eq!(first.interval, BASE_INTERVAL);

    let second = backoff.record_failure(7, "fingerprint", "boom", now);
    assert!(!second.first);
    assert_eq!(second.failures, 2);
    assert_eq!(second.interval, BASE_INTERVAL * 2);
    assert!(
        second.interval_changed,
        "the escalation must be reported while the interval still grows"
    );

    let third = backoff.record_failure(7, "fingerprint", "boom", now);
    assert_eq!(third.interval, BASE_INTERVAL * 4);
}

/// The mesh may not attempt again until the interval has elapsed.
#[test]
fn an_attempt_before_the_interval_is_refused() {
    let backoff = FillBackoff::new();
    let now = Instant::now();

    backoff.record_failure(7, "fingerprint", "boom", now);
    assert!(
        !backoff.allows_attempt(7, "fingerprint", now + Duration::from_secs(29)),
        "29 s into a 30 s interval the fill must still wait"
    );
    assert!(
        backoff.allows_attempt(7, "fingerprint", now + Duration::from_secs(30)),
        "the interval's end is the moment the fill may run again"
    );
}

/// A changed fingerprint means the recorded failure no longer describes the
/// situation, so the mesh retries at once — and the next failure is a *first*
/// failure again.
#[test]
fn a_changed_fingerprint_resets_the_backoff() {
    let backoff = FillBackoff::new();
    let now = Instant::now();

    backoff.record_failure(7, "fingerprint", "boom", now);
    assert!(!backoff.allows_attempt(7, "fingerprint", now));

    assert!(
        backoff.allows_attempt(7, "other-fingerprint", now),
        "a changed HEAD or setting must let the fill run again at once"
    );

    let report = backoff.record_failure(7, "other-fingerprint", "boom", now);
    assert!(
        report.first,
        "the reset starts a new failure streak, so the break is reported again"
    );
    assert_eq!(report.interval, BASE_INTERVAL);
}

/// A successful fill ends the backoff.
#[test]
fn a_success_drops_the_record() {
    let backoff = FillBackoff::new();
    let now = Instant::now();

    backoff.record_failure(7, "fingerprint", "boom", now);
    assert!(backoff.record_success(7), "the recovery must be reportable");
    assert!(
        backoff.allows_attempt(7, "fingerprint", now),
        "a recovered mesh fills again on the very next pass"
    );
    assert!(backoff.failure(7).is_none());
    assert!(
        !backoff.record_success(7),
        "a success with nothing to recover must not claim a recovery"
    );
}

/// Once the interval reaches the cap it stops changing, so the retries after
/// that point are silent — the escalation has already been reported.
#[test]
fn the_interval_stops_changing_at_the_cap() {
    let backoff = FillBackoff::new();
    let now = Instant::now();

    // Failures 1..=7 each double the interval (the seventh reaches the cap:
    // 30 s × 2⁶ = 1920 s, clamped to 1800 s).
    for failure in 1..=7u32 {
        let report = backoff.record_failure(7, "fingerprint", "boom", now);
        assert!(
            report.interval_changed,
            "failure {failure} must report its longer interval"
        );
    }

    let at_cap = backoff.record_failure(7, "fingerprint", "boom", now);
    assert_eq!(at_cap.interval, MAX_INTERVAL);
    assert!(
        !at_cap.interval_changed,
        "the failure that reached the cap must not report another escalation"
    );

    let later = backoff.record_failure(7, "fingerprint", "boom", now);
    assert_eq!(later.interval, MAX_INTERVAL);
    assert!(
        !later.interval_changed,
        "a failure that does not change the interval must stay silent"
    );
}

/// The record carries the reason the fill failed, so the health surface can
/// show it without re-running the fill.
#[test]
fn the_record_keeps_the_failure_reason() {
    let backoff = FillBackoff::new();
    backoff.record_failure(
        7,
        "fingerprint",
        "invalid path 'a:Zone.Identifier'",
        Instant::now(),
    );
    let record = backoff.failure(7).expect("a record");
    assert_eq!(record.reason, "invalid path 'a:Zone.Identifier'");
    assert_eq!(record.failures, 1);
}
