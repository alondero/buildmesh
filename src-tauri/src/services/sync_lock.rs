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
//! in-flight spawn; #698 wrapped the PR-spawn head fetch (the
//! `locked_fetch_pr_head` helper at `git::worktree::provision`,
//! covering both the same-repo `fetch_single_ref` and the fork
//! `fetch_fork_head` calls) so two concurrent PR-spawns (or a PR-spawn
//! racing a manual `git_sync`) can't collide on `.git/FETCH_HEAD`,
//! `.git/refs/remotes/<remote>/<ref>.lock`, or — for the fork branch —
//! the config files `git remote add/set-url` write.
//!
//! #709 added the worktree prune's `git fetch --prune` as the fourth and
//! final per-Mesh shell-out to route through this lock (the
//! `locked_prune_remote_tracking` helper at `commands::prune`), AND
//! consolidated the four sites into a uniform `locked_*` helper shape:
//! each underlying shell-out function (`do_sync`, `fetch_origin`,
//! `fetch_single_ref` / `fetch_fork_head`, the prune shell-out) has a
//! matching `locked_*` wrapper whose entire body is one
//! `with_mesh_sync_lock(key, || work())` call. The wrap is auditable in
//! one place per site rather than scattered across the call sites — and
//! the next per-Mesh shell-out site added (the issue's "copy-paste
//! trivial, not a new helper" AC) is a five-line `locked_*` function.
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

/// Like [`with_mesh_sync_lock`], but gives up after `timeout` instead of
/// waiting indefinitely. Returns `Some(f())` if the lock was acquired
/// within the deadline, `None` if the current holder didn't release in time.
///
/// Why this exists: the spawn-time auto-sync and the PR-head fetch used to
/// wait *unboundedly* on this lock. A manual Sync click (`git_sync` — up to
/// 300s fetch timeout plus 300s pull timeout under the same lock) or a
/// wedged remote could therefore stall every new-node spawn on that mesh
/// for many minutes: the "new nodes take minutes or never start" failure
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
    let arc_mutex = {
        let mut map = match MESH_LOCKS.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        map.entry(mesh_path.to_string())
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

    /// Different keys do NOT serialize: two threads with two distinct keys
    /// sleep 100ms each, and the maximum simultaneous-thread count inside
    /// the critical section must reach 2 at some point during the run.
    /// If the per-key lock were secretly global, only one thread could
    /// ever be inside at once, and the peak would stay at 1.
    ///
    /// This replaces the prior wallclock-bound assertion (`elapsed <
    /// 1.9 * SLEEP_MS`), which flaked on Windows whenever a busy CI box
    /// co-scheduled both threads on the same core. The handshake is the
    /// same pattern `same_key_serializes_concurrent_callers` already uses
    /// (issue #652 / commit 771bc79), so the test is now deterministic —
    /// no timing assumptions — while still catching the regression it's
    /// targeted at: a per-Mesh map accidentally replaced by a single
    /// global mutex. The wide 100ms sleep on each thread gives the OS
    /// scheduler plenty of room to actually overlap them — the assertion
    /// checks the overlap was *possible*, not how fast it happened.
    #[test]
    fn different_keys_run_in_parallel() {
        const SLEEP_MS: u64 = 100;

        // Threads INSIDE the critical section + the peak count seen
        // across the whole run. `fetch_add` returns the *previous* value,
        // so `prev + 1` is the count after the increment; `fetch_max`
        // records the peak.
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let i1 = Arc::clone(&in_flight);
        let p1 = Arc::clone(&peak);
        let h1 = thread::spawn(move || {
            with_mesh_sync_lock("mesh://alpha", move || {
                let new = i1.fetch_add(1, Ordering::SeqCst) + 1;
                p1.fetch_max(new, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(SLEEP_MS));
                i1.fetch_sub(1, Ordering::SeqCst);
            });
        });
        let i2 = Arc::clone(&in_flight);
        let p2 = Arc::clone(&peak);
        let h2 = thread::spawn(move || {
            with_mesh_sync_lock("mesh://beta", move || {
                let new = i2.fetch_add(1, Ordering::SeqCst) + 1;
                p2.fetch_max(new, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(SLEEP_MS));
                i2.fetch_sub(1, Ordering::SeqCst);
            });
        });
        h1.join().expect("alpha thread panicked");
        h2.join().expect("beta thread panicked");

        // Sanity: counter must be back to 0 after both threads exit.
        assert_eq!(in_flight.load(Ordering::SeqCst), 0);

        // The peak inside the critical section must reach 2, i.e. at some
        // moment both threads were inside their (different) per-key locks
        // simultaneously. A global-lock regression would keep the peak at 1.
        let observed_peak = peak.load(Ordering::SeqCst);
        assert_eq!(
            observed_peak, 2,
            "peak simultaneous per-key lock holders was {} (expected 2); \
             the per-Mesh lock was unexpectedly serialized \
             (issue #652 / different-keys regression)",
            observed_peak,
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

    // ── try_with_mesh_sync_lock_timeout (bounded spawn-path wait) ──────────

    /// Uncontended: the closure runs and its value round-trips.
    #[test]
    fn timeout_variant_runs_when_lock_is_free() {
        let result =
            try_with_mesh_sync_lock_timeout("mesh://timeout-free", Duration::from_secs(1), || {
                7_i32
            });
        assert_eq!(result, Some(7));
    }

    /// The headline regression for the "new nodes take minutes or never
    /// start" failure mode: a spawn-path caller must give up (return
    /// `None`, closure NOT run) when another sync holds the mesh lock past
    /// the deadline, instead of queueing behind it indefinitely.
    #[test]
    fn timeout_variant_gives_up_when_holder_outlives_deadline() {
        const KEY: &str = "mesh://timeout-held";
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();

        // Holder: takes the lock and keeps it until told to release —
        // stands in for a wedged manual `git_sync`.
        let holder = thread::spawn(move || {
            with_mesh_sync_lock(KEY, || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
        });
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("holder never acquired the lock");

        let start = Instant::now();
        let mut ran = false;
        let result =
            try_with_mesh_sync_lock_timeout(KEY, Duration::from_millis(300), || ran = true);
        let elapsed = start.elapsed();

        assert!(result.is_none(), "must give up while the lock is held");
        assert!(!ran, "the closure must NOT run when the deadline expires");
        // Bound check: gave up near the deadline (generous slack for CI),
        // not after some multi-second internal retry.
        assert!(
            elapsed < Duration::from_secs(2),
            "gave up too slowly: {elapsed:?}"
        );

        release_tx.send(()).unwrap();
        holder.join().unwrap();

        // Once released, the same key acquires again.
        let after = try_with_mesh_sync_lock_timeout(KEY, Duration::from_secs(1), || 1_i32);
        assert_eq!(after, Some(1), "lock must be reusable after release");
    }

    /// A caller that starts while the lock is held but whose deadline
    /// outlives the holder acquires once the holder releases — the bounded
    /// wait is a wait, not a `try_lock`-or-skip.
    #[test]
    fn timeout_variant_acquires_when_holder_releases_in_time() {
        const KEY: &str = "mesh://timeout-releases";
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();

        let holder = thread::spawn(move || {
            with_mesh_sync_lock(KEY, || {
                started_tx.send(()).unwrap();
                // Short hold — well inside the waiter's deadline.
                thread::sleep(Duration::from_millis(200));
            });
        });
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("holder never acquired the lock");

        let result = try_with_mesh_sync_lock_timeout(KEY, Duration::from_secs(5), || 99_i32);
        assert_eq!(
            result,
            Some(99),
            "waiter with headroom must acquire after the holder releases"
        );
        holder.join().unwrap();
    }
}
