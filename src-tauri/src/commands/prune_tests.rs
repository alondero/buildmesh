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
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Pool-path predicate used by every prune-enumeration test in this file.
/// Production callers wrap `db::is_warm_pool_path`; tests pass `|_| false`
/// so the pure enumeration stays DB-free and avoids the `database not
/// initialized` panic that hits any test touching `collect_prune_info`
/// before the global DB has been set up.
fn no_pool_paths(_path: &str) -> bool {
    false
}

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

/// Path comparison helper. libgit2 normalises internal paths to forward
/// slashes on every platform, so a worktree path returned from
/// `Worktree::path()` is `C:/foo/bar` even on Windows. A `PathBuf` built
/// locally (e.g. from a `TempDir`) keeps the platform-native separator
/// (`C:\foo\bar`). The two are equal on disk but compare unequal as
/// strings; normalise to one form before asserting.
fn normalize_slashes(p: &str) -> String {
    p.replace('\\', "/")
}

// ── env::active_node_branches contract (review follow-ups) ────────────────

/// `active_node_branches` filters out `Archived` nodes — the contract
/// documented in `env/mod.rs`. The original implementation deferred the
/// filter to the caller, which two of the three call sites (notably
/// `delete_branches` via `list_agent_nodes_by_mesh`) silently dropped —
/// producing a UI/backend mismatch where the Worktree Manager listed a
/// branch as idle (`is_active: false`) but `delete_branches` still
/// refused to drop it because the archived node's `branch` field stayed
/// in the set. Pinning the filter at the helper closes the gap.
#[test]
fn active_node_branches_excludes_archived_nodes() {
    use crate::env::active_node_branches;
    use crate::models::{AgentNode, EnvType, SessionStatus};

    let live = AgentNode {
        id: 1,
        mesh_id: 1,
        name: "live".into(),
        path: "/repo".into(),
        branch: "feature-live".into(),
        env: EnvType::Windows,
        provider: "anthropic".into(),
        status: SessionStatus::Running,
        ..Default::default()
    };
    let archived = AgentNode {
        id: 2,
        mesh_id: 1,
        name: "archived".into(),
        path: "/repo/.claude/worktrees/archived".into(),
        branch: "feature-archived".into(),
        env: EnvType::Windows,
        provider: "anthropic".into(),
        status: SessionStatus::Archived,
        ..Default::default()
    };

    let branches = active_node_branches(&[live, archived]);
    assert!(
        branches.contains(&"feature-live".to_string()),
        "live node's branch must be in the active set; got {:?}",
        branches
    );
    assert!(
        !branches.contains(&"feature-archived".to_string()),
        "archived node's branch must NOT be in the active set; got {:?}",
        branches
    );
}

// ── get_git_prune_info / collect_prune_info ─────────────────────────────────

#[test]
fn enumerates_local_branches() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "feature-a");
    branch_from_head(&repo, "feature-b");

    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
    let mut names: Vec<&str> = info.local_branches.iter().map(|b| b.name.as_str()).collect();
    names.sort();
    assert_eq!(names, vec!["feature-a", "feature-b", "main"]);
}

#[test]
fn head_branch_is_flagged() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "feature-a");

    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
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

    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
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

    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
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

    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
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

    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
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

    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
    assert_eq!(find_branch(&info, "trunk").is_merged_into_main, None);
}

#[test]
fn last_commit_date_present() {
    let dir = TempDir::new();
    init_repo(dir.path());
    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
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

    let info = collect_prune_info(&work.path_str(), &[], &[], &no_pool_paths).unwrap();
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

    let info = collect_prune_info(&work.path_str(), &[], &[], &no_pool_paths).unwrap();
    assert!(find_branch(&info, "main").is_orphan, "upstream ref is gone → orphan");
}

#[test]
fn branch_without_upstream_is_not_orphan() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "local-only");

    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
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

    let info = collect_prune_info(&work.path_str(), &[], &[], &no_pool_paths).unwrap();
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
    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
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
    let info = collect_prune_info(&dir.path_str(), &active, &[], &no_pool_paths).unwrap();

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

    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
    let wt = info
        .worktrees
        .iter()
        .find(|w| w.branch.as_deref() == Some("doomed"))
        .expect("worktree still enumerated");
    assert!(wt.is_stale, "branch no longer in local list → stale");
}

// ── checked_out_in_worktree annotation ──────────────────────────────────────

/// A branch checked out in a *linked* worktree has its worktree path
/// stamped on `BranchInfo.checked_out_in_worktree`. This is the data
/// that lets the prune UI disable the branch checkbox and direct the
/// user to the worktree row above (the only safe way to drop a branch
/// that's HEAD of another working tree — git2's `branch.delete()` would
/// otherwise return `class=Reference (4)` "current HEAD of a linked
/// repository").
#[test]
fn branch_in_linked_worktree_has_checked_out_path() {
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

    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
    let wt_branch = find_branch(&info, "wt-branch");
    let wt_path = wt_dir.path().to_string_lossy().trim_end_matches(['/', '\\']).to_string();
    assert_eq!(
        normalize_slashes(wt_branch.checked_out_in_worktree.as_deref().unwrap()),
        normalize_slashes(&wt_path),
        "linked worktree's branch must carry its worktree path"
    );
}

/// The main repo's HEAD branch is also HEAD of the main worktree (it's
/// literally what `main` means), so its `checked_out_in_worktree` is the
/// main workdir — distinct from a linked worktree but the same shape of
/// annotation. The `is_head` flag still distinguishes it in the UI (cyan
/// HEAD badge), but the worktree annotation is what disables deletion
/// regardless.
#[test]
fn main_head_branch_has_checked_out_path() {
    let dir = TempDir::new();
    init_repo(dir.path());

    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
    let main = find_branch(&info, "main");
    let main_path = dir.path().to_string_lossy().trim_end_matches(['/', '\\']).to_string();
    assert!(
        main.is_head,
        "main is HEAD of the main worktree in this fixture"
    );
    assert_eq!(
        normalize_slashes(main.checked_out_in_worktree.as_deref().unwrap()),
        normalize_slashes(&main_path),
        "main repo's HEAD branch carries the main workdir path"
    );
}

/// A standalone local branch (no worktree, never checked out anywhere)
/// reads `checked_out_in_worktree: None` — the safe baseline. Only
/// branches that are HEAD of *some* working tree get the annotation,
/// which is exactly the set the UI must block from direct deletion.
#[test]
fn standalone_branch_has_no_checked_out_path() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "feature-a");
    // `feature-a` was created from HEAD but never switched to or
    // checked out anywhere — no worktree, no HEAD pointer at it.

    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
    let feat = find_branch(&info, "feature-a");
    assert_eq!(
        feat.checked_out_in_worktree, None,
        "a branch never checked out has no worktree path"
    );
}

/// The whole point: `delete_branches` must STILL refuse a branch that's
/// HEAD of a linked worktree. The new `checked_out_in_worktree`
/// annotation is *UI-only* (so the row is disabled); the backend
/// continues to be defence-in-depth and lets libgit2's error surface
/// as before. This test pins that contract so a future "auto-cascade"
/// refactor doesn't silently change it.
#[test]
fn delete_branches_still_refuses_worktree_branch() {
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

    // `delete_branches` partitions active branches (the `active_branches`
    // arg) before attempting libgit2 deletes. Here we pass `wt-branch` as
    // *not* active, so it falls through to git2 — which then refuses.
    let err = delete_branches_in_repo(
        &dir.path_str(),
        &["wt-branch".to_string()],
        &[],
    )
    .expect_err("git2 refuses to delete a branch checked out in a linked worktree");
    assert!(
        err.contains("wt-branch") && err.contains("linked repository"),
        "expected libgit2's HEAD-of-linked-worktree error naming the branch; got: {}",
        err
    );

    // The branch and its worktree must both still exist after the
    // refused delete — nothing was partially removed.
    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
    assert!(
        info.local_branches.iter().any(|b| b.name == "wt-branch"),
        "wt-branch must survive a refused delete"
    );
    assert!(
        info.worktrees.iter().any(|w| w.branch.as_deref() == Some("wt-branch")),
        "linked worktree must survive a refused branch delete"
    );
}

// ── delete_branches ─────────────────────────────────────────────────────────

#[test]
fn delete_branches_removes_named_branches() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "feature-a");
    branch_from_head(&repo, "feature-b");

    delete_branches_in_repo(&dir.path_str(), &["feature-a".to_string()], &[]).unwrap();

    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
    let names: Vec<&str> = info.local_branches.iter().map(|b| b.name.as_str()).collect();
    assert!(!names.contains(&"feature-a"));
    assert!(names.contains(&"feature-b"));
    assert!(names.contains(&"main"));
}

#[test]
fn delete_branches_cannot_delete_head() {
    let dir = TempDir::new();
    init_repo(dir.path());

    let err = delete_branches_in_repo(&dir.path_str(), &["main".to_string()], &[]).unwrap_err();
    assert!(err.contains("main"), "error should name the failed branch: {}", err);

    // main must survive.
    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
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
        &[],
    )
    .unwrap_err();

    assert!(err.contains("main"));
    assert!(err.contains("ghost"));

    // The deletable branch was still removed despite the others failing.
    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
    assert!(!info.local_branches.iter().any(|b| b.name == "deletable"));
}

// ── active-branch flag + delete guard (sibling of worktree `is_active`) ────

/// `collect_prune_info` flags `is_active = true` for every branch whose
/// name is in the caller's active-branch set — mirrors the path-based
/// `WorktreeInfo.is_active` derivation. Without this flag the Worktree
/// Manager tab can't disable the row's checkbox or show the active
/// badge, so the user could click-delete a branch a live agent is on.
#[test]
fn collect_prune_info_marks_active_branches() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "feature-a");
    branch_from_head(&repo, "feature-b");

    // Only "feature-a" is held by an agent — the rest are idle.
    let active = vec!["feature-a".to_string()];
    let info = collect_prune_info(&dir.path_str(), &[], &active, &no_pool_paths).unwrap();

    assert!(find_branch(&info, "feature-a").is_active, "feature-a is in the active set");
    assert!(!find_branch(&info, "feature-b").is_active, "feature-b is not in the active set");
    assert!(!find_branch(&info, "main").is_active, "main is not in the active set");
}

/// Empty active-branch set: every branch is idle. Guards against a
/// future refactor that defaults `is_active` to `true` for safety —
/// idle branches must read `false` so the prune UI can select them.
#[test]
fn collect_prune_info_no_active_set_marks_all_idle() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "feature-a");

    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
    for b in &info.local_branches {
        assert!(!b.is_active, "{} should not be active with empty set", b.name);
    }
}

/// `delete_branches_in_repo` refuses to drop a branch whose name is in
/// the active set — defence-in-depth for the prune UI. The frontend
/// already disables the row's checkbox, but a stale UI or a direct
/// API call must not be able to drop a branch a node is on. The
/// rejection must (a) name every blocked branch in one combined error
/// (bulk-select UX) and (b) leave the rest of the deletion request
/// pending — no partial failure.
#[test]
fn delete_branches_rejects_active_branch() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "feature-a");
    branch_from_head(&repo, "feature-b");

    let active = vec!["feature-a".to_string()];
    let err = delete_branches_in_repo(
        &dir.path_str(),
        &["feature-a".to_string()],
        &active,
    )
    .expect_err("active branch must be rejected with Err");

    assert!(
        err.contains("active"),
        "error must name the active-branch class; got: {}",
        err
    );
    assert!(
        err.contains("feature-a"),
        "error must include the blocked branch name; got: {}",
        err
    );

    // The branch is intact — the rejection is a no-op.
    let info = collect_prune_info(&dir.path_str(), &[], &active, &no_pool_paths).unwrap();
    assert!(
        info.local_branches.iter().any(|b| b.name == "feature-a"),
        "rejected active branch must remain intact (no partial delete)"
    );
}

/// Mixed request: one active branch + one deletable branch. The active
/// one is named in a single combined error; the deletable one is NOT
/// touched (we reject the whole request rather than partially
/// processing — same posture as the pool-path rejection in
/// `delete_worktrees`).
#[test]
fn delete_branches_active_and_deletable_blocked_together() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "active-one");
    branch_from_head(&repo, "deletable");

    let active = vec!["active-one".to_string()];
    let err = delete_branches_in_repo(
        &dir.path_str(),
        &["active-one".to_string(), "deletable".to_string()],
        &active,
    )
    .expect_err("active branch in request → reject the whole call");

    assert!(err.contains("active-one"), "must name the active blocker");
    assert!(
        !err.contains("deletable"),
        "deletable is not the failure cause and shouldn't be in the error: {}",
        err
    );

    // Neither branch was touched.
    let info = collect_prune_info(&dir.path_str(), &[], &active, &no_pool_paths).unwrap();
    assert!(info.local_branches.iter().any(|b| b.name == "active-one"));
    assert!(info.local_branches.iter().any(|b| b.name == "deletable"));
}

/// Empty active-branch set: the guard is a no-op, ordinary deletes
/// still work. Pins the contract that the new arg does NOT silently
/// block anything when no agents are live.
#[test]
fn delete_branches_empty_active_set_unaffected() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "feature-a");

    delete_branches_in_repo(
        &dir.path_str(),
        &["feature-a".to_string()],
        &[],
    )
    .expect("empty active set → ordinary delete proceeds");

    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
    assert!(
        !info.local_branches.iter().any(|b| b.name == "feature-a"),
        "feature-a must be gone after a clean delete"
    );
}

/// Pin the archived-node contract at the integration point: even if a
/// caller passes an active-branch set containing a branch that ONLY an
/// archived node was on, the guard does NOT reject the delete — because
/// the helper (`active_node_branches`) is the boundary that drops
/// archived nodes, callers that route through it won't pass an archived
/// branch in the first place. This test exercises the lower-level
/// `delete_branches_in_repo` to document that the archived-status filter
/// lives in `active_node_branches` (the env helper), not here. If a
/// future caller passes archived-node branches directly, this test
/// documents the existing behaviour rather than fixing it at this layer
/// — the canonical fix is at the helper.
#[test]
fn delete_branches_in_repo_treats_passed_in_set_literally() {
    let dir = TempDir::new();
    let repo = init_repo(dir.path());
    branch_from_head(&repo, "feature-old");

    // Caller passes a set containing "feature-old" — the helper layer
    // (above) is responsible for dropping archived-node branches, so
    // by the time we get here, "feature-old" appears active. The guard
    // rejects it; the archive-status check is upstream.
    let active = vec!["feature-old".to_string()];
    let err = delete_branches_in_repo(
        &dir.path_str(),
        &["feature-old".to_string()],
        &active,
    )
    .expect_err("branches in the passed-in active set are rejected");
    assert!(err.contains("feature-old"), "{}", err);
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
    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
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
    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
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
    let info = collect_prune_info(&dir.path_str(), &[], &[], &no_pool_paths).unwrap();
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

// ── v22 / issue #611 — `delete_worktrees` rejects pool paths ────────────────

/// Per-process scratch path for the test DB. Mirrors the
/// `commands::agent::tests::pr_test_db_path` pattern: `db::init` is
/// one-shot per process, so all tests in this file that touch the
/// global DB share the same path. The `.db` suffix is required —
/// `Connection::open(path)` expects a file, not a directory (see
/// `commands/agent.rs:1598` for the full rationale).
fn prune_test_db_path() -> std::path::PathBuf {
    use std::sync::OnceLock;
    static PATH: OnceLock<std::path::PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let p = std::env::temp_dir().join(format!(
            "buildmesh_prune_pool_test_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        p
    })
    .clone()
}

/// `db::init` the global DB if it hasn't been already. Idempotent —
/// the OnceCell silently keeps whichever DB another test (e.g.
/// `db::mesh_tests`, `commands::agent::tests`) set up first. Mirrors
/// `commands::agent::tests::ensure_pr_db` so the two test families
/// don't trample each other.
fn ensure_prune_db() {
    if crate::db::is_initialized() {
        return;
    }
    std::sync::Once::new().call_once(|| {
        let _ = crate::db::init(&prune_test_db_path());
    });
}

/// Pin the `delete_worktrees` → `remove_worktrees` pool-rejection
/// contract (issue #611). A pool entry's directory is owned by the
/// `services::warm_pool` background worker; deleting it from the
/// Worktree Manager tab would either waste the warm-path latency win
/// or leave a stale row the worker can't GC until the next
/// reconcile. The UI disables the row's checkbox as the primary
/// defence; this backend check is defence-in-depth — a stale UI
/// (or a misrouted API call) must not be able to drop a pool entry.
///
/// The error message intentionally names the path so the user can
/// see exactly which entry was blocked when they bulk-select.
#[test]
fn delete_worktrees_rejects_pool_path() {
    ensure_prune_db();

    // Set up a mesh + a single `warm_worktrees` row at a fake path.
    // `is_warm_pool_path` only checks the DB row — it doesn't touch
    // the filesystem — so a path that doesn't exist on disk is fine.
    let mesh = crate::db::create_mesh("pool-test", "/tmp/buildmesh_pool_test_mesh").unwrap();
    let pool_path = "/tmp/buildmesh_pool_test_mesh/.claude/worktrees/pool-warm-xyz";
    crate::db::insert_warm_worktree(
        mesh.id,
        pool_path,
        "pool-warm-xyz",
        None,
        crate::db::WarmWorktreeStatus::Available,
    )
    .unwrap();

    // Sanity: the row exists so `is_warm_pool_path` reads true. If
    // this fails the rest of the test is meaningless — the rejection
    // path never runs.
    assert!(
        crate::db::is_warm_pool_path(pool_path).unwrap(),
        "fixture row must read as a pool path before we exercise the guard"
    );

    let err = remove_worktrees(&[pool_path.to_string()])
        .expect_err("remove_worktrees must reject a pool path with Err");

    assert!(
        err.contains("pre-spawn pool"),
        "error must name the pool-entry class so the UI surfaces a clear message; got: {}",
        err
    );
    assert!(
        err.contains(pool_path),
        "error must include the blocked path so bulk selections identify every entry; got: {}",
        err
    );

    // Defence-in-depth check: the DB row is still there. The
    // rejection must be a no-op on the row — we never partially
    // delete a pool entry (no DB write, no directory removal).
    assert!(
        crate::db::is_warm_pool_path(pool_path).unwrap(),
        "rejected pool row must remain intact (no partial delete)"
    );
}

/// A non-pool path is unaffected by the new guard — `delete_worktrees`
/// continues to attempt `git worktree remove` for any path the
/// partition classifies as `deletable`. This test only proves the
/// partition doesn't mis-route pool paths INTO the deletable bucket;
/// the actual `git worktree remove` outcome for a non-existent path
/// is covered by the existing `remove_worktree_treats_missing_working_dir_as_success`
/// test.
#[test]
fn delete_worktrees_does_not_reject_non_pool_path() {
    ensure_prune_db();

    // Insert a `warm_worktrees` row that is NOT the path we're
    // asking to delete. `is_warm_pool_path(other_path)` must read
    // false → no rejection → proceeds to the actual remove call
    // (which will fail because the directory doesn't exist, but
    // that's a separate error path covered above).
    let mesh = crate::db::create_mesh("non-pool-test", "/tmp/buildmesh_non_pool_test_mesh").unwrap();
    let pool_path = "/tmp/buildmesh_non_pool_test_mesh/.claude/worktrees/pool-warm-abc";
    crate::db::insert_warm_worktree(
        mesh.id,
        pool_path,
        "pool-warm-abc",
        None,
        crate::db::WarmWorktreeStatus::Available,
    )
    .unwrap();

    let non_pool_path = "/tmp/buildmesh_non_pool_test_mesh/.claude/worktrees/manual-branch";
    // Should NOT be rejected — the error (if any) will be from
    // `git worktree remove` failing on a missing directory, NOT from
    // the pool guard.
    let result = remove_worktrees(&[non_pool_path.to_string()]);
    if let Err(e) = &result {
        assert!(
            !e.contains("pre-spawn pool"),
            "non-pool path must not be rejected as a pool entry; got: {}",
            e
        );
    }
}

// ── prune_remote_tracking (issue #657) ──────────────────────────────────
//
// `prune_remote_tracking` now returns `Result<String, String>` so the
// frontend can surface git's stderr output to the user. These tests pin the
// return type and exercise both the success path (real local "remote" +
// clone) and the failure path (non-repo directory).

fn run_git(path: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {:?}: {}", args, e));
    assert!(
        output.status.success(),
        "git {:?} failed\nstdout: {}\nstderr: {}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_git_clone(from: &str, to: &str) {
    let output = std::process::Command::new("git")
        .args(["clone", from, to])
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn git clone: {}", e));
    assert!(
        output.status.success(),
        "git clone failed\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn prune_remote_tracking_returns_string_on_success() {
    // Bare "remote" → seed push → local clone. The clone is clean and
    // already in sync, so `git fetch --prune` exits 0 with no refs to
    // drop. The test pins the return type — `.unwrap()` must produce a
    // `String` (currently it produces `()`, which fails to bind).
    let bare = TempDir::new();
    fs::create_dir_all(bare.path()).unwrap();
    git2::Repository::init_bare(bare.path()).unwrap();

    let seed = TempDir::new();
    let _seed_repo = init_repo(seed.path());
    run_git(seed.path(), &["branch", "-M", "main"]);
    run_git(
        seed.path(),
        &["remote", "add", "origin", bare.path().to_str().unwrap()],
    );
    run_git(seed.path(), &["push", "-u", "origin", "main"]);

    let local = TempDir::new();
    run_git_clone(bare.path().to_str().unwrap(), local.path().to_str().unwrap());

    let result = prune_remote_tracking(local.path_str()).await;
    // Type-anchor: the success variant MUST be a String. If the signature
    // regresses to `Result<(), String>` the `: String` annotation fails
    // to compile, which is the desired RED state.
    let message: String = result.expect("prune should succeed against a clean clone");
}

#[tokio::test]
async fn prune_remote_tracking_returns_err_on_non_repo_path() {
    // A temp directory that is NOT a git repo: `git fetch --prune` exits
    // non-zero with "fatal: not a git repository" on stderr. The function
    // must surface that as `Err`, not swallow it.
    let dir = TempDir::new();
    fs::create_dir_all(dir.path()).unwrap();
    let result = prune_remote_tracking(dir.path_str()).await;
    assert!(
        result.is_err(),
        "expected Err for non-repo path, got {:?}",
        result
    );
}
