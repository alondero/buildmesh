use super::super::git::{
    get_git_status_blocking, get_git_summary_blocking, revert_file_blocking, stage_file_blocking,
    GitStatus,
};
use crate::env::test_helpers::{init_repo_with_commit, TestDir};
use crate::git::status_stats::status_metrics;
use git2::{DiffOptions, Patch, Repository};
use std::collections::HashMap;
use std::fs;

fn read(dir: &std::path::Path) -> Vec<GitStatus> {
    get_git_status_blocking(dir.to_string_lossy().into_owned()).unwrap()
}

/// The pre-optimization libgit2 implementation is the oracle, including both
/// path aliases. Tests call the actual command and compare each returned row.
fn assert_native_counts(repo: &Repository, rows: &[GitStatus]) {
    let repo = Repository::open(repo.path()).unwrap();
    let mut opts = DiffOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true);
    let head = repo.head().ok().and_then(|head| head.peel_to_tree().ok());
    let diff = repo
        .diff_tree_to_workdir_with_index(head.as_ref(), Some(&mut opts))
        .unwrap();
    let mut expected = HashMap::new();
    for i in 0..diff.deltas().len() {
        if let Some(patch) = Patch::from_diff(&diff, i).unwrap() {
            let (_, added, deleted) = patch.line_stats().unwrap();
            for path in [
                patch.delta().old_file().path(),
                patch.delta().new_file().path(),
            ]
            .into_iter()
            .flatten()
            {
                expected.insert(path.to_string_lossy().into_owned(), (added, deleted));
            }
        }
    }
    for row in rows {
        assert_eq!(
            (row.additions, row.deletions),
            expected.get(&row.path).copied().unwrap_or_default(),
            "{}",
            row.path
        );
    }
}

#[test]
fn clean_status_never_enters_the_line_diff_path() {
    let td = TestDir::new("status-clean-walks");
    init_repo_with_commit(td.path(), &[("file.txt", "before\n")]);
    status_metrics::take();
    assert!(read(td.path()).is_empty());
    assert_eq!(status_metrics::take(), [1, 0, 0]);
}

#[test]
fn small_deltas_keep_exact_native_counts() {
    let td = TestDir::new("status-simple-counts");
    let repo = init_repo_with_commit(
        td.path(),
        &[
            ("append.txt", "one\ntwo\n"),
            ("truncate.txt", "one\ntwo\nthree"),
            ("delete.txt", "one\ntwo"),
        ],
    );
    fs::write(td.path().join("append.txt"), "one\ntwo\nthree").unwrap();
    fs::write(td.path().join("truncate.txt"), "one\n").unwrap();
    fs::remove_file(td.path().join("delete.txt")).unwrap();
    fs::write(td.path().join("new.txt"), "first\nlast").unwrap();
    status_metrics::take();
    let rows = read(td.path());
    assert_eq!(status_metrics::take(), [1, 1, 4]);
    let values: Vec<_> = rows
        .iter()
        .map(|r| (r.path.as_str(), r.status.as_str(), r.additions, r.deletions))
        .collect();
    assert_eq!(
        values,
        [
            ("append.txt", "modified", 1, 0),
            ("delete.txt", "deleted", 0, 2),
            ("new.txt", "added", 2, 0),
            ("truncate.txt", "modified", 0, 2)
        ]
    );
    assert_native_counts(&repo, &rows);
}

#[test]
fn large_simple_deltas_keep_exact_counts_without_patches() {
    let td = TestDir::new("status-large-simple");
    let line = format!("{}\n", "x".repeat(65536));
    let truncated = format!("{line}removed\nlast");
    let repo = init_repo_with_commit(
        td.path(),
        &[("append.txt", &line), ("truncate.txt", &truncated)],
    );
    fs::write(td.path().join("append.txt"), format!("{line}last")).unwrap();
    fs::write(td.path().join("truncate.txt"), &line).unwrap();
    fs::write(td.path().join("new.txt"), &line).unwrap();
    status_metrics::take();
    let rows = read(td.path());
    assert_eq!(status_metrics::take(), [1, 1, 0]);
    let values: Vec<_> = rows
        .iter()
        .map(|r| (r.path.as_str(), r.additions, r.deletions))
        .collect();
    assert_eq!(
        values,
        [
            ("append.txt", 1, 0),
            ("new.txt", 1, 0),
            ("truncate.txt", 0, 2)
        ]
    );
    assert_native_counts(&repo, &rows);
}

#[test]
fn chunk_boundaries_binary_detection_and_eof_match_native_counts() {
    let td = TestDir::new("status-stream-boundaries");
    let repo = init_repo_with_commit(td.path(), &[("empty.txt", ""), ("no-eof.txt", "old")]);
    repo.config()
        .unwrap()
        .set_bool("core.autocrlf", false)
        .unwrap();
    let mut text = "record\n".repeat(40_000).into_bytes();
    text.push(b'x');
    fs::write(td.path().join("large.txt"), &text).unwrap();
    fs::write(td.path().join("empty.txt"), "now\n").unwrap();
    fs::write(td.path().join("no-eof.txt"), "old appended\n").unwrap();
    let mut early_nul = vec![b'x'; 65536];
    early_nul[7999] = 0;
    fs::write(td.path().join("binary.bin"), early_nul).unwrap();
    let mut late_nul = vec![b'x'; 65536];
    late_nul[8000] = 0;
    late_nul.push(b'\n');
    fs::write(td.path().join("late-nul.txt"), late_nul).unwrap();
    status_metrics::take();
    let rows = read(td.path());
    assert_eq!(status_metrics::take(), [1, 1, 2]);
    assert_eq!(
        rows.iter()
            .find(|r| r.path == "large.txt")
            .unwrap()
            .additions,
        40_001
    );
    assert_eq!(
        rows.iter()
            .find(|r| r.path == "binary.bin")
            .unwrap()
            .additions,
        0
    );
    assert_eq!(
        rows.iter()
            .find(|r| r.path == "late-nul.txt")
            .unwrap()
            .additions,
        1
    );
    assert_native_counts(&repo, &rows);
}

#[test]
fn attributes_and_crlf_keep_the_native_fallback() {
    let td = TestDir::new("status-attributes");
    let repo = init_repo_with_commit(
        td.path(),
        &[
            (
                ".gitattributes",
                "*.bin diff\n*.txt -diff\n*.lf text eol=lf\n",
            ),
            ("base", "before\n"),
        ],
    );
    fs::write(
        td.path().join("forced.bin"),
        format!("a\0{}\nc\n", "b".repeat(65536)),
    )
    .unwrap();
    fs::write(
        td.path().join("hidden.txt"),
        format!("{}\ntext\n", "text".repeat(16384)),
    )
    .unwrap();
    fs::write(
        td.path().join("converted.lf"),
        format!("{}\r\nb\r\n", "a".repeat(65536)),
    )
    .unwrap();
    let rows = read(td.path());
    assert_eq!(
        rows.iter()
            .find(|r| r.path == "forced.bin")
            .unwrap()
            .additions,
        2
    );
    assert_eq!(
        rows.iter()
            .find(|r| r.path == "hidden.txt")
            .unwrap()
            .additions,
        0
    );
    assert_native_counts(&repo, &rows);
    repo.config()
        .unwrap()
        .set_str("core.autocrlf", "true")
        .unwrap();
    fs::write(td.path().join("plain"), "a\r\nb\r\n").unwrap();
    assert_native_counts(&repo, &read(td.path()));
}

#[test]
fn legacy_crlf_attributes_preserve_clean_filter_counts() {
    let td = TestDir::new("status-legacy-crlf");
    Repository::init(td.path())
        .unwrap()
        .config()
        .unwrap()
        .set_bool("core.autocrlf", false)
        .unwrap();
    let before = format!("{}\r\ntwo\r\n", "x".repeat(65536));
    let repo = init_repo_with_commit(td.path(), &[("file.txt", &before)]);
    repo.config()
        .unwrap()
        .set_bool("core.autocrlf", false)
        .unwrap();
    fs::write(td.path().join(".gitattributes"), "*.txt crlf\n").unwrap();
    fs::write(td.path().join("file.txt"), format!("{before}three\r\n")).unwrap();
    let rows = read(td.path());
    let file = rows.iter().find(|r| r.path == "file.txt").unwrap();
    assert_eq!((file.additions, file.deletions), (3, 2));
    assert_native_counts(&repo, &rows);
    fs::remove_file(td.path().join(".gitattributes")).unwrap();
    repo.config()
        .unwrap()
        .set_bool("core.autocrlf", true)
        .unwrap();
    // With LF in the index, autocrlf cleans the CRLF workdir even though HEAD
    // still contains CRLF. A raw prefix count would incorrectly report +1/-0.
    fs::write(td.path().join("file.txt"), before.replace("\r\n", "\n")).unwrap();
    stage_file_blocking(&td.path().to_string_lossy(), "file.txt").unwrap();
    fs::write(td.path().join("file.txt"), format!("{before}three\r\n")).unwrap();
    let rows = read(td.path());
    assert_eq!((rows[0].additions, rows[0].deletions), (3, 2));
    assert_native_counts(&repo, &rows);
}

#[test]
fn staged_unstaged_rename_and_project_rows_match_native() {
    let td = TestDir::new("status-index-project");
    let repo = init_repo_with_commit(
        td.path(),
        &[
            ("nested/edit.txt", "one\ntwo\n"),
            ("rename.txt", "one\ntwo\n"),
            ("outside.txt", "before\n"),
        ],
    );
    let selected = td.path().join("nested");
    let path = selected.to_string_lossy().into_owned();
    fs::write(selected.join("edit.txt"), "one\nstaged\n").unwrap();
    stage_file_blocking(&path, "nested/edit.txt").unwrap();
    fs::write(selected.join("edit.txt"), "one\nworking\nlast\n").unwrap();
    fs::rename(td.path().join("rename.txt"), td.path().join("renamed.txt")).unwrap();
    let mut index = repo.index().unwrap();
    index
        .remove_path(std::path::Path::new("rename.txt"))
        .unwrap();
    index.add_path(std::path::Path::new("renamed.txt")).unwrap();
    index.write().unwrap();
    fs::write(td.path().join("outside.txt"), "outside\n").unwrap();
    let rows = read(&selected);
    let values: Vec<_> = rows
        .iter()
        .map(|r| {
            (
                r.path.as_str(),
                r.project_path.as_deref(),
                r.status.as_str(),
                r.additions,
                r.deletions,
            )
        })
        .collect();
    assert_eq!(
        values,
        [
            ("nested/edit.txt", Some("edit.txt"), "modified", 2, 1),
            ("outside.txt", None, "modified", 1, 1),
            ("rename.txt", None, "deleted", 0, 2),
            ("renamed.txt", None, "added", 2, 0)
        ]
    );
    assert_native_counts(&repo, &rows);
}

#[test]
fn unborn_head_and_repeated_reads_do_not_retain_observations() {
    let td = TestDir::new("status-freshness");
    let repo = Repository::init(td.path()).unwrap();
    repo.config()
        .unwrap()
        .set_bool("core.autocrlf", false)
        .unwrap();
    fs::write(td.path().join("new.txt"), "one\n").unwrap();
    let path = td.path().to_string_lossy().into_owned();
    assert_eq!(read(td.path())[0].additions, 1);
    fs::write(td.path().join("new.txt"), "one\ntwo\n").unwrap();
    assert_eq!(read(td.path())[0].additions, 2);
    stage_file_blocking(&path, "new.txt").unwrap();
    assert_native_counts(&repo, &read(td.path()));
    revert_file_blocking(&path, "new.txt").unwrap();
    assert!(read(td.path()).is_empty());
    assert_eq!(get_git_summary_blocking(path.clone()).unwrap().total, 0);
    // A failure is not retained as clean; the next read reopens the path.
    drop(repo);
    fs::rename(td.path().join(".git"), td.path().join("saved-git")).unwrap();
    assert!(get_git_status_blocking(path.clone()).is_err());
    assert!(get_git_summary_blocking(path.clone()).is_err());
    fs::rename(td.path().join("saved-git"), td.path().join(".git")).unwrap();
    fs::write(td.path().join("new.txt"), "recovered\n").unwrap();
    assert_eq!(
        get_git_status_blocking(path.clone()).unwrap()[0].additions,
        1
    );
    stage_file_blocking(&path, "new.txt").unwrap();
    let repo = Repository::open(td.path()).unwrap();
    let tree_id = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("test", "test@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "first", &tree, &[])
        .unwrap();
    assert!(read(td.path()).is_empty());
    fs::write(td.path().join("new.txt"), "after\nmore\n").unwrap();
    let rows = read(td.path());
    assert_eq!((rows[0].additions, rows[0].deletions), (2, 1));
    revert_file_blocking(&path, "new.txt").unwrap();
    assert!(read(td.path()).is_empty());
}
