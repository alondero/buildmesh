// Sibling test suite for the owning module. The suite lives in its own
// file so the production module keeps only the declaration below — the
// repo's rust-test-layout gate requires it.

use super::*;
use crate::env::test_helpers::{init_repo_with_commit, TestDir};
use git2::Signature;
use std::fs;
use std::path::Path;

/// A repository with one committed file.
fn repo_with_commit() -> (TestDir, String) {
    let dir = TestDir::new("git_diff");
    init_repo_with_commit(dir.path(), &[("f.txt", "one\n")]);
    let path = dir.path().to_string_lossy().into_owned();
    (dir, path)
}

/// A commit on top of HEAD that changes `f.txt`.
fn commit_change(repo: &Repository, root: &Path, content: &str) -> git2::Oid {
    let signature = Signature::now("test", "test@example.com").expect("signature");
    fs::write(root.join("f.txt"), content).expect("write");
    let mut index = repo.index().expect("index");
    index.add_path(Path::new("f.txt")).expect("add");
    index.write().expect("write index");
    let tree_oid = index.write_tree().expect("tree");
    let tree = repo.find_tree(tree_oid).expect("tree");
    let parent = repo.head().expect("head").peel_to_commit().expect("commit");
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "change",
        &tree,
        &[&parent],
    )
    .expect("commit")
}

/// A base ref that shares history with HEAD resolves to the merge-base, so
/// the diff covers everything the branch changed since it forked.
#[test]
fn resolve_base_tree_uses_the_merge_base_of_head_and_the_base_ref() {
    let (dir, _path) = repo_with_commit();
    let repo = Repository::open(dir.path()).expect("reopen repo");
    let base_tree = repo
        .head()
        .expect("head")
        .peel_to_commit()
        .expect("commit")
        .tree()
        .expect("tree")
        .id();
    commit_change(&repo, dir.path(), "two\n");

    let resolved = resolve_base_tree(&repo, "HEAD~1").expect("base tree");

    assert_eq!(resolved.id(), base_tree);
}

/// Unrelated histories have no merge-base, so the base ref's own tree is the
/// baseline — the second arm of the fallback chain.
#[test]
fn resolve_base_tree_falls_back_to_the_base_ref_for_unrelated_histories() {
    let (dir, _path) = repo_with_commit();
    let repo = Repository::open(dir.path()).expect("reopen repo");
    let signature = Signature::now("test", "test@example.com").expect("signature");
    let tree_oid = repo
        .treebuilder(None)
        .expect("treebuilder")
        .write()
        .expect("tree");
    let tree = repo.find_tree(tree_oid).expect("tree");
    let orphan = repo
        .commit(None, &signature, &signature, "orphan", &tree, &[])
        .expect("commit");

    let resolved = resolve_base_tree(&repo, &orphan.to_string()).expect("base tree");

    assert_eq!(resolved.id(), tree_oid);
}

/// A base ref that cannot be resolved falls back to HEAD's tree, so the diff
/// still has a baseline.
#[test]
fn resolve_base_tree_falls_back_to_head_for_an_unresolvable_ref() {
    let (dir, _path) = repo_with_commit();
    let repo = Repository::open(dir.path()).expect("reopen repo");
    let head_tree = repo
        .head()
        .expect("head")
        .peel_to_commit()
        .expect("commit")
        .tree()
        .expect("tree")
        .id();

    let resolved = resolve_base_tree(&repo, "no-such-ref").expect("base tree");

    assert_eq!(resolved.id(), head_tree);
}

/// A repository with no commits has no tree to diff against — the empty
/// tree is the honest baseline, and the resolution says so with `None`.
#[test]
fn resolve_base_tree_is_none_for_an_unborn_head() {
    let dir = TestDir::new("git_diff_unborn");
    Repository::init(dir.path()).expect("init repo");
    let repo = Repository::open(dir.path()).expect("reopen repo");

    assert!(resolve_base_tree(&repo, "HEAD").is_none());
}

/// Every delta maps onto the vocabulary the UI renders.
#[test]
fn delta_status_str_maps_deltas_to_the_buildmesh_vocabulary() {
    assert_eq!(delta_status_str(git2::Delta::Added), "added");
    assert_eq!(delta_status_str(git2::Delta::Deleted), "deleted");
    assert_eq!(delta_status_str(git2::Delta::Renamed), "renamed");
    assert_eq!(delta_status_str(git2::Delta::Copied), "renamed");
    assert_eq!(delta_status_str(git2::Delta::Untracked), "untracked");
    assert_eq!(delta_status_str(git2::Delta::Modified), "modified");
}

/// The baseline side of a file is read out of the tree; a path that is not in
/// the tree has no content to show.
#[test]
fn tree_blob_string_reads_the_baseline_content() {
    let (dir, _path) = repo_with_commit();
    let repo = Repository::open(dir.path()).expect("reopen repo");
    let tree = repo.head().expect("head").peel_to_tree().expect("tree");

    assert_eq!(tree_blob_string(&repo, Some(&tree), "f.txt"), "one\n");
    assert_eq!(tree_blob_string(&repo, Some(&tree), "missing.txt"), "");
    assert_eq!(tree_blob_string(&repo, None, "f.txt"), "");
}

/// The HEAD side of a file carries its committed content and the worktree
/// root the caller reads the other side from.
#[test]
fn head_file_at_returns_the_committed_content_and_the_worktree_root() {
    let (dir, path) = repo_with_commit();

    let file = head_file_at(&path, "f.txt").expect("head file");

    assert_eq!(file.content.as_deref(), Some("one\n"));
    assert_eq!(
        file.root.canonicalize().expect("canonical root"),
        dir.path().canonicalize().expect("canonical dir")
    );
}

/// A path HEAD never had has no committed content — the read says so with
/// `None` rather than an error.
#[test]
fn head_file_at_has_no_content_for_a_path_head_does_not_have() {
    let (_dir, path) = repo_with_commit();

    let file = head_file_at(&path, "missing.txt").expect("head file");

    assert!(file.content.is_none());
}

/// The plan lists the changed files with their baseline-side content, so the
/// per-file diff view has both sides.
#[test]
fn diff_plan_lists_the_changed_files_with_their_baseline_content() {
    let (dir, path) = repo_with_commit();
    fs::write(dir.path().join("f.txt"), "one\ntwo\n").expect("write");

    let plan = diff_plan(&path, &Baseline::WorkingTree, None).expect("plan");

    let entry = plan
        .entries
        .iter()
        .find(|entry| entry.path == "f.txt")
        .expect("entry");
    assert_eq!(entry.status, "modified");
    assert_eq!(entry.old_content, "one\n");
}

/// A file the baseline does not have has no baseline content to show.
#[test]
fn diff_plan_reports_an_untracked_file_with_no_baseline_content() {
    let (dir, path) = repo_with_commit();
    fs::write(dir.path().join("new.txt"), "hello\n").expect("write");

    let plan = diff_plan(&path, &Baseline::WorkingTree, None).expect("plan");

    let entry = plan
        .entries
        .iter()
        .find(|entry| entry.path == "new.txt")
        .expect("entry");
    assert_eq!(entry.status, "untracked");
    assert!(entry.old_content.is_empty());
}

/// The plan can be narrowed to one path — the per-file diff view asks for
/// exactly that.
#[test]
fn diff_plan_can_be_restricted_to_one_path() {
    let (dir, path) = repo_with_commit();
    fs::write(dir.path().join("f.txt"), "one\ntwo\n").expect("write");
    fs::write(dir.path().join("other.txt"), "other\n").expect("write");

    let plan = diff_plan(&path, &Baseline::WorkingTree, Some("f.txt")).expect("plan");

    assert_eq!(plan.entries.len(), 1);
    assert_eq!(plan.entries[0].path, "f.txt");
}
