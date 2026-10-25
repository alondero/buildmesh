// Sibling test suite for the owning module. The suite lives in its own
// file so the production module keeps only the declaration below — the
// repo's rust-test-layout gate requires it.

use super::*;
use crate::env::test_helpers::{init_repo_with_commit, TestDir};
use git2::Repository;
use std::fs;
use std::path::Path;

/// A repository with one committed file.
fn repo_with_commit() -> (TestDir, String) {
    let dir = TestDir::new("git_index");
    init_repo_with_commit(dir.path(), &[("f.txt", "one\n")]);
    let path = dir.path().to_string_lossy().into_owned();
    (dir, path)
}

/// Staging a new file puts it in the index.
#[test]
fn stage_path_adds_a_new_file_to_the_index() {
    let (dir, path) = repo_with_commit();
    fs::write(dir.path().join("new.txt"), "hello\n").expect("write");

    stage_path(&path, "new.txt").expect("stage");

    let repo = Repository::open(dir.path()).expect("reopen repo");
    let index = repo.index().expect("index");
    assert!(
        index.get_path(Path::new("new.txt"), 0).is_some(),
        "the staged file must be in the index"
    );
}

/// Staging a file that is missing on disk stages the *deletion* — the index
/// must not keep an entry for a file the worktree no longer has.
#[test]
fn stage_path_stages_the_deletion_of_a_file_that_is_missing_on_disk() {
    let (dir, path) = repo_with_commit();
    fs::remove_file(dir.path().join("f.txt")).expect("remove");

    stage_path(&path, "f.txt").expect("stage deletion");

    let repo = Repository::open(dir.path()).expect("reopen repo");
    let index = repo.index().expect("index");
    assert!(
        index.get_path(Path::new("f.txt"), 0).is_none(),
        "the deletion must be staged, not left in the index"
    );
}

/// Staging a modified file updates the index entry to the worktree content.
#[test]
fn stage_path_updates_the_index_entry_for_a_modified_file() {
    let (dir, path) = repo_with_commit();
    fs::write(dir.path().join("f.txt"), "two\n").expect("write");

    stage_path(&path, "f.txt").expect("stage");

    let repo = Repository::open(dir.path()).expect("reopen repo");
    let index = repo.index().expect("index");
    let entry = index.get_path(Path::new("f.txt"), 0).expect("index entry");
    let blob = repo.find_blob(entry.id).expect("blob");
    assert_eq!(blob.content(), b"two\n");
}

/// Reverting a tracked file puts the committed content back — the file's
/// local edit is gone and the worktree matches HEAD again.
#[test]
fn revert_path_restores_a_modified_tracked_file() {
    let (dir, path) = repo_with_commit();
    fs::write(dir.path().join("f.txt"), "changed\n").expect("write");

    revert_path(&path, "f.txt").expect("revert");

    assert_eq!(
        fs::read_to_string(dir.path().join("f.txt")).expect("read"),
        "one\n"
    );
}

/// Reverting a file that HEAD never had removes it: there is nothing to
/// restore to.
#[test]
fn revert_path_removes_a_file_that_head_does_not_contain() {
    let (dir, path) = repo_with_commit();
    fs::write(dir.path().join("untracked.txt"), "x\n").expect("write");

    revert_path(&path, "untracked.txt").expect("revert");

    assert!(
        !dir.path().join("untracked.txt").exists(),
        "a file HEAD never had must be removed"
    );
}

/// Reverting a newly staged file removes the file *and* its index entry — a
/// revert that left the entry behind would show the file as staged-but-
/// missing.
#[test]
fn revert_path_drops_a_staged_new_file() {
    let (dir, path) = repo_with_commit();
    fs::write(dir.path().join("new.txt"), "x\n").expect("write");
    stage_path(&path, "new.txt").expect("stage");

    revert_path(&path, "new.txt").expect("revert");

    assert!(!dir.path().join("new.txt").exists());
    let repo = Repository::open(dir.path()).expect("reopen repo");
    let index = repo.index().expect("index");
    assert!(
        index.get_path(Path::new("new.txt"), 0).is_none(),
        "the staged entry must be dropped with the file"
    );
}

/// The structural gate: a path that resolves outside the worktree is
/// refused, even though the command layer's own traversal check is the
/// load-bearing one. This is the defence in depth behind it.
#[test]
fn stage_path_refuses_a_path_that_escapes_the_repository() {
    let dir = TestDir::new("git_index_escape");
    let repo_dir = dir.path().join("repo");
    fs::create_dir_all(&repo_dir).expect("mkdir");
    init_repo_with_commit(&repo_dir, &[("f.txt", "one\n")]);
    fs::write(dir.path().join("outside.txt"), "x\n").expect("write");
    let path = repo_dir.to_string_lossy().into_owned();

    let error = stage_path(&path, "../outside.txt").expect_err("must refuse");

    assert!(
        error.contains("escapes repository"),
        "the error must name the escape, got {error}"
    );
}
