//! The per-node cancellation registry for the diff walk (issue #1181).
//!
//! When the Agent Review panel re-fetches on every `git-changed` event
//! while an agent is editing, overlapping `run_blocking` tasks pile up on
//! the pool. Slice 1 (issue #1165) bounds the *rate* of fetches at the
//! frontend via `minRefetchIntervalMs`; this module bounds the *pool
//! pressure* on the Rust side by cancelling a stale in-flight diff when a
//! fresher one arrives for the same `node_id`. The map is keyed by
//! `node_id` because `AgentReviewPanel` and `CenterDiffOverlay` are each
//! focused on a single node at a time — so the natural granularity of
//! "stale" is "older request for the same node".
//!
//! `Arc<AtomicBool>` (rather than a `JoinHandle::abort()`) because
//! `spawn_blocking` runs a `FnOnce` closure that cannot be interrupted —
//! the only way to short-circuit a blocking walk is a flag the closure
//! polls between deltas. The flag also doubles as the seam a future
//! Tauri `AbortSignal` channel would flip.

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(test)]
#[path = "cancel_tests.rs"]
mod tests;

static DIFF_NODE_CANCEL: Lazy<Mutex<HashMap<i64, Arc<AtomicBool>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Sentinel returned to the frontend so its race guard can identify
/// "cancelled, not a real failure" and skip the `setError` branch (a
/// genuine error must still surface).
pub const DIFF_CANCELLED: &str = "diff_node_against_base cancelled";

/// Flip any prior flag for `node_id` (so its in-flight blocking task
/// exits at its next poll) and return a fresh flag for the new
/// invocation. Acquired inside the `run_blocking` closure, never in
/// the async wrapper, so an async-cancel between the `#[command]`
/// entry and `spawn_blocking` can't leak a map entry.
pub fn acquire_diff_cancel(node_id: i64) -> Arc<AtomicBool> {
    let mut map = DIFF_NODE_CANCEL
        .lock()
        .expect("DIFF_NODE_CANCEL mutex poisoned");
    if let Some(prev) = map.get(&node_id) {
        prev.store(true, Ordering::SeqCst);
    }
    let token = Arc::new(AtomicBool::new(false));
    map.insert(node_id, Arc::clone(&token));
    token
}

/// Lenient cleanup: ignore a poisoned mutex (the cleanup is best-effort
/// anyway, and the next `acquire_diff_cancel` for this `node_id` will
/// overwrite whatever stale entry we leave behind).
pub fn release_diff_cancel(node_id: i64, token: &Arc<AtomicBool>) {
    let Ok(mut map) = DIFF_NODE_CANCEL.lock() else {
        return;
    };
    // Only clear if our token is still the current one — a fresh
    // arrival will already have replaced us, and we mustn't stomp
    // their flag.
    if map
        .get(&node_id)
        .is_some_and(|current| Arc::ptr_eq(current, token))
    {
        map.remove(&node_id);
    }
}

/// `Drop` guard that releases the per-node cancel flag when the
/// `run_blocking` closure returns (success, error, panic, or cancel).
/// Without this, an early `Err(DIFF_CANCELLED)` return would leave the
/// prior token in `DIFF_NODE_CANCEL` until the next `acquire_diff_cancel`
/// for the same `node_id` displaces it — not a correctness bug (the next
/// acquire flips + replaces the entry either way), but the guard makes
/// the lifecycle explicit and ensures the map doesn't transiently hold
/// a `Arc<AtomicBool>` whose only reader has already exited.
pub struct DiffCancelGuard {
    node_id: i64,
    token: Arc<AtomicBool>,
}

impl DiffCancelGuard {
    /// Take the guard for `node_id` and its freshly acquired token.
    pub fn new(node_id: i64, token: Arc<AtomicBool>) -> Self {
        Self { node_id, token }
    }
}

impl Drop for DiffCancelGuard {
    fn drop(&mut self) {
        release_diff_cancel(self.node_id, &self.token);
    }
}

/// Single place the diff walk consults its cancellation token, so the
/// delta-boundary and per-hunk polls can't drift apart in ordering or in
/// which polls the test seam below can see.
pub fn cancel_requested(cancel: Option<&AtomicBool>) -> bool {
    let Some(token) = cancel else { return false };
    #[cfg(test)]
    // allow-test-in-production: the poll counter is the seam the cancellation test reads.
    note_cancel_poll();
    token.load(Ordering::SeqCst)
}

// Test-only observer for the polls in `cancel_requested` (issue #2229).
//
// The mid-walk cancellation test has to flip the token *between* two
// polls of a walk that is already running. A sibling thread that sleeps
// and then stores leaves the flip's timing to the scheduler; issue
// #2229 recorded the walk reaching the end first, so the assertion got
// `Ok` rather than the sentinel. The probe fires from the one poll
// helper on the walking thread, so the test picks the poll at which the
// flip happens. Each test thread has its own slot, so a probe one test
// installs is invisible to the tests running beside it.
/// Probe handed the 0-based poll number at each cancellation poll.
#[cfg(test)]
// allow-test-in-production: the poll probe is the seam the cancellation test drives.
pub(crate) type CancelPollProbe = std::rc::Rc<dyn Fn(usize)>;

#[cfg(test)]
// allow-test-in-production: the poll counters are the seam the cancellation test reads.
thread_local! {
    /// Polls observed on this thread since the probe was installed.
    static CANCEL_POLL_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static CANCEL_POLL_PROBE: std::cell::RefCell<Option<CancelPollProbe>> =
        const { std::cell::RefCell::new(None) };
}

/// Install (or clear) the cancellation-poll probe for the current thread
/// and reset its poll count.
#[cfg(test)]
// allow-test-in-production: the poll probe is the seam the cancellation test drives.
pub(crate) fn set_cancel_poll_probe(probe: Option<CancelPollProbe>) {
    CANCEL_POLL_COUNT.with(|count| count.set(0));
    CANCEL_POLL_PROBE.with(|slot| *slot.borrow_mut() = probe);
}

/// Polls this thread reached since [`set_cancel_poll_probe`] installed
/// its probe.
#[cfg(test)]
// allow-test-in-production: the poll counters are the seam the cancellation test reads.
pub(crate) fn cancel_poll_count() -> usize {
    CANCEL_POLL_COUNT.with(|count| count.get())
}

/// The token the registry currently holds for `node_id`, if any.
///
/// The registry has no production reader — the walk holds the token it was
/// given — so this exists for the test that asserts a stale release cannot
/// displace the current entry.
#[cfg(test)]
// allow-test-in-production: the registry reader is the seam the cancellation test asserts on.
pub(crate) fn current_cancel_token(node_id: i64) -> Option<Arc<AtomicBool>> {
    DIFF_NODE_CANCEL.lock().ok()?.get(&node_id).cloned()
}

/// Announce a poll that is about to load the token, handing the probe the
/// 0-based poll number. Called without the slot borrowed, so a probe that
/// blocks on a channel — or panics — leaves the seam usable.
#[cfg(test)]
// allow-test-in-production: the poll counter is the seam the cancellation test reads.
fn note_cancel_poll() {
    let index = CANCEL_POLL_COUNT.with(|count| {
        let index = count.get();
        count.set(index + 1);
        index
    });
    let probe = CANCEL_POLL_PROBE.with(|slot| slot.borrow().clone());
    if let Some(probe) = probe {
        probe(index);
    }
}
