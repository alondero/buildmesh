//! Device-session expiry sweep (issue #589).
//!
//! # The problem this owns
//!
//! `device_sessions` had no expiry at all. A phone that re-pairs — cleared
//! site data, a new browser, a reinstall — leaves its previous row behind:
//! the old token is never presented again, so the row sits in the "Authorized
//! Devices" panel as a stale duplicate that still authenticates as **Admin**.
//! `created_at` and `last_active_at` were recorded but drove no decision, so
//! those orphans accumulated for the lifetime of the install and every one of
//! them stayed a live credential.
//!
//! # The sweep
//!
//! One pass deletes every device session that has not authenticated for
//! [`DEVICE_SESSION_IDLE_DAYS`], and kicks any live WebSocket the deleted
//! device still held. The row's `last_active_at` is the idle clock, and an
//! authenticated request refreshes it (`db::refresh_device_session_activity`)
//! — so the idle window only opens under a device that is genuinely gone, not
//! one that is merely quiet.
//!
//! # Why this is its own worker
//!
//! Deleting a device row is a *revocation*, not the garbage collection of a
//! history table: it removes a live credential and it has to close the
//! sockets that credential opened. The ledger GC
//! (`services::coordinator_ledger_maintenance`) owns bounded-age GC of
//! append-only history rows and explicitly says what belongs there; a
//! credential revocation is a different kind of sweep, so it gets its own
//! module, its own cadence and its own tests.
//!
//! # Worker shape
//!
//! The same long-lived worker pattern as the other background threads: a
//! `std::thread::spawn` whose body is
//! `loop { sweep…; sleep(SWEEP_INTERVAL); }`. The very first iteration runs
//! immediately — sweeping on every launch is the point of the worker, because
//! an install that was closed for a year should not come back with a year of
//! orphans in the panel. Failures are logged and swallowed per pass: a
//! transient SQLite error must not kill the worker thread, and the next tick
//! re-tries.

use std::time::Duration;

use crate::process_util::run_worker_pass;

/// How long (in days) a paired device session may go without authenticating
/// before the sweep deletes it.
///
/// 90 days is long enough that a device in regular use is never at risk — an
/// authenticated request refreshes the row's activity, so an in-use device
/// stays warm — and short enough that a phone which is genuinely gone does
/// not leave a live Admin credential behind for years. It is deliberately
/// shorter than the 400-day session cookie, so a phone that comes back after
/// a long absence is asked to re-pair rather than being silently trusted
/// again.
///
/// This constant is the single source of truth: bump it here, no schema
/// change and no code change elsewhere.
pub const DEVICE_SESSION_IDLE_DAYS: i64 = 90;

/// How often the sweep runs.
///
/// Six hours is a long interval — an orphan only becomes worth removing once
/// it has been idle for the whole window, so sweeping more often would
/// delete nothing extra. The sweep is a single bounded DELETE over a
/// single-digit-row table, so it costs nothing when it does fire.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Start the expiry worker. Called once from `lib.rs::setup`, alongside the
/// other background workers. The worker runs forever in its own thread;
/// failures inside a pass are logged and swallowed so a transient error can't
/// kill the loop.
pub fn start_worker() {
    std::thread::Builder::new()
        .name("device-session-expiry".to_string())
        .spawn(|| {
            // The very first iteration runs immediately: an install that has
            // been closed for months should not come back to life with a
            // panel full of orphans. No startup delay — the sweep is a single
            // bounded DELETE and is safe to run on every launch.
            loop {
                run_worker_pass("device-session-expiry", || {
                    if let Err(e) = sweep_once() {
                        tracing::warn!(
                            "device_session_expiry: sweep failed (next tick will retry): {e}"
                        );
                    }
                });
                std::thread::sleep(SWEEP_INTERVAL);
            }
        })
        .expect("failed to spawn device-session-expiry thread");
}

/// One sweep pass: delete every device session that has not authenticated for
/// [`DEVICE_SESSION_IDLE_DAYS`], and kick any live WebSocket the deleted
/// device still held.
///
/// The kick matters as much as the delete: a deleted row stops the *next*
/// request from authenticating, but an already-open WebSocket never re-checks
/// auth, so it would keep streaming a node's PTY to a device that has just
/// been expired. `http::revocation::revoke` is the same signal the desktop
/// revoke path fires.
///
/// Errors propagate so the worker can log and retry on the next tick.
pub fn sweep_once() -> Result<usize, String> {
    let expired = {
        let conn = crate::db::write_conn();
        crate::db::prune_inactive_device_sessions_inner(&conn, DEVICE_SESSION_IDLE_DAYS)
            .map_err(|e| e.to_string())?
    };
    let count = expired.len();
    for id in &expired {
        crate::http::revocation::revoke(*id);
    }
    if count > 0 {
        tracing::info!(
            "device_session_expiry: expired {count} device session(s) idle for more than {DEVICE_SESSION_IDLE_DAYS} days"
        );
    }
    Ok(count)
}

#[cfg(test)]
#[path = "device_session_expiry_tests.rs"]
mod tests;
