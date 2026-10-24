// Sibling test suite for the owning module. The suite lives in its own
// file so the production module keeps only the declaration below — the
// repo's rust-test-layout gate requires it.

use super::*;
use crate::env::test_helpers::{init_repo_with_commit, TestDir};
use git2::BranchType;

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
