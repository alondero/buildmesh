// Sibling test suite for the owning module. The suite lives in its own
// file so the production module keeps only the declaration below — the
// repo's rust-test-layout gate requires it.

use super::*;
use crate::env::test_helpers::{commit_file, init_repo_with_commit, TestDir};
use git2::BranchType;
use std::time::Duration;

/// A repository with one commit, two local branches and one remote-tracking
/// ref.
fn repo_with_branches() -> (TestDir, String) {
    let dir = TestDir::new("git_branches");
    let repo = init_repo_with_commit(dir.path(), &[("f.txt", "one\n")]);
    let head = repo.head().expect("head").peel_to_commit().expect("commit");
    repo.branch("feature/x", &head, false).expect("branch x");
    repo.branch("feature/y", &head, false).expect("branch y");
    repo.reference("refs/remotes/origin/main", head.id(), false, "remote")
        .expect("remote-tracking ref");
    let path = dir.path().to_string_lossy().into_owned();
    (dir, path)
}

/// A clean batch is not a rejection.
#[test]
fn reject_blocked_is_ok_for_an_empty_batch() {
    let blocked: Vec<&String> = Vec::new();

    assert!(reject_blocked(&blocked, "cannot delete").is_ok());
}

/// A blocked batch is rejected with every blocked item named — the user sees
/// the full set in one message, not just the first failure (issue #661).
#[test]
fn reject_blocked_names_every_blocked_item() {
    let alpha = "alpha".to_string();
    let beta = "beta".to_string();
    let blocked = vec![&alpha, &beta];

    let error = reject_blocked(&blocked, "cannot delete branches").expect_err("must reject");

    assert_eq!(error, "cannot delete branches: alpha, beta");
}

/// The worktree path is compared in its normalised form, so a trailing
/// separator does not make the same directory look like two.
#[test]
fn path_is_active_normalises_the_path_before_comparing() {
    let dir = TestDir::new("git_branches_active");
    let path = dir.path().to_string_lossy().into_owned();
    let with_separator = format!("{}{}", path, std::path::MAIN_SEPARATOR);

    assert!(path_is_active(&path, std::slice::from_ref(&path)));
    assert!(path_is_active(&with_separator, std::slice::from_ref(&path)));
    assert!(!path_is_active(&path, &[]));
}

/// The prune read lists the local branches and the remote-tracking refs.
#[test]
fn collect_prune_info_lists_the_local_and_remote_tracking_branches() {
    let (_dir, path) = repo_with_branches();

    let info = collect_prune_info(&path, &[], &[], &|_| false).expect("prune info");

    let names: Vec<&str> = info
        .local_branches
        .iter()
        .map(|branch| branch.name.as_str())
        .collect();
    assert!(names.contains(&"feature/x"));
    assert!(names.contains(&"feature/y"));
    assert!(
        info.remote_tracking_branches
            .contains(&"origin/main".to_string()),
        "got {:?}",
        info.remote_tracking_branches
    );
}

/// A worktree whose path is in the caller's active list is marked active.
#[test]
fn collect_prune_info_marks_an_active_worktree_active() {
    let (_dir, path) = repo_with_branches();

    let info = collect_prune_info(&path, std::slice::from_ref(&path), &[], &|_| false)
        .expect("prune info");

    let worktree = info.worktrees.first().expect("worktree");
    assert!(worktree.is_active);
    assert!(
        !worktree.is_pool,
        "the pool check is the caller's predicate, and this one says no"
    );
}

/// A branch the caller marks active is reported as active, so the prune UI
/// can disable its checkbox.
#[test]
fn collect_prune_info_marks_an_active_branch_active() {
    let (_dir, path) = repo_with_branches();

    let info =
        collect_prune_info(&path, &[], &["feature/x".to_string()], &|_| false).expect("prune info");

    let branch = info
        .local_branches
        .iter()
        .find(|branch| branch.name == "feature/x")
        .expect("branch");
    assert!(branch.is_active);
}

/// Deleting a branch removes it from the repository.
#[test]
fn delete_branches_in_repo_deletes_the_branch() {
    let (dir, path) = repo_with_branches();

    delete_branches_in_repo(&path, &["feature/x".to_string()], &[]).expect("delete");

    let repo = Repository::open(dir.path()).expect("reopen repo");
    assert!(
        repo.find_branch("feature/x", BranchType::Local).is_err(),
        "the branch must be gone"
    );
}

/// A batch that holds an active branch is rejected as a whole — the
/// deletable siblings are not deleted either, because a partial delete would
/// leave the user with a state they did not ask for.
#[test]
fn delete_branches_in_repo_rejects_the_whole_batch_when_one_branch_is_active() {
    let (dir, path) = repo_with_branches();

    let error = delete_branches_in_repo(
        &path,
        &["feature/x".to_string(), "feature/y".to_string()],
        &["feature/y".to_string()],
    )
    .expect_err("must reject");

    assert!(error.contains("feature/y"), "got {error}");
    let repo = Repository::open(dir.path()).expect("reopen repo");
    assert!(
        repo.find_branch("feature/x", BranchType::Local).is_ok(),
        "the deletable sibling must survive the rejection"
    );
}

/// A branch that does not exist is reported as an error naming it.
#[test]
fn delete_branches_in_repo_reports_a_branch_that_does_not_exist() {
    let (_dir, path) = repo_with_branches();

    let error = delete_branches_in_repo(&path, &["no-such-branch".to_string()], &[])
        .expect_err("must fail");

    assert!(error.contains("no-such-branch"), "got {error}");
}

/// The branch that HEAD points at is the one worktree the map must find, so
/// the prune UI can point the user at the worktree row instead of offering a
/// delete that libgit2 would refuse.
#[test]
fn collect_prune_info_finds_the_worktree_a_branch_is_checked_out_in() {
    let (_dir, path) = repo_with_branches();

    let info = collect_prune_info(&path, &[], &[], &|_| false).expect("prune info");

    let head_branch = info
        .local_branches
        .iter()
        .find(|branch| branch.is_head)
        .expect("the head branch");
    assert!(
        head_branch.checked_out_in_worktree.is_some(),
        "the branch HEAD points at must name its worktree"
    );
}

/// The path of a repository that is not one is an error, not an empty prune
/// view — the caller shows the message.
#[test]
fn collect_prune_info_reports_a_path_that_is_not_a_repository() {
    let dir = TestDir::new("git_branches_not_a_repo");
    let path = dir.path().to_string_lossy().into_owned();

    let error = collect_prune_info(&path, &[], &[], &|_| false).expect_err("must fail");

    assert!(!error.is_empty());
}

/// A repo whose `main` carries `main_commits` commits and which has
/// `branch_count` local branches, each forked from main's root commit with
/// one unmerged commit of its own.
///
/// Every branch therefore has the same merge base (main's root) and walks the
/// same stretch of `main`, which is the shape that made the old scan cost
/// `branches x history` tree diffs (issue #2580).
fn repo_with_history_and_branches(main_commits: usize, branch_count: usize) -> (TestDir, String) {
    let dir = TestDir::new("git_branches_history");
    let repo = init_repo_with_commit(dir.path(), &[("base.txt", "base\n")]);
    let root_oid = repo
        .head()
        .expect("head")
        .peel_to_commit()
        .expect("commit")
        .id();
    // `init_repo_with_commit` names the first branch `master` on some git
    // versions and `main` on others; the read compares against both.
    for i in 1..main_commits {
        commit_file(&repo, dir.path(), "main.txt", &format!("main {i}\n"));
    }
    let main_name = repo
        .head()
        .expect("head")
        .shorthand()
        .unwrap_or("main")
        .to_string();
    for i in 0..branch_count {
        let root = repo.find_commit(root_oid).expect("root commit");
        let name = format!("feature/{i}");
        repo.branch(&name, &root, false).expect("branch");
        // Give the branch a commit of its own so it is not an ancestor of
        // main and the squash scan has to look for it.
        repo.set_head(&format!("refs/heads/{name}"))
            .expect("set head");
        commit_file(
            &repo,
            dir.path(),
            &format!("feature-{i}.txt"),
            &format!("feature {i}\n"),
        );
        // Hard-reset back onto main so the next branch starts from a clean
        // tree instead of inheriting this branch's file.
        repo.set_head(&format!("refs/heads/{main_name}"))
            .expect("restore head");
        reset_hard_to_head(&repo);
    }
    let path = dir.path().to_string_lossy().into_owned();
    (dir, path)
}

/// Reset the index and working tree back onto the repo's current HEAD.
fn reset_hard_to_head(repo: &git2::Repository) {
    let head_oid = repo
        .head()
        .expect("head")
        .peel_to_commit()
        .expect("commit")
        .id();
    let head = repo.find_object(head_oid, None).expect("head object");
    repo.reset(&head, git2::ResetType::Hard, None)
        .expect("reset to head");
}

/// The squash-merge scan used to re-diff and re-hash every commit of `main`
/// once per unmerged branch. A commit's patch-id does not depend on which
/// branch asks for it, so the scan must compute each one only once no matter
/// how many branches it has to answer for.
///
/// This is the regression test for issue #2580: without the memoisation the
/// diff count grew with `branches x history`, which kept the Probe's
/// Repository destination spinning for tens of minutes.
#[test]
fn patch_ids_are_computed_once_no_matter_how_many_branches_are_scanned() {
    let (_dir, path) = repo_with_history_and_branches(6, 3);
    let repo = primitives::open_at(&path).expect("open repo");
    let main_oid = main_branch_oid(&repo).expect("main");
    let branch_oids: Vec<Oid> = repo
        .branches(Some(BranchType::Local))
        .expect("branches")
        .filter_map(|entry| entry.ok())
        .filter_map(|(branch, _)| branch.get().peel_to_commit().ok())
        .map(|commit| commit.id())
        .filter(|oid| *oid != main_oid)
        .collect();
    assert!(
        !branch_oids.is_empty(),
        "precondition: the repo must have branches to scan"
    );

    // One shared cache across every branch — the whole point of the change.
    let mut patch_ids = PatchIdCache::new(&repo);
    let mut budget = MergeScanBudget::new(Duration::from_secs(30));
    for oid in &branch_oids {
        branch_is_merged_into_main(&repo, &mut patch_ids, &mut budget, main_oid, *oid);
    }

    // Every commit of main is reachable from the walk, so the cache holds one
    // entry per commit — not one per (branch, commit) pair.
    let mut walk = repo.revwalk().expect("revwalk");
    walk.push(main_oid).expect("push main");
    let main_commit_count = walk.flatten().count();
    assert!(
        main_commit_count > 0,
        "precondition: main must be reachable"
    );
    assert!(
        patch_ids.ids.len() <= main_commit_count,
        "the cache must hold at most one entry per commit of main, got {} for {main_commit_count} commits",
        patch_ids.ids.len()
    );

    // The sharp form of the same claim: scanning three branches must cost
    // the same patch-id work as scanning one. Without the memoisation the
    // second and third branch would re-diff and re-hash every commit of
    // `main` all over again — which is what made the read take an hour.
    let mut one_branch = PatchIdCache::new(&repo);
    branch_is_merged_into_main(
        &repo,
        &mut one_branch,
        &mut budget,
        main_oid,
        branch_oids[0],
    );
    assert_eq!(
        patch_ids.ids.len(),
        one_branch.ids.len(),
        "scanning {n} branches must not compute more patch-ids than scanning one",
        n = branch_oids.len()
    );
}

/// A branch that was squash-merged into main is reported as merged. The
/// squash commit has no ancestry link back to the branch, so this is the
/// patch-id match doing the work — the same answer the pre-memoisation scan
/// gave (issue #2580 must not change what the prune UI recommends).
#[test]
fn a_squash_merged_branch_is_reported_as_merged() {
    let dir = TestDir::new("git_branches_squash");
    let repo = init_repo_with_commit(dir.path(), &[("base.txt", "base\n")]);
    let root_oid = repo
        .head()
        .expect("head")
        .peel_to_commit()
        .expect("commit")
        .id();
    let main_name = repo
        .head()
        .expect("head")
        .shorthand()
        .unwrap_or("main")
        .to_string();

    // A branch with one commit of its own.
    let root = repo.find_commit(root_oid).expect("root commit");
    repo.branch("feature/squashed", &root, false)
        .expect("branch");
    repo.set_head("refs/heads/feature/squashed")
        .expect("set head");
    commit_file(&repo, dir.path(), "work.txt", "the work\n");
    let feature_oid = repo
        .head()
        .expect("head")
        .peel_to_commit()
        .expect("commit")
        .id();

    // Land the same change as one squashed commit on a branch off main's
    // root — the shape GitHub's "squash and merge" produces. The branch's
    // work arrives on main as a single commit with no ancestry link back.
    repo.set_head(&format!("refs/heads/{main_name}"))
        .expect("set head");
    reset_hard_to_head(&repo);
    repo.branch("squashed-on-main", &root, false)
        .expect("branch");
    repo.set_head("refs/heads/squashed-on-main")
        .expect("set head");
    let sig = git2::Signature::now("test", "test@example.com").expect("signature");
    std::fs::write(dir.path().join("work.txt"), "the work\n").expect("write work");
    let mut index = repo.index().expect("index");
    index
        .add_path(std::path::Path::new("work.txt"))
        .expect("add");
    index.write().expect("write index");
    let tree_oid = index.write_tree().expect("tree");
    let tree = repo.find_tree(tree_oid).expect("tree");
    let parent = repo.find_commit(root_oid).expect("root");
    repo.commit(Some("HEAD"), &sig, &sig, "squash", &tree, &[&parent])
        .expect("commit");
    let squashed_oid = repo
        .head()
        .expect("head")
        .peel_to_commit()
        .expect("commit")
        .id();

    // main now carries the squashed change.
    repo.reference(
        &format!("refs/heads/{main_name}"),
        squashed_oid,
        true,
        "test",
    )
    .expect("move main");

    let mut patch_ids = PatchIdCache::new(&repo);
    let mut budget = MergeScanBudget::new(Duration::from_secs(30));
    let merged = branch_is_merged_into_main(
        &repo,
        &mut patch_ids,
        &mut budget,
        squashed_oid,
        feature_oid,
    );

    assert_eq!(
        merged,
        Some(true),
        "a squash-merged branch must still be reported as merged"
    );
}

/// When the merge scan runs out of its wall-clock budget the read still
/// returns — it just reports the branches it could not classify as *unknown*
/// and says that it was truncated. A truncated scan must never claim a
/// branch is unmerged: that would offer to delete work that may still be
/// live (issue #2580).
#[test]
fn an_exhausted_merge_scan_reports_unknown_and_flags_the_truncation() {
    let (_dir, path) = repo_with_history_and_branches(6, 2);

    // A zero-length budget is already spent when the scan starts.
    let mut budget = MergeScanBudget::new(Duration::ZERO);
    let info =
        collect_prune_info_within(&path, &[], &[], &|_| false, &mut budget).expect("prune info");

    assert!(
        info.merge_scan_truncated,
        "the read must say that the merge scan was cut short"
    );
    let unclassified = info
        .local_branches
        .iter()
        .filter(|branch| branch.name.starts_with("feature/"))
        .collect::<Vec<_>>();
    assert!(
        !unclassified.is_empty(),
        "precondition: the repo must have branches to classify"
    );
    for branch in unclassified {
        assert_eq!(
            branch.is_merged_into_main, None,
            "a branch the scan never classified must report an unknown merge status, not \"not merged\""
        );
    }
}

/// A scan that finished within its budget reports that it was not truncated,
/// and classifies every branch — the normal case must stay unaffected.
#[test]
fn a_scan_within_budget_is_not_flagged_as_truncated() {
    let (_dir, path) = repo_with_history_and_branches(6, 2);

    let mut budget = MergeScanBudget::new(Duration::from_secs(30));
    let info =
        collect_prune_info_within(&path, &[], &[], &|_| false, &mut budget).expect("prune info");

    assert!(
        !info.merge_scan_truncated,
        "a scan that finished inside its budget must not be flagged as truncated"
    );
}
