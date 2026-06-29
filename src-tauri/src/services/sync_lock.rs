//! Per-Mesh **synchronous** mutex for `git fetch` + `git pull --ff-only` runs.
//!
//! Why this exists (issues #652, #680)
//! -----------------------------------
//! The spawn-time auto-sync (`git::sync::fetch_origin`) and the manual
//! `git_sync` Tauri command (`commands::git::git_sync`) both shell out
//! to `git fetch <remote> <branch>` then `git pull --ff-only --no-rebase`
//! on the parent Mesh. Without serialization, N concurrent callers
//! against the same mesh race on `.git/FETCH_HEAD`, `.git/index.lock`,
//! and `.git/refs/heads/<branch>.lock` — one process's `git fetch`
//! succeeds while another's fails with
//! `FetchFailed("... another git process ...")`, leaving the losers
//! parked on a stale ref. A local bare-remote repro at N=2/5/10
//! produced zero failures (git's per-process fetch lock serializes
//! and the fetch is too fast to overlap meaningfully); real GitHub
//! remotes have wide enough timing windows to expose the collision.
//!
//! #652 wired the spawn-time auto-sync into this lock; #680 added the
//! manual `git_sync` command so a Sync click can't race against an
//! in-flight spawn. Today every `git fetch + git pull --ff-only`
//! shell-out in the spawn-time auto-sync and manual sync paths routes
//! through `with_mesh_sync_lock`.
//!
//! Out of scope (deliberate): the PR-spawn path's single-ref
//! `git fetch` shell-outs (`git::worktree::provision::fetch_single_ref`
//! and `fetch_fork_head`) and the worktree prune's `git fetch` operate
//! on different contention points (`refs/remotes/<remote>/<ref>.lock`
//! and a different worktree's `.git`, respectively) and are best
//! handled by their own helpers.
//!
//! The existing `services::pool_worker::FILL_LOCK` is `try_lock`-or-skip and
//! is the wrong shape here — a *skipped* fetch would leave the spawn without
//! a fresh ref, so the agent lands on stale commits. This module uses a
//! **blocking** mutex: the second concurrent caller waits for the first to
//! finish its `fetch` + `pull --ff-only`, then proceeds against the
//! freshly-populated refs. The second caller's natural outcome is then
//! `UpToDate` (the worktree is still cut from the right SHA, just via the
//! first caller's work rather than its own), which is the correct outcome.
//!
//! Shape
//! -----
//! `MESH_LOCKS: Lazy<Mutex<HashMap<String, Arc<Mutex<()>>>>>` — process-global
//! map from a Mesh's path string to a per-Mesh `Arc<Mutex<()>>`. The outer
//! `Mutex` only protects the map itself (microsecond lookup); the inner
//! `Mutex` is the actual contention point for that Mesh's sync. Two spawns
//! against *different* Meshes never block each other; two spawns against
//! the same Mesh serialize.
//!
//! `Arc` on the inner mutex lets the address survive any future map
//! compaction. Today the map is append-only (Meshes are a small fixed set
//! per app session — typically a handful), so memory growth is bounded.
//!
//! Key shape
//! ---------
//! Callers MUST pass the Mesh's DB-stored path (the same string stored on
//! `agent_nodes.path`). Buildmesh's `meshes.path` is the host-native form
//! the user typed — Windows for a Windows repo, Linux for a WSL repo (then
//! `env::to_host_path` re-maps it to the host-side UNC for the actual git
//! call). That means all spawns for one Mesh row share one key, including
//! WSL spawns (whose `node.path` is the WSL form and whose `fetch_origin`
//! internally remaps via `to_host_path`).
//!
//! Lock-poison recovery
//! ---------------------
//! A `Poisoned` inner lock (a prior holder panicked mid-fetch) is recovered
//! rather than propagated. The guarded unit `()` has no state to corrupt,
//! so the right thing is to let the next caller proceed. This matches
//! `services::pool_worker::try_with_fill_lock`'s policy.

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Process-global map from a Mesh's path string to its per-Mesh sync mutex.
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
/// sync doesn't permanently jam every future spawn for the same Mesh.
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
    let arc_mutex = {
        let mut map = match MESH_LOCKS.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        // `entry().or_insert_with` keeps the lookup-or-insert atomic so two
        // concurrent first-time callers for the same Mesh don't race to
        // allocate two distinct mutexes.
        map.entry(mesh_path.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    /// Same-key calls serialize: N=10 concurrent callers each sleep 50 ms,
    /// and the wallclock must be ≥ N*50ms (with scheduling slack, well
    /// above 50ms). This is the headline regression test for issue #652 —
    /// without the lock, 10 parallel spawn_blocking threads would each take
    /// ~50ms and finish in ~50ms; with the lock they must serialize.
    ///
    /// The concurrency assertion uses the return value of `fetch_add` (the
    /// value *before* our increment) as the "currently inside" count:
    /// if we were alone on entry, the previous value is 0; if another
    /// thread is still inside, the previous value is ≥ 1. (A naive
    /// `fetch_add(1)` followed by `load()` would conflate cumulative
    /// increments with concurrency and false-fail even on a working lock.)
    #[test]
    fn same_key_serializes_concurrent_callers() {
        const N: usize = 10;
        const SLEEP_MS: u64 = 50;

        // Counter of threads currently INSIDE the critical section.
        // +1 on entry, -1 on exit. `fetch_add` returns the previous value,
        // so a serializing lock means every caller observes a previous
        // value of 0 (i.e., it was alone when it entered).
        let in_flight = Arc::new(AtomicUsize::new(0));

        let start = Instant::now();
        let handles: Vec<_> = (0..N)
            .map(|_| {
                let in_flight = Arc::clone(&in_flight);
                thread::spawn(move || {
                    with_mesh_sync_lock("mesh://serialized", || {
                        let prev = in_flight.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(
                            prev, 0,
                            "per-mesh lock failed: {} thread(s) already inside the critical section",
                            prev,
                        );
                        thread::sleep(Duration::from_millis(SLEEP_MS));
                        in_flight.fetch_sub(1, Ordering::SeqCst);
                    });
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread panicked");
        }
        let elapsed = start.elapsed();

        // All threads exited cleanly — in_flight must be back to 0.
        assert_eq!(in_flight.load(Ordering::SeqCst), 0);

        // Serialization bound: N sleeps of SLEEP_MS each. Allow a small
        // scheduling slack (10ms) below the strict N*SLEEP_MS so the test
        // doesn't false-fail on a fast machine, but anything below ~75%
        // of N*SLEEP_MS means calls overlapped.
        let strict = Duration::from_millis(N as u64 * SLEEP_MS);
        assert!(
            elapsed + Duration::from_millis(10) >= strict,
            "elapsed ({:?}) is well below the serialization bound ({:?}); \
             the per-mesh lock did not serialize",
            elapsed,
            strict,
        );
    }

    /// Different keys do NOT serialize: two threads with two distinct
    /// keys sleep 100ms each, and the wallclock must be ≪ 2*100ms (i.e.
    /// the inner locks were never contended). This guards against an
    /// accidental global-mutex regression — the same-key test above
    /// already proves serialization *works*; this one proves it doesn't
    /// *over*-serialize.
    ///
    /// The bound is intentionally generous (1.9x the parallel-expected
    /// elapsed vs. 2x the serialized-expected elapsed) because Windows
    /// can co-schedule 2 threads on the same core under contention,
    /// costing up to one full `SLEEP_MS` of scheduler latency on a busy
    /// CI box. Tight bounds here flake on slow machines without adding
    /// real coverage — the same-key test is the precise correctness
    /// check; this one is a smoke test against "did I accidentally
    /// replace the per-Mesh map with a single global".
    #[test]
    fn different_keys_run_in_parallel() {
        const SLEEP_MS: u64 = 100;

        let start = Instant::now();
        let h1 = thread::spawn(|| {
            with_mesh_sync_lock("mesh://alpha", || {
                thread::sleep(Duration::from_millis(SLEEP_MS));
            });
        });
        let h2 = thread::spawn(|| {
            with_mesh_sync_lock("mesh://beta", || {
                thread::sleep(Duration::from_millis(SLEEP_MS));
            });
        });
        h1.join().unwrap();
        h2.join().unwrap();
        let elapsed = start.elapsed();

        // Serialized bound (worst case): 2 * SLEEP_MS = 200ms.
        // Parallel ceiling: 1.9x the parallel-expected elapsed of
        // SLEEP_MS = 190ms — leaves 10ms of headroom over a fully-
        // serialized run, generous enough for Windows scheduler
        // co-scheduling on a busy CI box.
        let parallel_ceiling = Duration::from_millis((SLEEP_MS as f64 * 1.9) as u64);
        assert!(
            elapsed < parallel_ceiling,
            "elapsed ({:?}) reached the serialized bound (~{:?}); \
             different keys unexpectedly serialized",
            elapsed,
            2 * SLEEP_MS,
        );
    }

    /// A panicking closure leaves the inner mutex `Poisoned`. The next
    /// caller for the same key must still proceed (poison recovery) rather
    /// than fail forever. Mirrors `try_with_fill_lock`'s recovery policy
    /// (module docs).
    #[test]
    fn poisoned_lock_is_recovered_on_next_call() {
        // Use a dedicated key so this test's poison doesn't bleed into
        // the other tests' locks.
        let key = "mesh://poison-test";

        // Spawn a thread that panics inside the closure with this key.
        // We deliberately do NOT use `assert!(false)` here so the panic
        // doesn't pollute the test output — `panic!` produces a single
        // line in the test log.
        let result = thread::spawn(move || {
            with_mesh_sync_lock(key, || {
                panic!("intentional panic inside the per-mesh lock");
            });
        })
        .join();
        assert!(
            result.is_err(),
            "the spawned thread should have panicked to leave the lock poisoned"
        );

        // Subsequent caller on the same key must run the closure to
        // completion. If poison recovery is broken, this would either
        // fail to acquire or propagate the PoisonError into `f()`.
        let mut ran = false;
        with_mesh_sync_lock(key, || {
            ran = true;
        });
        assert!(
            ran,
            "closure did not run after a poisoned lock — recovery broken"
        );
    }

    /// `with_mesh_sync_lock` returns whatever the closure returns. Cheap
    /// smoke test — the type plumbing is `FnOnce -> R`, and we want to
    /// confirm the generic signature round-trips a non-`()` value.
    #[test]
    fn returns_closure_value() {
        let result = with_mesh_sync_lock("mesh://return-value", || 42_i32);
        assert_eq!(result, 42);

        let s = with_mesh_sync_lock("mesh://return-value", || "hello".to_string());
        assert_eq!(s, "hello");
    }
}