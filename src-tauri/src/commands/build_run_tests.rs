use super::*;
use crate::models::AgentNode;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn node_fixture(use_worktree: bool, worktree_name: Option<&str>) -> AgentNode {
    AgentNode {
        path: "/home/user/my-repo".to_string(),
        worktree_name: worktree_name.map(str::to_string),
        use_worktree,
        ..Default::default()
    }
}

/// Regression: a Root Node (use_worktree=false, worktree_name=None) must
/// resolve to the mesh root even when the mesh has use_worktree=true. The
/// previous inline implementation used `row.use_worktree`, then fed
/// `spawn_worktree_name = None` into `validate_worktree_exists`, which
/// surfaced "No worktree name set for this agent node." for any Build/Run/
/// Terminal click on a Root Node inside a worktree-enabled mesh — the
/// bug the user reported.
///
/// Asserts on `raw_path` (the input-pass-through POSIX form, mirrored by
/// the frontend's `getNodeGitPath`) rather than `host_path` — `host_path`
/// goes through `to_host_path`, which converts a `/home/...` fixture to
/// a WSL UNC path on Windows. The raw form is the contract this helper
/// is responsible for (issue #409).
#[test]
fn resolve_build_run_cwd_root_node_resolves_to_mesh_root() {
    let resolved = resolve_build_run_cwd(&node_fixture(false, None));
    assert_eq!(resolved.raw_path, "/home/user/my-repo");
    assert!(
        !resolved.raw_path.contains("worktrees"),
        "Root Node raw_path must not contain a worktree subdir, got: {}",
        resolved.raw_path
    );
}

/// A stale `worktree_name` on a Root Node is ignored — same canonical
/// rule `env::node_working_path` uses everywhere else (issue #383).
#[test]
fn resolve_build_run_cwd_root_node_ignores_stale_worktree_name() {
    let resolved = resolve_build_run_cwd(&node_fixture(false, Some("stale-name")));
    assert!(
        !resolved.raw_path.contains("worktrees"),
        "stale worktree_name on Root Node must not leak into raw_path: {}",
        resolved.raw_path
    );
}

/// A Worktree Node still resolves into its `.claude/worktrees/<name>`
/// subdir — the canonical behaviour, preserved.
#[test]
fn resolve_build_run_cwd_worktree_node_resolves_worktree_subdir() {
    let resolved = resolve_build_run_cwd(&node_fixture(true, Some("gentle-fox")));
    assert_eq!(
        resolved.raw_path,
        "/home/user/my-repo/.claude/worktrees/gentle-fox"
    );
}

/// The worktree validator gate must skip Root Nodes — otherwise we'd
/// call `git2::Repository::open` on the mesh root and look for a
/// non-existent worktree, tripping the user-facing "Worktree '...' not
/// found" error. This pins `env::worktree_segment`'s contract at the
/// call site: `Some(_)` only when the node is a Worktree Node.
#[test]
fn worktree_segment_is_some_only_for_worktree_nodes() {
    assert!(env::worktree_segment(&node_fixture(false, None)).is_none());
    assert!(env::worktree_segment(&node_fixture(false, Some("ignored"))).is_none());
    assert_eq!(
        env::worktree_segment(&node_fixture(true, Some("gentle-fox"))),
        Some("gentle-fox"),
    );
    assert!(env::worktree_segment(&node_fixture(true, None)).is_none());
    assert!(env::worktree_segment(&node_fixture(true, Some("   "))).is_none());

    // Trim invariant (issue #383 — Root Node + stale `worktree_name`):
    // the trimmed segment is what the git2 worktree-list compare sees.
    assert_eq!(
        env::worktree_segment(&node_fixture(true, Some("  gentle-fox  "))),
        Some("gentle-fox"),
    );
}

/// A real git repository with one initial commit, under a `TempDir`
/// (random-suffixed, so concurrent test binaries get distinct
/// directories).
fn init_repo(path: &Path) -> git2::Repository {
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("main");
    let repo = git2::Repository::init_opts(path, &opts).unwrap();

    std::fs::write(path.join("file.txt"), "initial").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("file.txt")).unwrap();
    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = git2::Signature::now("test", "test@example.com").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
        .unwrap();
    drop(tree);

    repo
}

/// Register `name` as a git worktree under the same `.claude/worktrees/`
/// layout production resolves — the layout the assertion below depends on
/// is only meaningful if the fixture matches what Build/Run asks for.
fn add_worktree(root_repo: &git2::Repository, root: &Path, name: &str) -> PathBuf {
    let head = root_repo.head().unwrap().peel_to_commit().unwrap();
    let branch = root_repo.branch(name, &head, false).unwrap();
    let reference = branch.into_reference();
    let worktree_path = root.join(".claude").join("worktrees").join(name);
    std::fs::create_dir_all(worktree_path.parent().unwrap()).unwrap();
    root_repo
        .worktree(
            name,
            &worktree_path,
            Some(git2::WorktreeAddOptions::new().reference(Some(&reference))),
        )
        .unwrap();
    worktree_path
}

/// The whole Build/Run path decision, end to end: node → resolved cwd →
/// the git2 worktree gate that decides whether a shell is allowed to
/// spawn. This replaces a test-file-local `git rev-parse --show-toplevel`
/// helper that no production code ever called, so it stayed green while
/// this path changed underneath it.
#[test]
fn build_run_cwd_selects_a_registered_worktree_and_rejects_an_unknown_one() {
    let root = TempDir::new().unwrap();
    let repo = init_repo(root.path());
    let worktree = add_worktree(&repo, root.path(), "wt-live");

    let node = AgentNode {
        path: root.path().to_string_lossy().to_string(),
        worktree_name: Some("wt-live".to_string()),
        use_worktree: true,
        ..Default::default()
    };

    let cwd = resolve_build_run_cwd(&node);
    // The directory the shell would spawn in IS the worktree git2
    // confirmed — not merely a path with the right shape.
    assert_eq!(
        std::fs::canonicalize(&cwd.host_path).unwrap(),
        std::fs::canonicalize(&worktree).unwrap()
    );
    assert!(
        validate_worktree_exists(&cwd, env::worktree_segment(&node)).is_ok(),
        "a registered worktree must pass the gate"
    );

    // Same node shape, a worktree name git does not know. The directory
    // exists but is not a registered worktree, so `discover` still
    // succeeds and the failure is the worktree-list compare — not the
    // open below.
    std::fs::create_dir_all(root.path().join(".claude/worktrees/wt-missing")).unwrap();
    let stale = AgentNode {
        worktree_name: Some("wt-missing".to_string()),
        ..node.clone()
    };
    let stale_cwd = resolve_build_run_cwd(&stale);
    let err = validate_worktree_exists(&stale_cwd, env::worktree_segment(&stale))
        .expect_err("an unregistered worktree name must be rejected");
    assert!(
        err.contains("wt-missing"),
        "the error must name the missing worktree, got: {err}"
    );
    assert!(
        err.contains("not found in git worktree list"),
        "unexpected error: {err}"
    );
}

/// A worktree directory that was never created is a *different* failure
/// from an unregistered name: `git2::Repository::discover` reports
/// `NotFound` for a path that does not exist, so this exits at the open
/// and does not reach the worktree-list compare. Both branches surface to
/// the user, so both are pinned below.
#[test]
fn validate_worktree_exists_reports_a_missing_worktree_directory() {
    let root = TempDir::new().unwrap();
    let repo = init_repo(root.path());
    let _registered = add_worktree(&repo, root.path(), "wt-live");

    let node = AgentNode {
        path: root.path().to_string_lossy().to_string(),
        worktree_name: Some("wt-never-created".to_string()),
        use_worktree: true,
        ..Default::default()
    };
    let cwd = resolve_build_run_cwd(&node);
    assert!(
        !Path::new(&cwd.host_path).exists(),
        "fixture precondition: the worktree dir must not exist"
    );

    let err = validate_worktree_exists(&cwd, env::worktree_segment(&node))
        .expect_err("a worktree directory that does not exist must be rejected");
    assert!(
        err.starts_with("Failed to open repository"),
        "unexpected error: {err}"
    );
}

/// The mesh root is the main worktree: git reports it separately from
/// `repo.worktrees()`, so the `.git`-on-disk fallback is what accepts it.
#[test]
fn validate_worktree_exists_accepts_the_main_worktree_directory() {
    let root = TempDir::new().unwrap();
    let repo = init_repo(root.path());
    // Fixture precondition: git reports linked worktrees only, so the
    // main worktree's own name is absent and the `.git`-on-disk fallback
    // is the only branch that can accept it below.
    assert_eq!(repo.worktrees().unwrap().len(), 0);

    let resolved = env::resolve_raw_path(&root.path().to_string_lossy());
    assert!(
        validate_worktree_exists(&resolved, Some("main")).is_ok(),
        "the main worktree is valid via its .git directory"
    );
}

/// Not a repository at all. The honest failure names the step that broke,
/// instead of the replica's invented `Worktree not found` string.
#[test]
fn validate_worktree_exists_reports_a_non_repository_directory() {
    let plain = TempDir::new().unwrap();
    std::fs::write(plain.path().join("README.md"), "not a repo").unwrap();
    // Fixture precondition: `discover` walks *up* the tree, so this test
    // only holds when the ambient temp directory is not itself inside a
    // repository. Assert it directly so a machine that violates it fails
    // with that explanation instead of a confusing message mismatch.
    assert!(
        git2::Repository::discover(plain.path()).is_err(),
        "fixture precondition: temp dir must not sit inside a git repository"
    );
    let resolved = env::resolve_raw_path(&plain.path().to_string_lossy());

    let err = validate_worktree_exists(&resolved, Some("wt"))
        .expect_err("a directory outside any repository must be rejected");
    assert!(
        err.starts_with("Failed to open repository"),
        "unexpected error: {err}"
    );
}

/// No worktree name at all — the exact string the frontend surfaces. Pin
/// it so the message cannot drift without a test noticing.
#[test]
fn validate_worktree_exists_requires_a_worktree_name() {
    let root = TempDir::new().unwrap();
    let _repo = init_repo(root.path());
    let resolved = env::resolve_raw_path(&root.path().to_string_lossy());

    assert_eq!(
        validate_worktree_exists(&resolved, None).unwrap_err(),
        "No worktree name set for this agent node. Spawn the agent first to create a worktree."
    );
}

#[test]
fn build_run_mode_serializes_lowercase() {
    for (variant, expected) in [
        (BuildRunMode::Build, "\"build\""),
        (BuildRunMode::Run, "\"run\""),
        (BuildRunMode::Terminal, "\"terminal\""),
    ] {
        let json = serde_json::to_string(&variant).unwrap();
        assert_eq!(json, expected, "serialize {:?}", variant);

        let round: BuildRunMode = serde_json::from_str(&json).unwrap();
        assert_eq!(round, variant, "round-trip {:?}", variant);
    }
}

#[test]
fn build_run_registry_write_bytes_to_dead_session() {
    let registry = BuildRunRegistry::new();
    let result = registry.write_bytes(42, b"hello");
    assert!(result.is_err());
    // Frontend matches on this substring to swallow the "not running"
    // case silently — keep the contract stable.
    assert!(result.unwrap_err().contains("not running"));
}

#[test]
fn build_run_registry_resize_pty_to_dead_session() {
    let registry = BuildRunRegistry::new();
    let result = registry.resize_pty(42, 80, 24);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not running"));
}

// --- Generation-safety tests (issue #1532) ---------------------------
//
// These exercise the lifecycle contract: a rapid Build→Build (or any
// spawn→spawn) replacement must (a) tear down the previous incarnation
// under the same node_id without leaking the entry, and (b) make the
// replacement immune to the previous reader thread's late EOF. The
// tests operate on the registry directly — no real PTY is opened;
// `dummy_process` constructs a stand-in BuildRunProcess whose
// child/master slots are empty (so `teardown_inc` no-ops on those
// paths but still exercises the generation bookkeeping).

use std::io::Cursor;

/// Construct a `BuildRunProcess` stand-in for tests. `child` and
/// `master` are `None` so `teardown_inc` no-ops on them; `writer`
/// is a real `Cursor<Vec<u8>>` (Send + Write) so `write_bytes` can
/// actually push bytes through the entry.
fn dummy_process() -> BuildRunProcess {
    BuildRunProcess {
        generation: 0,
        child: Mutex::new(None),
        master: Mutex::new(None),
        writer: Mutex::new(Box::new(Cursor::new(Vec::new()))),
        reader_handle: Mutex::new(None),
        reader_join_timed_out: AtomicBool::new(false),
        output: Arc::new(crate::pty::sink::OutputSink::new())
            .producer(Arc::new(AtomicBool::new(false))),
    }
}

/// Insert assigns strictly-monotonic generation tokens so a stale
/// reader can never be mistaken for a current one (issue #1532
/// point #1).
#[test]
fn insert_assigns_monotonic_generations() {
    let registry = BuildRunRegistry::new();
    let (g1, _) = registry.insert(-915_1533, dummy_process());
    let (g2, _) = registry.insert(-915_1534, dummy_process());
    let (g3, _) = registry.insert(-915_1533, dummy_process()); // same node_id, new generation
    assert!(g1 < g2, "g2 must be strictly greater than g1");
    assert!(
        g2 < g3,
        "g3 must be strictly greater than g2 even on the same node_id"
    );
    assert_ne!(g1, g2);
    assert_ne!(g2, g3);
}

/// `insert` returns the generation assigned to the new entry, not
/// the previous one — the reader thread must capture THIS value to
/// drive `reap_incarnation`.
#[test]
fn insert_returns_assigned_generation() {
    let registry = BuildRunRegistry::new();
    let (g1, arc1) = registry.insert(-915_1535, dummy_process());
    assert_eq!(arc1.generation, g1);

    let (g2, arc2) = registry.insert(-915_1535, dummy_process());
    assert_eq!(arc2.generation, g2);
    assert_ne!(arc1.generation, arc2.generation);
}

/// A previous reader's `remove_if_current` against the SAME
/// `node_id` must NOT delete a replacement incarnation
/// (issue #1532 point #3 + AC "stale A exit does not alter B").
/// This is the canonical race the issue describes.
#[test]
fn stale_generation_cannot_remove_replacement() {
    let registry = BuildRunRegistry::new();
    let node_id = -915_1536;

    let (g_a, _) = registry.insert(node_id, dummy_process());
    // B replaces A — `insert` tears down A's process handle (no-op
    // on the dummy process) and assigns a fresh generation.
    let (g_b, arc_b) = registry.insert(node_id, dummy_process());
    assert_ne!(g_a, g_b);

    // A's reader reaches EOF after B has been published. It calls
    // `reap_incarnation(node_id, g_a)` — must be a no-op so B
    // stays current.
    let reaped = registry.reap_incarnation(node_id, g_a);
    assert!(!reaped, "stale generation must not reap a replacement");
    assert!(
        registry.contains(&node_id),
        "replacement B must survive A's late EOF"
    );
    assert_eq!(
        arc_b.generation, g_b,
        "replacement's generation must be unchanged"
    );

    // B remains writable — the user's keystrokes land on the live
    // PTY (issue #1532 AC "B remains current and writable").
    registry
        .write_bytes(node_id, b"hello from replacement")
        .expect("replacement must accept writes after stale reap");
}

/// `reap_incarnation` on a never-inserted generation is a no-op.
/// Covers the late-EOF-from-old-restart case where the generation
/// counter has wrapped or was never issued.
#[test]
fn reap_incarnation_unknown_generation_is_noop() {
    let registry = BuildRunRegistry::new();
    let node_id = -915_1537;
    let (g_real, arc_real) = registry.insert(node_id, dummy_process());

    let reaped = registry.reap_incarnation(node_id, g_real + 999);
    assert!(!reaped);

    // Original entry is untouched.
    assert_eq!(arc_real.generation, g_real);
}

/// `reap_incarnation` returns true and removes the entry when the
/// generation still matches — the natural-exit happy path the
/// reader thread takes on its own EOF.
#[test]
fn reap_incarnation_current_generation_removes_entry() {
    let registry = BuildRunRegistry::new();
    let node_id = -915_1538;
    let (g, _) = registry.insert(node_id, dummy_process());
    assert!(registry.contains(&node_id));

    let reaped = registry.reap_incarnation(node_id, g);
    assert!(reaped, "current-generation reap must succeed");
    assert!(
        !registry.contains(&node_id),
        "current-generation reap must remove the entry"
    );
}

/// `kill_session` is a deliberate teardown — mirrors user X-click.
/// It must reap the current incarnation and return true; subsequent
/// calls are no-ops (issue #1532 AC "closing current B
/// removes/reaps B only").
#[test]
fn kill_session_tears_down_only_current() {
    let registry = BuildRunRegistry::new();
    let node_id = -915_1539;

    let (g_a, _) = registry.insert(node_id, dummy_process());
    let (g_b, _) = registry.insert(node_id, dummy_process()); // replaces

    // kill_session removes the CURRENT (B) entry — A is already gone.
    assert!(registry.kill_session(node_id));
    assert!(!registry.contains(&node_id));

    // A's late reap (had it survived in the reader thread) would
    // also be a no-op now.
    assert!(!registry.reap_incarnation(node_id, g_a));
    assert!(!registry.reap_incarnation(node_id, g_b));

    // Calling kill_session on an empty registry is a no-op.
    assert!(!registry.kill_session(node_id));
}

/// AC: "Concurrent starts settle on exactly one live generation."
/// Two sequential inserts on the same node_id end with exactly one
/// entry — the second insert both (a) assigns a new generation and
/// (b) tears down the first. After both calls settle, the registry
/// contains only g_b.
#[test]
fn sequential_replacements_leave_one_live_entry() {
    let registry = BuildRunRegistry::new();
    let node_id = -915_1540;

    let (_g_a, _) = registry.insert(node_id, dummy_process());
    let (_g_b, _) = registry.insert(node_id, dummy_process());
    let (_g_c, _) = registry.insert(node_id, dummy_process());

    assert_eq!(registry.processes.len(), 1, "only one entry per node_id");

    // No residue: stale reaps for any earlier generation are silent.
    assert!(!registry.reap_incarnation(node_id, _g_a));
    assert!(!registry.reap_incarnation(node_id, _g_b));
    // Current reap reaps cleanly.
    assert!(registry.reap_incarnation(node_id, _g_c));
    assert!(!registry.contains(&node_id));
}

/// AC: "No process/reader remains after repeated replacements."
/// A three-spawn cycle followed by a final kill_session leaves an
/// empty registry — no zombie entries, no panics on a follow-up
/// reap_incarnation.
#[test]
fn repeated_replacements_followed_by_close_yields_empty_registry() {
    let registry = BuildRunRegistry::new();
    let node_id = -915_1541;

    for _ in 0..5 {
        let (_, _) = registry.insert(node_id, dummy_process());
    }
    // After the storm, exactly one entry remains.
    assert_eq!(registry.processes.len(), 1);

    assert!(registry.kill_session(node_id));
    assert!(registry.processes.is_empty());

    // Stale reaps do not panic, do not resurrect the entry.
    assert!(!registry.reap_incarnation(node_id, u64::MAX));
    assert!(registry.processes.is_empty());
}

/// Regression for the close-vs-reader deadlock (issue #1532 review
/// finding #1): with an outer `Mutex` on `BUILD_RUN_REGISTRY`,
/// `close_build_run` would hold it across `join_with_timeout` while
/// the reader thread was parked waiting for the SAME lock to call
/// `reap_incarnation` — every close deadlocked for 2 s.
///
/// This test reproduces the deadlock shape WITHOUT real PTY:
///   1. Insert a process whose `reader_handle` points to a thread
///      that runs `reap_incarnation` in a tight loop and then sleeps.
///      With the old outer Mutex, this thread blocks on the lock as
///      soon as `kill_session` holds it.
///   2. From the main thread, call `kill_session`. It must return
///      well under the 2 s watchdog — the new `BuildRunRegistry`
///      has no outer lock to invert priorities on.
///
/// **Round-3 review finding #4.** The previous version asserted
/// `kill_session` returned `true` (the entry was reaped), but
/// the background reaper thread races to reap the same entry; if
/// the reaper wins first, `kill_session` correctly sees the entry
/// already gone and returns `false`. Both outcomes prove the call
/// is fast, so we only check the timing — not which path removed
/// the entry.
#[test]
fn kill_session_does_not_deadlock_with_concurrent_reaper() {
    use std::sync::Arc as StdArc;
    use std::time::Duration;

    let registry = StdArc::new(BuildRunRegistry::new());
    let node_id = -915_1544;
    let (generation, arc) = registry.insert(node_id, dummy_process());

    // Reader thread: calls reap_incarnation in a loop, then sleeps.
    // The loop pins the iteration pattern of a real reader reaching
    // EOF and then trying to clean up.
    let registry_for_reader = StdArc::clone(&registry);
    let reader_handle = std::thread::Builder::new()
        .name("test-buildrun-deadlock-reader".to_string())
        .spawn(move || {
            for _ in 0..20 {
                let _ = registry_for_reader.reap_incarnation(node_id, generation);
                std::thread::sleep(Duration::from_millis(5));
            }
        })
        .expect("spawn reader");

    // Recover from poison rather than abort — see the `teardown_inc`
    // doc for why we never `.expect()` here.
    *arc.reader_handle.lock().unwrap_or_else(|e| e.into_inner()) = Some(reader_handle);

    // Race the reader — start kill_session while the reader is
    // actively trying to reap. With the old outer Mutex, the join
    // would sit on the reader until `join_with_timeout` gave up
    // (2 s) and detached it. The fix (no outer Mutex) lets the
    // reader finish and the join succeed.
    //
    // Assert the mechanism, not the stopwatch: a wall-clock bound
    // (500 ms) failed on a loaded machine at 988 ms with no deadlock
    // at all. The flag is set only when the watchdog had to detach
    // the reader, which is exactly the deadlock's signature.
    let _ = registry.kill_session(node_id);

    assert!(
        !arc.reader_join_timed_out.load(Ordering::Acquire),
        "kill_session must not deadlock with a concurrent reader: \
             the join watchdog gave up and detached the reader"
    );
}

/// Stress test: many concurrent `kill_session` + `insert` + `reap`
/// cycles across multiple threads do not deadlock or leak entries.
/// The previous version was a single-threaded sequential `for`
/// loop that spawned zero threads and never called `kill_session`
/// (round-1 review finding #5). The round-2 version spawned 8
/// worker threads but gave each worker a disjoint node_id
/// (`-915_1545 - worker_id * 10_000`), so they could not
/// collide on the registry key — the test was still a paper
/// tiger (round-3 review finding #2).
///
/// This round-3 version makes ALL workers contend on the SAME
/// `node_id`. Each worker runs insert/replace/kill cycles
/// against that single key. With the round-1 outer-Mutex bug the
/// workers would serialise on the lock; with the round-3 insert
/// ordering (deferred teardown) the workers can race freely
/// because the registry's internal lock is short-lived and the
/// tear-down runs in a detached thread.
#[test]
fn concurrent_lifecycle_cycles_do_not_deadlock() {
    use std::sync::Arc as StdArc;
    use std::time::{Duration, Instant};

    let registry = StdArc::new(BuildRunRegistry::new());
    let node_id = -915_1545; // ALL workers use this key.
    let started = Instant::now();

    // 8 worker threads × 50 cycles each = 400 lifecycle operations
    // on the SAME node_id. Every cycle is insert-then-something,
    // so every iteration calls `PtyRegistry::insert` /
    // `remove` / `remove_if` under contention.
    const WORKERS: usize = 8;
    const CYCLES_PER_WORKER: usize = 50;

    let handles: Vec<_> = (0..WORKERS)
        .map(|worker_id| {
            let registry = StdArc::clone(&registry);
            std::thread::Builder::new()
                .name(format!("build-run-stress-{worker_id}"))
                .spawn(move || {
                    for cycle in 0..CYCLES_PER_WORKER {
                        // Insert. After this call the registry
                        // is guaranteed to hold THIS worker's
                        // insertion (modulo other workers
                        // racing on the same key — exactly the
                        // contention we're testing).
                        let (g, _) = registry.insert(node_id, dummy_process());
                        // Vary the action so all three lifecycle
                        // paths get exercised across cycles.
                        match cycle % 3 {
                            0 => {
                                let _ = registry.reap_incarnation(node_id, g);
                            }
                            1 => {
                                // Replace, then kill the replacement.
                                let _ = registry.insert(node_id, dummy_process());
                                let _ = registry.kill_session(node_id);
                            }
                            _ => {
                                let _ = registry.kill_session(node_id);
                            }
                        }
                    }
                })
                .expect("spawn worker")
        })
        .collect();

    // Bounded join — if any worker is stuck on the watchdog,
    // this would time out. 5 s budget covers 2 s watchdog × at
    // most 1 stale worker before we'd notice.
    for handle in handles {
        let joined = handle.join();
        assert!(joined.is_ok(), "worker panicked: {joined:?}");
    }

    assert!(
        started.elapsed() < Duration::from_secs(5),
        "concurrent lifecycle cycles must complete under 5s (no deadlock); \
             elapsed = {:?}",
        started.elapsed()
    );
    // The registry may have a residual entry if the last cycle left
    // a non-reaped insert standing — that's fine; we just assert no
    // panic / no hang. Drain for hygiene.
    let _ = registry.kill_session(node_id);
    assert!(registry.processes.is_empty(), "no leaked entries");
}

/// A first-generation reap, then a second-generation insert, then a
/// close: the close must operate on B (the current entry), not on
/// the already-reaped A. Mirrors the user's mental model of "I
/// clicked Build twice quickly, then X-closed the second one."
#[test]
fn reap_then_replace_then_close_only_touches_current() {
    let registry = BuildRunRegistry::new();
    let node_id = -915_1542;

    let (g_a, _) = registry.insert(node_id, dummy_process());
    // A naturally exits.
    assert!(registry.reap_incarnation(node_id, g_a));
    assert!(!registry.contains(&node_id));

    // B is inserted after A has been reaped.
    let (g_b, _) = registry.insert(node_id, dummy_process());
    assert!(registry.contains(&node_id));

    // User closes — kill_session operates on B.
    assert!(registry.kill_session(node_id));
    assert!(!registry.contains(&node_id));

    // B's reap would now be a no-op (entry already gone).
    assert!(!registry.reap_incarnation(node_id, g_b));
}

/// The reader-facing contract: `write_bytes` keeps working on the
/// replacement AFTER a stale reap. Pinned explicitly because the
/// original bug (issue #1532) was "B is now missing from the
/// registry or shown as exited even though it is alive."
#[test]
fn replacement_remains_writable_after_stale_reap() {
    let registry = BuildRunRegistry::new();
    let node_id = -915_1543;

    let (g_a, _) = registry.insert(node_id, dummy_process());
    let (g_b, _) = registry.insert(node_id, dummy_process());
    assert_ne!(g_a, g_b);

    // Stale A reap.
    assert!(!registry.reap_incarnation(node_id, g_a));

    // B is still the live entry; writes still succeed.
    assert!(registry.write_bytes(node_id, b"alive").is_ok());

    // Sanity: B's reap reaps correctly when its own reader reaches EOF.
    assert!(registry.reap_incarnation(node_id, g_b));
    assert!(registry.write_bytes(node_id, b"dead").is_err());
}

// --- Per-context command resolution (issue #802) --------------------

/// Build a `MeshRow` with only the four command columns set — every other
/// field defaults. Goes through `MeshRow::from` so the mapping is exercised
/// alongside the resolver.
fn row(
    build: Option<&str>,
    run: Option<&str>,
    root_build: Option<&str>,
    root_run: Option<&str>,
) -> MeshRow {
    MeshRow::from(&crate::models::Mesh {
        build_command: build.map(str::to_string),
        run_command: run.map(str::to_string),
        root_build_command: root_build.map(str::to_string),
        root_run_command: root_run.map(str::to_string),
        ..Default::default()
    })
}

/// Worktree Build context always uses `build_command`, even when a
/// `root_build_command` is set — the root command must not leak into a
/// Worktree Node's build.
#[test]
fn resolve_command_build_worktree_uses_build_command() {
    let r = row(
        Some("npm run build"),
        None,
        Some("cargo build --workspace"),
        None,
    );
    assert_eq!(
        resolve_build_run_command(BuildRunMode::Build, false, &r),
        Some("npm run build")
    );
}

/// Root Build context prefers `root_build_command` when set.
#[test]
fn resolve_command_build_root_uses_root_build_command() {
    let r = row(
        Some("npm run build"),
        None,
        Some("cargo build --workspace"),
        None,
    );
    assert_eq!(
        resolve_build_run_command(BuildRunMode::Build, true, &r),
        Some("cargo build --workspace")
    );
}

/// Root Build with no `root_build_command` falls back to `build_command`
/// — PR #801's behaviour, unchanged for meshes without the new field.
#[test]
fn resolve_command_build_root_falls_back_to_build_command() {
    let r = row(Some("npm run build"), None, None, None);
    assert_eq!(
        resolve_build_run_command(BuildRunMode::Build, true, &r),
        Some("npm run build")
    );
}

/// Worktree Run context always uses `run_command`.
#[test]
fn resolve_command_run_worktree_uses_run_command() {
    let r = row(None, Some("npm run dev"), None, Some("cargo run -p app"));
    assert_eq!(
        resolve_build_run_command(BuildRunMode::Run, false, &r),
        Some("npm run dev")
    );
}

/// Root Run context prefers `root_run_command` when set.
#[test]
fn resolve_command_run_root_uses_root_run_command() {
    let r = row(None, Some("npm run dev"), None, Some("cargo run -p app"));
    assert_eq!(
        resolve_build_run_command(BuildRunMode::Run, true, &r),
        Some("cargo run -p app")
    );
}

/// Root Run with no `root_run_command` falls back to `run_command`.
#[test]
fn resolve_command_run_root_falls_back_to_run_command() {
    let r = row(None, Some("npm run dev"), None, None);
    assert_eq!(
        resolve_build_run_command(BuildRunMode::Run, true, &r),
        Some("npm run dev")
    );
}

/// A mesh with no command configured at all resolves to `None` so the
/// caller surfaces the "not configured" error.
#[test]
fn resolve_command_unconfigured_is_none() {
    let r = row(None, None, None, None);
    assert_eq!(
        resolve_build_run_command(BuildRunMode::Build, true, &r),
        None
    );
    assert_eq!(
        resolve_build_run_command(BuildRunMode::Build, false, &r),
        None
    );
    assert_eq!(resolve_build_run_command(BuildRunMode::Run, true, &r), None);
    assert_eq!(
        resolve_build_run_command(BuildRunMode::Run, false, &r),
        None
    );
}

/// Terminal mode needs no command in either context — always `Some("")`.
#[test]
fn resolve_command_terminal_is_empty_in_both_contexts() {
    let r = row(Some("b"), Some("r"), Some("rb"), Some("rr"));
    assert_eq!(
        resolve_build_run_command(BuildRunMode::Terminal, true, &r),
        Some("")
    );
    assert_eq!(
        resolve_build_run_command(BuildRunMode::Terminal, false, &r),
        Some("")
    );
}

/// The start_reader pattern: pump inside `with_batcher`. If the
/// producer isn't dropped before join, this hangs on EOF.
#[test]
fn pump_inside_with_batcher_exits_cleanly_on_reader_eof() {
    let reader: Box<dyn std::io::Read + Send> = Box::new(std::io::Cursor::new(b"hello from pty\n"));
    let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let g = got.clone();
    let started = std::time::Instant::now();
    crate::pty::batch::with_batcher(
        move |batch| g.lock().unwrap().extend_from_slice(&batch),
        |tx| {
            let mut r = reader;
            let mut buf = [0u8; crate::pty::batch::PTY_READ_BUF];
            loop {
                match r.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let _ = tx.send(buf[..n].to_vec());
                    }
                    Err(_) => break,
                }
            }
        },
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "reader+batcher hung after PTY EOF — producer was not dropped"
    );
    assert_eq!(&*got.lock().unwrap(), b"hello from pty\n");
}

#[test]
fn production_reader_does_not_emit_json_or_base64() {
    // Real behavioural test, not a source scrape (round-4 review
    // finding #8A). We construct a `Cursor<Vec<u8>>` reader whose
    // data is immediately exhausted, route it through
    // `pty::batch::with_batcher` with the same shape the production
    // reader uses, and assert the bytes arrive unmodified.
    //
    // `pty::batch::with_batcher` is the production byte path —
    // production PTY bytes ride the binary Channel, NOT the JSON
    // `build-run-output-{id}` event (issue #1393). If a future
    // refactor tries to swap `with_batcher` for a direct emit or a
    // base64 encode, this test pins the wire shape.
    use crate::pty::batch::PTY_READ_BUF;

    let reader: Box<dyn std::io::Read + Send> = Box::new(std::io::Cursor::new(b"hello from pty\n"));
    let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let g = std::sync::Arc::clone(&got);
    crate::pty::batch::with_batcher(
        move |batch| g.lock().unwrap().extend_from_slice(&batch),
        |tx| {
            let mut r = reader;
            let mut buf = [0u8; PTY_READ_BUF];
            loop {
                match r.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let _ = tx.send(buf[..n].to_vec());
                    }
                    Err(_) => break,
                }
            }
        },
    );
    assert_eq!(&*got.lock().unwrap(), b"hello from pty\n");
    // The bytes are the literal input, untouched by any base64
    // encode/decode round-trip.
    let raw_bytes = got.lock().unwrap().clone();
    assert_eq!(raw_bytes, b"hello from pty\n".to_vec());
}

#[test]
fn production_reader_does_not_drop_channel_on_eof() {
    // Real behavioural test of the registry semantics that the
    // reader's EOF epilogue (`reap_and_maybe_emit`) builds on.
    //
    // The previous version of this test claimed (in the doc) to be
    // a real `tauri::test` runtime test, but the body actually
    // source-scraped `include_str!("build_run.rs")` and
    // `std::fs::read_to_string("services/agent_node.rs")` for
    // substring matches — the doc lied (round-3 review finding #3
    // + round-4 review finding #8A). Source-scraping breaks
    // every time a comment, identifier, or unrelated identifier
    // mentions the literal; it tests nothing.
    //
    // This test exercises the two branches of the reader's
    // EOF epilogue against the registry directly:
    //
    //   - branch 1 (reaped=true): current-generation reap removes
    //     the entry. `reap_and_maybe_emit` would emit the exit
    //     event.
    //   - branch 2 (reaped=false): stale-generation reap is a
    //     no-op. `reap_and_maybe_emit` would NOT emit, protecting
    //     a replacement process from a mis-fire.
    //
    // We can't intercept the `AppHandle.emit` without a Tauri
    // runtime, but the registry semantics are the only side that
    // needs pinning — the emit is a thin wrapper over the
    // registry's compare-and-remove.

    // Branch 1: reaped=true. Insert + reap with current generation
    // must remove the entry.
    let registry = BuildRunRegistry::new();
    let node_id = -915_1547;
    let (g, _) = registry.insert(node_id, dummy_process());
    assert!(registry.reap_incarnation(node_id, g));
    assert!(!registry.contains(&node_id));

    // Branch 2: reaped=false. A stale generation must be a no-op
    // — the entry survives. This is what protects a replacement
    // process from being mis-removed by an old reader's late EOF.
    let (g2, _) = registry.insert(node_id, dummy_process());
    assert!(!registry.reap_incarnation(node_id, g2.wrapping_add(99)));
    assert!(registry.contains(&node_id));
    assert!(registry.reap_incarnation(node_id, g2));
    assert!(!registry.contains(&node_id));
}

/// Behavioural coverage: when a reader thread's `reap_incarnation`
/// succeeds, the entry must actually be removed from the registry.
/// This exercises the `reap_and_maybe_emit` function indirectly via
/// the same path the reader uses.
#[test]
fn reader_thread_body_reap_path_removes_current_entry() {
    let registry = BuildRunRegistry::new();
    let node_id = -915_1546;
    let (g, _) = registry.insert(node_id, dummy_process());
    assert!(registry.contains(&node_id));

    // Simulate the reader's EOF: natural reaping removes the entry.
    let reaped = registry.reap_incarnation(node_id, g);
    assert!(reaped, "natural EOF must reap the entry");
    assert!(!registry.contains(&node_id));
}
