//! The diff engine's own suite — moved with the engine (issue #2527).
//!
//! The test names are the ones the suite had inside `commands/diff.rs`; the
//! module they live in moved, the assertions did not.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::diff::cancel::{
    cancel_poll_count, set_cancel_poll_probe, DIFF_CANCELLED,
};
use crate::diff::engine::{
    build_hunk, build_sides, compute_file_diff, diff_against_base, group_into_hunks,
    CONTEXT_LINES,
};
use crate::diff::{changed_files_at, file_against_head};
use crate::env::test_helpers::{commit_file, init_repo_with_commit, repo_is_dirty, TestDir};
use crate::models::{DiffLine, DiffResult};

#[test]
fn scoped_project_diffs_remain_repository_relative() {
    let td = TestDir::new("scoped-diff");
    init_repo_with_commit(
        td.path(),
        &[
            ("family/parents/file.txt", "before\n"),
            ("sibling.txt", "before\n"),
        ],
    );
    std::fs::write(td.path().join("family/parents/file.txt"), "after\n").unwrap();
    std::fs::write(td.path().join("sibling.txt"), "outside\n").unwrap();
    let selected = td
        .path()
        .join("family/parents")
        .to_string_lossy()
        .to_string();
    let diff = file_against_head(&selected, "family/parents/file.txt").unwrap();
    assert_eq!(diff.files[0].path, "family/parents/file.txt");
    assert!(!diff.files[0].hunks.is_empty());
    let diff = diff_against_base(&selected, "HEAD", None, None).unwrap();
    assert!(diff.files.iter().any(|file| file.path == "sibling.txt"));
}

/// The baseline bug (mobile Changes view): a file the agent *committed* is
/// clean in the working tree, so a HEAD-relative status omits it — yet the
/// per-file diff (base_ref merge-base) shows it, so the file appears in the
/// tapped diff but never in the tree. The since-branch list must include it.
#[test]
fn changed_files_lists_file_committed_since_base() {
    let dir = TestDir::new("changed_committed_since_base");
    // Base commit, recorded as origin/main (the merge-base baseline).
    let repo = init_repo_with_commit(dir.path(), &[("base.txt", "base\n")]);
    let base_oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    repo.reference("refs/remotes/origin/main", base_oid, false, "test")
        .unwrap();
    // The agent COMMITS a new file → HEAD advances, working tree stays clean.
    commit_file(&repo, dir.path(), "foo.rs", "fn main() {}\n");
    assert!(
        !repo_is_dirty(dir.path()),
        "precondition: the worktree is clean after committing foo.rs"
    );

    let files = changed_files_at(dir.path().to_str().unwrap(), "origin/main").unwrap();
    assert!(
        files
            .iter()
            .any(|f| f.path == "foo.rs" && f.status == "added"),
        "a file committed since base must appear in the since-branch list; got {:?}",
        files
    );
}

/// Uncommitted edits since base show too — the since-branch set is committed
/// *and* uncommitted, matching the per-file diff.
#[test]
fn changed_files_lists_uncommitted_edit_since_base() {
    let dir = TestDir::new("changed_uncommitted_since_base");
    let repo = init_repo_with_commit(dir.path(), &[("base.txt", "base\n")]);
    let base_oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    repo.reference("refs/remotes/origin/main", base_oid, false, "test")
        .unwrap();
    // Edit base.txt on disk without committing.
    std::fs::write(dir.path().join("base.txt"), "base\nedited\n").unwrap();

    let files = changed_files_at(dir.path().to_str().unwrap(), "origin/main").unwrap();
    assert!(
        files
            .iter()
            .any(|f| f.path == "base.txt" && f.status == "modified"),
        "an uncommitted edit since base must appear in the list; got {:?}",
        files
    );
}

/// The list and the per-file diff must agree on what "changed" means — the
/// whole point of the `git::diff::resolve_base_tree` seam. A zero-byte
/// untracked file was a real disagreement point: it would be invisible to a
/// diff that doesn't set `show_untracked_content`, yet visible to the status
/// walk. Both readers now set the option; this test pins the agreement so a
/// future opts change can't silently re-introduce the mismatch.
#[test]
fn changed_files_and_diff_agree_on_zero_byte_untracked() {
    let dir = TestDir::new("zero_byte_untracked_agreement");
    let repo = init_repo_with_commit(dir.path(), &[("base.txt", "base\n")]);
    let base_oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    repo.reference("refs/remotes/origin/main", base_oid, false, "test")
        .unwrap();
    // Empty untracked file: would be invisible to a diff without
    // `show_untracked_content`; must appear in both readers.
    std::fs::write(dir.path().join("empty.rs"), b"").unwrap();

    let list_paths: std::collections::HashSet<String> =
        changed_files_at(dir.path().to_str().unwrap(), "origin/main")
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();
    let diff_paths: std::collections::HashSet<String> =
        diff_against_base(dir.path().to_str().unwrap(), "origin/main", None, None)
            .unwrap()
            .files
            .into_iter()
            .map(|f| f.path)
            .collect();
    assert!(
        list_paths.contains("empty.rs"),
        "empty untracked file must appear in the since-branch list; got {:?}",
        list_paths
    );
    assert!(
        diff_paths.contains("empty.rs"),
        "empty untracked file must appear in the per-file diff; got {:?}",
        diff_paths
    );
}

#[test]
fn compute_file_diff_identical_files() {
    let lines = compute_file_diff("hello\nworld", "hello\nworld");
    assert!(lines.iter().all(|l| l.line_type == "context"));
    assert_eq!(lines.len(), 2);
}

#[test]
fn compute_file_diff_added_lines() {
    let lines = compute_file_diff("a\n", "a\nb\n");
    let adds: Vec<_> = lines.iter().filter(|l| l.line_type == "add").collect();
    assert!(!adds.is_empty());
    assert!(adds.iter().any(|l| l.content == "b"));
}

#[test]
fn compute_file_diff_removed_lines() {
    let lines = compute_file_diff("a\nb\nc\n", "a\nc\n");
    let removes: Vec<_> = lines.iter().filter(|l| l.line_type == "remove").collect();
    assert!(!removes.is_empty());
    assert!(removes.iter().any(|l| l.content == "b"));
}

#[test]
fn compute_file_diff_empty_old() {
    let lines = compute_file_diff("", "new content\n");
    assert!(lines.iter().all(|l| l.line_type == "add"));
    assert!(!lines.is_empty());
}

#[test]
fn compute_file_diff_empty_new() {
    let lines = compute_file_diff("old content\n", "");
    assert!(lines.iter().all(|l| l.line_type == "remove"));
    assert!(!lines.is_empty());
}

#[test]
fn compute_file_diff_both_empty() {
    let lines = compute_file_diff("", "");
    assert!(lines.is_empty());
}

#[test]
fn compute_file_diff_line_numbers_context() {
    let lines = compute_file_diff("a\nb\nc\n", "a\nb\nc\n");
    assert_eq!(lines[0].old_num, Some(1));
    assert_eq!(lines[0].new_num, Some(1));
    assert_eq!(lines[2].old_num, Some(3));
    assert_eq!(lines[2].new_num, Some(3));
}

#[test]
fn compute_file_diff_add_has_no_old_num() {
    let lines = compute_file_diff("a\n", "a\nb\n");
    let add = lines.iter().find(|l| l.line_type == "add").unwrap();
    assert_eq!(add.old_num, None);
    assert!(add.new_num.is_some());
}

#[test]
fn compute_file_diff_remove_has_no_new_num() {
    let lines = compute_file_diff("a\nb\n", "a\n");
    let rem = lines.iter().find(|l| l.line_type == "remove").unwrap();
    assert!(rem.old_num.is_some());
    assert_eq!(rem.new_num, None);
}

#[test]
fn build_sides_separates_old_and_new() {
    let lines = vec![
        DiffLine {
            line_type: "context".to_string(),
            content: "same".to_string(),
            old_num: Some(1),
            new_num: Some(1),
        },
        DiffLine {
            line_type: "remove".to_string(),
            content: "old".to_string(),
            old_num: Some(2),
            new_num: None,
        },
        DiffLine {
            line_type: "add".to_string(),
            content: "new".to_string(),
            old_num: None,
            new_num: Some(2),
        },
    ];
    let (old, new) = build_sides(&lines);
    assert!(old.contains("same"));
    assert!(old.contains("old"));
    assert!(new.contains("same"));
    assert!(new.contains("new"));
}

#[test]
fn build_sides_empty_input() {
    let (old, new) = build_sides(&[]);
    assert_eq!(old, "");
    assert_eq!(new, "");
}

#[test]
fn group_into_hunks_empty_when_no_changes() {
    let lines = vec![
        DiffLine {
            line_type: "context".into(),
            content: "a".into(),
            old_num: Some(1),
            new_num: Some(1),
        },
        DiffLine {
            line_type: "context".into(),
            content: "b".into(),
            old_num: Some(2),
            new_num: Some(2),
        },
    ];
    assert!(group_into_hunks(&lines, 3).is_empty());
}

#[test]
fn group_into_hunks_bounds_context_for_isolated_change() {
    // 20 context lines, one change at index 20, 19 trailing context.
    // With context=3 the result must be a single hunk spanning indices
    // [17, 24) — the 3 context lines before the change plus the change
    // itself plus 3 after — regardless of how many other unchanged
    // lines are in the file.
    let mut lines = Vec::new();
    for i in 0..20 {
        lines.push(DiffLine {
            line_type: "context".into(),
            content: format!("a{}", i),
            old_num: Some(i + 1),
            new_num: Some(i + 1),
        });
    }
    lines.push(DiffLine {
        line_type: "add".into(),
        content: "NEW".into(),
        old_num: None,
        new_num: Some(21),
    });
    for i in 21..40 {
        lines.push(DiffLine {
            line_type: "context".into(),
            content: format!("a{}", i),
            old_num: Some(i + 1),
            new_num: Some(i + 1),
        });
    }

    let hunks = group_into_hunks(&lines, 3);
    assert_eq!(hunks.len(), 1);
    assert_eq!(
        hunks[0].len(),
        7,
        "expected 3 context + 1 add + 3 context = 7"
    );
    // First 3 are context, then the add, then 3 more context.
    for line in &hunks[0][..3] {
        assert_eq!(line.line_type, "context");
    }
    assert_eq!(hunks[0][3].line_type, "add");
    for line in &hunks[0][4..] {
        assert_eq!(line.line_type, "context");
    }
}

#[test]
fn group_into_hunks_splits_far_apart_changes() {
    // Two changes 50 context lines apart must produce two separate hunks
    // (one hunk would re-introduce the "entire file" rendering we are
    // trying to avoid).
    let mut lines = Vec::new();
    for i in 0..10 {
        lines.push(DiffLine {
            line_type: "context".into(),
            content: format!("a{}", i),
            old_num: Some(i + 1),
            new_num: Some(i + 1),
        });
    }
    lines.push(DiffLine {
        line_type: "add".into(),
        content: "FIRST".into(),
        old_num: None,
        new_num: Some(11),
    });
    for i in 11..61 {
        lines.push(DiffLine {
            line_type: "context".into(),
            content: format!("a{}", i),
            old_num: Some(i + 1),
            new_num: Some(i + 1),
        });
    }
    lines.push(DiffLine {
        line_type: "add".into(),
        content: "SECOND".into(),
        old_num: None,
        new_num: Some(62),
    });
    for i in 62..70 {
        lines.push(DiffLine {
            line_type: "context".into(),
            content: format!("a{}", i),
            old_num: Some(i + 1),
            new_num: Some(i + 1),
        });
    }

    let hunks = group_into_hunks(&lines, 3);
    assert_eq!(
        hunks.len(),
        2,
        "expected two hunks, got {}: {:#?}",
        hunks.len(),
        hunks
    );
    assert!(hunks[0].iter().any(|l| l.content == "FIRST"));
    assert!(hunks[1].iter().any(|l| l.content == "SECOND"));
    // Neither hunk should contain the full file (50 context lines
    // between changes must be dropped).
    assert!(hunks[0].len() < 10);
    assert!(hunks[1].len() < 10);
}

#[test]
fn group_into_hunks_merges_close_changes() {
    // Two changes 4 context lines apart (≤ 2 * CONTEXT) should merge
    // into a single hunk with a shared context window in the middle.
    let mut lines = Vec::new();
    for i in 0..5 {
        lines.push(DiffLine {
            line_type: "context".into(),
            content: format!("a{}", i),
            old_num: Some(i + 1),
            new_num: Some(i + 1),
        });
    }
    lines.push(DiffLine {
        line_type: "add".into(),
        content: "FIRST".into(),
        old_num: None,
        new_num: Some(6),
    });
    for i in 6..10 {
        lines.push(DiffLine {
            line_type: "context".into(),
            content: format!("a{}", i),
            old_num: Some(i + 1),
            new_num: Some(i + 1),
        });
    }
    lines.push(DiffLine {
        line_type: "add".into(),
        content: "SECOND".into(),
        old_num: None,
        new_num: Some(11),
    });
    for i in 11..15 {
        lines.push(DiffLine {
            line_type: "context".into(),
            content: format!("a{}", i),
            old_num: Some(i + 1),
            new_num: Some(i + 1),
        });
    }

    let hunks = group_into_hunks(&lines, 3);
    assert_eq!(hunks.len(), 1, "close changes should merge into one hunk");
}

// Regression: clicking a changed file used to render the entire file as
// context lines, drowning the actual diff. The diff must be grouped into
// hunks with a small window of context around each change (GitHub-style)
// so the user only sees the changed regions. We pin the end-to-end
// contract via the public `diff_files` command.
#[test]
fn compute_file_diff_bounds_context_for_small_change_in_large_file() {
    // 100-line file, one line changed at position 50.
    let mut old_content = String::new();
    for i in 0..100 {
        old_content.push_str(&format!("line {}\n", i));
    }
    let mut new_content = old_content.clone();
    let pos = old_content.find("line 50\n").unwrap();
    let end = pos + "line 50\n".len();
    new_content.replace_range(pos..end, "line FIFTY\n");

    // Today: the full 100 unchanged lines are returned as context plus
    // 1 remove + 1 add = 102. The fix groups lines into hunks and drops
    // long unchanged stretches, so the post-fix total should be a small
    // window around the change (~10-20 lines), not 102.
    //
    // To make the assertion meaningful regardless of which grouping API
    // ends up winning, we test the public command entry point: assemble
    // a tiny on-disk repo with one tracked file, call the Tauri command
    // (via the helper that doesn't require Tauri runtime), and bound
    // the returned `DiffResult` size.
    //
    // The helper used here (`diff_files`) wraps `compute_file_diff` plus
    // a single hunk; once hunk grouping is in place this helper should
    // be updated to produce multiple hunks and the test re-pinned to
    // `diff_file_against_head`. We assert a budget that the unfixed
    // implementation violates (~102 lines) and the fixed implementation
    // satisfies (under 30 lines).
    let tmp = DiffFixture::new();
    let old = tmp.join("old.txt");
    let new = tmp.join("new.txt");
    std::fs::write(&old, &old_content).unwrap();
    std::fs::write(&new, &new_content).unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(crate::commands::diff::diff_files(
        old.to_string_lossy().to_string(),
        new.to_string_lossy().to_string(),
    ));
    let result = result.expect("diff_files should succeed");

    // The public helper today still emits a single hunk containing every
    // line. Once hunk grouping is in place, total line count across all
    // hunks should be bounded to the context window.
    let total_lines: usize = result
        .files
        .iter()
        .flat_map(|f| f.hunks.iter())
        .map(|h| h.lines.len())
        .sum();
    assert!(
        total_lines < 30,
        "expected bounded diff (<30 lines) for a 1-line change in a 100-line file, got {} lines",
        total_lines
    );
}

// ----- merge-base diff (ADR 0005) -----------------------------------

/// Init a repo with a single base commit (`base.txt`) and a `base` branch
/// pointing at it, so tests can use `"base"` as `base_ref`.
fn init_repo_with_base(dir: &std::path::Path) -> git2::Repository {
    let repo = git2::Repository::init(dir).unwrap();
    let sig = git2::Signature::now("t", "t@example.com").unwrap();
    std::fs::write(dir.join("base.txt"), "one\ntwo\nthree\n").unwrap();
    // Scope every borrow of `repo` so it can be moved out on return.
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("base.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
            .unwrap();
        let base_commit = repo.find_commit(oid).unwrap();
        repo.branch("base", &base_commit, true).unwrap();
    }
    repo
}

/// Stage every change under the repo and commit it, advancing HEAD past the
/// `base` branch.
fn commit_all(repo: &git2::Repository, msg: &str) {
    let sig = git2::Signature::now("t", "t@example.com").unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &[&parent])
        .unwrap();
}

#[test]
fn diff_against_base_shows_committed_and_uncommitted_work() {
    let tmp = DiffFixture::new();
    let repo = init_repo_with_base(&tmp);

    // The agent commits a new file after branching...
    std::fs::write(tmp.join("committed.txt"), "a\nb\n").unwrap();
    commit_all(&repo, "agent work");
    // ...and leaves an uncommitted edit to an existing file.
    std::fs::write(tmp.join("base.txt"), "one\nTWO\nthree\n").unwrap();

    let result = diff_against_base(tmp.to_str().unwrap(), "base", None, None).unwrap();
    let paths: Vec<&str> = result.files.iter().map(|f| f.path.as_str()).collect();
    assert!(
        paths.contains(&"committed.txt"),
        "committed file missing: {:?}",
        paths
    );
    assert!(
        paths.contains(&"base.txt"),
        "uncommitted edit missing: {:?}",
        paths
    );

    let committed = result
        .files
        .iter()
        .find(|f| f.path == "committed.txt")
        .unwrap();
    assert_eq!(committed.status, "added");
    assert_eq!(committed.additions, 2);
    assert_eq!(committed.deletions, 0);
}

#[test]
fn diff_against_base_detects_rename() {
    let tmp = DiffFixture::new();
    let repo = init_repo_with_base(&tmp);

    std::fs::remove_file(tmp.join("base.txt")).unwrap();
    std::fs::write(tmp.join("renamed.txt"), "one\ntwo\nthree\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.remove_path(std::path::Path::new("base.txt")).unwrap();
        index.add_path(std::path::Path::new("renamed.txt")).unwrap();
        index.write().unwrap();
    }
    commit_all(&repo, "rename base.txt");

    let result = diff_against_base(tmp.to_str().unwrap(), "base", None, None).unwrap();
    let renamed = result
        .files
        .iter()
        .find(|f| f.path == "renamed.txt")
        .unwrap_or_else(|| {
            panic!(
                "renamed file missing: {:?}",
                result
                    .files
                    .iter()
                    .map(|f| (&f.path, &f.status))
                    .collect::<Vec<_>>()
            )
        });
    assert_eq!(renamed.status, "renamed");
    assert_eq!(renamed.old_path.as_deref(), Some("base.txt"));
}

#[test]
fn diff_against_base_falls_back_to_head_when_base_unresolvable() {
    let tmp = DiffFixture::new();
    // Drop the repo handle immediately — this test only needs the
    // on-disk repo structure, and `diff_against_base` opens its own
    // handle internally. Holding `_repo` would just keep the FDs
    // (and any Windows file locks) for the entire test, which the
    // Round 3 reviewer flagged as a Windows file-locking hazard.
    init_repo_with_base(&tmp);

    std::fs::write(tmp.join("base.txt"), "one\ntwo\nthree\nfour\n").unwrap();

    // A base_ref that doesn't resolve must fall back to HEAD, not error.
    let result =
        diff_against_base(tmp.to_str().unwrap(), "origin/does-not-exist", None, None).unwrap();
    let f = result.files.iter().find(|f| f.path == "base.txt").unwrap();
    assert_eq!(f.status, "modified");
    assert_eq!(f.additions, 1);
    assert_eq!(f.deletions, 0);
}

#[test]
fn diff_against_base_flags_binary_without_hunks() {
    let tmp = DiffFixture::new();
    // Drop the repo handle immediately — this test only needs the
    // on-disk repo structure, and `diff_against_base` opens its own
    // handle internally. Holding `_repo` would just keep the FDs (and
    // any Windows file locks) for the entire test.
    init_repo_with_base(&tmp);

    std::fs::write(tmp.join("blob.bin"), [0u8, 1, 2, 0, 255, 7]).unwrap();

    let result = diff_against_base(tmp.to_str().unwrap(), "base", None, None).unwrap();
    let bin = result.files.iter().find(|f| f.path == "blob.bin").unwrap();
    assert!(bin.binary, "binary file should be flagged");
    assert!(bin.hunks.is_empty(), "binary file must not dump byte hunks");
}

#[test]
fn build_hunk_emits_per_line_highlight_aligned_with_lines() {
    let lines = compute_file_diff("let x = 1;\n", "let x = 2;\n");
    let groups = group_into_hunks(&lines, CONTEXT_LINES);
    assert!(!groups.is_empty());
    let hunk = build_hunk(&groups[0], "main.rs");
    // One highlighted string per line, same order.
    assert_eq!(hunk.lines_highlighted.len(), hunk.lines.len());
    // Highlighted output is HTML spans, not raw source.
    assert!(hunk.lines_highlighted.iter().any(|h| h.contains("<span")));
    // The changed token survives into the highlighted HTML.
    assert!(
        hunk.lines_highlighted.iter().any(|h| h.contains('2')),
        "expected the new value to appear in highlighted output"
    );
}

#[test]
fn diff_against_base_only_restricts_to_one_path() {
    let tmp = DiffFixture::new();
    let repo = init_repo_with_base(&tmp);

    std::fs::write(tmp.join("a.txt"), "x\n").unwrap();
    std::fs::write(tmp.join("b.txt"), "y\n").unwrap();
    commit_all(&repo, "two files");

    let result = diff_against_base(tmp.to_str().unwrap(), "base", Some("a.txt"), None).unwrap();
    let paths: Vec<&str> = result.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["a.txt"], "only the filtered path should appear");
}

// ----- node → directory resolution (worktree vs root) ----------------

fn make_node(
    path: &str,
    use_worktree: bool,
    worktree_name: Option<&str>,
) -> crate::models::AgentNode {
    // Only `path` / `use_worktree` / `worktree_name` drive the diff
    // resolution under test (via `env::node_git_path`); the rest
    // spread through `..Default::default()` so future optional columns
    // don't reopen this fixture (issue #457).
    use crate::models::AgentNode;
    AgentNode {
        id: 1,
        path: path.to_string(),
        worktree_name: worktree_name.map(|s| s.to_string()),
        use_worktree,
        ..Default::default()
    }
}

// Regression: the review surface diffed `node.path` (the project root), but a
// worktree agent's edits live in `{path}/.claude/worktrees/{name}`. The root
// read clean while the agent's work sat in the worktree — so the panel showed
// "no changes" while the title-bar chip (which resolves the worktree) showed a
// count. The diff must follow the same resolved worktree path.
#[test]
fn diff_node_inspects_worktree_not_project_root() {
    // Project root: a clean repo with a `base` branch, nothing pending.
    // The test only needs the on-disk repo structure; drop the handle
    // immediately so it doesn't keep FDs (or Windows file locks) alive
    // for the rest of the test (Round 3 review Finding #6).
    let root = DiffFixture::new();
    init_repo_with_base(&root);

    let node = make_node(root.to_str().unwrap(), true, Some("wt"));
    let wt_path = crate::env::node_git_path(&node).host_path;
    assert!(
        wt_path
            .replace('\\', "/")
            .ends_with("/.claude/worktrees/wt"),
        "expected the worktree path, got {wt_path}"
    );

    // Materialise the worktree as its own repo with an uncommitted edit.
    // Same rationale as the root above — drop the handle immediately.
    std::fs::create_dir_all(&wt_path).unwrap();
    let wt_dir = std::path::Path::new(&wt_path);
    init_repo_with_base(wt_dir);
    std::fs::write(wt_dir.join("base.txt"), "one\nTWO\nthree\n").unwrap();

    // Following the resolved worktree path surfaces the agent's edit...
    let via_worktree = diff_against_base(&wt_path, "base", None, None).unwrap();
    assert!(
        via_worktree.files.iter().any(|f| f.path == "base.txt"),
        "worktree edit missing: {:?}",
        via_worktree
            .files
            .iter()
            .map(|f| &f.path)
            .collect::<Vec<_>>()
    );

    // ...while the old behaviour — diffing the project root — never surfaces
    // the agent's edit (the root's own `base.txt` is untouched; it can only
    // see the worktree as an opaque untracked subdir).
    let via_root = diff_against_base(node.path.as_str(), "base", None, None).unwrap();
    assert!(
        !via_root.files.iter().any(|f| f.path == "base.txt"),
        "project-root diff must not surface the worktree's edit, got {:?}",
        via_root.files.iter().map(|f| &f.path).collect::<Vec<_>>()
    );
}

/// `diff_against_base` returns the cancellation sentinel when its
/// cancel flag is flipped before the walk starts — the cheapest
/// proof that the polling seam works.
#[test]
fn diff_against_base_returns_cancelled_when_flag_pre_set() {
    let tmp = DiffFixture::new();
    // Drop the repo handle immediately — this test only needs the
    // on-disk repo structure, and `diff_against_base` opens its own
    // handle internally. Holding `_repo` would just keep the FDs
    // (and any Windows file locks) for the entire test.
    init_repo_with_base(&tmp);

    std::fs::write(tmp.join("edit.rs"), "fn main() {}\n").unwrap();

    let token = AtomicBool::new(true);
    let result = diff_against_base(tmp.to_str().unwrap(), "base", None, Some(&token));
    assert_eq!(
        result.as_ref().err().map(|error| error.to_string()),
        Some(DIFF_CANCELLED.to_string()),
        "pre-set flag must short-circuit with the cancellation sentinel"
    );
}

/// Bounded stand-in for `{:?}` on a whole `DiffResult`: this fixture
/// makes hundreds of highlighted lines, and dumping them turned an
/// assertion failure into a multi-megabyte log line that hid the
/// actual signal. Counts plus the first path still say which way the
/// walk went.
fn describe_diff_result(result: &Result<DiffResult, crate::diff::DiffError>) -> String {
    match result {
        Ok(diff) => {
            let hunks: usize = diff.files.iter().map(|f| f.hunks.len()).sum();
            format!(
                "Ok(DiffResult {{ files: {}, total_hunks: {}, first_path: {:?} }})",
                diff.files.len(),
                hunks,
                diff.files.first().map(|f| f.path.as_str())
            )
        }
        Err(err) => format!("Err({err:?})"),
    }
}

/// A flag flipped mid-walk returns the cancellation sentinel, and the
/// walk stops at the poll that observed it.
///
/// Determinism comes from a handshake, not a timing race. The walk
/// thread parks at poll `FLIP_AT` until a sibling thread has stored
/// `true` and acknowledged, so the flip lands between two polls of a
/// walk that has already built a file's hunks — the previous version
/// just slept 1 ms on a sibling thread, and on a loaded machine that
/// sleep (plus scheduling) outlasted the entire walk, so the
/// assertion saw `Ok` (issue #2229). The store still happens on
/// another thread, so the walk still proves it observes a
/// cross-thread flip of its `SeqCst` token.
///
/// The poll count replaces the old `elapsed < 1s` bound as the
/// short-circuit proof: timing a wall clock to argue that a walk
/// stopped early is the same flake wearing a different hat, whereas
/// "the walk stopped at the poll that saw the flip" is exact.
#[test]
fn diff_against_base_returns_cancelled_when_flag_flipped_mid_walk() {
    // Poll 0 is the first delta boundary and poll 1 the first file's
    // hunk, so poll 2 is reached only once the walk is genuinely
    // mid-file-set — with real work already done behind it.
    const FLIP_AT: usize = 2;
    // Bounds a stall of the flipper thread while the walk is parked;
    // it is a hang guard, not the mechanism under test.
    const FLIP_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    let tmp = DiffFixture::new();
    // Drop the repo handle immediately — this test only needs the
    // on-disk repo structure, and `diff_against_base` opens its own
    // handle internally. Holding `_repo` would just keep the FDs
    // (and any Windows file locks) for the entire test.
    init_repo_with_base(&tmp);

    // One changed file per reachable poll past the first delta is all
    // the walk needs; the files stay multi-line so each really
    // produces a hunk to highlight.
    for i in 0..3 {
        let mut body = String::new();
        for line in 0..100 {
            body.push_str(&format!("file {} line {} {}\n", i, line, "x".repeat(40)));
        }
        std::fs::write(tmp.join(format!("f{i}.txt")), body).unwrap();
    }

    let token = Arc::new(AtomicBool::new(false));
    let (flip_tx, flip_rx) = std::sync::mpsc::channel::<()>();
    let (ack_tx, ack_rx) = std::sync::mpsc::channel::<()>();
    let flipper_token = Arc::clone(&token);
    let flipper = std::thread::spawn(move || {
        // Bounded: a walk with no polls sends no signal, and the
        // regression under test should surface as a failed assertion
        // rather than a suite that hangs until the gate times out.
        if flip_rx.recv_timeout(FLIP_ACK_TIMEOUT).is_err() {
            return;
        }
        flipper_token.store(true, Ordering::SeqCst);
        // Acknowledge only after the store, so the walk resumes
        // its poll after the flip is visible.
        let _ = ack_tx.send(());
    });

    set_cancel_poll_probe(Some(std::rc::Rc::new(move |poll| {
        if poll == FLIP_AT {
            flip_tx.send(()).expect("flipper thread is alive");
            ack_rx
                .recv_timeout(FLIP_ACK_TIMEOUT)
                .expect("flipper must store the token while the walk is parked");
        }
    })));

    let result = diff_against_base(tmp.to_str().unwrap(), "base", None, Some(&token));
    let polls = cancel_poll_count();
    set_cancel_poll_probe(None);
    // Join only when the probe reached the flip point; the
    // assertions below report a walk that stopped short of it.
    if polls > FLIP_AT {
        flipper.join().unwrap();
    }

    assert_eq!(
        result.as_ref().err().map(|error| error.to_string()),
        Some(DIFF_CANCELLED.to_string()),
        "mid-walk flag flip must return the cancellation sentinel; got {}",
        describe_diff_result(&result)
    );
    assert_eq!(
        polls,
        FLIP_AT + 1,
        "the walk must stop at the poll that observed the flip, not run on to the end"
    );
}

// ----- fixtures ------------------------------------------------------

/// A mixed change set, rendered and pinned.
///
/// Issue #2527 moved the engine out of the command layer. The contract that
/// matters is that the output did not change: the same files, the same
/// statuses, the same hunk boundaries, the same line kinds and the same
/// summary counts. This test is the fixture for that — the engine's own
/// golden output.
///
/// The highlighted HTML is not pinned here: syntect's own output is pinned by
/// the highlighting suite, and the escaping rules by the same. What is
/// pinned is everything this module decides.
#[test]
fn a_mixed_change_set_renders_the_pinned_diff() {
    let tmp = DiffFixture::new();
    let repo = init_mixed_repo(&tmp);

    // Committed work since the base: a new file and a rename.
    std::fs::write(tmp.join("new.txt"), "new\nlines\n").unwrap();
    std::fs::write(tmp.join("renamed.txt"), "alpha\nbeta\n").unwrap();
    std::fs::remove_file(tmp.join("rename-me.txt")).unwrap();
    {
        let mut index = repo.index().unwrap();
        index.remove_path(std::path::Path::new("rename-me.txt")).unwrap();
        index.add_path(std::path::Path::new("new.txt")).unwrap();
        index.add_path(std::path::Path::new("renamed.txt")).unwrap();
        index.write().unwrap();
    }
    commit_all(&repo, "agent work");

    // Uncommitted work since the base: an edit, a deletion and a binary.
    std::fs::write(tmp.join("edit.txt"), "one\nTWO\nthree\nfour\nfive\n").unwrap();
    std::fs::remove_file(tmp.join("drop.txt")).unwrap();
    std::fs::write(tmp.join("blob.bin"), [0u8, 1, 2, 0, 255, 7]).unwrap();

    let result = diff_against_base(tmp.to_str().unwrap(), "base", None, None).unwrap();

    let rendered: Vec<String> = result
        .files
        .iter()
        .map(|file| {
            let hunks: Vec<String> = file
                .hunks
                .iter()
                .map(|hunk| {
                    let lines: Vec<String> = hunk
                        .lines
                        .iter()
                        .map(|line| format!("{}{}", &line.line_type[..1], line.content))
                        .collect();
                    format!(
                        "@{}+{}..{}+{} {}",
                        hunk.old_start,
                        hunk.old_lines,
                        hunk.new_start,
                        hunk.new_lines,
                        lines.join("|")
                    )
                })
                .collect();
            format!(
                "{} [{}] +{} -{} old={:?} binary={} hunks=[{}]",
                file.path,
                file.status,
                file.additions,
                file.deletions,
                file.old_path,
                file.binary,
                hunks.join(" ")
            )
        })
        .collect();

    assert_eq!(
        rendered,
        vec![
            "blob.bin [untracked] +0 -0 old=None binary=true hunks=[]".to_string(),
            "drop.txt [deleted] +0 -1 old=None binary=false hunks=[@1+1..0+0 rgone]".to_string(),
            "edit.txt [modified] +1 -1 old=None binary=false hunks=[@1+1..1+1 cone|rtwo|aTWO|cthree|cfour|cfive]".to_string(),
            "new.txt [added] +2 -0 old=None binary=false hunks=[@0+0..1+2 anew|alines]".to_string(),
            "renamed.txt [renamed] +0 -0 old=Some(\"rename-me.txt\") binary=false hunks=[]".to_string(),
        ]
    );

    // The counts over the same change set.
    let summary = crate::git::state::summarize_statuses(
        result.files.iter().map(|file| file.status.as_str()),
    );
    assert_eq!(summary.total, 5);
    assert_eq!(summary.added, 2, "added counts new + untracked");
    assert_eq!(summary.modified, 2, "a rename reads as a modification");
    assert_eq!(summary.deleted, 1);
}

/// A repository whose `base` branch carries the four files the mixed-change-set
/// fixture needs: an edited file, a deleted file, a renamed file and an
/// untouched one.
fn init_mixed_repo(dir: &std::path::Path) -> git2::Repository {
    let repo = git2::Repository::init(dir).unwrap();
    let sig = git2::Signature::now("t", "t@example.com").unwrap();
    std::fs::write(dir.join("base.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
    std::fs::write(dir.join("drop.txt"), "gone\n").unwrap();
    std::fs::write(dir.join("rename-me.txt"), "alpha\nbeta\n").unwrap();
    std::fs::write(dir.join("edit.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        for name in ["base.txt", "drop.txt", "rename-me.txt", "edit.txt"] {
            index.add_path(std::path::Path::new(name)).unwrap();
        }
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, "base tree", &tree, &[])
            .unwrap();
        let base_commit = repo.find_commit(oid).unwrap();
        repo.branch("base", &base_commit, true).unwrap();
    }
    repo
}

/// Per-test fixture: a unique temp directory under `%TEMP%` for
/// hand-built Git repos. `Cleaning` removes the directory via
/// `tempfile::TempDir`'s `Drop`; `Kept` (opt-in via
/// `BUILDMESH_KEEP_DIFF_FIXTURES=1`) survives the test for
/// post-mortem inspection.
///
/// Issue #1544: the previous helper returned a bare `PathBuf` with
/// no `Drop` and leaked ~150 MB / 7,000+ `buildmesh-diff-test-*`
/// directories on the review machine across contributor/CI runs.
///
/// `Deref<Target = Path>` so callers can pass `&fx` to functions
/// expecting `&Path` and use `fx.join(...)` / `fx.to_str()` directly,
/// mirroring how `tempfile::TempDir` itself is used throughout the
/// codebase and avoiding `.path()` boilerplate at every call site.
enum DiffFixture {
    Cleaning(tempfile::TempDir),
    Kept(std::path::PathBuf),
}

impl DiffFixture {
    fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("buildmesh-diff-test-")
            .tempdir()
            .expect("create fixture tempdir");
        // `TempDir::keep()` disables the cleanup `Drop` and
        // returns the path - exactly the "preserve this directory"
        // semantic the opt-in wants. Stays inside `TempDir`'s own
        // RAII machinery instead of reaching for `mem::forget`.
        if keep_diff_fixtures() {
            let path = temp.keep();
            eprintln!("[buildmesh diff fixture] preserved at {}", path.display());
            Self::Kept(path)
        } else {
            Self::Cleaning(temp)
        }
    }

    /// Test-only constructor: produce a preserved fixture without
    /// reading `BUILDMESH_KEEP_DIFF_FIXTURES`. The opt-in test uses
    /// this instead of mutating the process environment, so it
    /// cannot race concurrent tests that read the same env var via
    /// `DiffFixture::new()`. The caller is responsible for removing
    /// the directory afterwards (it survives past the test's `Drop`).
    #[cfg(test)]
    fn new_preserved() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("buildmesh-diff-test-")
            .tempdir()
            .expect("create fixture tempdir");
        let path = temp.keep();
        eprintln!("[buildmesh diff fixture] preserved at {}", path.display());
        Self::Kept(path)
    }
}

impl std::ops::Deref for DiffFixture {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        match self {
            Self::Cleaning(t) => t.path(),
            Self::Kept(p) => p,
        }
    }
}

impl AsRef<std::path::Path> for DiffFixture {
    fn as_ref(&self) -> &std::path::Path {
        match self {
            Self::Cleaning(t) => t.path(),
            Self::Kept(p) => p,
        }
    }
}

/// Pairs a `git2::Repository` handle with its `DiffFixture` so the
/// handle drops BEFORE the temp directory on Windows. Rust struct
/// field drop order is top-to-bottom; declaring `repo` above `dir`
/// makes the cleanup invariant architectural, not a convention
/// callers have to remember (issue #1544 step #4 — pass the temp
/// path to Git helpers and keep the guard until all repositories
/// are dropped).
struct GitFixture {
    repo: git2::Repository,
    dir: DiffFixture,
}

impl GitFixture {
    fn new() -> Self {
        let dir = DiffFixture::new();
        let repo = init_repo_with_base(&dir);
        Self { repo, dir }
    }
}

/// Pure parser for the `BUILDMESH_KEEP_DIFF_FIXTURES` opt-in. Accepts
/// only the literal string `"1"` — the repository convention for
/// boolean env-var toggles (CLAUDE.md, `commands/watchdog.rs:95`).
/// Anything else — unset, empty, `"0"`, `"false"`, `"no"`, `"yes"`
/// — reads as disabled. Extracted as a free function so the cases
/// can be pinned with a table-driven test (no env mutation, no
/// mutex, no `unsafe`).
fn parse_keep_fixtures(val: Option<&std::ffi::OsStr>) -> bool {
    val.and_then(|v| v.to_str()) == Some("1")
}

/// Read the process env at the call site and forward to the pure
/// parser. This is the only place `std::env::var_os` appears in
/// tests' control flow; tests cover `parse_keep_fixtures` directly
/// rather than mutating the process environment.
fn keep_diff_fixtures() -> bool {
    parse_keep_fixtures(std::env::var_os("BUILDMESH_KEEP_DIFF_FIXTURES").as_deref())
}

/// Pins the cleanup contract from issue #1544 end-to-end: creating
/// a `GitFixture`, writing tracked content, and dropping it must
/// remove both the `.git` directory and the temp directory root —
/// proving the leak is gone for the production scenario (open git
/// handles + tracked files), not just for an empty tempdir.
///
/// This test never reads or writes the env var; it always exercises
/// the cleanup path (CLAUDE.md "no paper-tiger tests" rule). The
/// preserved path is covered by `diff_fixture_keep_opt_in_preserves_dir`
/// below via `new_preserved()`, which bypasses env mutation entirely.
#[test]
fn diff_fixture_cleans_up_tempdir_and_git_repo_on_drop() {
    let captured_root;
    let captured_git_dir;
    {
        let fx = GitFixture::new();
        // Commit a real file so `.git/` has objects + refs to remove.
        commit_all(&fx.repo, "fixture cleanup probe");
        assert!(
            fx.dir.join("base.txt").exists(),
            "tracked file must exist while fixture is in scope"
        );
        assert!(
            fx.dir.join(".git").exists(),
            ".git directory must exist while fixture is in scope"
        );
        captured_root = fx.dir.as_ref().to_path_buf();
        captured_git_dir = captured_root.join(".git");
        // `fx` drops here — Rust drops struct fields top-to-bottom,
        // so `fx.repo` releases its libgit2 handles first, then
        // `fx.dir` removes the tempdir (issue #1544 step #4
        // guarantee, now architectural instead of a convention).
    }
    assert!(
        !captured_root.exists(),
        "fixture root must be removed after drop (issue #1544 leak at {})",
        captured_root.display()
    );
    assert!(
        !captured_git_dir.exists(),
        "fixture .git directory must be removed after drop (residue at {})",
        captured_git_dir.display()
    );
}

/// The opt-in flag preserves the directory so a failing test can be
/// inspected on disk. We do NOT short-circuit the cleanup test on
/// this flag (that would be a paper-tiger test); instead this
/// dedicated test verifies the preservation contract directly via
/// `DiffFixture::new_preserved()`, which never touches the process
/// environment and therefore cannot race concurrent tests that
/// read `BUILDMESH_KEEP_DIFF_FIXTURES` via `DiffFixture::new()`.
///
/// The directory survives the test by design; the test cleans up
/// after itself so it does not itself leak in CI.
#[test]
fn diff_fixture_keep_opt_in_preserves_dir() {
    let preserved = DiffFixture::new_preserved();
    let preserved_path = preserved.as_ref().to_path_buf();
    assert!(
        preserved_path.exists(),
        "kept fixture must exist while in scope"
    );

    // Sibling uses the env-var-driven path; it cleans up on drop
    // unless the contributor has set `BUILDMESH_KEEP_DIFF_FIXTURES=1`
    // (in which case `Cleaning` vs `Kept` doesn't matter for this
    // assertion — both produce distinct directories).
    let sibling = DiffFixture::new();
    assert_ne!(
        sibling.as_ref(),
        preserved_path,
        "two DiffFixture calls in one test must produce distinct dirs"
    );

    // `Kept` has no Drop cleanup by design (that's the whole point);
    // `Cleaning` cleans up its sibling. Remove the preserved path
    // manually so the test does not itself leak in CI.
    drop(sibling);
    drop(preserved);
    let _ = std::fs::remove_dir_all(&preserved_path);
}

/// Pins `parse_keep_fixtures` end-to-end so the repository
/// convention for boolean env-var toggles (`== "1"`) cannot drift.
/// No env mutation, no mutex, no `unsafe` — every cell is a
/// pure-function input → output pair.
#[test]
fn parse_keep_fixtures_accepts_only_one() {
    let cases: &[(Option<&str>, bool)] = &[
        (None, false),
        (Some(""), false),
        (Some("0"), false),
        (Some("false"), false),
        (Some("no"), false),
        (Some("off"), false),
        (Some("yes"), false),
        (Some("1"), true),
    ];
    for (input, expected) in cases {
        let os_input = input.map(std::ffi::OsStr::new);
        let got = parse_keep_fixtures(os_input);
        assert_eq!(
            got, *expected,
            "parse_keep_fixtures({:?}) = {}, expected {}",
            input, got, expected
        );
    }
}
