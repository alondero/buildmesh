//! Durable worktree cleanup queue: drain, retry, dismissal, and staged-delete recovery.
//!
//! Queue recovery stays together so retries and operator dismissal share the same
//! staging and backoff rules; filesystem operations stay outside DB connections.

use super::*;
/// Serializes drains. Each drain processes the *whole* queue, so two running at
/// once (e.g. closing two nodes in quick succession, or a close overlapping the
/// startup reconcile) would both try to remove the same just-enqueued worktree
/// and race on its `.removing` staging directory. Holding this for the drain's
/// duration means a second drain waits, then re-lists and finds the first
/// already dequeued — no wasted work, no spurious cleanup-failed warnings.
pub(super) static DRAIN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Drain the pending worktree-removal queue: remove each queued worktree's
/// directory, dequeuing only the ones that succeed. Removals that still fail
/// (e.g. a handle is somehow held) stay queued for the next drain or next
/// launch, and are returned so the caller can warn the user. Safe to run from
/// the close path and from startup reconcile — `remove_one_worktree` is
/// idempotent (a missing directory counts as already removed).
///
/// Issue #2139 — a failure is no longer only "returned". Each one is persisted
/// on its queue row (failed step, OS error, attempt count, backoff deadline)
/// and reported at most once per *unchanged* blocker: the drain backs off
/// instead of retrying a block it cannot clear, and the user hears about a
/// stuck worktree once instead of on every drain.
pub fn process_pending_removals() -> Vec<BlockedCleanup> {
    drain_pending_removals(now_epoch_ms(), backoff_secs)
}

/// Persist each failed attempt, then decide which ones the user still needs to
/// hear about. Kept separate from [`process_removals`] so the "what happened"
/// half stays pure and DB-free while the bookkeeping half is the only DB-aware
/// part of the drain.
///
/// A blocker that the user has already been told about (`notified_error` holds
/// its signature) produces no event: a worktree that is still held by the same
/// process in the same way is not new information (issue #2139).
pub(super) fn record_blocked_cleanups(
    failures: Vec<(
        PendingWorktreeRemoval,
        crate::git::worktree::WorktreeRemovalFailure,
    )>,
    now: i64,
    backoff: impl Fn(i64) -> i64,
) -> Vec<BlockedCleanup> {
    let mut blocked = Vec::new();
    for (removal, failure) in failures {
        let attempts = removal.attempt_count + 1;
        let retry_not_before = now + backoff(attempts) * 1000;
        if let Err(e) = db::record_pending_removal_failure(
            &removal.worktree_path,
            failure.operation,
            &failure.detail,
            now,
            retry_not_before,
        ) {
            tracing::error!(
                "worktree cleanup for {} failed but its failure could not be recorded: {}",
                removal.node_name,
                e
            );
        }

        let notify = !removal.already_notified(failure.operation, &failure.detail, now);
        if notify {
            if let Err(e) = db::mark_pending_removal_notified(
                &removal.worktree_path,
                &crate::models::blocker_signature(failure.operation, &failure.detail),
                now,
            ) {
                tracing::error!(
                    "could not record that the user was warned about {}: {}",
                    removal.node_name,
                    e
                );
            }
        }

        let record =
            removal.with_failure(failure.operation, &failure.detail, now, retry_not_before);
        tracing::warn!(
            "worktree cleanup for {} still blocked after {} attempt(s): {} failed — {} \
             (next automatic retry not before {} ms{})",
            record.node_name,
            record.attempt_count,
            record.last_operation.as_deref().unwrap_or("unknown step"),
            failure.detail,
            retry_not_before,
            if notify {
                ""
            } else {
                "; blocker unchanged, warning suppressed"
            }
        );
        blocked.push(BlockedCleanup {
            removal: record,
            notify,
        });
    }
    blocked
}

/// A worktree cleanup that is still blocked, together with the evidence the UI
/// needs to make it actionable (issue #2139): the queue row after the failed
/// attempt, and whether this blocker is being reported for the first time.
pub struct BlockedCleanup {
    pub removal: PendingWorktreeRemoval,
    /// `false` when the user has already been told about this exact blocker —
    /// the drain still retries (with backoff), it just does not warn again.
    pub notify: bool,
}

/// The backoff schedule for automatic worktree-removal retries, in seconds.
///
/// A blocked cleanup is usually blocked by something the user must close (an
/// Explorer window, a stray helper). Retrying every few milliseconds buys
/// nothing and floods the log and the UI, so the drain waits: 5s, 10s, 20s …
/// capped at 5 minutes. The cap matters because a worktree blocked overnight
/// must still be attempted soon after launch rather than much later.
///
/// `attempt_count` is the number of failures *including* the one just made.
pub fn backoff_secs(attempt_count: i64) -> i64 {
    let exponent = attempt_count.saturating_sub(1).clamp(0, 30) as u32;
    (CLEANUP_BACKOFF_BASE_SECS << exponent).min(CLEANUP_BACKOFF_MAX_SECS)
}

pub(super) const CLEANUP_BACKOFF_BASE_SECS: i64 = 5;
pub(super) const CLEANUP_BACKOFF_MAX_SECS: i64 = 300;

/// Epoch milliseconds, used for the queue's attempt/backoff bookkeeping.
pub(super) fn now_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Every queued worktree cleanup with its current bookkeeping, in queue order.
/// The blocked-cleanup surface (and its actions) reads from here so what the
/// user sees is exactly what the drain knows.
pub fn list_blocked_worktree_cleanups() -> db::SqlResult<Vec<PendingWorktreeRemoval>> {
    db::list_pending_worktree_removals()
}

/// Retry one blocked cleanup now, ignoring its backoff.
///
/// A failed retry records the failure and schedules the next backoff, so
/// hammering a blocked path through the UI cannot turn into a retry storm.
///
/// Issue #653 guard, same as the drain: a path the warm pool currently holds as
/// `claimed` belongs to a live spawn's worktree, so the removal is skipped and
/// the tombstone is dequeued (claim supersedes the close's intent). Without
/// this check a manual Retry could delete a live agent's directory — the drain
/// is careful, the manual path has to be too.
pub fn retry_pending_worktree_removal(path: &str) -> CleanupRetryOutcome {
    // Serialise against the drain: a background drain and a manual retry must
    // not both remove (and both record) the same worktree.
    let _guard = DRAIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let record = match db::get_pending_worktree_removal(path) {
        Ok(Some(record)) => record,
        Ok(None) => return CleanupRetryOutcome::Gone,
        Err(e) => {
            // A read failure is not evidence that nothing is queued: say so,
            // attempt nothing, and leave the row exactly as it is.
            tracing::error!(
                "could not read the pending worktree removal for {}; not retrying: {}",
                path,
                e
            );
            return CleanupRetryOutcome::QueueReadFailed;
        }
    };

    match db::warm_pool_claims_path(path) {
        Ok(true) => {
            tracing::info!(
                "warm_pool: skipping manual retry for {} — path is claimed by a warm-pool spawn",
                path
            );
            if let Err(e) = db::delete_pending_worktree_removal(path) {
                tracing::error!(
                    "warm_pool: failed to dequeue superseded tombstone for claimed path {}: {}",
                    path,
                    e
                );
            }
            return CleanupRetryOutcome::ClaimedByLiveSpawn;
        }
        Ok(false) => {}
        Err(e) => {
            // Same fail-closed rule as the drain: an unreadable claim means we
            // do not know whether a live spawn owns this directory, so nothing
            // is touched.
            tracing::error!(
                "warm_pool: is_claimed check failed for {} ({}); not retrying",
                path,
                e
            );
            return CleanupRetryOutcome::QueueReadFailed;
        }
    }

    match crate::git::worktree::remove_one_worktree_and_branch_detailed(path) {
        Ok(()) => {
            if let Err(e) = db::delete_pending_worktree_removal(path) {
                tracing::error!(
                    "worktree {} was removed but failed to dequeue it: {}",
                    record.node_name,
                    e
                );
            }
            CleanupRetryOutcome::Removed
        }
        Err(failure) => {
            let now = now_epoch_ms();
            let attempts = record.attempt_count + 1;
            let retry_not_before = now + backoff_secs(attempts) * 1000;
            if let Err(e) = db::record_pending_removal_failure(
                path,
                failure.operation,
                &failure.detail,
                now,
                retry_not_before,
            ) {
                tracing::error!(
                    "could not record the failed retry for {}: {}",
                    record.node_name,
                    e
                );
            }
            CleanupRetryOutcome::StillBlocked(record.with_failure(
                failure.operation,
                &failure.detail,
                now,
                retry_not_before,
            ))
        }
    }
}

/// The outcome of a user-initiated retry of one blocked cleanup.
pub enum CleanupRetryOutcome {
    /// The directory and its git bookkeeping are gone; the queue entry is
    /// dequeued.
    Removed,
    /// Still blocked. Carries the updated row so the UI can show the new
    /// evidence without a second read.
    StillBlocked(PendingWorktreeRemoval),
    /// The queue entry no longer exists — already cleaned or dismissed.
    Gone,
    /// The warm pool has adopted this path as a live spawn's worktree. The
    /// tombstone was dequeued and the directory was left alone (issue #653).
    ClaimedByLiveSpawn,
    /// The queue row or the claim guard could not be read, so nothing was
    /// attempted. Distinct from `Gone`, which is a definite "nothing queued".
    QueueReadFailed,
}

/// What "Keep worktree" found on disk, so the UI can tell the truth about what
/// it did (issue #2139). The user-visible wording lives in [`Self::message`];
/// this type is not sent over the wire.
#[derive(Debug, Clone)]
pub enum WorktreeCleanupDismissal {
    /// The worktree was exactly where the queue row said, and is still there.
    /// Only the cleanup intent was cancelled.
    Unchanged,
    /// A removal had already renamed the worktree to `<path>.removing`; it has
    /// been moved back so the directory is usable again.
    RestoredFromStaging,
    /// The staged copy was moved back, but the staged delete had already
    /// removed part of it, so the restored directory is incomplete.
    RestoredIncomplete,
    /// The original path is absent and there is no staging copy to restore, so
    /// there is nothing left on disk to keep.
    NothingOnDisk,
    /// The staged copy could not be moved back — something still holds it — so
    /// the worktree is *not* where the queue row says it is. The cleanup intent
    /// is preserved: the row stays queued and the drain keeps trying, exactly
    /// as it would for any other blocked removal (issue #2139 review round 2).
    RestoreFailed,
}

impl WorktreeCleanupDismissal {
    /// What the user is told, given the worktree path that was requested.
    pub fn message(&self, path: &str) -> String {
        match self {
            Self::Unchanged => format!("Worktree kept in place: {path}"),
            Self::RestoredFromStaging => {
                format!("Worktree restored to {path} — the cleanup had already moved it aside")
            }
            Self::RestoredIncomplete => format!(
                "Worktree restored to {path}, but the cleanup had already deleted part of it"
            ),
            Self::NothingOnDisk => {
                format!("Nothing left to keep at {path} — the cleanup had already removed it")
            }
            Self::RestoreFailed => format!(
                "Couldn't move the worktree back to {path} — something still holds {path}.removing. \
                 The worktree is still there and the cleanup stays queued, so close what is \
                 holding it and try Keep worktree again."
            ),
        }
    }
}

/// The result of "Keep worktree": what happened on disk, and what happened to
/// the cleanup intent.
pub struct WorktreeCleanupDismissed {
    pub kind: WorktreeCleanupDismissal,
    /// Whether the cleanup intent was cancelled (the queue row is gone). False
    /// when the restore could not finish: the intent is deliberately preserved
    /// so the drain keeps trying, rather than abandoning a worktree that is
    /// not even where the queue row says it is.
    pub cancelled: bool,
    /// Set when the row *should* have been dequeued but could not be (only
    /// possible for the non-`RestoreFailed` outcomes).
    pub dequeue_error: Option<String>,
}

/// "Keep worktree" — the user has decided to leave the directory where it is.
///
/// The cleanup intent is cancelled so the drain stops retrying and stops
/// warning. The disk is only touched in the one case where a removal had already
/// moved the worktree aside (the `<path>.removing` staging from
/// `remove_worktree_dir_with_retry`): the staged copy is moved back, because
/// leaving it there would strand the agent's work under a name the user never
/// chose while the queue row claimed the worktree was still at `path`. A staged
/// delete that had already removed part of the tree is reported as such rather
/// than promised as an intact restore.
///
/// If that move-back fails — something still holds the staged copy — nothing is
/// cancelled: the disk no longer matches what the queue row says, so cancelling
/// the intent would abandon both the folder and the git bookkeeping. The row
/// stays queued, the drain keeps trying, and the user is told exactly that.
pub fn dismiss_pending_worktree_removal(path: &str) -> WorktreeCleanupDismissed {
    let kind = restore_staged_worktree(path);
    if matches!(kind, WorktreeCleanupDismissal::RestoreFailed) {
        return WorktreeCleanupDismissed {
            kind,
            cancelled: false,
            dequeue_error: None,
        };
    }
    let dequeue_error = db::delete_pending_worktree_removal(path).err().map(|e| {
        tracing::error!(
            "keep worktree: cancelled the intent for {} but failed to dequeue it: {}",
            path,
            e
        );
        e.to_string()
    });
    WorktreeCleanupDismissed {
        kind,
        cancelled: dequeue_error.is_none(),
        dequeue_error,
    }
}

/// Undo the staging rename a removal may have left behind at `path`, reporting
/// what was actually found. Returns `Unchanged` when the worktree is where the
/// queue row says it is.
pub(super) fn restore_staged_worktree(path: &str) -> WorktreeCleanupDismissal {
    let staging = format!("{}.removing", path.trim_end_matches(['/', '\\']));
    if std::path::Path::new(path).exists() {
        // The worktree is intact where it belongs; any leftover staging is a
        // separate, already-proven-unheld directory that the next removal will
        // reclaim on its own.
        return WorktreeCleanupDismissal::Unchanged;
    }
    if !std::path::Path::new(&staging).exists() {
        return WorktreeCleanupDismissal::NothingOnDisk;
    }
    match std::fs::rename(&staging, path) {
        Ok(()) => {
            // A staged delete that had started would have left the copy
            // partial; say so rather than promising an intact worktree.
            if staged_delete_had_started(path) {
                WorktreeCleanupDismissal::RestoredIncomplete
            } else {
                WorktreeCleanupDismissal::RestoredFromStaging
            }
        }
        Err(e) => {
            // The rename back failed, so something still holds the staged copy.
            // Nothing is restored, and the caller must keep the cleanup intent
            // rather than cancel it: the worktree is not where the queue row
            // says it is (issue #2139 review round 2).
            tracing::warn!(
                "keep worktree: could not move {} back to {}: {}",
                staging,
                path,
                e
            );
            WorktreeCleanupDismissal::RestoreFailed
        }
    }
}

/// Whether a staged copy shows signs that its delete had already begun. The
/// `.git` link is what makes a directory a worktree at all, so a staged copy
/// without it was gutted mid-delete — as was one with nothing left inside.
pub(super) fn staged_delete_had_started(path: &str) -> bool {
    let dir = std::path::Path::new(path);
    if !dir.join(".git").exists() {
        return true;
    }
    std::fs::read_dir(dir)
        .map(|entries| entries.flatten().count() == 0)
        .unwrap_or(false)
}

/// Pure orchestration for the drain: for each removal, attempt the directory
/// remove; on success dequeue it; on failure keep it queued and collect it.
/// Dependencies are injected so the "dequeue only on success" invariant — the
/// thing that guarantees failed cleanups get retried rather than lost — is
/// testable without the global DB or a real filesystem.
///
/// `is_claimed` is the issue #653 deleter-side guard: when it returns `true`
/// for a removal's path, the drain skips the `remove` call (the directory
/// belongs to a live spawn's worktree — deleting it would destroy the
/// agent's work) AND dequeues the pending entry (claim supersedes tombstone
/// intent). It does NOT append to `failures` — the path is alive, not
/// broken; no user warning is warranted.
pub(super) fn process_removals(
    pending: Vec<PendingWorktreeRemoval>,
    remove: impl Fn(&str) -> Result<(), crate::git::worktree::WorktreeRemovalFailure>,
    dequeue: impl Fn(&str) -> db::SqlResult<()>,
    is_claimed: impl Fn(&str) -> db::SqlResult<bool>,
) -> Vec<(
    PendingWorktreeRemoval,
    crate::git::worktree::WorktreeRemovalFailure,
)> {
    let mut failures = Vec::new();
    for removal in pending {
        match is_claimed(&removal.worktree_path) {
            Ok(true) => {
                // Issue #653 — the warm pool has this path as a live spawn's
                // worktree. Dequeue the tombstone (the spawn's intent
                // supersedes the close's intent) but do NOT touch the
                // directory. The spawn's `forget_after_spawn` will drop the
                // `claimed` row when the agent is up; any subsequent close
                // that enqueues a fresh tombstone at this path will be
                // handled normally because the row is then gone.
                tracing::info!(
                    "warm_pool: skipping pending removal for {} — path is claimed by a warm-pool spawn",
                    removal.worktree_path
                );
                if let Err(e) = dequeue(&removal.worktree_path) {
                    tracing::error!(
                        "warm_pool: failed to dequeue superseded tombstone for claimed path {}: {}",
                        removal.worktree_path,
                        e
                    );
                }
            }
            Ok(false) => match remove(&removal.worktree_path) {
                Ok(()) => {
                    if let Err(e) = dequeue(&removal.worktree_path) {
                        tracing::error!(
                            "removed worktree {} but failed to dequeue it: {}",
                            removal.worktree_path,
                            e
                        );
                    }
                }
                Err(failure) => {
                    tracing::warn!(
                        "worktree removal for {} failed during {}, will retry: {}",
                        removal.node_name,
                        failure.operation,
                        failure.detail
                    );
                    failures.push((removal, failure));
                }
            },
            Err(e) => {
                // The guard query failed — fail closed (don't remove the
                // directory) but DON'T drop the tombstone either, so the
                // next drain retries. This is the conservative choice: a
                // transient DB error must not cascade into "agent's worktree
                // silently deleted because we couldn't read the pool table".
                //
                // Deliberately NOT pushing to `failures`: the caller
                // (`commands::agent_node::drain_pending_removals`) emits a
                // `worktree-cleanup-failed` Tauri event per failure, which
                // renders a "worktree cleanup failed" toast to the user —
                // but no worktree cleanup was attempted here, only a pool-
                // claim read. A transient SQLite error during the guard
                // check must NOT surface as user-facing "cleanup failed"
                // noise; the next drain retries silently.
                tracing::warn!(
                    "warm_pool: is_claimed check failed for {} ({}); leaving tombstone queued for retry",
                    removal.worktree_path,
                    e
                );
            }
        }
    }
    failures
}

/// The drain itself, with its clock and backoff schedule injected.
///
/// Production passes the wall clock and the exponential schedule above; the
/// tests inject a fixed clock and a zero backoff so they can run a *real*
/// second drain immediately instead of either waiting out the first window or
/// simulating the bookkeeping half by hand (issue #2139 review round 1).
pub(crate) fn drain_pending_removals(
    now: i64,
    backoff: impl Fn(i64) -> i64,
) -> Vec<BlockedCleanup> {
    let _guard = DRAIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let pending = match db::list_pending_worktree_removals() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("could not read pending worktree removals: {}", e);
            return Vec::new();
        }
    };

    // Backoff gate: an entry inside its retry window is left untouched — no
    // attempt, no new failure row, no notification. This is what stops the
    // drain from hammering a block it already knows about (issue #2139).
    let due: Vec<PendingWorktreeRemoval> = pending
        .into_iter()
        .filter(|removal| {
            if removal.retry_due(now) {
                true
            } else {
                tracing::debug!(
                    "worktree cleanup for {} is backing off until {} ms; skipping",
                    removal.node_name,
                    removal.retry_not_before
                );
                false
            }
        })
        .collect();

    let failures = process_removals(
        due,
        crate::git::worktree::remove_one_worktree_and_branch_detailed,
        db::delete_pending_worktree_removal,
        // Issue #653 deleter-side guard: skip paths whose `warm_worktrees`
        // row is currently `claimed` (a live spawn has just adopted the
        // directory). The skip deques the pending removal — claim supersedes
        // tombstone intent, and the live agent's worktree must survive. When
        // the spawn's `forget_after_spawn` later drops the row, the path is
        // no longer claimed and any *new* pending removal (e.g. a subsequent
        // close of the same node) flows through normally.
        db::warm_pool_claims_path,
    );

    record_blocked_cleanups(failures, now, backoff)
}
