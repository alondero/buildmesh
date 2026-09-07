//! Bounded `JoinHandle::join` for PTY reader / writer threads.
//!
//! Only [`join_with_timeout`] is shared across the codebase; every call
//! site is `commands::build_run::teardown_inc` or
//! `agent::process::teardown_incarnation`. The `JoinPolicy` enum that
//! used to live here is intentionally NOT in this module:
//! `commands::build_run` has its own minimal `JoinPolicy`
//! (`Join` / `Drop`) right next to `teardown_inc`, and
//! `agent::process` keeps its own local `JoinPolicy` (`Both` /
//! `WriterOnly`) — both with a doc comment explaining the difference.
//! Lifting either into `pty::lifecycle` would force one of them to
//! carry an irrelevant variant; the round-3 review's exact complaint
//! (review finding #5).
//!
//! **Why a watchdog.** `JoinHandle::join_timeout` does not exist on
//! stable, so we run the actual join on a separate thread and wait
//! on a oneshot channel. If the channel recv times out, we drop the
//! watchdog's `JoinHandle` at the end of its closure, which detaches
//! the inner reader per `JoinHandle::drop` docs. The watchdog thread
//! itself is cheap (it's idle in `join`) and exits as soon as the
//! reader does.

use std::thread::JoinHandle;

/// Join a thread, detaching it (via a watchdog) if it hasn't returned in
/// `timeout`. The reader thread keeps running but no longer holds the
/// joiner's stack; the watchdog outlives the timeout only when the reader
/// is genuinely stuck.
///
/// If the handle is already finished, the join runs inline — no watchdog
/// spawned.
pub fn join_with_timeout(handle: JoinHandle<()>, timeout: std::time::Duration) {
    if handle.is_finished() {
        let _ = handle.join();
        return;
    }
    let watch_name = match handle.thread().name() {
        Some(name) => format!("join-watch-{name}"),
        None => "join-watch-pty-worker".to_string(),
    };
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let _watchdog = std::thread::Builder::new()
        .name(watch_name)
        .spawn(move || {
            let _ = handle.join();
            // If the receiver is gone (we hit the timeout and detached),
            // the send errors silently; the `JoinHandle` is still dropped
            // on closure exit, detaching the reader thread.
            let _ = tx.send(());
        })
        .expect("failed to spawn join watchdog");
    let _ = rx.recv_timeout(timeout);
}

#[cfg(test)]
mod tests {
    //! Unit tests for the bounded-join watchdog. These pin the two
    //! invariants the close path depends on:
    //!
    //! 1. A finished handle joins inline (no watchdog spawned, no extra
    //!    wait).
    //! 2. A wedged handle is detached after `timeout` — the call
    //!    returns; the watchdog thread (which is genuinely stuck in
    //!    `join`) is dropped, leaving the OS thread orphaned but the
    //!    caller unblocked.

    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    /// A finished `JoinHandle` must take the fast lane — no watchdog
    /// spawned, the function returns immediately.
    #[test]
    fn finished_handle_joins_inline_without_watchdog() {
        let started = Instant::now();
        let handle = thread::spawn(|| {
            // Return unit so the JoinHandle<()> type matches
            // `join_with_timeout`'s parameter.
        });
        // Yield so the spawned thread has a chance to finish.
        thread::sleep(Duration::from_millis(10));
        join_with_timeout(handle, Duration::from_secs(2));
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "finished handle must join inline (no watchdog spawn); \
             elapsed = {:?}",
            started.elapsed()
        );
    }

    /// A wedged handle (one that sleeps past the timeout) must be
    /// detached after the watchdog fires — the caller returns within
    /// the timeout, the inner thread keeps running but is detached.
    ///
    /// We don't observe the wedge marker (the OS thread is detached but
    /// still running); we only assert the bounded-call contract.
    #[test]
    fn wedged_handle_is_detached_after_timeout() {
        let watchdog_fired = Arc::new(AtomicBool::new(false));
        let watchdog_fired_clone = Arc::clone(&watchdog_fired);

        let handle = thread::Builder::new()
            .name("test-pty-wedged-worker".to_string())
            .spawn(move || {
                // Sleep longer than the join timeout so the watchdog
                // join is guaranteed to be in flight by the time the
                // timeout fires.
                thread::sleep(Duration::from_secs(10));
                watchdog_fired_clone.store(true, Ordering::SeqCst);
            })
            .expect("spawn wedged worker");

        let started = Instant::now();
        join_with_timeout(handle, Duration::from_millis(200));
        let elapsed = started.elapsed();

        // The call must return well within the 10 s wedge (the 200 ms
        // timeout plus a small slack).
        assert!(
            elapsed < Duration::from_secs(1),
            "join_with_timeout must return after the watchdog fires; \
             elapsed = {:?}",
            elapsed
        );
        // Suppress unused-arc warning when the marker is never read.
        let _ = watchdog_fired;
    }
}