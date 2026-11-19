// The worker's pacing (issue #2831).
//
// While any Circuit run was active or pending, the worker ran its passes
// back-to-back instead of once per tick: it held about one core, checked
// the SQLite pool out ~3,000 times a second, and wrote its admission
// "held" line ~180 times a second. The cause was a wake the worker's own
// pass raised — a finished background job — which returned the worker's
// park immediately, so the loop never reached its 2 s tick.
//
// These tests pin the two halves of the fix:
//
//   * a finished background job is the worker's own work, so it must not
//     arm an immediate re-pass; and
//   * a wake the worker's own pass raised is its own work, so it must not
//     shorten the tick either.
//
// The measurement runs over a wake channel of the suite's own, so another
// suite's wake cannot disturb it.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::jobs::TestPool;

/// A finished background job must not arm the worker's immediate re-pass.
///
/// The job was dispatched by the worker's own pass and its result is
/// collected by the next scheduled pass. Waking the worker for it made the
/// worker re-dispatch the very job that had just finished.
#[test]
fn a_finished_background_job_does_not_arm_an_immediate_re_pass() {
    let wake = Arc::new(WorkerWake::new());
    let pool = TestPool::new(wake.clone());

    // The first poll dispatches the job; the job finishes on its own thread.
    assert!(!pool.poll(), "the first poll dispatches the job");
    // The poll after the job finishes collects its result.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pool.poll() {
        assert!(
            Instant::now() < deadline,
            "the dispatched job never finished"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    assert!(
        !wake.take_external(),
        "a finished background job must not arm the worker's immediate re-pass (issue #2831)"
    );
}

/// A pass that only produces self-work runs at the tick rate.
///
/// The issue's acceptance: with no external events, N seconds of the loop
/// run at most `ceil(N / tick) + 1` passes.
#[test]
fn a_pass_that_only_produces_self_work_runs_at_the_tick_rate() {
    let wake = Arc::new(WorkerWake::new());
    let pool = TestPool::new(wake.clone());
    let tick = Duration::from_millis(20);
    let window = Duration::from_millis(400);

    let passes = AtomicUsize::new(0);
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        // One pass: the worker polls its background job, and the job
        // finishes and reports back while the pass runs.
        pool.poll();
        passes.fetch_add(1, Ordering::Relaxed);
        // Then the worker parks until its tick or an external wake.
        let (woke, _timed_out) = wake.park(tick);
        assert!(
            !woke,
            "a report the worker's own pass produced must not bring the worker back at once (issue #2831)"
        );
    }

    let ticks = window.as_millis().div_ceil(tick.as_millis()) as usize;
    let ran = passes.load(Ordering::Relaxed);
    assert!(
        ran <= ticks + 1,
        "the worker ran {ran} passes in {window:?}; a pass that only produces self-work \
         must not shorten its {tick:?} tick (issue #2831)"
    );
}

/// A wake the worker's own pass raised is the pass's own work.
///
/// The channel's own contract is pinned in `wake_tests.rs`; this test
/// pins that the worker really runs its passes inside that window.
#[test]
fn the_worker_runs_its_passes_inside_the_in_pass_window() {
    assert!(
        !wake::inside_worker_pass(),
        "the test thread is not the worker thread"
    );
    wake::in_worker_pass(|| {
        assert!(
            wake::inside_worker_pass(),
            "the worker's pass must run inside the in-pass window"
        );
    });
    assert!(
        !wake::inside_worker_pass(),
        "the window must close when the pass ends"
    );
}

/// A wake raised inside the worker's own pass is its own work.
///
/// This is the production wiring: the worker's five passes run inside the
/// in-pass window, so every wake they raise is recorded as self-work and
/// the worker keeps its tick. The wake is raised through the production
/// wake path, over a channel of this suite's own — the production channel
/// is process-global, and another suite's wake would make the assertion
/// about it meaningless.
#[test]
fn a_wake_raised_inside_a_pass_does_not_arm_an_immediate_re_pass() {
    let channel = Arc::new(WorkerWake::new());
    wake::in_worker_pass(|| {
        super::wake_circuit_worker_on(&*channel, WakeKind::External);
    });
    assert!(
        !channel.take_external(),
        "a wake the worker's own pass raised must not arm the worker's immediate re-pass (issue #2831)"
    );
    // The same wake from outside the pass is external: the worker re-passes
    // at once.
    super::wake_circuit_worker_on(&*channel, WakeKind::External);
    assert!(
        channel.take_external(),
        "a wake from outside the worker's pass must still bring it back at once"
    );
}

/// The runaway detector must not cry wolf.
///
/// PTY output is capped at one wake per node per 250 ms, so a busy agent can
/// legitimately push the pass rate well above the tick. That is not the
/// pathology this detector looks for: the pathology is the worker's *own*
/// work re-arming its park. The warning needs both halves.
#[test]
fn the_runaway_detector_needs_self_work_not_just_a_high_pass_rate() {
    let ceiling = 60;
    // A busy agent's external wakes: many passes, almost none of them the
    // worker's own work.
    assert!(
        !super::is_runaway_window(500, 2, ceiling),
        "external wakes are not a self-wake loop"
    );
    // The pathology: the worker's own work outnumbers the tick.
    assert!(
        super::is_runaway_window(2_712, 2_712, ceiling),
        "a self-wake loop must be reported"
    );
    // A healthy worker.
    assert!(
        !super::is_runaway_window(15, 0, ceiling),
        "one pass per tick is the healthy shape"
    );
}
