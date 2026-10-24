//! Tests for the capability-shaped state reads (issue #2526).
//!
//! Two behaviours are pinned here:
//!
//! 1. **The two changed-file views agree.** The Changed Files Section
//!    (`git::state`'s working-tree rows) and the Node Change Set
//!    (`git::state`'s base-ref list) used to compute a file's line counts with
//!    two implementations, so the two views could disagree. They now share
//!    one, and this test holds them to it.
//! 2. **The unborn-HEAD fallback.** A repository with no commits has no tree
//!    to diff against; every file in it reads as an addition against the
//!    empty tree. That fallback predates this module and must not change.

use std::collections::HashMap;

use super::{state_at, Baseline};
use crate::env::test_helpers::{init_repo_with_commit, TestDir};

/// A repository with a mixed change set: one modified file, one deleted
/// file, one staged addition, one untracked file, and one ignored file.
///
/// The committed `.gitignore` covers `ignored.txt`, so that file must not
/// appear in either view.
fn mixed_change_set() -> TestDir {
    let td = TestDir::new("state_mixed_change_set");
    let root = td.path();
    init_repo_with_commit(
        root,
        &[
            ("modified.txt", "one\ntwo\nthree\n"),
            ("deleted.txt", "gone\n"),
            (".gitignore", "ignored.txt\n"),
        ],
    );

    // Modified: "two" becomes "two-changed", and a fourth line is appended.
    std::fs::write(root.join("modified.txt"), "one\ntwo-changed\nthree\nfour\n").unwrap();
    // Deleted from the working tree.
    std::fs::remove_file(root.join("deleted.txt")).unwrap();
    // Staged addition — in the index, so it reads as "added", not "untracked".
    std::fs::write(root.join("added.txt"), "new\nlines\n").unwrap();
    let repo = git2::Repository::open(root).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("added.txt")).unwrap();
    index.write().unwrap();
    // Untracked — on disk, not in the index.
    std::fs::write(root.join("untracked.txt"), "loose\n").unwrap();
    // Ignored — covered by the committed `.gitignore`.
    std::fs::write(root.join("ignored.txt"), "hidden\n").unwrap();

    td
}

/// The line counts of a view, keyed by path.
fn counts_by_path<T>(
    entries: &[T],
    path: impl Fn(&T) -> &str,
    counts: impl Fn(&T) -> (usize, usize),
) -> HashMap<String, (usize, usize)> {
    entries
        .iter()
        .map(|entry| (path(entry).to_string(), counts(entry)))
        .collect()
}

/// The two views agree on the line counts of every changed file.
///
/// The Changed Files Section reads the working tree; the Node Change Set
/// reads a base ref. Pointed at the same baseline — `HEAD` — they must
/// describe the same change set with the same line counts, because both
/// derive the counts from `git::status_stats::delta_counts`.
#[test]
fn the_two_changed_file_views_agree_on_line_counts() {
    let td = mixed_change_set();
    let path = td.path().to_str().unwrap();

    let working = state_at(path, Baseline::WorkingTree).unwrap();
    let since_head = state_at(path, Baseline::BaseRef("HEAD".to_string())).unwrap();

    let status_counts = counts_by_path(
        &working.statuses,
        |entry| entry.path.as_str(),
        |entry| (entry.additions, entry.deletions),
    );
    let change_counts = counts_by_path(
        &since_head.changed_files,
        |entry| entry.path.as_str(),
        |entry| (entry.additions, entry.deletions),
    );

    // The mixed set is present in both views.
    for expected in ["modified.txt", "deleted.txt", "added.txt", "untracked.txt"] {
        assert!(
            status_counts.contains_key(expected),
            "{expected} missing from the working-tree view: {status_counts:?}"
        );
        assert!(
            change_counts.contains_key(expected),
            "{expected} missing from the change-set view: {change_counts:?}"
        );
    }

    // The ignored file is in neither view.
    assert!(
        !status_counts.contains_key("ignored.txt"),
        "the ignored file must not be listed as changed"
    );
    assert!(
        !change_counts.contains_key("ignored.txt"),
        "the ignored file must not be listed as changed"
    );

    // The line counts agree, file for file.
    for (path, counts) in &status_counts {
        assert_eq!(
            change_counts.get(path),
            Some(counts),
            "the two views disagree about {path}: status {counts:?} vs change-set {:?}",
            change_counts.get(path)
        );
    }
    assert_eq!(
        status_counts
            .keys()
            .collect::<std::collections::BTreeSet<_>>(),
        change_counts
            .keys()
            .collect::<std::collections::BTreeSet<_>>(),
        "the two views cover different files"
    );

    // And the counts are the ones libgit2 reports — pinned so a future
    // change to the shared implementation cannot quietly change the UI.
    // "modified.txt" changes one line and appends another: one deletion,
    // two additions.
    assert_eq!(status_counts.get("modified.txt"), Some(&(2, 1)));
    assert_eq!(status_counts.get("deleted.txt"), Some(&(0, 1)));
    assert_eq!(status_counts.get("added.txt"), Some(&(2, 0)));
    assert_eq!(status_counts.get("untracked.txt"), Some(&(1, 0)));
}

/// The two views label an untracked file differently, and that difference is
/// deliberate.
///
/// The status walk reports a new file as "added" whatever the index says,
/// while the diff walk distinguishes a staged addition from a loose file.
/// The line counts still agree — that is what the shared implementation
/// guarantees — but the labels are each view's own vocabulary, and the UI
/// shows them as they are.
#[test]
fn the_two_views_keep_their_own_label_for_an_untracked_file() {
    let td = mixed_change_set();
    let path = td.path().to_str().unwrap();

    let working = state_at(path, Baseline::WorkingTree).unwrap();
    let since_head = state_at(path, Baseline::BaseRef("HEAD".to_string())).unwrap();

    let status_label = working
        .statuses
        .iter()
        .find(|entry| entry.path == "untracked.txt")
        .map(|entry| entry.status.as_str());
    let change_label = since_head
        .changed_files
        .iter()
        .find(|entry| entry.path == "untracked.txt")
        .map(|entry| entry.status.as_str());

    assert_eq!(status_label, Some("added"));
    assert_eq!(change_label, Some("untracked"));
}

/// A repository with no commits has no tree to diff against.
///
/// Every file in it reads as an addition against the empty tree — the
/// fallback the working-tree baseline has always used, and the one the
/// line-count path depends on.
#[test]
fn an_unborn_head_falls_back_to_the_empty_tree() {
    let td = TestDir::new("state_unborn_head");
    let root = td.path();
    git2::Repository::init(root).unwrap();
    std::fs::write(root.join("first.txt"), "one\ntwo\n").unwrap();
    std::fs::write(root.join("second.txt"), "three\n").unwrap();

    let state = state_at(root.to_str().unwrap(), Baseline::WorkingTree).unwrap();

    // HEAD is unborn: there is no branch, and the state says so rather than
    // failing.
    assert!(state.head.unborn);
    assert_eq!(state.head.branch, None);
    assert_eq!(state.head.short_sha, "");

    // Every file is an addition against the empty tree.
    let counts = counts_by_path(
        &state.statuses,
        |entry| entry.path.as_str(),
        |entry| (entry.additions, entry.deletions),
    );
    assert_eq!(counts.get("first.txt"), Some(&(2, 0)));
    assert_eq!(counts.get("second.txt"), Some(&(1, 0)));

    // And the summary counts them as additions.
    assert_eq!(state.summary.total, 2);
    assert_eq!(state.summary.added, 2);
    assert_eq!(state.summary.modified, 0);
    assert_eq!(state.summary.deleted, 0);
}

/// An unborn HEAD reads the same through the base-ref baseline.
#[test]
fn an_unborn_head_reads_the_same_through_a_base_ref() {
    let td = TestDir::new("state_unborn_head_base_ref");
    let root = td.path();
    git2::Repository::init(root).unwrap();
    std::fs::write(root.join("only.txt"), "one\ntwo\n").unwrap();

    let state = state_at(
        root.to_str().unwrap(),
        Baseline::BaseRef("HEAD".to_string()),
    )
    .unwrap();

    assert!(state.head.unborn);
    let counts = counts_by_path(
        &state.changed_files,
        |entry| entry.path.as_str(),
        |entry| (entry.additions, entry.deletions),
    );
    assert_eq!(counts.get("only.txt"), Some(&(2, 0)));
}

/// A clean repository reports nothing changed, and never reaches the diff.
#[test]
fn a_clean_repository_reports_nothing_changed() {
    let td = TestDir::new("state_clean_repo");
    let root = td.path();
    init_repo_with_commit(root, &[("kept.txt", "content\n")]);

    let state = state_at(root.to_str().unwrap(), Baseline::WorkingTree).unwrap();

    assert!(state.statuses.is_empty());
    assert!(state.changed_files.is_empty());
    assert_eq!(state.summary.total, 0);
    // HEAD is a real commit here. The branch name depends on the machine's
    // `init.defaultBranch`, so only its presence is pinned.
    assert!(!state.head.unborn);
    assert!(state.head.branch.is_some());
    assert!(!state.head.short_sha.is_empty());
}
