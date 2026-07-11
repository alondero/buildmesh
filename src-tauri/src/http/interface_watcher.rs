//! OS-level network interface change listener (issue #591).
//!
//! `apply_binding` (issue #585) re-enumerates interfaces on every rebind, but
//! only when something triggers it — a user LAN toggle, or startup. An
//! interface that appears **without** a user action (VPN connecting mid-session,
//! DHCP lease change, Wi-Fi reconnect) is still missed until the user re-toggles
//! LAN exposure or restarts. The TLS cert's SAN list is stale, and the per-
//! request `Host` guard rejects the new IP.
//!
//! The watcher below fires `on_change` whenever the OS signals that the
//! interface set may have shifted. The callback is debounced — DHCP renews
//! emit repeated events in tens of milliseconds; coalescing them inside this
//! module means one network change produces one rebind, not one per kernel
//! signal.
//!
//! ## Platform split
//!
//! - **Windows** — `NotifyAddrChange` from `Iphlpapi.dll`. The syscall blocks
//!   the current thread until any address change occurs, then returns. We
//!   run it on `spawn_blocking` and loop so a single watcher call outlives
//!   any number of changes.
//! - **macOS / Linux** — polling fallback. `list_afinet_netifas` is called
//!   every [`POLL_INTERVAL`]; an event is emitted only when the snapshot
//!   differs from the previous one, so a stable network stays silent.
//!
//! ## Test seam
//!
//! The debouncer is the testable unit. Tests construct their own
//! `tokio::sync::mpsc::channel`, call [`debounce_loop`] directly, and push
//! synthetic events through the sender — the production source code path
//! is exercised by smoke-test, not unit-test.

use std::time::Duration;

use tokio::sync::mpsc;

/// Quiet window for coalescing bursts of interface-change events. DHCP renews
/// can fire repeatedly within tens of milliseconds; we wait this long after the
/// last event before triggering a rebind so one network change → one rebind,
/// not one per kernel signal. Pinned at 250 ms (matches the issue's
/// proposal) — long enough to absorb a DHCP burst, short enough that the
/// user-visible "QR URL became reachable" lag is imperceptible.
pub const DEBOUNCE: Duration = Duration::from_millis(250);

/// Channel capacity for raw interface-change events. Bursts above this back-
/// pressure the source — by design, since the debouncer collapses them. Eight
/// is enough headroom for a typical DHCP burst (renew + route flush + link
/// update) without unbounded queueing.
const CHANNEL_CAPACITY: usize = 8;

/// Polling interval for the non-Windows fallback. A VPN that connects mid-
/// session is detected within this interval. 1 s keeps the user-perceived
/// "rebind lag" invisible while keeping the background wakeup cheap on
/// macOS / Linux.
#[cfg(not(target_os = "windows"))]
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Spawn the OS interface-change watcher and forward debounced events to
/// `on_change`. Returns immediately; the watcher lives for the rest of the
/// process. If the source task exits unexpectedly the cause is logged but
/// the server keeps running — a user toggle still rebinds.
///
/// `on_change` runs on a Tokio task; it should be `Send + Sync + 'static` and
/// cheap to call (it triggers a full `apply_binding` cycle, which is async-
/// safe and idempotent).
pub fn spawn_interface_watcher<F>(on_change: F)
where
    F: Fn() + Send + Sync + 'static,
{
    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);

    spawn_production_source(tx);

    // The debouncer is the part tests drive directly. It owns the receiver
    // and runs `on_change` once per debounce window.
    //
    // CRITICAL: must use `tauri::async_runtime::spawn`, NOT `tokio::spawn`.
    // `spawn_interface_watcher` is called from `start_http_server` which is
    // called from the Tauri setup callback. Setup runs synchronously during
    // `Builder::build` — before Tauri's tokio runtime is wired up — so
    // `tokio::spawn` panics with "there is no reactor running, must be called
    // from the context of a Tokio 1.x runtime" (issue #591 follow-up).
    // `tauri::async_runtime::spawn` uses Tauri's own runtime, which is
    // available from the very first setup invocation.
    tauri::async_runtime::spawn(debounce_loop(rx, on_change));
}

/// Coalesce a stream of raw interface-change events into debounced
/// `on_change` calls. Public so tests can drive it directly with a synthetic
/// `mpsc::Sender`, bypassing the platform source.
///
/// Semantics: after the first event in a burst, wait up to [`DEBOUNCE`] for
/// another event. Each new event resets the timer. When the timer elapses
/// without a new event, invoke `on_change` once and wait for the next burst.
/// A closed channel returns cleanly.
pub async fn debounce_loop<F>(mut rx: mpsc::Receiver<()>, on_change: F)
where
    F: Fn(),
{
    // Wait for the first event of a burst (or the channel closing).
    while rx.recv().await.is_some() {
        // Collect follow-up events within the quiet window. Each `recv` inside
        // the timeout resets the wait — classic debounce.
        loop {
            match tokio::time::timeout(DEBOUNCE, rx.recv()).await {
                Ok(Some(())) => continue, // another event arrived; keep waiting
                Ok(None) => return,       // channel closed; bail before invoking
                Err(_) => break,          // quiet window elapsed
            }
        }
        on_change();
    }
}

#[cfg(target_os = "windows")]
fn spawn_production_source(tx: mpsc::Sender<()>) {
    // Use `tauri::async_runtime::spawn_blocking`, NOT `tokio::task::spawn_blocking`.
    // Same reasoning as the debouncer above: `spawn_interface_watcher` is
    // called from the Tauri setup callback before any tokio runtime is in
    // scope. `tauri::async_runtime` provides a runtime handle from the first
    // call onward.
    tauri::async_runtime::spawn_blocking(move || windows_event_loop(tx));
}

/// Windows event loop: block on `NotifyAddrChange` forever, forwarding each
/// wakeup to the debouncer channel. The syscall blocks the current thread
/// until any address change occurs on the host — exactly what we want, since
/// `spawn_blocking` runs this on a dedicated thread pool slot.
#[cfg(target_os = "windows")]
fn windows_event_loop(tx: mpsc::Sender<()>) {
    use windows_sys::Win32::NetworkManagement::IpHelper::NotifyAddrChange;
    loop {
        // SAFETY: `NotifyAddrChange` with a NULL `Handle` selects synchronous
        // semantics — blocks the calling thread until an address change
        // occurs, then returns `NO_ERROR` (0). Non-zero is an error: log and
        // exit; the next user toggle will recover.
        let status = unsafe { NotifyAddrChange(std::ptr::null_mut(), std::ptr::null()) };
        if status != 0 {
            tracing::warn!(
                "NotifyAddrChange returned non-zero status {}; interface watcher exiting",
                status
            );
            return;
        }
        // `blocking_send` because we're on a `spawn_blocking` thread, not in
        // an async context. If the debouncer has been dropped (process is
        // tearing down) we exit cleanly.
        if tx.blocking_send(()).is_err() {
            return;
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn spawn_production_source(tx: mpsc::Sender<()>) {
    // Same reasoning as the Windows path: must use Tauri's runtime, not raw
    // tokio, because this is called from the Tauri setup callback.
    tauri::async_runtime::spawn(polling_event_loop(tx));
}

/// Polling fallback for macOS / Linux. Enumerate interfaces on a fixed
/// interval and emit when the snapshot changes.
///
/// Two safeguards prevent false-positive rebinds on a stable network:
/// - **Sort before compare.** `enumerate_interfaces()` returns OS-enumeration
///   order, which is unstable on macOS (`en0` vs `en1` swap). `Vec::==`
///   would fire a redundant rebind per poll. Sorting the snapshot before
///   comparing makes the diff order-independent.
/// - **Skip when LAN is off.** The debouncer would emit events into the
///   void and the callback would no-op in `reapply_binding`, paying a
///   syscall per second for nothing.
#[cfg(not(target_os = "windows"))]
async fn polling_event_loop(tx: mpsc::Sender<()>) {
    let mut last = sorted_snapshot();
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        // Skip the syscall when LAN exposure is off. `unwrap_or(false)` so a
        // wedged DB reads as "off" rather than blocking the watcher. We
        // still refresh `last` so an interface change during a LAN-off
        // window doesn't produce a stale diff when the user re-enables.
        // `crate::db`, not `super` — the setting lives on `app_settings`
        // (`db::lan_exposure_enabled`); the old `super::` path never existed
        // and only compiled because this whole fn is `cfg(not(windows))`
        // and the project is built on Windows.
        if !crate::db::lan_exposure_enabled().unwrap_or(false) {
            last = sorted_snapshot();
            continue;
        }
        let now = sorted_snapshot();
        if now != last {
            last = now;
            if tx.send(()).await.is_err() {
                return;
            }
        }
    }
}

/// Enumerate the host's interfaces and return a deterministic snapshot.
/// Sorting by `IpAddr` makes the order match across calls regardless of the
/// kernel's enumeration order, so a `Vec::==` diff on the polling path
/// doesn't false-positive on a stable network whose OS returns a different
/// order each poll.
#[cfg(not(target_os = "windows"))]
fn sorted_snapshot() -> Vec<std::net::IpAddr> {
    let mut snap = super::enumerate_interfaces();
    snap.sort();
    snap
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Regression test for the startup crash discovered 2026-06-30:
    /// `spawn_interface_watcher` is called from `start_http_server` which is
    /// called from the Tauri setup callback. Setup runs synchronously during
    /// `Builder::build` — BEFORE Tauri's tokio runtime is wired up — so a raw
    /// `tokio::spawn` or `tokio::task::spawn_blocking` from this context panics
    /// with "there is no reactor running, must be called from the context of a
    /// Tokio 1.x runtime". This test reproduces that environment (a fresh
    /// `std::thread` with no tokio runtime in scope) and asserts the watcher
    /// spawns cleanly.
    ///
    /// Failure mode caught: any use of `tokio::spawn` / `tokio::task::spawn_blocking`
    /// inside `spawn_interface_watcher` or its `spawn_production_source`
    /// helpers. The fix is to use `tauri::async_runtime::spawn` /
    /// `tauri::async_runtime::spawn_blocking` instead — Tauri's runtime is
    /// available from the first setup invocation onward.
    ///
    /// NB: the spawned production source (Windows `NotifyAddrChange` block,
    /// non-Windows 1 s polling) and the debouncer run for the rest of the
    /// process. They're cheap background work — no events fire during a test
    /// so the callback counter stays at zero. When the test process exits,
    /// the detached tasks are cleaned up.
    #[test]
    fn spawn_interface_watcher_runs_without_a_tokio_runtime() {
        let count = Arc::new(AtomicUsize::new(0));
        let count_cb = count.clone();

        // Bare thread — no tokio runtime in scope. Mirrors what the Tauri
        // setup callback looks like at the moment `start_http_server` runs.
        let handle = std::thread::Builder::new()
            .name("regression-spawn-interface-watcher".to_string())
            .spawn(move || {
                super::spawn_interface_watcher(move || {
                    count_cb.fetch_add(1, Ordering::SeqCst);
                });
            })
            .expect("failed to spawn test thread");

        // If `spawn_interface_watcher` panicked inside the thread — the
        // signature of `tokio::spawn` from a non-runtime context — `join()`
        // returns `Err`.
        handle.join().expect(
            "spawn_interface_watcher panicked — likely a tokio::spawn \
             from a context with no tokio runtime in scope. Use \
             tauri::async_runtime::spawn(/_blocking) instead.",
        );
    }

    /// One event arriving on a quiet channel → one `on_change` call after the
    /// debounce window. Pins the basic trigger contract.
    #[tokio::test]
    async fn single_event_triggers_one_call() {
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let count = Arc::new(AtomicUsize::new(0));
        let count_cb = count.clone();
        let task = tokio::spawn(debounce_loop(rx, move || {
            count_cb.fetch_add(1, Ordering::SeqCst);
        }));

        tx.send(()).await.unwrap();
        // Wait at least one full debounce window for the callback to fire.
        tokio::time::sleep(DEBOUNCE * 2).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "one event must trigger exactly one on_change call"
        );

        drop(tx);
        task.await.unwrap();
    }

    /// Five events inside one debounce window → exactly one `on_change` call.
    /// The DHCP-renew burst case from the issue's test plan.
    #[tokio::test]
    async fn burst_of_five_events_collapses_to_one_call() {
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let count = Arc::new(AtomicUsize::new(0));
        let count_cb = count.clone();
        let task = tokio::spawn(debounce_loop(rx, move || {
            count_cb.fetch_add(1, Ordering::SeqCst);
        }));

        // 5 events in 100 ms — well inside the 250 ms quiet window, so the
        // debouncer treats them as a single burst.
        for _ in 0..5 {
            tx.send(()).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Wait long enough for the post-burst quiet window to elapse.
        tokio::time::sleep(DEBOUNCE * 2).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "5 events in 100 ms must collapse to 1 on_change call, not 5"
        );

        drop(tx);
        task.await.unwrap();
    }

    /// A late event AFTER the debounce fires starts a new burst. This is the
    /// "second network change after the first rebind settled" case — we want
    /// two rebinds, not one forever-debounced mega-burst.
    #[tokio::test]
    async fn event_after_quiet_window_starts_new_burst() {
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let count = Arc::new(AtomicUsize::new(0));
        let count_cb = count.clone();
        let task = tokio::spawn(debounce_loop(rx, move || {
            count_cb.fetch_add(1, Ordering::SeqCst);
        }));

        // First burst: 1 event.
        tx.send(()).await.unwrap();
        tokio::time::sleep(DEBOUNCE * 2).await;
        assert_eq!(count.load(Ordering::SeqCst), 1, "first burst fires once");

        // Gap longer than the debounce window, then a second burst.
        tokio::time::sleep(DEBOUNCE + Duration::from_millis(50)).await;
        tx.send(()).await.unwrap();
        tokio::time::sleep(DEBOUNCE * 2).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "a second burst after the quiet window must fire again"
        );

        drop(tx);
        task.await.unwrap();
    }

    /// Closing the channel exits the debouncer cleanly. No callback, no hang.
    /// The `task.await` returns `Ok(())` rather than panicking on a dropped
    /// receiver — important because `spawn_interface_watcher`'s source task
    /// holds the only sender, and we need shutdown to be silent.
    #[tokio::test]
    async fn closed_channel_exits_debouncer_cleanly() {
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let count = Arc::new(AtomicUsize::new(0));
        let count_cb = count.clone();
        let task = tokio::spawn(debounce_loop(rx, move || {
            count_cb.fetch_add(1, Ordering::SeqCst);
        }));

        // Close without sending anything.
        drop(tx);
        // Bound the await so a regression that hangs (e.g. blocking forever
        // on `recv`) fails the test instead of stalling the suite.
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("debouncer did not exit after channel close")
            .expect("debouncer task panicked");
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }
}