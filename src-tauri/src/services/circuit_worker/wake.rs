//! The Circuit worker's wake protocol.
//!
//! The worker parks between passes. A wake says *what happened*, and the
//! kind decides whether the worker comes back at once:
//!
//! * **External** — something outside the worker changed state the next
//!   pass must act on: a user's action, a native receipt, PTY output, a run
//!   minted elsewhere. The worker re-passes immediately.
//! * **SelfWork** — the worker's own pass produced work the next scheduled
//!   pass will observe: a background job finished, a durable commit landed.
//!   The worker keeps its tick.
//!
//! The distinction is load-bearing (issue #2831). A wake the worker's own
//! pass raised used to return the worker's park immediately, so a pass that
//! always produced a wake never reached its 2 s tick: the worker ran
//! back-to-back passes at ~90 a second, held a core busy, checked the SQLite
//! pool out thousands of times a second and wrote its admission log hundreds
//! of times a second. Work the worker itself started is collected by the
//! next scheduled pass, so waking for it only re-dispatched the work that
//! had just finished.
//!
//! The channel also counts the wakes it receives, by call site, and counts
//! the worker's passes. The worker uses both to notice that it is running
//! faster than its tick allows and to name the site that kept the loop hot
//! (issue #2831, work item 1).

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use once_cell::sync::Lazy;

#[cfg(test)]
#[path = "wake_tests.rs"]
mod tests;

/// What a wake reports.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum WakeKind {
    /// Something outside the worker changed state the next pass must act on.
    External,
    /// The worker's own pass produced work the next scheduled pass observes.
    SelfWork,
}

/// The worker's wake channel.
///
/// The worker parks on it between passes. A wake says what happened; the
/// channel decides whether the worker must come back at once.
pub(super) trait WakeChannel: Send + Sync {
    /// Record a wake of the given kind.
    fn notify(&self, kind: WakeKind);
    /// Park until an external wake arrives or `timeout` elapses.
    /// Returns `(woke, timed_out)`.
    fn park(&self, timeout: Duration) -> (bool, bool);
    /// Take a pending external wake.
    fn take_external(&self) -> bool;
    /// Take the self-work recorded since the last call.
    fn take_self_work(&self) -> u64;
}

/// The production wake channel: an external-wake flag, a condvar the worker
/// parks on, and the self-work counter.
pub(super) struct WorkerWake {
    /// The channel's flag. Public to the worker module so the poison
    /// recovery path can be exercised against the production channel.
    pub(super) state: Mutex<WakeState>,
    cvar: Condvar,
}

#[derive(Default)]
pub(super) struct WakeState {
    /// An external wake is pending: the worker re-passes at once.
    external: bool,
    /// Work the worker's own pass produced, awaiting the next pass.
    self_work: u64,
}

impl WorkerWake {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(WakeState::default()),
            cvar: Condvar::new(),
        }
    }

    /// Lock the state, recovering from a poisoned mutex instead of panicking
    /// (issue #1224): a panic mid-write must not brick the worker's park.
    fn state(&self) -> std::sync::MutexGuard<'_, WakeState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl WakeChannel for WorkerWake {
    fn notify(&self, kind: WakeKind) {
        let mut state = self.state();
        match kind {
            WakeKind::External => {
                state.external = true;
                // Hold the lock across the notify so a worker about to wait
                // either sees the flag before waiting, or is already waiting
                // and the notify wakes it.
                self.cvar.notify_all();
            }
            WakeKind::SelfWork => {
                // Deliberately no notify: the next scheduled pass observes
                // this work, and waking now would only re-dispatch the work
                // that had just finished (issue #2831).
                state.self_work = state.self_work.saturating_add(1);
            }
        }
    }

    fn park(&self, timeout: Duration) -> (bool, bool) {
        let state = self.state();
        let (mut state, timeout_result) =
            match self
                .cvar
                .wait_timeout_while(state, timeout, |state| !state.external)
            {
                Ok(pair) => pair,
                Err(poisoned) => poisoned.into_inner(),
            };
        let woke = state.external;
        state.external = false;
        (woke, timeout_result.timed_out())
    }

    fn take_external(&self) -> bool {
        let mut state = self.state();
        std::mem::replace(&mut state.external, false)
    }

    fn take_self_work(&self) -> u64 {
        let mut state = self.state();
        std::mem::replace(&mut state.self_work, 0)
    }
}

/// The production wake channel.
pub(super) static WAKE: Lazy<Arc<WorkerWake>> = Lazy::new(|| Arc::new(WorkerWake::new()));

/// A shared channel is still a channel.
///
/// The job pool holds its channel as an `Arc<dyn WakeChannel>`, so the
/// production channel has to answer through the same trait.
impl<T: WakeChannel + ?Sized> WakeChannel for Arc<T> {
    fn notify(&self, kind: WakeKind) {
        (**self).notify(kind);
    }

    fn park(&self, timeout: Duration) -> (bool, bool) {
        (**self).park(timeout)
    }

    fn take_external(&self) -> bool {
        (**self).take_external()
    }

    fn take_self_work(&self) -> u64 {
        (**self).take_self_work()
    }
}

thread_local! {
    /// Set while the worker thread is inside one of its own passes.
    ///
    /// A wake raised in that window is the pass's own work: the next
    /// scheduled pass observes it. Without this marker the worker's own
    /// commits re-armed its park, and the loop never rested (issue #2831).
    static IN_WORKER_PASS: Cell<bool> = const { Cell::new(false) };
}

/// Restores the in-pass marker when the guard drops, so a panic inside a
/// pass cannot leave the worker thread permanently marked.
struct InPassGuard;

impl Drop for InPassGuard {
    fn drop(&mut self) {
        IN_WORKER_PASS.with(|flag| flag.set(false));
    }
}

/// Run `body` as the worker thread's own pass.
///
/// Every wake raised inside the window is the pass's own work, unless the
/// site says otherwise with a durable-change wake.
pub(super) fn in_worker_pass<R>(body: impl FnOnce() -> R) -> R {
    IN_WORKER_PASS.with(|flag| flag.set(true));
    let _guard = InPassGuard;
    body()
}

/// Is the calling thread inside the worker thread's own pass?
pub(super) fn inside_worker_pass() -> bool {
    IN_WORKER_PASS.with(|flag| flag.get())
}

/// The kind a requested wake actually has.
///
/// A wake the worker's own pass raised is its own work, so it is recorded
/// as self-work: the next scheduled pass observes it. A site whose change
/// the next pass must act on *at once* — a terminal state that frees a
/// capacity slot — asks for a durable wake instead, which is never
/// downgraded.
pub(super) fn effective_kind(requested: WakeKind) -> WakeKind {
    if inside_worker_pass() {
        WakeKind::SelfWork
    } else {
        requested
    }
}

// ---- Diagnostics ----------------------------------------------------------

/// Wake sources: how many wakes each call site raised.
///
/// The worker logs the breakdown when it notices it is running more passes
/// than its tick allows, so the next occurrence names the site that kept
/// the loop hot instead of only its symptoms (issue #2831).
static WAKE_SOURCES: Lazy<Mutex<HashMap<String, u64>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// Passes the worker has run.
static PASSES: AtomicU64 = AtomicU64::new(0);

/// Count one worker pass.
pub(super) fn record_pass() -> u64 {
    PASSES.fetch_add(1, Ordering::Relaxed) + 1
}

/// Passes the worker has run.
pub(super) fn passes() -> u64 {
    PASSES.load(Ordering::Relaxed)
}

/// Record a wake and the site that raised it.
fn record_source(site: &std::panic::Location<'_>, kind: WakeKind) {
    let key = format!("{}:{} ({:?})", site.file(), site.line(), kind);
    let mut sources = match WAKE_SOURCES.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let counter = sources.entry(key).or_insert(0);
    *counter = counter.saturating_add(1);
}

/// The wake sites that raised the most wakes, most frequent first.
pub(super) fn top_wake_sources(limit: usize) -> Vec<(String, u64)> {
    let sources = match WAKE_SOURCES.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let mut ranked: Vec<(String, u64)> = sources
        .iter()
        .map(|(site, count)| (site.clone(), *count))
        .collect();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    ranked.truncate(limit);
    ranked
}

/// Record a wake and the site that raised it, then deliver it on `channel`.
///
/// The site is passed explicitly because `#[track_caller]` only propagates
/// through a chain of `#[track_caller]` functions; the worker's entry
/// points forward their own caller so the diagnostic names the site that
/// asked for the wake, not the wrapper.
#[track_caller]
pub(super) fn notify_at(
    channel: &dyn WakeChannel,
    kind: WakeKind,
    site: &std::panic::Location<'_>,
) {
    record_source(site, kind);
    channel.notify(kind);
}
