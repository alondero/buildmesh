//! Tests for branch & worktree prune operations.
//!
//! These build real temporary git repositories with known branch/worktree
//! state and verify the enumeration and deletion behaviour. Only external
//! behaviour (inputs → outputs) is asserted — no internal state inspection.
//!
//! Run with: cd src-tauri && cargo test prune

use super::*;
// `remove_one_worktree` moved to the git module (ADR 0007); these removal
// regression tests exercise it from here, alongside the worktree enumeration.
use crate::git::worktree::{remove_one_worktree, remove_one_worktree_and_branch};
use crate::models::AgentNode;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Unique temp directory with RAII cleanup on drop.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let tmp = std::env::temp_dir().join(format!("buildmesh_prune_test_{}_{}", pid, id));
        Self(tmp)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn path_str(&self) -> String {
        self.0.to_string_lossy().to_string()
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

/// Init a repo with an initial commit on `main` (and no stray default branch).
fn init_repo(path: &Path) -> git2::Repository {
    fs::create_dir_all(path).unwrap();
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("main");
    let repo = git2::Repository::init_opts(path, &opts).unwrap();
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

/// Add a commit on the currently checked-out HEAD.
fn commit_file(repo: &git2::Repository, name: &str, content: &str) -> git2::Oid {
    let workdir = repo.workdir().unwrap();
    fs::write(workdir.join(name), content).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new(name)).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let s = sig();
    repo.commit(Some("HEAD"), &s, &s, "commit", &tree, &[&parent])
        .unwrap()
}

/// Create a branch pointing at HEAD without checking it out.
fn branch_from_head(repo: &git2::Repository, name: &str) {
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch(name, &head, false).unwrap();
}

fn find_branch<'a>(info: &'a GitRepoPruneInfo, name: &str) -> &'a BranchInfo {
    info.local_branches
        .iter()
        .find(|b| b.name == name)
        .unwrap_or_else(|| panic!("branch {} not enumerated", name))
}

// ── active_node_paths (#607) ────────────────────────────────────────────────
//
// `n.path` alone is the mesh root. A Worktree Node's work lives at
// `<mesh>/.claude/worktrees/<name>` — that subdir must also enter the active
// set, or `path_is_active` matches every linked worktree against the mesh
// root alone and flags them all `is_active: false` in the Worktree Manager.
// `env::node_worktree_path` is the single canonical reader of the
// `use_worktree` + trimmed-`worktree_name` rule (diff, PR, file-watcher, and
// close-safety all go through it); delegating preserves the one-rule
// invariant rather than re-spelling the worktree layout here.

/// Minimal `AgentNode` fixture — only the fields the worktree-rule reads.
/// `use_worktree=false` plus `worktree_name=None` builds a Root Node; pass
/// `true` + `Some(name)` for a Worktree Node. Mirrors the `env::tests::node`
/// shape (per #457: optional `..Default::default()`).
fn node(use_worktree: bool, worktree_name: Option<&str>) -> AgentNode {
    AgentNode {
        path: "/home/user/repo".to_string(),
        use_worktree,
        worktree_name: worktree_name.map(str::to_string),
        ..Default::default()
    }
}

/// Regression for #607: a Worktree Node must contribute BOTH its mesh path
/// AND its resolved worktree dir, so the linked worktree on disk matches
/// against the active set instead of being flagged inactive/stale.
#[test]
fn active_node_paths_includes_resolved_worktree_dir_for_worktree_nodes() {
    let nodes = vec![node(true, Some("gentle-fox"))];

    let paths = active_node_paths(&nodes);

    assert!(
        paths.iter().any(|p| p == "/home/user/repo"),
        "mesh path must be present so the main worktree still matches: {:?}",
        paths
    );
    assert!(
        paths.iter().any(|p| p.contains("gentle-fox")),
        "Worktree Node must contribute its resolved worktree dir (#607): {:?}",
        paths
    );
}

/// A Root Node has no worktree dir to add — only its mesh path participates.
#[test]
fn active_node_paths_root_node_contributes_only_mesh_path() {
    let nodes = vec![node(false, None)];

    let paths = active_node_paths(&nodes);

    assert_eq!(paths.len(), 1, "root node contributes exactly one path: {:?}", paths);
    assert_eq!(paths[0], "/home/user/repo");
}

/// A whitespace-only `worktree_name` collapses to "no worktree" per the
/// canonical rule in `env::node_worktree_path`, so it contributes only the
/// mesh path — same as a Root Node.
#[test]
fn active_node_paths_blank_worktree_name_contributes_only_mesh_path() {
    let nodes = vec![node(true, Some("   "))];

    let paths = active_node_paths(&nodes);

    assert_eq!(paths.len(), 1, "blank worktree name is treated as no worktree: {:?}", paths);
    assert_eq!(paths[0], "/home/user/repo");
}

// ── get_git_prune_info / collect_prune_info ─────────────────────────────────

#[test]
fn enumerates_local_branches() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "feature-a");
    branch_from_head(&repo, "feature-b");

    let info = collect_prune_info(&dir.path_str(), &[]).unwrap();
    let mut names: Vec<&str> = info.local_branches.iter().map(|b| b.name.as_str()).collect();
    names.sort();
    assert_eq!(names, vec!["feature-a", "feature-b", "main"]);
}

#[test]
fn head_branch_is_flagged() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "feature-a");

    let info = collect_prune_info(&dir.path_str(), &[]).unwrap();
    assert!(find_branch(&info, "main").is_head);
    assert!(!find_branch(&info, "feature-a").is_head);
}

#[test]
fn merged_branch_detected() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    // feature-a points at an ancestor of main → merged.
    branch_from_head(&repo, "feature-a");
    commit_file(&repo, "more.txt", "more"); // advances main past feature-a

    let info = collect_prune_info(&dir.path_str(), &[]).unwrap();
    assert_eq!(find_branch(&info, "feature-a").is_merged_into_main, Some(true));
    // main is trivially "merged into" itself.
    assert_eq!(find_branch(&info, "main").is_merged_into_main, Some(true));
}

#[test]
fn unmerged_branch_detected() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "feature-a");
    // Check out feature-a and add a commit not contained in main.
    repo.set_head("refs/heads/feature-a").unwrap();
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
    commit_file(&repo, "feat.txt", "feature work");

    let info = collect_prune_info(&dir.path_str(), &[]).unwrap();
    assert_eq!(find_branch(&info, "feature-a").is_merged_into_main, Some(false));
}

#[test]
fn squash_merged_branch_detected() {
    // A squash merge rewrites the whole branch into a single new commit on main
    // with no ancestry link back — so `graph_descendant_of` (the old check)
    // reports it unmerged and it never gets recommended for pruning. The
    // branch's cumulative diff against the merge base equals that squash
    // commit's patch, so patch-id matching must catch it.
    let dir = TempDir::new();
    let repo = init_repo(dir.path());

    branch_from_head(&repo, "feature");
    repo.set_head("refs/heads/feature").unwrap();
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
    commit_file(&repo, "feature.txt", "feature work");

    // Back on main, land the *same change* as one fresh commit — exactly what a
    // squash merge produces. main lacks feature's commit but holds its net diff.
    repo.set_head("refs/heads/main").unwrap();
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
    commit_file(&repo, "feature.txt", "feature work");

    let info = collect_prune_info(&dir.path_str(), &[]).unwrap();
    assert_eq!(
        find_branch(&info, "feature").is_merged_into_main,
        Some(true),
        "squash-merged branch (same net diff already on main) must read as merged"
    );
}

#[test]
fn genuinely_unmerged_branch_not_falsely_merged() {
    // Guard against patch-id over-recommending: a branch whose work is absent
    // from main in any form must still read as unmerged.
    let dir = TempDir::new();
    let repo = init_repo(dir.path());

    branch_from_head(&repo, "feature");
    repo.set_head("refs/heads/feature").unwrap();
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
    commit_file(&repo, "feature.txt", "feature work");

    repo.set_head("refs/heads/main").unwrap();
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
    commit_file(&repo, "unrelated.txt", "different work");

    let info = collect_prune_info(&dir.path_str(), &[]).unwrap();
    assert_eq!(
        find_branch(&info, "feature").is_merged_into_main,
        Some(false),
        "a branch whose diff is absent from main must not be flagged merged"
    );
}

#[test]
fn merged_state_none_without_main() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    // Rename main → trunk so there's no main/master to compare against.
    {
        let mut b = repo.find_branch("main", git2::BranchType::Local).unwrap();
        b.rename("trunk", true).unwrap();
        repo.set_head("refs/heads/trunk").unwrap();
    }

    let info = collect_prune_info(&dir.path_str(), &[]).unwrap();
    assert_eq!(find_branch(&info, "trunk").is_merged_into_main, None);
}

#[test]
fn last_commit_date_present() {
    let dir = TempDir::new();
    init_repo(dir.path());
    let info = collect_prune_info(&dir.path_str(), &[]).unwrap();
    assert!(find_branch(&info, "main").last_commit_date.is_some());
}

#[test]
fn ahead_behind_against_upstream() {
    // Build an "origin" bare-ish setup by cloning via a remote-tracking ref.
    let origin = TempDir::new();
    let origin_repo = init_repo(origin.path());
    commit_file(&origin_repo, "shared.txt", "v1");

    let work = TempDir::new();
    let repo = git2::Repository::clone(origin.path().to_str().unwrap(), work.path()).unwrap();

    // Local advances by one commit ahead of origin/main.
    commit_file(&repo, "local.txt", "local change");

    // Origin advances by one commit; fetch so the remote-tracking ref moves.
    commit_file(&origin_repo, "remote.txt", "remote change");
    repo.find_remote("origin")
        .unwrap()
        .fetch::<&str>(&[], None, None)
        .unwrap();

    let info = collect_prune_info(&work.path_str(), &[]).unwrap();
    let main = find_branch(&info, "main");
    assert_eq!(main.ahead, 1, "one local commit not on origin");
    assert_eq!(main.behind, 1, "one origin commit not local");
    assert!(!main.is_orphan);
}

#[test]
fn orphan_branch_detected() {
    let origin = TempDir::new();
    init_repo(origin.path());

    let work = TempDir::new();
    let repo = git2::Repository::clone(origin.path().to_str().unwrap(), work.path()).unwrap();

    // main now tracks origin/main. Delete the remote-tracking ref to orphan it.
    repo.find_reference("refs/remotes/origin/main")
        .unwrap()
        .delete()
        .unwrap();

    let info = collect_prune_info(&work.path_str(), &[]).unwrap();
    assert!(find_branch(&info, "main").is_orphan, "upstream ref is gone → orphan");
}

#[test]
fn branch_without_upstream_is_not_orphan() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "local-only");

    let info = collect_prune_info(&dir.path_str(), &[]).unwrap();
    let b = find_branch(&info, "local-only");
    assert!(!b.is_orphan);
    assert_eq!(b.ahead, 0);
    assert_eq!(b.behind, 0);
}

#[test]
fn remote_tracking_branches_listed_without_head() {
    let origin = TempDir::new();
    init_repo(origin.path());

    let work = TempDir::new();
    git2::Repository::clone(origin.path().to_str().unwrap(), work.path()).unwrap();

    let info = collect_prune_info(&work.path_str(), &[]).unwrap();
    assert!(
        info.remote_tracking_branches.iter().any(|b| b == "origin/main"),
        "expected origin/main, got {:?}",
        info.remote_tracking_branches
    );
    assert!(
        !info.remote_tracking_branches.iter().any(|b| b.ends_with("/HEAD")),
        "origin/HEAD should be filtered out"
    );
}

#[test]
fn main_worktree_enumerated() {
    let dir = TempDir::new();
    init_repo(dir.path());
    let info = collect_prune_info(&dir.path_str(), &[]).unwrap();
    assert_eq!(info.worktrees.len(), 1);
    assert_eq!(info.worktrees[0].branch.as_deref(), Some("main"));
    assert!(!info.worktrees[0].is_active);
    assert!(!info.worktrees[0].is_stale);
}

#[test]
fn linked_worktree_enumerated_and_active_flag() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "wt-branch");

    let wt_dir = TempDir::new();
    repo.worktree(
        "wt1",
        wt_dir.path(),
        Some(git2::WorktreeAddOptions::new().reference(Some(
            &repo.find_reference("refs/heads/wt-branch").unwrap(),
        ))),
    )
    .unwrap();

    // Mark the worktree path active.
    let active = vec![wt_dir.path_str()];
    let info = collect_prune_info(&dir.path_str(), &active).unwrap();

    let wt = info
        .worktrees
        .iter()
        .find(|w| w.branch.as_deref() == Some("wt-branch"))
        .expect("linked worktree enumerated");
    assert!(wt.is_active, "worktree path matches an active node");
    assert!(!wt.is_stale, "branch still exists");
}

#[test]
fn stale_worktree_when_branch_deleted() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "doomed");

    let wt_dir = TempDir::new();
    repo.worktree(
        "wt1",
        wt_dir.path(),
        Some(git2::WorktreeAddOptions::new().reference(Some(
            &repo.find_reference("refs/heads/doomed").unwrap(),
        ))),
    )
    .unwrap();

    // Delete the branch the worktree was based on (it's checked out, but the
    // worktree's own HEAD still names it). Force a stale state by removing the
    // ref directly.
    repo.find_reference("refs/heads/doomed").unwrap().delete().unwrap();

    let info = collect_prune_info(&dir.path_str(), &[]).unwrap();
    let wt = info
        .worktrees
        .iter()
        .find(|w| w.branch.as_deref() == Some("doomed"))
        .expect("worktree still enumerated");
    assert!(wt.is_stale, "branch no longer in local list → stale");
}

// ── delete_branches ─────────────────────────────────────────────────────────

#[test]
fn delete_branches_removes_named_branches() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "feature-a");
    branch_from_head(&repo, "feature-b");

    delete_branches_in_repo(&dir.path_str(), &["feature-a".to_string()]).unwrap();

    let info = collect_prune_info(&dir.path_str(), &[]).unwrap();
    let names: Vec<&str> = info.local_branches.iter().map(|b| b.name.as_str()).collect();
    assert!(!names.contains(&"feature-a"));
    assert!(names.contains(&"feature-b"));
    assert!(names.contains(&"main"));
}

#[test]
fn delete_branches_cannot_delete_head() {
    let dir = TempDir::new();
    init_repo(dir.path());

    let err = delete_branches_in_repo(&dir.path_str(), &["main".to_string()]).unwrap_err();
    assert!(err.contains("main"), "error should name the failed branch: {}", err);

    // main must survive.
    let info = collect_prune_info(&dir.path_str(), &[]).unwrap();
    assert!(info.local_branches.iter().any(|b| b.name == "main"));
}

#[test]
fn delete_branches_continues_on_individual_failure() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "deletable");

    // "main" (HEAD) fails, "deletable" succeeds, "ghost" doesn't exist.
    let err = delete_branches_in_repo(
        &dir.path_str(),
        &[
            "main".to_string(),
            "deletable".to_string(),
            "ghost".to_string(),
        ],
    )
    .unwrap_err();

    assert!(err.contains("main"));
    assert!(err.contains("ghost"));

    // The deletable branch was still removed despite the others failing.
    let info = collect_prune_info(&dir.path_str(), &[]).unwrap();
    assert!(!info.local_branches.iter().any(|b| b.name == "deletable"));
}

// ── delete_worktrees ────────────────────────────────────────────────────────

#[test]
fn remove_worktrees_removes_linked_worktree() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "wt-branch");

    let wt_dir = TempDir::new();
    repo.worktree(
        "wt1",
        wt_dir.path(),
        Some(git2::WorktreeAddOptions::new().reference(Some(
            &repo.find_reference("refs/heads/wt-branch").unwrap(),
        ))),
    )
    .unwrap();
    assert!(wt_dir.path().exists());

    remove_worktrees(&[wt_dir.path_str()]).unwrap();

    assert!(!wt_dir.path().exists(), "working directory should be gone");
    // The worktree admin entry is pruned too.
    let info = collect_prune_info(&dir.path_str(), &[]).unwrap();
    assert_eq!(info.worktrees.len(), 1, "only the main worktree remains");
}

#[test]
fn remove_worktrees_cannot_remove_main() {
    let dir = TempDir::new();
    init_repo(dir.path());

    let err = remove_worktrees(&[dir.path_str()]).unwrap_err();
    assert!(
        err.contains("not a removable worktree") || err.contains(&dir.path_str()),
        "main worktree removal should fail: {}",
        err
    );
    assert!(dir.path().exists(), "main worktree must survive");
}

/// Closing a node removes the worktree *and* its branch — the leftover branches
/// were the thing piling up. `remove_one_worktree_and_branch` is what the close
/// drain calls.
#[test]
fn close_removes_worktree_and_its_branch() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "wt-branch");

    let wt_dir = TempDir::new();
    repo.worktree(
        "wt1",
        wt_dir.path(),
        Some(git2::WorktreeAddOptions::new().reference(Some(
            &repo.find_reference("refs/heads/wt-branch").unwrap(),
        ))),
    )
    .unwrap();
    assert!(wt_dir.path().exists());

    remove_one_worktree_and_branch(&wt_dir.path_str()).unwrap();

    assert!(!wt_dir.path().exists(), "working directory should be gone");
    let info = collect_prune_info(&dir.path_str(), &[]).unwrap();
    assert!(
        !info.local_branches.iter().any(|b| b.name == "wt-branch"),
        "the worktree's branch must be deleted on close, not left orphaned"
    );
}

/// The Worktree Manager's manual worktree delete stays worktree-only — branches
/// are a separate, independently-selectable list there, so removing a worktree
/// must NOT silently take its branch with it.
#[test]
fn manual_worktree_removal_keeps_branch() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "wt-branch");

    let wt_dir = TempDir::new();
    repo.worktree(
        "wt1",
        wt_dir.path(),
        Some(git2::WorktreeAddOptions::new().reference(Some(
            &repo.find_reference("refs/heads/wt-branch").unwrap(),
        ))),
    )
    .unwrap();

    remove_one_worktree(&wt_dir.path_str()).unwrap();

    assert!(!wt_dir.path().exists());
    let info = collect_prune_info(&dir.path_str(), &[]).unwrap();
    assert!(
        info.local_branches.iter().any(|b| b.name == "wt-branch"),
        "manual worktree removal must leave the branch for separate selection"
    );
}

/// Build a repo with one linked worktree, then pin that worktree the way a live
/// agent does: a shell (`cmd`) with a long-running grandchild (`ping`) whose
/// inherited stdout is an open file handle inside the tree. An open handle
/// blocks `rmdir` on Windows until the whole process tree is killed. Returns the
/// worktree TempDir and the still-running locker child.
///
/// (`dir` — the parent repo — is returned so the caller keeps it alive.)
#[cfg(windows)]
fn worktree_pinned_by_process_tree() -> (TempDir, TempDir, std::process::Child) {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "wt-branch");

    let wt_dir = TempDir::new();
    repo.worktree(
        "wt1",
        wt_dir.path(),
        Some(git2::WorktreeAddOptions::new().reference(Some(
            &repo.find_reference("refs/heads/wt-branch").unwrap(),
        ))),
    )
    .unwrap();
    assert!(wt_dir.path().exists());

    // Dropping our own copy of the handle leaves the child as the sole holder.
    let lock_handle = fs::File::create(wt_dir.path().join("agent.lock")).expect("create lock file");
    let locker = crate::process_util::command_no_window("cmd")
        .args(["/c", "ping -n 30 127.0.0.1"])
        .current_dir(wt_dir.path())
        .stdout(std::process::Stdio::from(lock_handle))
        .spawn()
        .expect("spawn locking process");
    // Give the tree a moment to start writing before we probe.
    std::thread::sleep(std::time::Duration::from_millis(300));

    (dir, wt_dir, locker)
}

/// Reproduces the Windows close-node failure: while an agent process holds an
/// open handle inside the worktree, removing it fails. On Unix this never
/// reproduces (open handles don't block `rmdir`), so the test is Windows-only.
#[cfg(windows)]
#[test]
fn remove_worktree_fails_while_process_pins_it() {
    let (_dir, wt_dir, mut locker) = worktree_pinned_by_process_tree();

    let err = remove_one_worktree(&wt_dir.path_str())
        .expect_err("prune must fail while a process holds a handle in the worktree");
    assert!(
        err.contains("being used by another process")
            || err.to_lowercase().contains("access is denied")
            || err.to_lowercase().contains("cannot access"),
        "expected a Windows file-in-use error, got: {}",
        err
    );

    crate::process_util::kill_process_tree(locker.id());
    let _ = locker.wait();
}

/// The fix: killing the whole process tree (shell + agent grandchild) releases
/// the worktree, and removal then succeeds — the rmdir retry rides out the
/// async handle-release window after the kill.
#[cfg(windows)]
#[test]
fn remove_worktree_succeeds_after_killing_process_tree() {
    let (_dir, wt_dir, mut locker) = worktree_pinned_by_process_tree();

    crate::process_util::kill_process_tree(locker.id());
    let _ = locker.wait();

    remove_one_worktree(&wt_dir.path_str()).expect("prune succeeds once the tree is gone");
    assert!(!wt_dir.path().exists(), "working directory should be gone");
}

/// #239 regression: a removal that fails because a live agent still pins the
/// worktree must leave the working tree *fully intact*, never half-deleted.
/// The old `remove_dir_all` walked entries one by one, gutting the source files
/// and the `.git` gitlink before it hit the locked file and bailed — leaving a
/// broken stub that could no longer be opened as a repo, so the node could
/// never be closed. Removal must be all-or-nothing.
#[cfg(windows)]
#[test]
fn remove_worktree_does_not_gut_tree_when_pinned() {
    let (_dir, wt_dir, mut locker) = worktree_pinned_by_process_tree();

    remove_one_worktree(&wt_dir.path_str())
        .expect_err("removal must fail while a process pins the worktree");

    // Everything the failed removal would otherwise have gutted must survive.
    assert!(
        wt_dir.path().join("file.txt").exists(),
        "committed source file must survive a failed removal, not be gutted"
    );
    assert!(
        wt_dir.path().join(".git").exists(),
        ".git gitlink must survive a failed removal, not be gutted"
    );

    crate::process_util::kill_process_tree(locker.id());
    let _ = locker.wait();
}

/// #239: an already-gone working directory is success — there is nothing to
/// delete, so removal must not error (the node can still be closed cleanly).
#[test]
fn remove_worktree_treats_missing_working_dir_as_success() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "wt-branch");

    let wt_dir = TempDir::new();
    repo.worktree(
        "wt1",
        wt_dir.path(),
        Some(git2::WorktreeAddOptions::new().reference(Some(
            &repo.find_reference("refs/heads/wt-branch").unwrap(),
        ))),
    )
    .unwrap();

    // Simulate a worktree whose working directory has already vanished.
    fs::remove_dir_all(wt_dir.path()).unwrap();
    assert!(!wt_dir.path().exists());

    remove_one_worktree(&wt_dir.path_str())
        .expect("a missing working directory is nothing to remove → success");
}
