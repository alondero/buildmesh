//! Synchronous coordination for fetch, sync and worktree creation.
//!
//! Git discovery resolves a canonical common-directory key before the map lock,
//! so root, subfolder and linked-worktree Meshes share repository coordination.
//! Non-Git paths retain their supplied key. Pool inventory remains per Mesh.
//! Call on blocking threads outside DB connections. Spawn paths use the bounded
//! variant to avoid waiting indefinitely behind manual synchronization.

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Process-global map from repository identity to its sync mutex.
/// Outer `Mutex` only guards map mutation; the inner `Arc<Mutex<()>>` is
/// the actual contention point.
static MESH_LOCKS: Lazy<Mutex<HashMap<String, Arc<Mutex<()>>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Run `f` under the per-Mesh sync lock for `mesh_path`. Blocking: callers
/// wait their turn rather than being skipped, because a skipped fetch
/// would leave the spawn on a stale ref (issue #652).
///
/// Lock-poison-safe: a `Poisoned` inner lock (a prior holder panicked
/// mid-`fetch`/`pull`) is recovered, so a transient panic in one spawn's
/// sync doesn't permanently jam each future spawn for the same Mesh.
///
/// Caller responsibilities:
/// - Pass the Mesh's DB-stored path string (typically `agent_nodes.path`).
/// - Call from a `spawn_blocking` thread (the inner work is a shell-out
///   to `git fetch` / `git pull --ff-only`); holding this lock across an
///   `.await` would block a tokio worker.
pub fn with_mesh_sync_lock<F, R>(mesh_path: &str, f: F) -> R
where
    F: FnOnce() -> R,
{
    // Get-or-insert the per-Mesh Arc<Mutex<()>>. The outer lock is held
    // for microseconds (one HashMap lookup or insert); the heavy lifting
    // happens on the inner lock, which is what callers actually contend on.
    let repository_key = crate::git::project::coordination_key(mesh_path);
    let arc_mutex = {
        let mut map = match MESH_LOCKS.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        // `entry().or_insert_with` keeps the lookup-or-insert atomic so two
        // concurrent first-time callers for the same Mesh don't race to
        // allocate two distinct mutexes.
        map.entry(repository_key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };

    // Acquire the per-Mesh lock. Blocking — a skipped fetch is wrong here
    // (see module docs). Poison recovery matches `try_with_fill_lock`.
    let _guard = match arc_mutex.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };

    f()
}

/// Like [`with_mesh_sync_lock`], but gives up after `timeout` instead of
/// waiting indefinitely. Returns `Some(f())` if the lock was acquired
/// within the deadline, `None` if the current holder didn't release in time.
///
/// Why this exists: the spawn-time auto-sync and the PR-head fetch used to
/// wait *unboundedly* on this lock. A manual Sync click (`git_sync` — up to
/// 300s fetch timeout plus 300s pull timeout under the same lock) or a
/// wedged remote could therefore stall each new-node spawn on that mesh
/// for many minutes: the "new nodes take minutes or fail to start" failure
/// mode. The spawn path's sync is best-effort by contract (ADR 0001:
/// always proceed from local HEAD on any sync failure), so a bounded wait
/// that degrades to the existing `mesh-sync-warning` toast is strictly
/// better than queueing behind a stuck sync.
///
/// `std::sync::Mutex` has no timed lock on stable, so this polls
/// `try_lock` on a 100ms tick — the same pattern (and justification) as
/// `process_util::run_command_with_timeout`. Callers run on the blocking
/// pool, so sleeping the polling thread is fine. Poison recovery matches
/// [`with_mesh_sync_lock`].
pub fn try_with_mesh_sync_lock_timeout<F, R>(
    mesh_path: &str,
    timeout: std::time::Duration,
    f: F,
) -> Option<R>
where
    F: FnOnce() -> R,
{
    let repository_key = crate::git::project::coordination_key(mesh_path);
    let arc_mutex = {
        let mut map = match MESH_LOCKS.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        map.entry(repository_key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };

    let deadline = std::time::Instant::now() + timeout;
    loop {
        match arc_mutex.try_lock() {
            Ok(_guard) => return Some(f()),
            // A poisoned lock means a prior holder panicked mid-sync; the
            // guarded unit `()` has no state to corrupt, so proceed — same
            // policy as the blocking acquire above.
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                let _guard = poisoned.into_inner();
                return Some(f());
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                if std::time::Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
}

#[cfg(test)]
#[path = "sync_lock_tests.rs"]
mod tests;
