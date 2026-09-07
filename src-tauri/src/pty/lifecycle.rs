//! Shared PTY lifecycle helpers — used by both [`crate::commands::build_run`]
//! and [`crate::agent::process`] to join reader threads with a bounded timeout
//! and to share a single JoinPolicy vocabulary between them. Previously each
//! module had its own copy (issue #1532 review finding: copy-pasted
//! architecture); this module is the single source of truth.
//!
//! **Why a watchdog.** `JoinHandle::join_timeout` does not exist on stable,
//! so we run the actual join on a separate thread and wait on a oneshot
//! channel. If the channel recv times out, we drop the watchdog's
//! `JoinHandle` at the end of its closure, which detaches the inner
//! reader per `JoinHandle::drop` docs. The watchdog thread itself is cheap
//! (it's idle in `join`) and exits as soon as the reader does.

use std::thread::JoinHandle;

/// Whether teardown should join the PTY reader thread or just detach it.
///
/// `Drop` is the case where the caller IS the reader (natural EOF reaping);
/// joining yourself is a guaranteed self-deadlock. `Join` is every other
/// teardown path (explicit close, replacement spawn).
#[derive(Debug, Clone, Copy)]
pub enum JoinPolicy {
    /// Wait up to the caller's chosen `timeout` for the reader to exit,
    /// then detach if it hasn't.
    Join,
    /// The caller IS the reader — drop the handle without joining.
    /// `JoinHandle::drop` detaches per stdlib docs.
    Drop,
}

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