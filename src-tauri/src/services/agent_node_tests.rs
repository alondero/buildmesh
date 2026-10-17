//! Tests for `services::agent_node`.
//!
//! Split out of `src-tauri/src/services/agent_node.rs` by issue #2451. The conventions this
//! layout depends on — why the module keeps the name `tests`, and why every
//! `#[path]` module needs an entry in `RUST_PATH_INCLUDES` — are
//! documented once in `docs/development/rust-conventions.md`, under *Rust unit
//! tests live in sibling `*_tests.rs` files*.
//!
//! Run with: cd src-tauri && cargo test services::agent_node::tests

use super::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let tmp = std::env::temp_dir().join(format!("buildmesh_agent_node_test_{}_{}", pid, id));
        Self(tmp)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn sig() -> git2::Signature<'static> {
    git2::Signature::now("test", "test@example.com").unwrap()
}

fn init_repo(path: &Path) -> git2::Repository {
    fs::create_dir_all(path).unwrap();
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("main");
    let repo = git2::Repository::init_opts(path, &opts).unwrap();

    fs::write(path.join("file.txt"), "initial").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("file.txt")).unwrap();
    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let s = sig();
    repo.commit(Some("HEAD"), &s, &s, "initial commit", &tree, &[])
        .unwrap();
    drop(tree);

    repo
}

fn add_worktree(root_repo: &git2::Repository, root: &TempDir, name: &str) -> PathBuf {
    let head = root_repo.head().unwrap().peel_to_commit().unwrap();
    let branch = root_repo.branch(name, &head, false).unwrap();
    let reference = branch.into_reference();
    let worktree_path = root.path().join(".claude").join("worktrees").join(name);
    fs::create_dir_all(worktree_path.parent().unwrap()).unwrap();
    root_repo
        .worktree(
            name,
            &worktree_path,
            Some(git2::WorktreeAddOptions::new().reference(Some(&reference))),
        )
        .unwrap();
    worktree_path
}

#[test]
fn drain_removes_real_worktree_and_dequeues_only_on_success() {
    let root = TempDir::new();
    let root_repo = init_repo(root.path());
    let good = add_worktree(&root_repo, &root, "wt-good");

    let pending = vec![
        PendingWorktreeRemoval::new(&good.to_string_lossy(), "wt-good"),
        PendingWorktreeRemoval::new("/nonexistent/wt-bad", "wt-bad"),
    ];

    let dequeued = std::cell::RefCell::new(Vec::<String>::new());
    let failures = process_removals(
        pending,
        |path| {
            // A missing parent repo can't be opened → genuine failure, and
            // the failure names the step that failed (issue #2139).
            if path.starts_with("/nonexistent") {
                return Err(crate::git::worktree::WorktreeRemovalFailure::for_test(
                    crate::git::worktree::OP_OPEN_WORKTREE_REPO,
                    "not a removable worktree",
                ));
            }
            crate::git::worktree::remove_one_worktree_and_branch_detailed(path)
        },
        |path| {
            dequeued.borrow_mut().push(path.to_string());
            Ok(())
        },
        // Issue #653: this test predates the deleter-side guard and its
        // fixture doesn't touch `warm_worktrees`, so the guard is a
        // pure no-op. `_| Ok(false)` keeps the original behaviour intact.
        |_path| Ok(false),
    );

    assert!(
        !good.exists(),
        "the real worktree directory must be removed"
    );
    assert_eq!(
        dequeued.into_inner(),
        vec![good.to_string_lossy().to_string()],
        "only the successful removal is dequeued"
    );
    assert_eq!(
        failures.len(),
        1,
        "the failed removal is returned for warning"
    );
    assert_eq!(failures[0].0.node_name, "wt-bad");
    assert_eq!(
        failures[0].1.operation,
        crate::git::worktree::OP_OPEN_WORKTREE_REPO,
        "the failure carries the step that failed so the UI can label it"
    );
}

/// Issue #653 deleter-side guard: a pending-removal entry whose path is
/// currently `claimed` in `warm_worktrees` (a live spawn has just adopted
/// the directory) must be SKIPPED — `remove` is not called (the directory
/// belongs to a live agent) and the tombstone IS dequeued (claim
/// supersedes the close's intent, so the entry is dead). It must NOT be
/// reported as a failure (no user-facing warning for a healthy state).
///
/// Without this guard, a concurrent close + spawn on the same path would
/// race `process_pending_removals` to delete the directory out from under
/// the spawning agent — issue #653's exact symptom (Error node + stranded
/// `claimed` row).
#[test]
fn process_removals_skips_claimed_path_and_dequeues() {
    let pending = vec![
        // Live spawn's worktree: warm pool row is `claimed`. The drain
        // must skip — no `remove`, dequeue + no failure.
        PendingWorktreeRemoval::new("/repo/m/.claude/worktrees/wt-claimed", "wt-claimed"),
        // Normal cold removal: not claimed. The drain proceeds.
        PendingWorktreeRemoval::new("/repo/m/.claude/worktrees/wt-cold", "wt-cold"),
    ];

    let removed = std::cell::RefCell::new(Vec::<String>::new());
    let dequeued = std::cell::RefCell::new(Vec::<String>::new());
    let failures = process_removals(
        pending,
        |path| {
            removed.borrow_mut().push(path.to_string());
            Ok(())
        },
        |path| {
            dequeued.borrow_mut().push(path.to_string());
            Ok(())
        },
        // Warm pool says `claimed` for the first path only.
        |path| Ok(path.ends_with("wt-claimed")),
    );

    assert_eq!(
            removed.into_inner(),
            vec!["/repo/m/.claude/worktrees/wt-cold".to_string()],
            "only the non-claimed path is touched by the removal driver — the live spawn's directory must survive"
        );
    // Both paths are dequeued: the skip branch consumes the tombstone
    // (claim supersedes intent), and the successful removal consumes its
    // own tombstone. The shared log is what proves the order-agnostic
    // "both paths leave the queue" invariant.
    let dequeued = dequeued.into_inner();
    assert!(
        dequeued.contains(&"/repo/m/.claude/worktrees/wt-claimed".to_string()),
        "the superseded tombstone must be dequeued (claim supersedes close intent)"
    );
    assert!(
        dequeued.contains(&"/repo/m/.claude/worktrees/wt-cold".to_string()),
        "the normal removal's tombstone must be dequeued"
    );
    assert_eq!(dequeued.len(), 2, "every pending entry must exit the queue");
    assert!(
        failures.is_empty(),
        "a `claimed` skip is not a failure — no user warning warranted (got: {failures:?})"
    );
}

/// A transient `is_claimed` error (e.g. SQLite lock contention mid-drain)
/// must fail CLOSED: don't remove the directory (could be a live spawn)
/// and don't drop the tombstone — the next drain retries. The caller
/// MUST NOT see this as a worktree-cleanup failure: only `remove()`
/// errors reach `failures`, so a guard-query error stays silent on the
/// user side (no spurious "worktree cleanup failed" toast) and the
/// tombstone survives for the next drain to retry.
#[test]
fn process_removals_fails_closed_when_is_claimed_errors() {
    let pending = vec![PendingWorktreeRemoval::new(
        "/repo/m/.claude/worktrees/wt-uncertain",
        "wt-uncertain",
    )];

    let removed = std::cell::RefCell::new(Vec::<String>::new());
    let dequeued = std::cell::RefCell::new(Vec::<String>::new());
    let failures = process_removals(
        pending,
        |path| {
            removed.borrow_mut().push(path.to_string());
            Ok(())
        },
        |path| {
            dequeued.borrow_mut().push(path.to_string());
            Ok(())
        },
        |_path| Err(rusqlite::Error::QueryReturnedNoRows), // guard query failed
    );

    assert!(
        removed.borrow().is_empty(),
        "must NOT touch the directory when the guard check errors — could be a live spawn"
    );
    assert!(
        dequeued.borrow().is_empty(),
        "must NOT dequeue when the guard check errors — leave the tombstone for the next drain"
    );
    assert!(
        failures.is_empty(),
        "a guard-query error must NOT surface as a worktree-cleanup failure \
             (would emit a misleading user toast); the next drain retries silently"
    );
}

// -------------------------------------------------------------------
// Issue #2139 — blocked cleanups are persisted, backed off and reported
// at most once per unchanged blocker.
// -------------------------------------------------------------------

/// The backoff schedule must grow, so a blocker the drain cannot clear
/// costs less and less rows/second, and must cap so a worktree blocked
/// overnight is still retried promptly after the next launch.
#[test]
fn backoff_schedule_doubles_from_five_seconds_and_caps() {
    assert_eq!(backoff_secs(0), 5, "a never-attempted row is due now");
    assert_eq!(backoff_secs(1), 5, "first retry after 5 seconds");
    assert_eq!(backoff_secs(2), 10, "the wait doubles each failure");
    assert_eq!(backoff_secs(3), 20);
    assert_eq!(backoff_secs(6), 160);
    assert_eq!(
        backoff_secs(64),
        300,
        "capped so a stale block is retried promptly"
    );
}

/// A directory that is not a worktree can never be removed, which makes it
/// a deterministic blocker for the drain-level tests below (no process
/// juggling, no Windows-specific locking). Returns the directory (so the
/// test can assert it was left alone) and its path.
fn enqueue_blocked_cleanup(root: &TempDir, name: &str) -> (PathBuf, String) {
    let blocker = root.path().join(name);
    fs::create_dir_all(&blocker).unwrap();
    let path = blocker.to_string_lossy().to_string();
    db::enqueue_worktree_removal_inner(&db::write_conn(), &path, name).unwrap();
    (blocker, path)
}

/// The user hears about a blocked worktree once. The same blocker on the
/// next drain is not new information, so it retries silently instead of
/// raising another warning toast — the pre-#2139 behaviour was one toast
/// per drain per worktree.
///
/// Both drains are real `drain_pending_removals` calls with a fixed clock
/// and a zero backoff, so the second pass goes through the same backoff
/// gate, the same removal and the same bookkeeping as production — the
/// previous version called the bookkeeping half by hand and its comment
/// claimed the gate had let it through, which it never had (issue #2139
/// review round 1).
#[test]
fn blocked_cleanup_warns_once_and_suppresses_the_unchanged_blocker() {
    let _db = crate::db::test_support::isolated();
    let td = TempDir::new();
    let (blocker, path) = enqueue_blocked_cleanup(&td, "wt-blocked");
    let zero_backoff = |_attempts: i64| 0i64;

    let first = drain_pending_removals(1_000, zero_backoff);
    assert_eq!(first.len(), 1, "one blocked cleanup reported");
    assert!(
        first[0].notify,
        "the first failure is new information and must reach the user"
    );
    assert_eq!(first[0].removal.attempt_count, 1);
    assert_eq!(
        first[0].removal.last_operation.as_deref(),
        Some(crate::git::worktree::OP_OPEN_WORKTREE_REPO),
        "the failed step is persisted for the UI"
    );
    assert!(
        first[0].removal.last_error.is_some(),
        "the OS error is persisted"
    );

    // A real second drain, a second later in injected time. The backoff the
    // first pass wrote is due (zero-length window), the blocker is
    // unchanged, so it is attempted again and stays silent.
    let second = drain_pending_removals(2_000, zero_backoff);
    assert_eq!(second.len(), 1, "the cleanup is still blocked");
    assert!(
        second[0].removal.retry_due(2_000),
        "the second drain was not held back by the backoff"
    );
    assert!(
        !second[0].notify,
        "an unchanged blocker must not warn again (issue #2139)"
    );
    assert_eq!(
        second[0].removal.attempt_count, 2,
        "attempts keep accumulating"
    );

    // The evidence survived both attempts and reads back from the queue.
    let [row] = &list_blocked_worktree_cleanups().unwrap()[..] else {
        panic!("the queue holds exactly one blocked cleanup");
    };
    assert_eq!(row.worktree_path, path);
    assert_eq!(row.attempt_count, 2);
    assert!(row.last_error.is_some() && row.last_operation.is_some());
    assert!(
        blocker.exists(),
        "the blocked directory is left untouched — no gutting (issue #239)"
    );
}

/// A worktree inside its backoff window is not attempted at all: no
/// removal, no new failure row, no notification. Without the gate the
/// drain retries a block it already knows about on every pass.
#[test]
fn a_cleanup_inside_its_backoff_window_is_not_attempted() {
    let _db = crate::db::test_support::isolated();
    let td = TempDir::new();
    let (blocker, path) = enqueue_blocked_cleanup(&td, "wt-backoff");
    db::record_pending_removal_failure(
        &path,
        crate::git::worktree::OP_RENAME_TO_STAGING,
        "being used by another process (os error 32)",
        1_000,
        i64::MAX, // far future: retry not due
    )
    .unwrap();

    let blocked = process_pending_removals();

    assert!(
        blocked.is_empty(),
        "an entry inside its backoff window must not be attempted or reported"
    );
    let [row] = &list_blocked_worktree_cleanups().unwrap()[..] else {
        panic!("the queue still holds the backing-off cleanup")
    };
    assert_eq!(row.attempt_count, 1, "no second attempt was made");
    assert_eq!(
        row.last_operation.as_deref(),
        Some(crate::git::worktree::OP_RENAME_TO_STAGING),
        "the previous failure record is untouched"
    );
    assert!(
        blocker.exists(),
        "the blocked directory must not be touched while backing off"
    );
}

/// A user-triggered retry of a worktree that is no longer blocked must
/// actually remove it and dequeue the tombstone.
#[test]
fn retry_removes_a_worktree_once_it_is_unblocked() {
    let _db = crate::db::test_support::isolated();
    let root = TempDir::new();
    let root_repo = init_repo(root.path());
    let good = add_worktree(&root_repo, &root, "wt-retry");
    let path = good.to_string_lossy().to_string();
    db::enqueue_worktree_removal_inner(&db::write_conn(), &path, "wt-retry").unwrap();

    match retry_pending_worktree_removal(&path) {
        CleanupRetryOutcome::Removed => {}
        other => panic!(
            "expected the retry to remove the worktree, got {:?}",
            outcome_tag(&other)
        ),
    }

    assert!(!good.exists(), "the worktree directory must be gone");
    assert!(
        list_blocked_worktree_cleanups().unwrap().is_empty(),
        "a successful retry dequeues the tombstone"
    );
}

/// A user-triggered retry of a still-blocked worktree records the failure
/// (so the evidence grows) and leaves the tombstone queued, rather than
/// reporting a bare success.
#[test]
fn retry_of_a_still_blocked_worktree_records_the_failure() {
    let _db = crate::db::test_support::isolated();
    let td = TempDir::new();
    let (_blocker, path) = enqueue_blocked_cleanup(&td, "wt-still-blocked");

    match retry_pending_worktree_removal(&path) {
        CleanupRetryOutcome::StillBlocked(record) => {
            assert_eq!(record.attempt_count, 1, "the retry is one more attempt");
            assert!(
                record.last_operation.is_some() && record.last_error.is_some(),
                "the retry persists its own evidence"
            );
        }
        other => panic!("expected StillBlocked, got {:?}", outcome_tag(&other)),
    }

    let [row] = &list_blocked_worktree_cleanups().unwrap()[..] else {
        panic!("the blocked cleanup stays queued after a failed retry")
    };
    assert_eq!(row.attempt_count, 1);
}

/// A retry for a path the queue no longer holds reports `Gone` instead of
/// pretending it removed something.
#[test]
fn retry_after_dismiss_reports_the_entry_is_gone() {
    let _db = crate::db::test_support::isolated();
    let td = TempDir::new();
    let (_blocker, path) = enqueue_blocked_cleanup(&td, "wt-dismissed");

    let dismissed = dismiss_pending_worktree_removal(&path);
    assert!(
        dismissed.cancelled,
        "the clean cases cancel the cleanup intent"
    );
    assert!(matches!(
        dismissed.kind,
        WorktreeCleanupDismissal::Unchanged
    ));

    assert!(matches!(
        retry_pending_worktree_removal(&path),
        CleanupRetryOutcome::Gone
    ));
    assert!(
        list_blocked_worktree_cleanups().unwrap().is_empty(),
        "dismissing cancels the cleanup intent"
    );
}

/// A blocker that *changes shape* is new information and must warn again:
/// the error that stopped the removal changed, so the user needs a fresh
/// (accurate) diagnosis rather than a suppressed repeat.
#[test]
fn a_changed_blocker_warns_again() {
    let _db = crate::db::test_support::isolated();
    let path = "/repo/m/.claude/worktrees/wt-changing";
    db::enqueue_worktree_removal_inner(&db::write_conn(), path, "wt-changing").unwrap();

    // First failure: access denied.
    let first = record_blocked_cleanups(
        vec![(
            PendingWorktreeRemoval::new(path, "wt-changing"),
            failed(
                crate::git::worktree::OP_OPEN_WORKTREE_REPO,
                "access is denied",
            ),
        )],
        1_000,
        |_attempts: i64| 0i64,
    );
    assert!(first[0].notify, "the first blocker is new information");

    // Same blocker again, seeded from the row that pass persisted.
    let row = list_blocked_worktree_cleanups().unwrap().remove(0);
    assert!(
        row.already_notified(
            crate::git::worktree::OP_OPEN_WORKTREE_REPO,
            "access is denied",
            2_000
        ),
        "the recorded signature suppresses the identical blocker"
    );
    let repeated = record_blocked_cleanups(
        vec![(
            row,
            failed(
                crate::git::worktree::OP_OPEN_WORKTREE_REPO,
                "access is denied",
            ),
        )],
        2_000,
        |_attempts: i64| 0i64,
    );
    assert!(
        !repeated[0].notify,
        "an identical blocker on the next drain stays silent"
    );

    // Same step, a *different* OS error: the blocker changed shape.
    let row = list_blocked_worktree_cleanups().unwrap().remove(0);
    let changed = record_blocked_cleanups(
        vec![(
            row,
            failed(crate::git::worktree::OP_OPEN_WORKTREE_REPO, "os error 32"),
        )],
        3_000,
        |_attempts: i64| 0i64,
    );
    assert!(changed[0].notify, "a changed blocker warns again");
}

/// Test helper: a failure for a specific step of the worktree-removal
/// vocabulary, so a test can say which step broke.
fn failed(operation: &'static str, detail: &str) -> crate::git::worktree::WorktreeRemovalFailure {
    crate::git::worktree::WorktreeRemovalFailure::for_test(operation, detail)
}

/// Label for panic messages: `StillBlocked`/`Removed`/`Gone`.
fn outcome_tag(outcome: &CleanupRetryOutcome) -> &'static str {
    match outcome {
        CleanupRetryOutcome::Removed => "Removed",
        CleanupRetryOutcome::StillBlocked(_) => "StillBlocked",
        CleanupRetryOutcome::Gone => "Gone",
        CleanupRetryOutcome::ClaimedByLiveSpawn => "ClaimedByLiveSpawn",
        CleanupRetryOutcome::QueueReadFailed => "QueueReadFailed",
    }
}

/// Issue #653, manual path: a blocked cleanup whose path a warm-pool spawn
/// has since adopted must NOT be removed by a user-triggered retry. The
/// automatic drain skips claimed paths; a manual Retry that ignored the
/// claim would delete a live agent's worktree, uncommitted work included.
#[test]
fn retry_leaves_a_live_spawns_claimed_worktree_alone() {
    let _db = crate::db::test_support::isolated();
    let root = TempDir::new();
    let root_repo = init_repo(root.path());
    let claimed = add_worktree(&root_repo, &root, "wt-claimed");
    let path = claimed.to_string_lossy().to_string();

    // A spawn has just adopted this directory.
    let mesh = crate::db::create_mesh("claim-test", root.path().to_str().unwrap()).unwrap();
    crate::db::insert_warm_worktree(
        mesh.id,
        &path,
        "wt-claimed",
        None,
        crate::db::WarmWorktreeStatus::Claimed,
    )
    .unwrap();
    db::enqueue_worktree_removal_inner(&db::write_conn(), &path, "wt-claimed").unwrap();

    match retry_pending_worktree_removal(&path) {
        CleanupRetryOutcome::ClaimedByLiveSpawn => {}
        other => panic!("expected ClaimedByLiveSpawn, got {}", outcome_tag(&other)),
    }

    assert!(
        claimed.exists(),
        "a claimed worktree belongs to a live agent and must survive a manual retry"
    );
    assert!(
        list_blocked_worktree_cleanups().unwrap().is_empty(),
        "the superseded tombstone is dequeued, as in the drain (issue #653)"
    );
}

/// "Keep worktree" on a worktree that was never touched cancels the intent
/// and leaves the disk exactly as it is.
#[test]
fn dismiss_of_an_untouched_worktree_leaves_the_disk_alone() {
    let _db = crate::db::test_support::isolated();
    let root = TempDir::new();
    let root_repo = init_repo(root.path());
    let good = add_worktree(&root_repo, &root, "wt-keep");
    let path = good.to_string_lossy().to_string();
    db::enqueue_worktree_removal_inner(&db::write_conn(), &path, "wt-keep").unwrap();

    let dismissed = dismiss_pending_worktree_removal(&path);
    assert!(
        dismissed.cancelled,
        "the clean cases cancel the cleanup intent"
    );

    assert!(matches!(
        dismissed.kind,
        WorktreeCleanupDismissal::Unchanged
    ));
    assert!(good.exists(), "the worktree is still where it was");
    assert!(good.join("file.txt").exists(), "its contents are untouched");
    assert!(list_blocked_worktree_cleanups().unwrap().is_empty());
    assert!(
        dismissed.kind.message(&path).contains(&path),
        "the message names the path the user asked about"
    );
}

/// A removal that had already renamed the worktree to `<path>.removing`
/// (the #239 staging rename) leaves the worktree at a name the user never
/// chose. "Keep worktree" must move it back, so the agent's work is not
/// stranded, and must say that the folder moved.
#[test]
fn dismiss_restores_a_worktree_the_removal_had_moved_aside() {
    let _db = crate::db::test_support::isolated();
    let root = TempDir::new();
    let root_repo = init_repo(root.path());
    let moved = add_worktree(&root_repo, &root, "wt-staged");
    let path = moved.to_string_lossy().to_string();
    let staging = format!("{path}.removing");

    // An interrupted removal: the rename into staging landed, the staged
    // delete never ran.
    fs::rename(&moved, &staging).unwrap();
    assert!(!std::path::Path::new(&path).exists());
    db::enqueue_worktree_removal_inner(&db::write_conn(), &path, "wt-staged").unwrap();

    let dismissed = dismiss_pending_worktree_removal(&path);
    assert!(
        dismissed.cancelled,
        "the clean cases cancel the cleanup intent"
    );

    assert!(
        matches!(
            dismissed.kind,
            WorktreeCleanupDismissal::RestoredFromStaging
        ),
        "the staging copy is moved back, not left stranded"
    );
    assert!(std::path::Path::new(&path).exists(), "the worktree is back");
    assert!(!std::path::Path::new(&staging).exists(), "staging is gone");
    assert!(
        std::path::Path::new(&path).join("file.txt").exists(),
        "the restored worktree is intact"
    );
    assert!(
        dismissed.kind.message(&path).contains("moved"),
        "the message admits the folder had moved, got: {}",
        dismissed.kind.message(&path)
    );
    assert!(list_blocked_worktree_cleanups().unwrap().is_empty());
}

/// A worktree whose staged delete had already begun is reported as
/// incomplete rather than promised as an intact restore.
#[test]
fn dismiss_reports_a_partially_deleted_worktree_as_incomplete() {
    let _db = crate::db::test_support::isolated();
    let root = TempDir::new();
    let root_repo = init_repo(root.path());
    let moved = add_worktree(&root_repo, &root, "wt-partial");
    let path = moved.to_string_lossy().to_string();
    let staging = format!("{path}.removing");

    fs::rename(&moved, &staging).unwrap();
    // A staged delete that removed the git link and a source file before
    // failing leaves the copy partial. (`.git` in a worktree is a file.)
    fs::remove_file(std::path::Path::new(&staging).join(".git")).unwrap();
    fs::remove_file(std::path::Path::new(&staging).join("file.txt")).unwrap();
    db::enqueue_worktree_removal_inner(&db::write_conn(), &path, "wt-partial").unwrap();

    let dismissed = dismiss_pending_worktree_removal(&path);
    assert!(
        dismissed.cancelled,
        "the clean cases cancel the cleanup intent"
    );

    assert!(
        matches!(dismissed.kind, WorktreeCleanupDismissal::RestoredIncomplete),
        "a gutted worktree must not be reported as restored"
    );
    assert!(
        dismissed.kind.message(&path).contains("deleted part of it"),
        "got: {}",
        dismissed.kind.message(&path)
    );
}

/// Nothing on disk at all (the removal finished the directory but the queue
/// row was never dequeued) is reported as such rather than as a keep.
#[test]
fn dismiss_reports_nothing_to_keep_when_the_folder_is_already_gone() {
    let _db = crate::db::test_support::isolated();
    let root = TempDir::new();
    let gone = root.path().join("wt-vanished");
    fs::create_dir_all(&gone).unwrap();
    let path = gone.to_string_lossy().to_string();
    fs::remove_dir_all(&gone).unwrap();
    db::enqueue_worktree_removal_inner(&db::write_conn(), &path, "wt-vanished").unwrap();

    let dismissed = dismiss_pending_worktree_removal(&path);
    assert!(
        dismissed.cancelled,
        "the clean cases cancel the cleanup intent"
    );

    assert!(matches!(
        dismissed.kind,
        WorktreeCleanupDismissal::NothingOnDisk
    ));
    assert!(dismissed
        .kind
        .message(&path)
        .contains("Nothing left to keep"));
    assert!(list_blocked_worktree_cleanups().unwrap().is_empty());
}

/// A worktree staged by a removal and still held by a live process cannot
/// be moved back. "Keep worktree" must not then cancel the cleanup: the
/// folder is at `<path>.removing`, so the queue row — which says the
/// worktree is at `path` — is the only thing still tracking it. Cancelling
/// would strand the work under a name the user never chose and stop the
/// drain from retrying (issue #2139 review round 2).
#[test]
#[cfg(windows)]
fn dismiss_keeps_the_intent_when_the_staged_worktree_cannot_be_moved_back() {
    use crate::env::test_helpers::ScopedChild;
    let _db = crate::db::test_support::isolated();
    let root = TempDir::new();
    let root_repo = init_repo(root.path());
    let moved = add_worktree(&root_repo, &root, "wt-held-staging");
    let path = moved.to_string_lossy().to_string();
    let staging = format!("{path}.removing");

    // An interrupted removal: the rename into staging landed. Something is
    // still holding the staged copy, so the rename back fails too.
    fs::rename(&moved, &staging).unwrap();
    let mut command = crate::process_util::command_no_window("cmd");
    command
        .args(["/c", "ping -n 30 127.0.0.1"])
        .current_dir(&staging)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let _holder = ScopedChild::spawn(command);
    std::thread::sleep(std::time::Duration::from_millis(500));
    db::enqueue_worktree_removal_inner(&db::write_conn(), &path, "wt-held-staging").unwrap();

    let dismissed = dismiss_pending_worktree_removal(&path);

    assert!(
        matches!(dismissed.kind, WorktreeCleanupDismissal::RestoreFailed),
        "nothing was restored, so that is what must be reported"
    );
    assert!(
        !dismissed.cancelled,
        "the cleanup intent must survive a failed restore"
    );
    assert!(
        dismissed
            .kind
            .message(&path)
            .contains("cleanup stays queued"),
        "got: {}",
        dismissed.kind.message(&path)
    );
    // The queue keeps tracking the worktree, so the drain can retry once
    // the holder is gone, and the folder is left exactly where it was.
    assert_eq!(
        list_blocked_worktree_cleanups().unwrap().len(),
        1,
        "the tombstone is still queued"
    );
    assert!(
        std::path::Path::new(&staging).exists() && !std::path::Path::new(&path).exists(),
        "the staged copy is left alone for the next removal to reclaim"
    );
}

/// A retry on a path the queue no longer holds is a definite "nothing to
/// do", distinct from an unreadable queue.
#[test]
fn retry_without_a_queue_row_reports_gone() {
    let _db = crate::db::test_support::isolated();
    assert!(matches!(
        retry_pending_worktree_removal("/repo/m/.claude/worktrees/wt-absent"),
        CleanupRetryOutcome::Gone
    ));
}

// -------------------------------------------------------------------
// Issue #448 — pin the `source_pr = None` invariant on the issue-spawn
// and hand-spawn paths. `services::agent_node::create` and
// `create_pending` are the only entry points non-PR spawns go through
// (issue-spawn, handover, generic mobile spawn, Tauri hand-spawn);
// `spawn_agent_inner` uses `node.source_pr.is_some()` to decide whether
// to take the worktree-adoption path. A future caller — an importer,
// a migration script — could silently write a
// `source_pr` onto a node that didn't actually come from a PR, and the
// user would see a confusing "could not fetch PR head ref" warning on
// a node spawned from a regular issue.
//
// The wrappers now refuse `Some(_)` for `source_pr` and
// `source_pr_pinned_sha` at function entry (PR-spawn fields). The PR-spawn
// entry points (`create_with_source_pr_fork` /
// `create_pending_with_source_pr_fork`) remain the only functions that
// can write a non-`None` `source_pr` — those tests live with the PR-spawn
// callers in `commands::agent_tests`.
//
// Two complementary tests cover the invariant:
//
//   * Positive (happy-path) tests exercise `create` / `create_pending`
//     with `source_pr = None` and assert the persisted row reads back
//     `source_pr = None`. These need a real DB row, so they call
//     `fresh_mesh()`, which creates a mesh inside this test's own
//     database (`db::test_support::isolated`, issue #2048).
//
//   * Negative `#[should_panic]` tests call the wrappers with
//     `source_pr = Some(_)`. The assertion fires before any DB call, so
//     these don't need a DB at all — they catch a regression at the
//     wrapper boundary with zero infrastructure.
// -------------------------------------------------------------------

// Each test below installs its own database with `db::test_support::isolated`.

#[test]
fn spawn_configurations_snapshot_reaches_idle_and_pending_nodes() {
    use crate::preferences::spawn_configurations::SpawnConfiguration;
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();
    let mut configuration = SpawnConfiguration {
        id: "sol".into(),
        name: "Sol Max".into(),
        spawn_option_id: "codex".into(),
        model: Some("gpt-5.6-sol".into()),
        effort: None,
        extra_args: Some("--search".into()),
        ..Default::default()
    };
    for pending in [false, true] {
        let node = create_blocking_configured(
            mesh_id,
            Some("codex"),
            Some("main"),
            None,
            None,
            Some(false),
            pending,
            Some(&configuration),
            None,
            None,
        )
        .unwrap()
        .node;
        assert_eq!(node.provider, "codex");
        assert_eq!(
            node.status,
            if pending {
                SessionStatus::Pending
            } else {
                SessionStatus::Idle
            }
        );
        let saved = db::node_spawn_configuration(node.id, "codex")
            .unwrap()
            .unwrap();
        assert_eq!(saved.id, configuration.id);
        assert_eq!(saved.model, configuration.model);
        assert_eq!(saved.resolved.unwrap().harness.harness, "codex");
    }
    let node = create_blocking_configured(
        mesh_id,
        Some("codex"),
        Some("main"),
        None,
        None,
        Some(false),
        false,
        Some(&configuration),
        None,
        None,
    )
    .unwrap()
    .node;
    configuration.model = Some("edited-later".into());
    assert_eq!(
        db::node_spawn_configuration(node.id, "codex")
            .unwrap()
            .unwrap()
            .model
            .as_deref(),
        Some("gpt-5.6-sol")
    );
    db::set_agent_node_provider(node.id, "claude:minimax").unwrap();
    assert!(db::node_spawn_configuration(node.id, "claude:minimax")
        .unwrap()
        .is_none());
    db::set_agent_node_provider(node.id, "codex").unwrap();
    assert!(db::node_spawn_configuration(node.id, "codex")
        .unwrap()
        .is_none());
    let defaults = create_blocking(
        mesh_id,
        Some("codex"),
        Some("main"),
        None,
        None,
        Some(false),
        false,
        None,
    )
    .unwrap();
    assert_eq!(
        db::node_spawn_configuration(defaults.id, "codex")
            .unwrap()
            .unwrap()
            .id,
        "launch/codex"
    );
    assert!(db::node_spawn_configuration(i64::MAX, "codex")
        .unwrap()
        .is_none());
}

/// Create a fresh mesh at a unique per-test path and return its id. Each
/// call uses a monotonic counter so parallel tests can't collide on the
/// `meshes.path` UNIQUE constraint.
///
/// The caller installs the database with `db::test_support::isolated()` and
/// holds that guard for the rest of the test (issue #2048). A helper that
/// installed the database and dropped the guard on return would leave the
/// rest of the test reading the process-global database, or panicking with
/// "database not initialized" — and returning the guard in a tuple would
/// invite `let (mesh_id, _) = fresh_mesh()`, which drops it immediately.
fn fresh_mesh() -> i64 {
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    let path = format!("/tmp/buildmesh_invariant_test_{}", id);
    let mesh_id = crate::db::create_mesh(&format!("invariant-{}", id), &path)
        .expect("fresh_mesh: create_mesh should succeed")
        .id;
    mesh_id
}

#[test]
fn create_returns_node_with_source_pr_none() {
    // The wrapper contract: passing `source_pr = None` for an
    // issue-spawn / hand-spawn call persists `source_pr = None`.
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();

    let node = create(
        mesh_id,
        "/tmp/buildmesh_invariant_test",
        "main",
        Some("anthropic"),
        None,
        None,
        None,
        None,
        None,
    )
    .expect("create with source_pr=None must succeed");

    assert_eq!(
        node.source_pr, None,
        "issue-spawn wrapper must persist source_pr=None"
    );
    assert_eq!(
        node.source_pr_pinned_sha, None,
        "issue-spawn wrapper must persist source_pr_pinned_sha=None"
    );
    assert_eq!(
        node.source_issue, None,
        "hand-spawn wrapper must persist source_issue=None when not supplied"
    );
}

#[test]
fn create_pending_returns_node_with_source_pr_none_and_pending_status() {
    // `create_pending` is the fast stage-1 of the two-stage issue-spawn
    // flow — it must also persist `source_pr = None` so stage-2's
    // `node.source_pr.is_some()` branch doesn't accidentally fire.
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();

    let node = create_pending(
        mesh_id,
        "/tmp/buildmesh_invariant_test",
        "main",
        Some("anthropic"),
        None,
        None,
        None,
        None,
    )
    .expect("create_pending with source_pr=None must succeed");

    assert_eq!(
        node.source_pr, None,
        "create_pending must persist source_pr=None"
    );
    assert_eq!(
        node.source_pr_pinned_sha, None,
        "create_pending must persist source_pr_pinned_sha=None"
    );
    assert_eq!(
        node.status,
        SessionStatus::Pending,
        "create_pending must leave the row in Pending so the frontend \
             can distinguish stage-1-done from idle-and-ready-to-resume"
    );
}

#[test]
fn create_with_source_issue_set_keeps_source_pr_independent() {
    // Optional step 4 from the issue: when `source_issue = Some(N)`,
    // `source_pr` must still come back as `None` — the two source
    // fields are independent columns on the row.
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();

    let node = create(
        mesh_id,
        "/tmp/buildmesh_invariant_test",
        "main",
        Some("anthropic"),
        Some(123),
        None,
        None,
        None,
        Some("gh123-test-slug"),
    )
    .expect("create with source_issue=Some(N), source_pr=None must succeed");

    assert_eq!(
        node.source_issue,
        Some(123),
        "source_issue must round-trip independently of source_pr"
    );
    assert_eq!(
        node.source_pr, None,
        "source_pr must remain None even when source_issue is set"
    );
    assert_eq!(node.source_pr_pinned_sha, None);
}

#[test]
#[should_panic(expected = "source_pr=Some(_)")]
fn create_rejects_source_pr_some() {
    // A future caller — importer, migration script — tries to set
    // `source_pr` on a node that didn't actually come
    // from a PR. The wrapper must refuse at the boundary rather than
    // silently persist it. The assertion fires before any DB call, so
    // we don't even need `db::test_support::isolated()` here — a missing DB is the
    // strongest possible failure signal for this regression.
    let _ = create(
        /* mesh_id */ 0,
        "/tmp/never-read",
        "main",
        None,
        None,
        Some(42),
        None,
        None,
        None,
    );
}

#[test]
#[should_panic(expected = "source_pr=Some(_)")]
fn create_pending_rejects_source_pr_some() {
    let _ = create_pending(
        /* mesh_id */ 0,
        "/tmp/never-read",
        "main",
        None,
        None,
        Some(42),
        None,
        None,
    );
}

#[test]
#[should_panic(expected = "source_pr_pinned_sha=Some(_)")]
fn create_rejects_source_pr_pinned_sha_some() {
    // Same boundary for the SHA-pinning field (#444) — a caller that
    // tries to pin the head SHA on a non-PR spawn gets the same
    // refused-at-entry treatment.
    let _ = create(
        /* mesh_id */ 0,
        "/tmp/never-read",
        "main",
        None,
        None,
        None,
        Some("deadbeef"),
        None,
        None,
    );
}

// -----------------------------------------------------------------------
// `create_blocking` (issue #1658 step 5) — single shared runner used by
// Tauri commands and HTTP routes to fold the mesh-lookup +
// branch-resolution + node-create sequence into one helper. Every test
// here is a regression pin against a feature drop: a future refactor
// that loses a `branch_override` short-circuit, the Pending-status
// branch, or the "mesh not found" sentinel fails review by failing these
// tests first.
// -----------------------------------------------------------------------

#[test]
fn create_blocking_with_branch_override_matches_legacy_create_row() {
    // The headline equivalence pin: when `branch_override = Some("main")`,
    // `create_blocking` must produce a row identical to today's
    // `services::agent_node::create(mesh_id, &mesh.path, "main", provider, ...)`
    // would have produced. The four entry points that migrate to
    // `create_blocking` (issue #1658 step 5) depend on this byte-identical
    // row, so a regression that drops the explicit branch, or mangles the
    // provider column, fails the assertion rather than silently regressing
    // the mobile/desktop row shape.
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();

    let explicit_branch_row = create_blocking(
        mesh_id,
        Some("anthropic"),
        Some("main"),
        None,
        None,
        None,
        false, /* pending */
        None,
    )
    .expect("create_blocking with explicit branch must succeed");

    let legacy_row = create(
        mesh_id,
        "/tmp/buildmesh_invariant_test",
        "main",
        Some("anthropic"),
        None,
        None,
        None,
        None,
        None,
    )
    .expect("legacy create must succeed for the same inputs");

    assert_eq!(
        explicit_branch_row.branch, legacy_row.branch,
        "explicit branch_override must reach the persisted row identically"
    );
    assert_eq!(
        explicit_branch_row.provider, legacy_row.provider,
        "provider must round-trip identically between helper and legacy create"
    );
    assert_eq!(
        explicit_branch_row.mesh_id, legacy_row.mesh_id,
        "mesh_id must match"
    );
    assert_eq!(
        explicit_branch_row.status, legacy_row.status,
        "Idle status must be preserved when pending=false"
    );
}

#[test]
fn create_blocking_with_pending_true_returns_pending_status() {
    // `create_issue_node` (the desktop two-stage stage-1) and the
    // HTTP mobile route diverge on initial status: Pending vs Idle.
    // `pending: true` must land the row in Pending so the frontend
    // can distinguish "stage-1-done, awaiting background spawn"
    // from "ready-to-resume idle". Without this pin, a future
    // refactor that flattens the helper to always-Idle would
    // silently break the desktop flow.
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();

    let node = create_blocking(
        mesh_id,
        Some("anthropic"),
        Some("main"),
        Some(123), /* source_issue */
        Some("gh123-test-slug"),
        None,
        true, /* pending */
        None,
    )
    .expect("create_blocking with pending=true must succeed");

    assert_eq!(
        node.status,
        SessionStatus::Pending,
        "pending=true must leave the row in Pending"
    );
    assert_eq!(
        node.source_issue,
        Some(123),
        "source_issue must round-trip through the helper"
    );
    assert_eq!(
        node.source_pr, None,
        "non-PR spawn must persist source_pr=None (mirrors legacy create)"
    );
}

#[test]
fn create_blocking_returns_mesh_not_found_for_unknown_mesh_id() {
    // Install this test's own database up front and keep the guard for
    // the whole body (issue #2048) — without it, the unknown-id lookup
    // fails on "database not initialized" instead of surfacing the
    // intended `QueryReturnedNoRows`, which masks the sentinel we're
    // trying to pin.
    let _db = crate::db::test_support::isolated();
    // The HTTP route (`http::routes::nodes::create`) maps the
    // typed `AgentNodeError::MeshNotFound(_)` variant to its
    // "400 Bad Request — Mesh not found" response. A regression
    // that loses the mapping — falling back to a raw
    // `AgentNodeError::Db(rusqlite::Error::QueryReturnedNoRows)` —
    // would surface as "500 Internal Server Error" on the SPA
    // even though the user typo'd a mesh id.
    let err = create_blocking(
        /* mesh_id */ -1, /* not in the per-test DB */
        Some("anthropic"),
        Some("main"),
        None,
        None,
        None,
        false, /* pending */
        None,
    )
    .expect_err("create_blocking on unknown mesh_id must return Err");

    match err {
        AgentNodeError::MeshNotFound(mesh_id) => assert_eq!(
            mesh_id, -1,
            "the variant must carry the rejected mesh id so the route can include it in any log"
        ),
        other => panic!(
            "create_blocking on unknown mesh_id must return MeshNotFound(-1), got {:?}",
            other
        ),
    }
}

#[test]
fn create_blocking_with_branch_override_none_resolves_via_git() {
    // **Review-round-2 pin** (Finding #3): the prior test suite
    // exercised `branch_override = Some("main")` exclusively.
    // The `None` fallback path (which calls
    // `commands::git::get_default_branch_blocking`) was never
    // tested. Pin it: the helper must resolve the branch through
    // the canonical sync helper when no override is supplied,
    // landing on `"main"` because the per-test mesh path
    // (`/tmp/buildmesh_invariant_test_<id>`) doesn't exist as a
    // git repo and `get_default_branch_blocking` falls back.
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();

    let node = create_blocking(
        mesh_id,
        Some("anthropic"),
        /* branch_override = */ None,
        None,
        None,
        None,
        false, /* pending */
        None,
    )
    .expect("create_blocking with branch_override=None must succeed");

    assert_eq!(
        node.branch, "main",
        "branch_override=None must fall back to get_default_branch_blocking's \
             open-failure default (\"main\"); the per-test mesh has no git repo, so \
             the open-failure branch fires"
    );
}

#[test]
fn create_blocking_propagates_use_worktree_override_on_idle_path() {
    // **Review-round-2 pin** (Finding #1+#3): the prior test
    // suite never exercised `use_worktree_override = Some(_)`.
    // Idle path (`pending = false`) delegates to
    // `create_with_source_pr_fork`, which forwards the override.
    // Pinned here so a future refactor that drops the argument
    // (silently swallowing the caller intent) fails this test
    // before shipping.
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();

    let node = create_blocking(
        mesh_id,
        Some("anthropic"),
        Some("main"),
        None,
        None,
        /* use_worktree_override = */ Some(false),
        false, /* pending */
        None,
    )
    .expect("create_blocking on idle path must succeed");

    // `create_with_source_pr_fork` writes `use_worktree` only if
    // `session_name` is non-empty AND the override is not None.
    // `Some(false)` writes `false` (Root Node shape — `worktree_name = None`).
    assert!(
        !node.use_worktree,
        "use_worktree_override=Some(false) must persist as use_worktree=false on the row"
    );
    assert_eq!(
        node.worktree_name, None,
        "use_worktree=false on a fresh mesh must produce a Root Node (no worktree_name)"
    );
}

#[test]
fn create_blocking_propagates_use_worktree_override_on_pending_path() {
    // **Review-round-2 pin** (Finding #1+#3): the symmetric
    // path on `pending = true`. The original PR delegated through
    // `create_pending_with_source_pr_fork`, which drops the
    // override (`use_worktree_override = None` is hardcoded into
    // its inner call). The fix delegates directly to
    // `create_pending_with_source_pr_fork_and_worktree`, which
    // DOES forward the override. Pin BOTH paths so a future
    // refactor that re-introduces the asymmetric drop fails this
    // test.
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();

    let node = create_blocking(
        mesh_id,
        Some("anthropic"),
        Some("main"),
        None,
        None,
        /* use_worktree_override = */ Some(false),
        true, /* pending */
        None,
    )
    .expect("create_blocking on pending path must succeed");

    assert!(
        !node.use_worktree,
        "use_worktree_override=Some(false) must persist as use_worktree=false \
             on the Pending row too — Finding #1's regression"
    );
    assert_eq!(
        node.status,
        SessionStatus::Pending,
        "pending=true must still land in Pending on the override-forwarding path"
    );
    assert_eq!(
        node.worktree_name, None,
        "use_worktree=false on a fresh mesh must produce a Root Node even on Pending rows"
    );
}

// -----------------------------------------------------------------------
// Issue #2237 — one runner owns create and import, including the
// initial-prompt capability rule. Terminal cannot take a prompt.
// Codex can, and that success must not depend on the install menu.
// -----------------------------------------------------------------------

#[test]
fn prompted_terminal_create_rejects_and_inserts_nothing() {
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();
    let err = create_blocking(
        mesh_id,
        Some("terminal"),
        Some("main"),
        None,
        None,
        Some(false),
        false,
        Some("ship this"),
    )
    .expect_err("a prompt for Terminal must be rejected");
    assert!(matches!(err, AgentNodeError::InitialPromptUnsupported));
    assert_eq!(err.to_string(), INITIAL_PROMPT_UNSUPPORTED);
    assert!(db::list_agent_nodes_by_mesh(mesh_id).unwrap().is_empty());
}

#[test]
fn unprompted_terminal_create_still_inserts() {
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();
    let node = create_blocking(
        mesh_id,
        Some("terminal"),
        Some("main"),
        None,
        None,
        Some(false),
        false,
        None,
    )
    .expect("Terminal without a prompt is a normal create");
    assert_eq!(node.provider, "terminal");
    let blank = create_blocking(
        mesh_id,
        Some("terminal"),
        Some("main"),
        None,
        None,
        Some(false),
        false,
        Some("   "),
    )
    .expect("whitespace is not an initial prompt");
    assert_eq!(blank.provider, "terminal");
}

#[test]
fn prompted_codex_create_succeeds_without_the_install_menu() {
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();
    let node = create_blocking(
        mesh_id,
        Some("codex"),
        Some("main"),
        None,
        None,
        Some(false),
        false,
        Some("review the diff"),
    )
    .expect("codex accepts an initial prompt even when the CLI is absent");
    assert_eq!(node.provider, "codex");
}

#[test]
fn prompted_create_of_an_unknown_mesh_is_mesh_not_found() {
    let _db = crate::db::test_support::isolated();
    let err = create_blocking(
        -1,
        Some("terminal"),
        Some("main"),
        None,
        None,
        None,
        false,
        Some("hello"),
    )
    .expect_err("unknown mesh wins over the prompt rule");
    assert!(matches!(err, AgentNodeError::MeshNotFound(-1)));
}

#[test]
fn create_persists_default_worktree_path() {
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();
    let mesh = db::get_mesh_by_id(mesh_id).unwrap();
    let node = create_blocking(
        mesh_id,
        Some("codex"),
        Some("main"),
        None,
        Some("new-node"),
        Some(true),
        false,
        None,
    )
    .expect("create a default worktree node");
    let expected = format!("{}/.worktrees/new-node", mesh.path);
    assert_eq!(node.worktree_path.as_deref(), Some(expected.as_str()));
    assert_eq!(env::node_git_path(&node).raw_path, expected);
    let saved = db::get_agent_node_by_id(node.id).unwrap();
    assert_eq!(saved.worktree_path, node.worktree_path);
}

#[test]
fn import_preserves_default_checkout_after_directory_override() {
    let root = TempDir::new();
    let _repo = init_repo(root.path());
    let _db = crate::db::test_support::isolated();
    let mesh = db::create_mesh("default-import", root.path().to_str().unwrap()).unwrap();
    let path = format!("{}/.worktrees/default-node", mesh.path);
    crate::git::worktree::create_git_worktree(
        &mesh.path,
        &path,
        "default-node",
        "detached",
        "HEAD",
    )
    .unwrap();
    db::set_mesh_worktree_directory(mesh.id, Some("custom-wt")).unwrap();
    let node = import_discovered_session(
        mesh.id,
        "22222222-2222-2222-2222-222222222222",
        "main",
        Some("default-node"),
        "codex",
    )
    .unwrap();
    assert_eq!(node.worktree_path.as_deref(), Some(path.as_str()));
    assert_eq!(env::node_git_path(&node).raw_path, path);
    assert!(!root.path().join("custom-wt/default-node").exists());
}

#[test]
fn import_preserves_existing_legacy_checkout_after_default_changes() {
    let root = TempDir::new();
    let repo = init_repo(root.path());
    let legacy_path = add_worktree(&repo, &root, "legacy-node");
    let _db = crate::db::test_support::isolated();
    let mesh = db::create_mesh("legacy-import", root.path().to_str().unwrap()).unwrap();
    let node = import_discovered_session(
        mesh.id,
        "11111111-1111-1111-1111-111111111111",
        "legacy-node",
        Some("legacy-node"),
        "codex",
    )
    .unwrap();
    let expected = env::resolve_agent_path(&mesh.path, Some("legacy-node"));
    assert_eq!(
        node.worktree_path.as_deref(),
        Some(expected.raw_path.as_str())
    );
    assert_eq!(env::node_git_path(&node).raw_path, expected.raw_path);
    assert!(legacy_path.join("file.txt").is_file());
    assert!(!root.path().join(".worktrees/legacy-node").exists());
}

#[test]
fn import_with_worktree_persists_the_mesh_path() {
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();
    db::set_mesh_worktree_directory(mesh_id, Some("wt")).unwrap();
    let mesh = db::get_mesh_by_id(mesh_id).unwrap();
    let session_id = "11111111-1111-1111-1111-111111111111";
    let node = import_discovered_session(mesh_id, session_id, "feature", Some("feature"), "codex")
        .expect("import with a worktree name");
    assert!(node.use_worktree);
    assert_eq!(node.worktree_name.as_deref(), Some("feature"));
    let expected_path = format!("{}/wt/feature", mesh.path);
    assert_eq!(node.worktree_path.as_deref(), Some(expected_path.as_str()));
    assert_eq!(node.path, mesh.path);
    assert_eq!(node.cli_session_id.as_deref(), Some(session_id));
    assert_eq!(node.provider, "codex");
}

#[test]
fn import_without_worktree_is_a_root_node_on_the_mesh_path() {
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();
    let mesh = db::get_mesh_by_id(mesh_id).unwrap();
    let session_id = "22222222-2222-2222-2222-222222222222";
    let node = import_discovered_session(mesh_id, session_id, "main", None, "anthropic")
        .expect("import without a worktree");
    assert!(!node.use_worktree);
    assert_eq!(node.worktree_name, None);
    assert_eq!(node.worktree_path, None);
    assert_eq!(node.path, mesh.path);
    assert_eq!(node.provider, "anthropic");
    assert_eq!(node.cli_session_id.as_deref(), Some(session_id));
}

#[test]
fn blank_worktree_name_on_create_stores_no_path() {
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();
    let node = create_blocking(
        mesh_id,
        Some("anthropic"),
        Some("main"),
        None,
        Some("   "),
        Some(true),
        false,
        None,
    )
    .expect("a whitespace name still creates");
    assert_eq!(node.worktree_name.as_deref(), Some("   "));
    assert_eq!(node.worktree_path, None);
    assert!(node.use_worktree);
}

#[test]
fn import_of_an_empty_worktree_name_keeps_the_flag_and_no_path() {
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();
    let node = import_discovered_session(
        mesh_id,
        "33333333-3333-3333-3333-333333333333",
        "main",
        Some(""),
        "cursor",
    )
    .expect("an empty discovered name is still a worktree flag");
    assert!(node.use_worktree);
    assert_eq!(node.worktree_name.as_deref(), Some(""));
    assert_eq!(node.worktree_path, None);
}

#[test]
fn import_of_an_unknown_mesh_is_mesh_not_found() {
    let _db = crate::db::test_support::isolated();
    let err = import_discovered_session(
        -1,
        "11111111-1111-1111-1111-111111111111",
        "main",
        None,
        "anthropic",
    )
    .expect_err("unknown mesh");
    assert!(matches!(err, AgentNodeError::MeshNotFound(-1)));
}

// -----------------------------------------------------------------------
// Harness provenance (issue #2332) — the archive entry's originating
// harness is the executor, and an unknown one is refused rather than
// guessed. These pin the whole import boundary: the stored row, the
// executor the spawn pipeline will resolve from it, and the argv that
// executor composes.
// -----------------------------------------------------------------------

/// The regression this issue filed: a Cursor conversation discovered on
/// disk used to be imported with no provider, defaulted to `anthropic`,
/// and launched through Claude Code. The stored row must carry the
/// originating harness, and the spawn pipeline must resolve it.
#[test]
fn import_stores_the_originating_harness_not_anthropic() {
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();
    let session_id = "44444444-4444-4444-4444-444444444444";
    let node = import_discovered_session(mesh_id, session_id, "main", None, "cursor")
        .expect("import a Cursor conversation");
    assert_eq!(node.provider, "cursor");
    assert_eq!(node.cli_session_id.as_deref(), Some(session_id));

    let provider = crate::agent::provider::resolve_node_provider(&node);
    assert_eq!(provider, crate::models::Provider::Cursor);
    assert_eq!(
        provider.adapter().resume_args(session_id),
        vec!["--resume".to_string(), session_id.to_string()]
    );
}

/// Antigravity is the case where guessing does not even fail cleanly: its
/// resume flag is `--conversation`, so a wrong executor's argv has no way
/// to name the conversation at all.
#[test]
fn import_stores_agy_so_the_conversation_flag_is_composed() {
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();
    let session_id = "55555555-5555-5555-5555-555555555555";
    let node = import_discovered_session(mesh_id, session_id, "main", None, "agy")
        .expect("import an Antigravity conversation");
    let provider = crate::agent::provider::resolve_node_provider(&node);
    assert_eq!(provider, crate::models::Provider::Agy);
    assert_eq!(
        provider.adapter().resume_args(session_id),
        vec!["--conversation".to_string(), session_id.to_string()]
    );
}

/// A Proxied Provider row (`claude:minimax`) is not an adapter id, but it
/// is what the desktop picker submits and what the row must keep storing
/// verbatim. Validation follows it to the Claude executor.
#[test]
fn import_accepts_a_proxied_spawn_option_id_verbatim() {
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();
    let node = import_discovered_session(
        mesh_id,
        "66666666-6666-6666-6666-666666666666",
        "main",
        None,
        "claude:minimax",
    )
    .expect("a proxied Claude profile still resumes");
    assert_eq!(node.provider, "claude:minimax");
    assert_eq!(
        crate::agent::provider::resolve_node_provider(&node),
        crate::models::Provider::Anthropic
    );
}

/// No provenance at all is the mobile client's old request shape. It must
/// be refused with the actionable message and leave no row behind — the
/// old `unwrap_or("anthropic")` created one.
#[test]
fn import_without_provenance_is_refused_and_creates_no_node() {
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();
    let err = import_discovered_session(
        mesh_id,
        "77777777-7777-7777-7777-777777777777",
        "main",
        None,
        "",
    )
    .expect_err("absent provenance is not a license to guess Anthropic");
    assert!(matches!(err, AgentNodeError::UnresumableHarness(_)));
    assert_eq!(err.to_string(), UNRESUMABLE_HARNESS);
    assert!(
        db::list_agent_nodes_by_mesh(mesh_id).unwrap().is_empty(),
        "a refused import must not leave a node row behind"
    );
}

/// A scanner this build cannot name is the same refusal: Buildmesh knows
/// the conversation exists but not which CLI wrote it.
#[test]
fn import_of_an_unknown_harness_is_refused() {
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();
    let err = import_discovered_session(
        mesh_id,
        "88888888-8888-8888-8888-888888888888",
        "main",
        None,
        "harness-from-the-future",
    )
    .expect_err("an unknown harness cannot be resumed");
    assert!(matches!(err, AgentNodeError::UnresumableHarness(_)));
    assert!(
        db::list_agent_nodes_by_mesh(mesh_id).unwrap().is_empty(),
        "an unknown harness must not become an Anthropic node"
    );
}

/// A known harness that cannot resume (Terminal, DSH) is refused by name
/// instead of being accepted and quietly producing an unusable node.
#[test]
fn import_of_a_non_resumable_harness_is_refused_by_name() {
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();
    let err = import_discovered_session(
        mesh_id,
        "99999999-9999-9999-9999-999999999999",
        "main",
        None,
        "terminal",
    )
    .expect_err("Terminal cannot resume a conversation");
    assert_eq!(
        err.to_string(),
        "Terminal cannot resume an archived conversation."
    );
}

/// `resolve_resumable_harness` is what the HTTP route consults before it
/// picks a session-id gate, so it has to hand back the typed executor and
/// refuse the same cases the service does.
#[test]
fn resolve_resumable_harness_returns_the_executor_and_refuses_the_rest() {
    assert_eq!(
        resolve_resumable_harness("commandcode").unwrap(),
        crate::models::Provider::CommandCode
    );
    // The Harness Profile alias `claude` and the adapter id `anthropic`
    // are the same executor.
    assert_eq!(
        resolve_resumable_harness("claude").unwrap(),
        crate::models::Provider::Anthropic
    );
    assert!(resolve_resumable_harness("").is_err());
    assert!(resolve_resumable_harness("   ").is_err());
    assert!(resolve_resumable_harness("nope").is_err());
    assert!(resolve_resumable_harness("dsh").is_err());
}

// -----------------------------------------------------------------------
// Regenerate (issue #775) — validation + resume-decision surface.
//
// The three pure helpers (`validate_status_eligible`,
// `should_skip_kill_for_regenerate`, `decide_resume`) own the new
// logic. The async `regenerate` orchestrator is exercised end-to-end
// via the Playwright e2e suite and the manual smoke test, so the
// unit tests here stay focused on the rules the orchestrator
// composes.
// -----------------------------------------------------------------------

#[test]
fn validate_status_eligible_allows_running_idle_awaiting_error_suspended_completed() {
    // The six states the regenerate orchestrator accepts:
    //   * Running / Idle / AwaitingInput / Error — "process is or
    //     was running", so kill + respawn makes sense.
    //   * Suspended — no live process to kill; the orchestrator
    //     branches on `should_skip_kill_for_regenerate` to avoid
    //     the unconditional `on_idle` tail of `kill_agent`
    //     (commands/agent.rs:1172) clobbering Suspended→Idle
    //     before the new spawn lands. The frontend gates the
    //     user-driven Resume affordance on `cli_session_id` non-
    //     empty to keep the autopilot-gate case (Suspended +
    //     NULL session id) from surfacing a confusing toast.
    //   * Completed — autopilot-finished wrap-up (#485). The agent
    //     PTY is still alive at the moment of `complete_autopilot_run`
    //     (knowledge claim, not unit-tested: see
    //     circuit::verification::complete_autopilot_run — the function
    //     never calls `kill_agent`), so we route through the
    //     kill+respawn path. `decide_resume` reuses the captured
    //     `cli_session_id` so a same-harness Regenerate picks up
    //     exactly where autopilot left off — which is the "I want
    //     to keep interacting with this session" affordance that
    //     #774 ticket 04 framed.
    for status in [
        SessionStatus::Running,
        SessionStatus::Idle,
        SessionStatus::AwaitingInput,
        SessionStatus::Error,
        SessionStatus::Suspended,
        SessionStatus::Completed,
    ] {
        assert!(
            validate_status_eligible(status).is_ok(),
            "expected {status:?} to be eligible, got: {:?}",
            validate_status_eligible(status),
        );
    }
}

#[test]
fn validate_status_eligible_rejects_spawning_pending_archived() {
    // The three "no process to swap" states — reject at the
    // boundary. `Suspended` moved to the allowed list (see the
    // matching positive test above) when user-driven recovery for
    // Suspended nodes shipped.
    for status in [
        SessionStatus::Spawning,
        SessionStatus::Pending,
        SessionStatus::Archived,
    ] {
        assert!(
            validate_status_eligible(status).is_err(),
            "expected {status:?} to be rejected",
        );
    }
}

#[test]
fn validate_status_eligible_message_includes_status() {
    // The error message must name the rejected state so the frontend
    // toast (or a dev grepping `buildmesh.log`) can tell *why* the
    // regenerate was refused without a stack trace.
    let err = validate_status_eligible(SessionStatus::Spawning).unwrap_err();
    assert!(
        err.contains("spawning"),
        "error must mention the rejected status, got: {err}",
    );
}

#[test]
fn should_skip_kill_for_regenerate_only_true_for_suspended() {
    // The kill-skip branch is the load-bearing pairing with
    // `validate_status_eligible` accepting Suspended — without it,
    // the unconditional `on_idle` tail of `kill_agent_blocking`
    // would clobber Suspended→Idle before the new spawn lands.
    assert!(
        should_skip_kill_for_regenerate(SessionStatus::Suspended),
        "Suspended must skip the kill (no live process to terminate)"
    );
    for status in [
        SessionStatus::Idle,
        SessionStatus::AwaitingInput,
        SessionStatus::Error,
        SessionStatus::Running,
        // Completed — autopilot wrap-up flipped the status, but the
        // agent PTY is still alive (Claude Code sits at the prompt
        // after `gh pr create`). Regenerate must kill it so
        // `spawn_agent_inner`'s `is_agent_already_running` short-
        // circuit doesn't return early at spawn.rs:743.
        SessionStatus::Completed,
    ] {
        assert!(
            !should_skip_kill_for_regenerate(status),
            "{status:?} must NOT skip the kill (live or recently-live process)",
        );
    }
    // Sanity: Spawning / Pending / Archived never reach the
    // orchestrator's step 3 (validate_status_eligible rejects
    // them), so the skip rule is intentionally undefined for them.
    // The helper still returns a deterministic value — pinned here
    // so a future refactor doesn't accidentally expand the skip
    // set without a test witness.
    assert!(!should_skip_kill_for_regenerate(SessionStatus::Spawning));
    assert!(!should_skip_kill_for_regenerate(SessionStatus::Pending));
    assert!(!should_skip_kill_for_regenerate(SessionStatus::Archived));
}

#[test]
fn decide_resume_continues_same_harness_with_session_id() {
    // Same harness, supports resume, has session id → continue.
    assert_eq!(
        decide_resume("claude:minimax", "claude:openrouter", Some("uuid-abc")),
        Some("uuid-abc".to_string()),
    );
    // Native → proxied within the same harness is also a continue.
    assert_eq!(
        decide_resume("claude", "claude:minimax", Some("uuid-abc")),
        Some("uuid-abc".to_string()),
    );
}

#[test]
fn decide_resume_starts_fresh_when_no_session_id() {
    // Same harness, supports resume, but no captured session id
    // (e.g. the agent was killed before the reader caught one).
    assert_eq!(
        decide_resume("claude:minimax", "claude:openrouter", None),
        None,
    );
}

#[test]
fn decide_resume_starts_fresh_on_cross_harness() {
    // The load-bearing case: a Claude → Codex swap with a Claude
    // session id must NOT resume — the UUID isn't a valid Codex
    // session id. Without this, the new agent would crash on
    // startup with "Conversation not found" and the user would
    // lose the work.
    assert_eq!(
        decide_resume("claude:minimax", "codex", Some("uuid-abc")),
        None,
        "cross-harness swaps must not pass the old session id to the new harness",
    );
    // And the reverse: Codex → Claude with a Codex id also fresh.
    assert_eq!(
        decide_resume("codex", "claude:minimax", Some("codex-session-xyz")),
        None,
    );
}

#[test]
fn decide_resume_starts_fresh_when_new_provider_lacks_resume() {
    // Terminal has no session identity. Even a same-harness
    // Terminal → Terminal swap is fresh.
    assert_eq!(
        decide_resume("terminal", "terminal", Some("oc-session")),
        None,
        "a same-harness swap to a non-resumable adapter must start fresh",
    );
}

#[test]
fn decide_resume_continues_opencode_with_captured_ses_id() {
    assert_eq!(
        decide_resume(
            "opencode",
            "opencode",
            Some("ses_fc52ccfb9ffek1jl23ZwpRuSP7"),
        ),
        Some("ses_fc52ccfb9ffek1jl23ZwpRuSP7".to_string()),
        "OpenCode now supports --session resume; a captured ses_ id must continue",
    );
}

#[test]
fn set_agent_node_provider_persists_and_round_trips() {
    // Issue #775 — the Regenerate command mutates `agent_nodes.provider`
    // to swap the Model Provider on respawn. Verify the column write
    // persists and reads back the new value, and that a re-write is
    // idempotent (the function comment claims it as a property).
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();
    let node = create(
        mesh_id,
        "/tmp/buildmesh_provider_test",
        "main",
        Some("anthropic"),
        None,
        None,
        None,
        None,
        None,
    )
    .expect("seed node should succeed");
    assert_eq!(node.provider, "anthropic");

    db::set_agent_node_provider(node.id, "claude:minimax")
        .expect("set_agent_node_provider should succeed");
    let reloaded = db::get_agent_node_by_id(node.id).expect("get_agent_node_by_id should succeed");
    assert_eq!(
        reloaded.provider, "claude:minimax",
        "provider column must round-trip through the UPDATE",
    );

    // Idempotent — a no-op write should still succeed.
    db::set_agent_node_provider(node.id, "claude:minimax")
        .expect("idempotent write should succeed");
}

#[test]
fn close_phase1_then_phase2_is_idempotent_for_both_worktree_modes() {
    // Regression: a Circuit step's `CloseAgentNode` (or any backend-driven
    // delete) can remove a node's row while the frontend still holds it as a
    // tab. The real close is Phase 1 `get_worktree_close_safety` then Phase 2
    // `delete`; both used to fault with `QueryReturnedNoRows` on the retry,
    // aborting the close and leaving the stale card on screen behind an
    // error toast. Exercise that exact order for both `remove_worktree`
    // values, then repeat it against the now-missing row.
    for remove_worktree in [false, true] {
        let _db = crate::db::test_support::isolated();
        let mesh_id = fresh_mesh();
        let node = create(
            mesh_id,
            &format!("/tmp/buildmesh_close_idempotent_{remove_worktree}"),
            "main",
            Some("anthropic"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("seed node should succeed");

        let state_owner = crate::agent::hook_state::for_node(node.id);
        state_owner.lock().question(
            "pending",
            crate::agent::hook_state::QuestionKind::Foreground,
        );

        // Real close order: Phase 1 (safety) precedes Phase 2 (delete).
        get_worktree_close_safety(node.id).expect("Phase 1 safety on a live row must succeed");
        delete(node.id, remove_worktree).expect("Phase 2 close must succeed");
        let replacement_state = crate::agent::hook_state::for_node(node.id);
        assert!(
            !std::sync::Arc::ptr_eq(&state_owner, &replacement_state),
            "deleting a node must release its process-lifetime HookState entry",
        );
        crate::agent::node_teardown::release(node.id);
        assert!(
            matches!(
                db::get_agent_node_by_id(node.id),
                Err(rusqlite::Error::QueryReturnedNoRows),
            ),
            "the row must be gone after close (remove_worktree={remove_worktree})",
        );

        // The ghost retry: both phases must tolerate the missing row.
        let safety =
            get_worktree_close_safety(node.id).expect("Phase 1 must tolerate a missing row");
        assert_eq!(safety.worktree_path, None);
        assert!(!safety.has_uncommitted && !safety.has_unpushed && !safety.is_detached);
        delete(node.id, remove_worktree)
            .expect("Phase 2 must tolerate a missing row (remove_worktree=true included)");
    }
}

/// A close retires the node's PTY fanout channel, and it does so *after* the
/// row delete commits — because the retirement records a tombstone that
/// refuses future channel creation, so doing it while the row could still
/// survive an error would strand a live node with no way to stream again
/// (#2019).
#[test]
fn closing_a_node_retires_its_channel_only_once_the_row_is_gone() {
    let _db = crate::db::test_support::isolated();
    // An id no other test can reach: every isolated database numbers its
    // rows from 1, and `delete` now records a tombstone for the id it
    // deletes — so a database-assigned id here could be fenced by an
    // unrelated test that deletes its own node 1.
    const NODE_ID: i64 = 9_500_101;
    let mesh_id = fresh_mesh();
    {
        let conn = db::write_conn();
        conn.execute(
            "INSERT INTO agent_nodes (id, mesh_id, name, path, status) \
                 VALUES (?1, ?2, 'close-retires', '/tmp/buildmesh_close_retires', 'idle')",
            (NODE_ID, mesh_id),
        )
        .expect("seed node with an explicit id should succeed");
    }
    let node = crate::db::get_agent_node_by_id(NODE_ID).expect("the seeded node should exist");

    // A live node must keep a working channel.
    crate::http::ws::ensure_pty_channel(node.id);
    let mut before_close = crate::http::ws::subscribe_pty(node.id);
    crate::http::ws::send_pty_output(node.id, b"before close");
    assert_eq!(before_close.try_recv().unwrap().as_ref(), b"before close");

    delete(node.id, false).expect("close should succeed");

    assert!(
        matches!(
            db::get_agent_node_by_id(node.id),
            Err(rusqlite::Error::QueryReturnedNoRows)
        ),
        "the row must be gone before the channel is retired"
    );
    // The retirement took effect: the fence refuses a new channel, and the
    // old receiver saw the close.
    crate::http::ws::ensure_pty_channel(node.id);
    assert!(
        matches!(
            crate::http::ws::subscribe_pty(node.id).try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Closed)
        ),
        "a deleted node must not get a channel back"
    );
    assert!(
        before_close.try_recv().is_err(),
        "the live receiver must be closed by the retirement"
    );
}

// PR #1388 review feedback 2 — the service-layer `regenerate`
// used to orchestrate three interleaved `spawn_blocking` hops and
// a chain of pre-clones (`new_provider_for_update`,
// `old_provider_for_update`) to satisfy the `'static` borrow on
// each closure. The refactor splits the work into three single-
// purpose sync helpers that the command boundary wraps once each,
// threading owned data via return tuples instead of captured clones.

#[test]
fn regenerate_load_blocking_returns_old_provider_and_skip_kill() {
    // Idle node: provider is captured verbatim, kill is NOT skipped.
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();
    let idle_node = create(
        mesh_id,
        "/tmp/buildmesh_regen_load_idle",
        "main",
        Some("anthropic"),
        None,
        None,
        None,
        None,
        None,
    )
    .expect("seed idle node");
    let (old_provider, skip_kill) =
        regenerate_load_blocking(idle_node.id).expect("load should succeed");
    assert_eq!(old_provider, "anthropic");
    assert!(
        !skip_kill,
        "Idle has no live process to skip, but the skip rule is about Suspended only",
    );

    // Suspended node: skip_kill must be true (this is the whole
    // point of the helper — `kill_agent_blocking`'s `on_idle`
    // tail would clobber Suspended→Idle if we killed).
    let suspended_node = create(
        mesh_id,
        "/tmp/buildmesh_regen_load_suspended",
        "main",
        Some("anthropic"),
        None,
        None,
        None,
        None,
        None,
    )
    .expect("seed suspended node");
    db::update_agent_node_status(suspended_node.id, SessionStatus::Suspended)
        .expect("set suspended status");
    let (suspended_provider, suspended_skip_kill) =
        regenerate_load_blocking(suspended_node.id).expect("load should succeed");
    assert_eq!(suspended_provider, "anthropic");
    assert!(suspended_skip_kill, "Suspended must skip the kill");
}

#[test]
fn regenerate_load_blocking_rejects_spawning_status() {
    // The status guard from `validate_status_eligible` must surface
    // as an error so the command boundary can map it to the user's
    // "regenerate unavailable: node is in X state" toast.
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();
    let node = create(
        mesh_id,
        "/tmp/buildmesh_regen_load_spawning",
        "main",
        Some("anthropic"),
        None,
        None,
        None,
        None,
        None,
    )
    .expect("seed node");
    db::update_agent_node_status(node.id, SessionStatus::Spawning).expect("set spawning status");
    let err = regenerate_load_blocking(node.id).unwrap_err();
    match err {
        AgentNodeError::Status(msg) => assert!(
            msg.contains("spawning"),
            "error must name the rejected status, got: {msg}"
        ),
        other => panic!("expected AgentNodeError::Status, got {other:?}"),
    }
}

#[test]
fn regenerate_apply_blocking_writes_provider_and_decides_resume() {
    let prefs_dir = tempfile::tempdir().unwrap();
    crate::preferences::init_for_tests(prefs_dir.path().into());
    crate::preferences::save(serde_json::from_value(serde_json::json!({
            "provider_accounts": [{"id":"minimax","name":"MiniMax","enabled":true,"billing_mode":"pay_as_you_go","api_key":"test-key"}],
            "provider_pairings": [{"harness_id":"claude","provider_id":"minimax","surface":"anthropic","base_url":"https://api.minimax.io/anthropic","model_tiers":{"default":"MiniMax-M3[1m]"}}]
        })).unwrap()).unwrap();
    // Same-harness provider swap with a captured cli_session_id:
    // apply_blocking must write the new provider AND report
    // `resume = true` (because `decide_resume` continues the
    // session for same-harness swaps).
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();
    let node = create(
        mesh_id,
        "/tmp/buildmesh_regen_apply",
        "main",
        Some("claude"),
        None,
        None,
        None,
        None,
        None,
    )
    .expect("seed node");
    db::update_cli_session_id(node.id, "sess-abc").expect("set cli_session_id");

    let resume = regenerate_apply_blocking(node.id, "claude", "claude:minimax")
        .expect("apply should succeed");
    assert!(resume, "same-harness swap with a session id must resume");

    let reloaded = db::get_agent_node_by_id(node.id).expect("reload");
    assert_eq!(
        reloaded.provider, "claude:minimax",
        "apply_blocking must persist the new provider before returning"
    );
    assert_eq!(
        reloaded.cli_session_id.as_deref(),
        Some("sess-abc"),
        "apply_blocking must NOT clear cli_session_id — the spawner owns that policy",
    );
}

#[test]
fn regenerate_apply_blocking_starts_fresh_on_cross_harness() {
    let prefs_dir = tempfile::tempdir().unwrap();
    crate::preferences::init_for_tests(prefs_dir.path().into());
    // Claude → Codex: the captured Claude session id is not a valid
    // Codex id, so the apply step must report `resume = false`.
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();
    let node = create(
        mesh_id,
        "/tmp/buildmesh_regen_apply_cross",
        "main",
        Some("claude"),
        None,
        None,
        None,
        None,
        None,
    )
    .expect("seed node");
    db::update_cli_session_id(node.id, "claude-uuid").expect("set cli_session_id");

    let resume = regenerate_apply_blocking(node.id, "claude:anthropic", "codex")
        .expect("apply should succeed");
    assert!(
        !resume,
        "cross-harness swap must report resume=false so spawn picks Fresh intent"
    );
}

#[test]
fn regenerate_reload_blocking_returns_fresh_node() {
    // The reload helper is the final hop in the orchestrator — it
    // must return the current row so the command can hand it back
    // to the frontend store.
    let _db = crate::db::test_support::isolated();
    let mesh_id = fresh_mesh();
    let node = create(
        mesh_id,
        "/tmp/buildmesh_regen_reload",
        "main",
        Some("anthropic"),
        None,
        None,
        None,
        None,
        None,
    )
    .expect("seed node");
    let reloaded = regenerate_reload_blocking(node.id).expect("reload should succeed");
    assert_eq!(reloaded.id, node.id);
    assert_eq!(reloaded.provider, "anthropic");
}
