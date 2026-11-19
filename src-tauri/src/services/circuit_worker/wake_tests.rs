// The wake channel's own contract (issue #2831).
//
// These tests pin the channel the worker parks on: what a wake of each kind
// does to the worker's tick, and what the worker's own pass does to a
// wake. The pacing suite in `tests/worker_pacing.rs` pins the end-to-end
// behaviour over a background job.

use super::*;
use std::time::Duration;

#[test]
fn an_external_wake_brings_the_worker_back_at_once() {
    let wake = WorkerWake::new();
    wake.notify(WakeKind::External);
    let (woke, timed_out) = wake.park(Duration::from_secs(30));
    assert!(woke, "an external wake must not sit out the tick");
    assert!(!timed_out, "that wake must not be a timeout");
}

#[test]
fn self_work_does_not_shorten_the_workers_park() {
    let wake = WorkerWake::new();
    wake.notify(WakeKind::SelfWork);
    assert!(
        !wake.take_external(),
        "self-work must not arm an immediate re-pass"
    );
    let (woke, timed_out) = wake.park(Duration::from_millis(20));
    assert!(!woke, "self-work must not bring the worker back at once");
    assert!(timed_out, "the park must wait out the tick");
    assert_eq!(
        wake.take_self_work(),
        1,
        "the self-work must still be recorded for the next pass"
    );
}

#[test]
fn a_wake_the_workers_own_pass_raised_is_its_own_work() {
    assert_eq!(
        effective_kind(WakeKind::External),
        WakeKind::External,
        "a wake from outside the worker's pass is external"
    );
    in_worker_pass(|| {
        assert_eq!(
            effective_kind(WakeKind::External),
            WakeKind::SelfWork,
            "a wake the worker's own pass raised is its own work (issue #2831)"
        );
    });
}

#[test]
fn the_in_pass_marker_is_restored_after_a_panic() {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        in_worker_pass(|| panic!("the pass panicked"));
    }));
    assert!(result.is_err(), "the panic must reach the pass boundary");
    assert!(
        !inside_worker_pass(),
        "a panicking pass must not leave the worker thread marked"
    );
}

#[test]
fn the_channel_counts_the_passes_it_has_seen() {
    let before = passes();
    record_pass();
    record_pass();
    assert_eq!(
        passes(),
        before + 2,
        "the channel must count the worker's passes"
    );
}

#[test]
fn the_channel_records_which_site_raised_a_wake() {
    let wake = WorkerWake::new();
    notify_at(&wake, WakeKind::SelfWork, std::panic::Location::caller());
    let sources = top_wake_sources(20);
    assert!(
        sources
            .iter()
            .any(|(site, _)| site.contains("wake_tests.rs") && site.contains("SelfWork")),
        "the wake source must name the site that raised it: {sources:?}"
    );
}
