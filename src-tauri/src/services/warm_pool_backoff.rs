//! Per-mesh fill-failure backoff for the pre-spawn pool (issue #2602).
//!
//! Why
//! ---
//! A mesh whose `git worktree add` fails *deterministically* — a tracked
//! path Windows refuses to check out (`playbook app icon.png:Zone.Identifier`),
//! a repository whose checkout cannot complete — failed on each idle fill
//! pass. The pool worker ticked on a 30-second cadence, so the mesh burned a
//! `git worktree add`, a `warm_worktrees` row and a WARN line per tick for
//! as long as the app stayed open. No retry could succeed and no retry
//! learned anything from the one before it.
//!
//! The backoff
//! -----------
//! Each mesh keeps its own failure record. After a failed fill the next
//! attempt waits 30 s, then 60 s, then 120 s, … capped at 30 minutes. The
//! record is dropped when the mesh's HEAD or settings change — the inputs
//! the failure depended on — or when a fill succeeds, so a repaired mesh
//! recovers on the next pass instead of waiting out the interval.
//!
//! The record lives in memory on purpose. The pool's *bookkeeping* is
//! durable (the `warm_worktrees` rows); its *retry policy* is a property of
//! this app run, and a restart is a natural reset — the user closed the app
//! and came back, which is the moment to try again.
//!
//! The fingerprint
//! ---------------
//! The record is keyed by a fingerprint of the fill's inputs: the mesh's
//! path, its base ref, its pool size, its worktree container and the
//! repository's HEAD. When the fingerprint changes, the recorded failure
//! described a situation that no longer holds, so it is discarded and the
//! fill is attempted at once.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The wait after a mesh's first fill failure.
pub(crate) const BASE_INTERVAL: Duration = Duration::from_secs(30);

/// The longest the backoff ever waits between two attempts.
pub(crate) const MAX_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// The interval to wait after the `failures`-th consecutive failure.
///
/// Doubles from [`BASE_INTERVAL`] and is capped at [`MAX_INTERVAL`].
pub(crate) fn interval_for(failures: u32) -> Duration {
    if failures <= 1 {
        return BASE_INTERVAL;
    }
    // 30 s doubled 32 times already overflows a `u64` second count, so the
    // exponent is clamped *before* the shift. The cap makes the exponents
    // beyond the point where the interval saturates equivalent anyway.
    let exponent = (failures - 1).min(32);
    let seconds = BASE_INTERVAL
        .as_secs()
        .saturating_mul(1u64 << exponent)
        .min(MAX_INTERVAL.as_secs());
    Duration::from_secs(seconds)
}

/// One mesh's fill-failure record.
#[derive(Debug, Clone)]
pub(crate) struct FillFailure {
    /// Consecutive failures since the last success or fingerprint change.
    pub(crate) failures: u32,
    /// How long the next attempt has to wait.
    pub(crate) interval: Duration,
    /// When the next attempt may run.
    pub(crate) next_attempt: Instant,
    /// The fingerprint the failure was recorded against.
    pub(crate) fingerprint: String,
    /// The reason of the most recent failure, verbatim.
    pub(crate) reason: String,
}

/// What a recorded failure means for the caller's log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FailureReport {
    /// `true` for the mesh's first failure since its last success — the line
    /// that tells the user their pool is broken.
    pub(crate) first: bool,
    /// `true` when the retry interval changed. The interval stops changing
    /// once it reaches [`MAX_INTERVAL`], so the retries after that are
    /// silent: the escalation has already been reported.
    pub(crate) interval_changed: bool,
    /// How many consecutive failures have now happened.
    pub(crate) failures: u32,
    /// How long until the next attempt.
    pub(crate) interval: Duration,
}

/// The per-mesh fill-failure records.
#[derive(Debug, Default)]
pub(crate) struct FillBackoff {
    records: Mutex<HashMap<i64, FillFailure>>,
}

impl FillBackoff {
    /// A backoff with no recorded failures.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The records, tolerating a poisoned lock: a panic in one fill must not
    /// take the pool worker down with it.
    fn records(&self) -> MutexGuard<'_, HashMap<i64, FillFailure>> {
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Whether `mesh_id` may attempt a fill right now.
    ///
    /// A record whose fingerprint no longer matches is dropped first: the
    /// mesh's HEAD or settings changed, so the recorded failure no longer
    /// describes the situation, and the fill is attempted at once.
    pub(crate) fn allows_attempt(&self, mesh_id: i64, fingerprint: &str, now: Instant) -> bool {
        let mut records = self.records();
        match records.get(&mesh_id) {
            None => true,
            Some(record) if record.fingerprint != fingerprint => {
                records.remove(&mesh_id);
                true
            }
            Some(record) => now >= record.next_attempt,
        }
    }

    /// Record a failed fill and report what the caller should log.
    pub(crate) fn record_failure(
        &self,
        mesh_id: i64,
        fingerprint: &str,
        reason: &str,
        now: Instant,
    ) -> FailureReport {
        let mut records = self.records();
        let previous = records.get(&mesh_id);
        let failures = previous.map(|record| record.failures + 1).unwrap_or(1);
        let interval = interval_for(failures);
        let first = previous.is_none();
        let interval_changed = previous
            .map(|record| record.interval != interval)
            .unwrap_or(true);
        records.insert(
            mesh_id,
            FillFailure {
                failures,
                interval,
                next_attempt: now + interval,
                fingerprint: fingerprint.to_string(),
                reason: reason.to_string(),
            },
        );
        FailureReport {
            first,
            interval_changed,
            failures,
            interval,
        }
    }

    /// Record a successful fill: the mesh's backoff is over.
    ///
    /// Returns `true` when there was a record to drop, so the caller can
    /// report the recovery.
    pub(crate) fn record_success(&self, mesh_id: i64) -> bool {
        self.records().remove(&mesh_id).is_some()
    }

    /// The mesh's current failure record, if any.
    pub(crate) fn failure(&self, mesh_id: i64) -> Option<FillFailure> {
        self.records().get(&mesh_id).cloned()
    }
}

#[cfg(test)]
#[path = "warm_pool_backoff_tests.rs"]
mod tests;
