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
    //! Unit tests for the bounded-join watchdog. Covers the two
    //! distinct branches:
    //!
    //! 1. `is_finished()` inline path — no watchdog spawned.
    //! 2. Watchdog + channel-receive timeout path — the wedge is
    //!    released via a release channel so the worker thread does
    //!    NOT leak for 10 s past the timeout (round-5 review finding
    //!    #2: the previous version slept 10 s and leaked the thread
    //!    on every `cargo test` run).

    use super::*;
    use std::sync::mpsc;
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

    /// The watchdog branch: a wedged worker must be detached after
    /// `timeout`. The worker blocks on a release channel; the test
    /// sets a short `timeout` (50 ms), waits for `join_with_timeout`
    /// to return (proving the timeout branch fired), then sends on
    /// the channel so the worker terminates cleanly. NO leaked
    /// thread — the worker exits as soon as the test sends on the
    /// channel (round-5 review finding #2 pattern).
    #[test]
    fn wedged_handle_is_detached_after_timeout() {
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let handle = thread::Builder::new()
            .name("test-pty-wedged-worker".to_string())
            .spawn(move || {
                // Block until the test releases us. A short safety
                // fallback (500 ms) ensures the worker exits even if
                // the test forgets to send — defensive belt-and-braces.
                let _ = release_rx.recv_timeout(Duration::from_millis(500));
            })
            .expect("spawn wedged worker");

        let started = Instant::now();
        // Bounded timeout: the watchdog must detach at 50 ms while
        // the worker is still blocked on release_rx.
        join_with_timeout(handle, Duration::from_millis(50));
        let elapsed = started.elapsed();

        // join_with_timeout returned at >= timeout (it had to wait)
        // and << 2×timeout (the watchdog fired promptly).
        assert!(
            elapsed >= Duration::from_millis(50),
            "join_with_timeout must respect the timeout; elapsed = {:?}",
            elapsed
        );
        assert!(
            elapsed < Duration::from_millis(300),
            "join_with_timeout must return promptly after timeout; \
             elapsed = {:?}",
            elapsed
        );

        // Unblock the worker so it terminates immediately. After this
        // send, the worker thread returns and exits — no leaked OS
        // thread past this line.
        release_tx.send(()).expect("release worker");
    }
}