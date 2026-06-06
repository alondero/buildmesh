//! Tests for mesh health detection and recovery (issue #231).
//!
//! These build real temporary git repositories with known branch / worktree /
//! upstream state and verify that `compute_mesh_health`, `find_base_branch_holder`,
//! `restore_to_base_impl`, and `free_base_branch_impl` produce the right
//! answers and refuse unsafe operations. The Tauri command wrappers
//! (`get_mesh_health`, `restore_mesh_to_base`, `free_base_branch`) are tested
//! by being registered in `lib.rs::invoke_handler!` (a missing entry is a
//! runtime "command not found" — see the `tauri-command-registration` memory).
//!
//! Run with: `cd src-tauri && cargo test mesh_health_tests`

use super::*;
use git2::{Repository, WorktreeAddOptions};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
// allow-warnings: items appear once MeshHealth backend is implemented in git.rs.

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Unique temp directory with RAII cleanup on drop. Tracks any sibling
/// "outer" directories it created (for worktrees that must live outside
/// the root to keep the root's working tree clean for the dirty/unpushed
/// guards) and cleans them up too.
struct TempDir {
    root: PathBuf,
    outer: Vec<PathBuf>,
}

impl TempDir {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let tmp = std::env::temp_dir().join(format!("buildmesh_mesh_health_test_{}_{}", pid, id));
        Self { root: tmp, outer: Vec::new() }
    }
    fn path(&self) -> &Path {
        &self.root
    }
    /// Allocate a sibling directory outside `self.root` (so the root's
    /// working tree stays clean) and track it for cleanup.
    fn outer(&mut self, name: &str) -> PathBuf {
        let parent = self.root.parent().unwrap_or(Path::new("."));
        let pid = std::process::id();
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let p = parent.join(format!(
            "buildmesh_mesh_health_wt_{}_{}_{}",
            pid, id, name
        ));
        self.outer.push(p.clone());
        p
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
        for p in &self.outer {
            let _ = fs::remove_dir_all(p);
        }
    }
}

fn sig() -> git2::Signature<'static> {
    git2::Signature::now("test", "test@example.com").unwrap()
}

/// Init a repo with an initial commit on `main` (forces `main` as the
/// default branch, no stray `master`).
fn init_repo(path: &Path) -> Repository {
    fs::create_dir_all(path).unwrap();
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("main");
    let repo = Repository::init_opts(path, &opts).unwrap();
    {
        let s = sig();
        fs::write(path.join("file.txt"), "initial").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        repo.commit(Some("HEAD"), &s, &s, "initial commit", &tree, &[])
            .unwrap();
    }
    repo
}

/// Init a repo with an initial commit on `master` and NO `main` branch —
/// models a repo whose trunk predates the `main` rename. The buildmesh
/// default `base_ref` of `origin/main` does not resolve in such a repo.
fn init_repo_on_master(path: &Path) -> Repository {
    fs::create_dir_all(path).unwrap();
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("master");
    let repo = Repository::init_opts(path, &opts).unwrap();
    {
        let s = sig();
        fs::write(path.join("file.txt"), "initial").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        repo.commit(Some("HEAD"), &s, &s, "initial commit", &tree, &[])
            .unwrap();
    }
    repo
}

/// Add a commit on the currently checked-out HEAD. Returns the new OID.
fn commit_file(repo: &Repository, name: &str, content: &str) -> git2::Oid {
    let workdir = repo.workdir().unwrap();
    fs::write(workdir.join(name), content).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new(name)).unwrap();
    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let s = sig();
    let tree = repo.find_tree(tree_oid).unwrap();
    repo.commit(Some("HEAD"), &s, &s, "commit", &tree, &[&parent])
        .unwrap()
}

/// Make a new local branch pointing at HEAD without checking it out.
fn create_branch_at_head(repo: &Repository, name: &str) {
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch(name, &head, false).unwrap();
}

/// Check out an existing branch (the working tree must be clean).
fn checkout_branch(repo: &Repository, name: &str) {
    let tree = repo.head().unwrap().peel_to_commit().unwrap().tree().unwrap();
    let tree_obj = tree.into_object();
    repo.checkout_tree(&tree_obj, None).unwrap();
    repo.set_head(&format!("refs/heads/{}", name)).unwrap();
}

/// Set the configured upstream of `branch` to a specific remote-tracking ref
/// (`refs/remotes/origin/main`) regardless of the local branch tip.
fn set_branch_upstream(repo: &Repository, branch: &str, remote_branch_ref: &str) {
    let cfg_remote = format!("branch.{}.remote", branch);
    let cfg_merge = format!("branch.{}.merge", branch);
    let mut cfg = repo.config().unwrap();
    cfg.set_str(&cfg_remote, "origin").unwrap();
    cfg.set_str(&cfg_merge, remote_branch_ref).unwrap();
}

/// Move the main worktree off `main` so a linked worktree can check it out.
/// (git only allows one worktree to have a given branch checked out at a time.)
fn move_root_off_main(repo: &Repository) {
    create_branch_at_head(repo, "feat/root");
    checkout_branch(repo, "feat/root");
}

/// Add a linked worktree at `wt_path` checking out `branch`. The branch
/// must already exist in the repo AND must not be checked out in the main
/// worktree — callers should run `move_root_off_main` first. The parent
/// directory of `wt_path` must exist (git2's worktree() creates the leaf
/// directory but not the chain above it).
fn add_worktree_on_branch(repo: &Repository, wt_path: &Path, branch: &str) {
    if let Some(parent) = wt_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut opts = WorktreeAddOptions::new();
    opts.checkout_existing(true);
    let _branch_ref = repo
        .find_branch(branch, git2::BranchType::Local)
        .unwrap()
        .into_reference();
    repo.worktree(branch, wt_path, Some(&opts)).unwrap();
}

// ── parse_local_branch ──────────────────────────────────────────────────────

#[test]
fn parse_local_branch_accepts_origin_slash_main() {
    assert_eq!(parse_local_branch("origin/main"), Some("main".to_string()));
}

#[test]
fn parse_local_branch_accepts_bare_main() {
    assert_eq!(parse_local_branch("main"), Some("main".to_string()));
}

#[test]
fn parse_local_branch_accepts_refs_heads_form() {
    assert_eq!(
        parse_local_branch("refs/heads/main"),
        Some("main".to_string())
    );
}

#[test]
fn parse_local_branch_accepts_master_and_develop() {
    assert_eq!(parse_local_branch("origin/master"), Some("master".to_string()));
    assert_eq!(parse_local_branch("origin/develop"), Some("develop".to_string()));
}

#[test]
fn parse_local_branch_keeps_nested_branch_after_remote() {
    // The remote is one segment; the branch is everything after.
    // "origin/feature/auth" → "feature/auth" — NOT "auth" (which would
    // be the bug from the old `rsplit('/').next()` implementation).
    assert_eq!(
        parse_local_branch("origin/feature/auth"),
        Some("feature/auth".to_string())
    );
    assert_eq!(
        parse_local_branch("origin/release/v1.0"),
        Some("release/v1.0".to_string())
    );
}

#[test]
fn parse_local_branch_keeps_nested_branch_with_full_ref_form() {
    // The full `refs/heads/...` and `refs/remotes/<remote>/...` forms
    // disambiguate nested branches — no heuristic guessing.
    assert_eq!(
        parse_local_branch("refs/heads/feature/auth"),
        Some("feature/auth".to_string())
    );
    assert_eq!(
        parse_local_branch("refs/remotes/origin/feature/auth"),
        Some("feature/auth".to_string())
    );
}

#[test]
fn parse_local_branch_rejects_head() {
    assert_eq!(parse_local_branch("HEAD"), None);
    assert_eq!(parse_local_branch("origin/HEAD"), None);
}

#[test]
fn parse_local_branch_rejects_empty() {
    assert_eq!(parse_local_branch(""), None);
}

#[test]
fn parse_local_branch_rejects_whitespace_only() {
    assert_eq!(parse_local_branch("   "), None);
}

// ── compute_mesh_health — drift detection ────────────────────────────────────

#[test]
fn drift_detected_when_root_on_feature_branch() {
    let td = TempDir::new();
    let repo = init_repo(td.path());
    // Make a feature branch and check it out.
    create_branch_at_head(&repo, "feat/x");
    checkout_branch(&repo, "feat/x");

    let health = compute_mesh_health(&repo, "origin/main").unwrap();
    assert_eq!(health.local_base_branch.as_deref(), Some("main"));
    assert_eq!(health.current_branch.as_deref(), Some("feat/x"));
    assert!(health.is_drifted, "feat/x is not main");
    assert!(!health.is_detached);
}

#[test]
fn drift_detected_when_detached_on_non_base_commit() {
    let td = TempDir::new();
    let repo = init_repo(td.path());
    // Create a feature branch with one commit past main, then detach at it.
    // The working tree will be in sync with the detached HEAD (no dirty).
    create_branch_at_head(&repo, "feat/x");
    checkout_branch(&repo, "feat/x");
    commit_file(&repo, "extra.txt", "extra\n");
    let head_oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    repo.set_head_detached(head_oid).unwrap();

    let health = compute_mesh_health(&repo, "origin/main").unwrap();
    assert!(health.is_detached);
    assert!(health.is_drifted, "detached on a non-base commit is drifted");
}

#[test]
fn drift_clear_when_root_on_base_branch() {
    let td = TempDir::new();
    let repo = init_repo(td.path());
    // Already on main from init_repo; sanity check
    let health = compute_mesh_health(&repo, "origin/main").unwrap();
    assert_eq!(health.current_branch.as_deref(), Some("main"));
    assert!(!health.is_drifted);
    assert!(!health.is_detached);
}

#[test]
fn drift_clear_when_detached_at_base_oid() {
    let td = TempDir::new();
    let repo = init_repo(td.path());
    // Detach HEAD at main's tip OID (same as the base branch's tip).
    let main_oid = repo
        .find_branch("main", git2::BranchType::Local)
        .unwrap()
        .get()
        .peel_to_commit()
        .unwrap()
        .id();
    repo.set_head_detached(main_oid).unwrap();

    let health = compute_mesh_health(&repo, "origin/main").unwrap();
    assert!(health.is_detached, "shorthand() of a detached HEAD is HEAD");
    assert!(
        !health.is_drifted,
        "detached at the base branch's OID is not drifted (close enough)"
    );
}

#[test]
fn is_drifted_false_when_base_ref_unresolvable() {
    // base_ref of "HEAD" yields local_base_branch = None → no drift
    let td = TempDir::new();
    let repo = init_repo(td.path());
    create_branch_at_head(&repo, "feat/x");
    checkout_branch(&repo, "feat/x");

    let health = compute_mesh_health(&repo, "HEAD").unwrap();
    assert_eq!(health.local_base_branch, None);
    assert!(
        !health.is_drifted,
        "no base configured → no drift condition to report"
    );
}

#[test]
fn drift_clear_when_base_branch_absent_from_repo() {
    // A repo whose trunk is `master` carrying the default `origin/main`
    // base_ref: there is no `main` branch (local or remote-tracking) to
    // drift from, so the root being on `master` must NOT be flagged as
    // drift. Regression for the false "Root on master, base is main"
    // badge (#265 follow-up).
    let td = TempDir::new();
    let repo = init_repo_on_master(td.path());

    let health = compute_mesh_health(&repo, "origin/main").unwrap();
    assert_eq!(health.current_branch.as_deref(), Some("master"));
    assert_eq!(health.local_base_branch.as_deref(), Some("main"));
    assert!(
        !health.is_drifted,
        "base branch `main` does not exist in this repo — cannot drift from a phantom"
    );
}

#[test]
fn drift_still_detected_when_base_branch_present_but_off_it() {
    // Guard against over-suppression: when the base branch DOES exist and
    // the root is on a different branch, drift is still reported.
    let td = TempDir::new();
    let repo = init_repo(td.path());
    create_branch_at_head(&repo, "feat/x");
    checkout_branch(&repo, "feat/x");

    let health = compute_mesh_health(&repo, "origin/main").unwrap();
    assert!(
        health.is_drifted,
        "local `main` exists and root is on feat/x → genuine drift"
    );
}

#[test]
fn is_dirty_reports_uncommitted_changes() {
    let td = TempDir::new();
    let repo = init_repo(td.path());
    fs::write(td.path().join("untracked.txt"), "scratch").unwrap();
    let health = compute_mesh_health(&repo, "origin/main").unwrap();
    assert!(health.is_dirty);
}

#[test]
fn is_dirty_false_on_clean_repo() {
    let td = TempDir::new();
    let repo = init_repo(td.path());
    let health = compute_mesh_health(&repo, "origin/main").unwrap();
    assert!(!health.is_dirty);
}

#[test]
fn unpushed_ahead_reported_for_local_commits() {
    let td = TempDir::new();
    let repo = init_repo(td.path());
    // Add the "origin" remote so `branch_upstream_name` can resolve
    // `branch.<name>.remote = origin` → `refs/remotes/origin/<name>`.
    repo.remote("origin", "https://example.com/test.git").unwrap();
    // Add a second commit on main, then point origin/main at the first commit.
    let first_oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    commit_file(&repo, "f.txt", "second\n");
    let second_oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    assert_ne!(first_oid, second_oid);
    repo.reference("refs/remotes/origin/main", first_oid, true, "test").unwrap();
    set_branch_upstream(&repo, "main", "refs/heads/main");

    let health = compute_mesh_health(&repo, "origin/main").unwrap();
    assert!(health.has_upstream, "branch.main has remote 'origin' configured");
    assert_eq!(
        health.unpushed_ahead, 1,
        "one commit on main is not on the remote-tracking ref"
    );
}

#[test]
fn unpushed_ahead_zero_on_branch_at_base_tip_with_no_upstream() {
    let td = TempDir::new();
    let repo = init_repo(td.path());
    // Fresh feature branch at the same commit as main, no upstream.
    create_branch_at_head(&repo, "feat/empty");
    checkout_branch(&repo, "feat/empty");

    let health = compute_mesh_health(&repo, "origin/main").unwrap();
    assert!(!health.has_upstream);
    assert_eq!(
        health.unpushed_ahead, 0,
        "a branch at the same tip as base has no local commits to lose"
    );
}

#[test]
fn unpushed_ahead_nonzero_for_no_upstream_branch_with_local_commits() {
    let td = TempDir::new();
    let repo = init_repo(td.path());
    // New feature branch with one local commit, no upstream.
    create_branch_at_head(&repo, "feat/with-commit");
    checkout_branch(&repo, "feat/with-commit");
    commit_file(&repo, "new.txt", "work\n");

    let health = compute_mesh_health(&repo, "origin/main").unwrap();
    assert!(!health.has_upstream);
    assert_eq!(
        health.unpushed_ahead, 1,
        "one local commit ahead of base, even without upstream"
    );
}

// ── find_base_branch_holder ─────────────────────────────────────────────────

#[test]
fn hostage_detected_when_linked_worktree_holds_base_branch() {
    let mut td = TempDir::new();
    let repo = init_repo(td.path());
    // Free up main by moving the root off it first.
    move_root_off_main(&repo);
    let wt_path = td.outer("wt-main");
    add_worktree_on_branch(&repo, &wt_path, "main");

    let holder = find_base_branch_holder(&repo, "main", &[]);
    assert!(holder.is_some(), "main is checked out in the linked worktree");
    let h = holder.unwrap();
    assert!(h.path.contains("wt-main"));
    assert!(!h.is_active);
}

#[test]
fn hostage_clear_when_root_holds_base_branch() {
    // The repo root (main worktree) sitting on the base branch is the
    // HEALTHY, desired state — never a hostage. A hostage is specifically a
    // *linked* worktree holding the base branch and blocking `git checkout`
    // from the root; the root holding its own branch blocks nothing.
    // Regression for the false "main held by <repo>" badge (#265 follow-up).
    let td = TempDir::new();
    let repo = init_repo(td.path());
    // init_repo leaves us on `main`, with no linked worktrees.
    let holder = find_base_branch_holder(&repo, "main", &[]);
    assert!(
        holder.is_none(),
        "the root being on the base branch is not a hostage"
    );
}

#[test]
fn hostage_clear_when_worktree_detached() {
    let mut td = TempDir::new();
    let repo = init_repo(td.path());
    move_root_off_main(&repo);
    let wt_path = td.outer("wt-detach");
    add_worktree_on_branch(&repo, &wt_path, "main");

    // Detach the worktree's HEAD at its current commit.
    let wt_repo = Repository::open(&wt_path).unwrap();
    let head_oid = wt_repo.head().unwrap().peel_to_commit().unwrap().id();
    wt_repo.set_head_detached(head_oid).unwrap();

    let holder = find_base_branch_holder(&repo, "main", &[]);
    assert!(holder.is_none(), "after detach, no worktree holds main");
}

#[test]
fn hostage_marks_active_when_path_matches_an_agent_node() {
    let mut td = TempDir::new();
    let repo = init_repo(td.path());
    move_root_off_main(&repo);
    let wt_path = td.outer("wt-active");
    add_worktree_on_branch(&repo, &wt_path, "main");

    // Pretend the wt is an active agent node by listing it in active_paths.
    let wt_str = wt_path.to_string_lossy().to_string();
    let holder = find_base_branch_holder(&repo, "main", std::slice::from_ref(&wt_str));
    assert!(holder.is_some());
    let h = holder.unwrap();
    assert!(h.is_active, "path matches an active node → is_active = true");
}

#[test]
fn hostage_is_none_when_no_worktree_holds_base_branch() {
    let td = TempDir::new();
    let repo = init_repo(td.path());
    // Move the root to a feature branch so no worktree holds main.
    create_branch_at_head(&repo, "feat/x");
    checkout_branch(&repo, "feat/x");
    let holder = find_base_branch_holder(&repo, "main", &[]);
    assert!(holder.is_none());
}

// ── restore_to_base_impl — recovery guards ──────────────────────────────────

#[test]
fn restore_succeeds_on_clean_root_with_no_unpushed() {
    let td = TempDir::new();
    let repo = init_repo(td.path());
    create_branch_at_head(&repo, "feat/clean");
    checkout_branch(&repo, "feat/clean");

    // Branch is at the same tip as main → no unpushed work to lose.
    let moved = restore_to_base_impl(&repo, "main").expect("should succeed");
    assert!(moved, "HEAD moved from feat/clean to main");

    // Verify HEAD is now on main.
    let head_branch = repo
        .head()
        .unwrap()
        .shorthand()
        .unwrap()
        .to_string();
    assert_eq!(head_branch, "main");
}

#[test]
fn restore_blocked_with_dirty_root() {
    let td = TempDir::new();
    let repo = init_repo(td.path());
    create_branch_at_head(&repo, "feat/dirty");
    checkout_branch(&repo, "feat/dirty");
    fs::write(td.path().join("scratch.txt"), "untracked").unwrap();

    let err = restore_to_base_impl(&repo, "main").unwrap_err();
    assert!(err.contains("uncommitted"), "error must name the guard: {}", err);
}

#[test]
fn restore_blocked_with_unpushed_commits() {
    let td = TempDir::new();
    let repo = init_repo(td.path());
    create_branch_at_head(&repo, "feat/wip");
    checkout_branch(&repo, "feat/wip");
    commit_file(&repo, "wip.txt", "work\n");

    let err = restore_to_base_impl(&repo, "main").unwrap_err();
    assert!(err.contains("unpushed") || err.contains("no upstream"),
            "error must name the unpushed guard: {}", err);
}

#[test]
fn restore_blocked_when_base_branch_hostage() {
    let mut td = TempDir::new();
    let repo = init_repo(td.path());
    // Free up main for the linked worktree.
    move_root_off_main(&repo);
    let wt_path = td.outer("wt-hostage");
    add_worktree_on_branch(&repo, &wt_path, "main");
    // Root is on a feature branch (different from feat/root).
    create_branch_at_head(&repo, "feat/x");
    checkout_branch(&repo, "feat/x");

    let err = restore_to_base_impl(&repo, "main").unwrap_err();
    assert!(err.contains("held by") || err.contains("hostage"),
            "error must name the hostage guard: {}", err);
}

#[test]
fn restore_blocked_when_detached_on_wrong_commit() {
    let td = TempDir::new();
    let repo = init_repo(td.path());
    // A detached HEAD on a non-base commit has the local commit B that isn't
    // reachable from main's tip A. The unpushed guard fires (1 commit ahead
    // of base), refusing the restore — the user must create a branch on B
    // before they can switch away.
    create_branch_at_head(&repo, "feat/x");
    checkout_branch(&repo, "feat/x");
    commit_file(&repo, "extra.txt", "extra\n");
    let head_oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    repo.set_head_detached(head_oid).unwrap();

    let err = restore_to_base_impl(&repo, "main").unwrap_err();
    assert!(err.contains("unpushed") || err.contains("no upstream"),
            "detached on a non-base commit is blocked by the unpushed guard: {}", err);
}

#[test]
fn restore_no_op_when_already_on_base() {
    let td = TempDir::new();
    let repo = init_repo(td.path());
    // Already on main; noop expected.
    let moved = restore_to_base_impl(&repo, "main").expect("noop success");
    assert!(!moved, "HEAD was already on main; nothing to move");
}

// ── free_base_branch_impl ───────────────────────────────────────────────────

#[test]
fn free_detaches_at_current_commit_preserves_files() {
    let mut td = TempDir::new();
    let repo = init_repo(td.path());
    move_root_off_main(&repo);
    let wt_path = td.outer("wt-free");
    add_worktree_on_branch(&repo, &wt_path, "main");
    // Add a file in the worktree.
    fs::write(wt_path.join("inside.txt"), "worktree content\n").unwrap();
    let wt_repo = Repository::open(&wt_path).unwrap();
    let mut index = wt_repo.index().unwrap();
    index.add_path(Path::new("inside.txt")).unwrap();
    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let parent = wt_repo.head().unwrap().peel_to_commit().unwrap();
    let s = sig();
    let tree = wt_repo.find_tree(tree_oid).unwrap();
    let commit_oid = wt_repo
        .commit(Some("HEAD"), &s, &s, "worktree commit", &tree, &[&parent])
        .unwrap();

    let result = free_base_branch_impl(wt_path.to_str().unwrap(), "main");
    let detached_sha = result.expect("free should succeed");

    // Reopen and verify the worktree is detached at the same commit, files intact.
    let wt_repo_after = Repository::open(&wt_path).unwrap();
    assert!(wt_repo_after.head_detached().unwrap_or(false),
            "worktree should now have a detached HEAD");
    let new_head_oid = wt_repo_after.head().unwrap().peel_to_commit().unwrap().id();
    assert_eq!(new_head_oid, commit_oid,
               "detached HEAD must point at the same commit");
    let content = fs::read_to_string(wt_path.join("inside.txt")).unwrap();
    assert_eq!(content, "worktree content\n",
               "free_base_branch must not touch working tree files");
    assert!(!detached_sha.is_empty(), "returns a short SHA");
}

#[test]
fn free_idempotent_on_already_detached_worktree() {
    let mut td = TempDir::new();
    let repo = init_repo(td.path());
    move_root_off_main(&repo);
    let wt_path = td.outer("wt-idem");
    add_worktree_on_branch(&repo, &wt_path, "main");

    // First call detaches.
    let first = free_base_branch_impl(wt_path.to_str().unwrap(), "main")
        .expect("first call succeeds");
    // Second call on the same path is a no-op (the worktree is already
    // detached, so it no longer holds `main`).
    let second = free_base_branch_impl(wt_path.to_str().unwrap(), "main")
        .expect("second call succeeds idempotently");
    assert_eq!(first, second, "idempotent: returns the same short SHA");
}

#[test]
fn free_refuses_when_worktree_does_not_hold_base_branch() {
    let mut td = TempDir::new();
    let repo = init_repo(td.path());
    let wt_path = td.outer("wt-feat");
    // Worktree on a feature branch — does NOT hold main.
    create_branch_at_head(&repo, "feat-y");
    add_worktree_on_branch(&repo, &wt_path, "feat-y");

    let err = free_base_branch_impl(wt_path.to_str().unwrap(), "main").unwrap_err();
    assert!(err.contains("does not hold") || err.contains("not the base"),
            "error must say the worktree is not the holder: {}", err);
}
