//! Diff computation using difference-rs with syntect syntax highlighting for Buildmesh

use crate::db;
use crate::models::{DiffHunk, DiffLine, FileDiff, DiffResult};
use difference_rs::{Changeset, Difference};
use once_cell::sync::Lazy;
use syntect::highlighting::ThemeSet;
use syntect::html::highlighted_html_for_string;
use syntect::parsing::SyntaxSet;
use std::path::PathBuf;
use std::fs;
use tauri::command;

static SYNTAX_SET: Lazy<SyntaxSet> = Lazy::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: Lazy<ThemeSet> = Lazy::new(ThemeSet::load_defaults);

/// Highlight a string with syntect, returning HTML
fn highlight_content(content: &str, path: &str) -> String {
    let ext = ext_for_path(path);
    let syntax = SYNTAX_SET
        .find_syntax_by_extension(&ext)
        .unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text());

    let theme = &THEME_SET.themes["base16-ocean.dark"];

    highlighted_html_for_string(content, &SYNTAX_SET, syntax, theme)
        .unwrap_or_else(|_| content.to_string())
}

/// Get file extension from path
fn ext_for_path(path: &str) -> String {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string()
}

/// Compute a diff between two strings, returning DiffLine structs
fn compute_file_diff(old_content: &str, new_content: &str) -> Vec<DiffLine> {
    let changeset = Changeset::new(old_content, new_content, "\n");
    let mut lines = Vec::new();
    let mut old_num = 1usize;
    let mut new_num = 1usize;

    for diff in changeset.diffs {
        match diff {
            Difference::Same(text) => {
                for line in text.lines() {
                    lines.push(DiffLine {
                        line_type: "context".to_string(),
                        content: line.to_string(),
                        old_num: Some(old_num),
                        new_num: Some(new_num),
                    });
                    old_num += 1;
                    new_num += 1;
                }
            }
            Difference::Add(text) => {
                for line in text.lines() {
                    lines.push(DiffLine {
                        line_type: "add".to_string(),
                        content: line.to_string(),
                        old_num: None,
                        new_num: Some(new_num),
                    });
                    new_num += 1;
                }
            }
            Difference::Rem(text) => {
                for line in text.lines() {
                    lines.push(DiffLine {
                        line_type: "remove".to_string(),
                        content: line.to_string(),
                        old_num: Some(old_num),
                        new_num: None,
                    });
                    old_num += 1;
                }
            }
        }
    }
    lines
}

/// Build highlighted old/new content strings from diff lines
fn build_sides(lines: &[DiffLine]) -> (String, String) {
    let mut old_lines: Vec<&str> = Vec::new();
    let mut new_lines: Vec<&str> = Vec::new();

    for line in lines {
        match line.line_type.as_str() {
            "context" => {
                old_lines.push(&line.content);
                new_lines.push(&line.content);
            }
            "add" => {
                old_lines.push("");
                new_lines.push(&line.content);
            }
            "remove" => {
                old_lines.push(&line.content);
                new_lines.push("");
            }
            _ => {}
        }
    }

    (old_lines.join("\n"), new_lines.join("\n"))
}

/// Number of unchanged context lines to show on each side of a change
/// region. Mirrors the default in `git diff -U3` and GitHub's collapsed
/// diff view, so the user sees the change with enough surrounding code to
/// orient themselves but is not flooded with the whole file.
const CONTEXT_LINES: usize = 3;

/// Split a flat diff into hunks with bounded context. Long stretches of
/// unchanged lines that fall outside the context window around any change
/// are dropped. Adjacent change regions whose context windows overlap
/// (i.e. the gap between them is ≤ 2 * CONTEXT_LINES) are merged into a
/// single hunk. Returns an empty vec if the diff contains no changes.
fn group_into_hunks(lines: &[DiffLine], context: usize) -> Vec<Vec<DiffLine>> {
    let change_indices: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.line_type != "context")
        .map(|(i, _)| i)
        .collect();

    if change_indices.is_empty() {
        return vec![];
    }

    // Window of [i - context, i + context] around each change, clamped to
    // the diff line range. End is exclusive (Rust range convention).
    let mut windows: Vec<(usize, usize)> = change_indices
        .iter()
        .map(|&i| {
            let start = i.saturating_sub(context);
            let end = (i + context + 1).min(lines.len());
            (start, end)
        })
        .collect();

    // Merge overlapping / touching windows so that close changes share
    // their context lines and render as a single hunk.
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for w in windows.drain(..) {
        if let Some(last) = merged.last_mut() {
            if w.0 <= last.1 {
                last.1 = last.1.max(w.1);
                continue;
            }
        }
        merged.push(w);
    }

    merged
        .iter()
        .map(|(start, end)| lines[*start..*end].to_vec())
        .collect()
}

/// Build a single DiffHunk for a group of diff lines, computing syntax
/// highlighting and hunk header metadata (old_start / new_start).
fn build_hunk(group: &[DiffLine], highlight_path: &str) -> DiffHunk {
    let (old_highlighted, new_highlighted) = build_sides(group);
    let old_hl = highlight_content(&old_highlighted, highlight_path);
    let new_hl = highlight_content(&new_highlighted, highlight_path);
    // hunk header: oldest new/old line number in this group, plus the
    // count of remove/add lines (matches the previous single-hunk
    // convention; consumers that want total hunk length can read lines.len()).
    let old_start = group.iter().find_map(|l| l.old_num).unwrap_or(0);
    let new_start = group.iter().find_map(|l| l.new_num).unwrap_or(0);
    let old_lines = group.iter().filter(|l| l.line_type == "remove").count();
    let new_lines = group.iter().filter(|l| l.line_type == "add").count();
    DiffHunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
        old_highlighted: old_hl,
        new_highlighted: new_hl,
        lines: group.to_vec(),
    }
}

/// Diff two files on disk
#[command]
pub async fn diff_files(
    old_path: String,
    new_path: String,
) -> Result<DiffResult, String> {
    let old_content = fs::read_to_string(&old_path).unwrap_or_default();
    let new_content = fs::read_to_string(&new_path).unwrap_or_default();
    let lines = compute_file_diff(&old_content, &new_content);
    let groups = group_into_hunks(&lines, CONTEXT_LINES);
    let hunks = groups
        .iter()
        .map(|g| build_hunk(g, &new_path))
        .collect();

    Ok(DiffResult {
        files: vec![FileDiff {
            path: new_path,
            hunks,
        }],
    })
}

/// Diff a single file against HEAD
#[command]
pub async fn diff_file_against_head(
    session_path: String,
    file_path: String,
) -> Result<DiffResult, String> {
    let repo = git2::Repository::open(&session_path)
        .map_err(|e| e.to_string())?;

    let head = repo.head().ok();
    let head_content = if let Some(reference) = &head {
        reference.peel_to_commit().ok().and_then(|commit| {
            commit.tree().ok().and_then(|tree| {
                tree.get_path(std::path::Path::new(&file_path)).ok().and_then(|entry| {
                    entry.to_object(&repo).ok().and_then(|obj| {
                        obj.as_blob().map(|b| String::from_utf8_lossy(b.content()).to_string())
                    })
                })
            })
        })
    } else {
        None
    };

    let current_content = fs::read_to_string(format!("{}/{}", session_path, file_path))
        .unwrap_or_default();

    let old_content = head_content.as_deref().unwrap_or("");
    let lines = compute_file_diff(old_content, &current_content);
    let groups = group_into_hunks(&lines, CONTEXT_LINES);
    let hunks = groups
        .iter()
        .map(|g| build_hunk(g, &file_path))
        .collect();

    Ok(DiffResult {
        files: vec![FileDiff {
            path: file_path,
            hunks,
        }],
    })
}

/// Diff session files against a checkpoint
#[command]
pub async fn diff_session_checkpoint(
    session_id: i64,
    checkpoint_id: i64,
) -> Result<DiffResult, String> {
    let node = db::get_agent_node_by_id(session_id)
        .map_err(|e| e.to_string())?;
    let checkpoint = db::get_checkpoint_by_id(checkpoint_id)
        .map_err(|e| e.to_string())?;
    let ref_name = format!("refs/heads/conductor/checkpoints/c{}", checkpoint.turn_index);

    let repo = git2::Repository::open(&node.path)
        .map_err(|e| e.to_string())?;

    let reference = repo.find_reference(&ref_name)
        .map_err(|e| e.to_string())?;
    let commit = reference.peel_to_commit()
        .map_err(|e| e.to_string())?;
    let tree = commit.as_object().peel_to_tree()
        .map_err(|e| e.to_string())?;

    let mut files = Vec::new();
    let current_path = PathBuf::from(&node.path);

    if let Ok(entries) = fs::read_dir(&current_path) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_file() {
                let file_path = path.to_string_lossy().to_string();
                let rel_path = path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();

                let current_content = fs::read_to_string(&path).unwrap_or_default();

                if let Ok(blob) = tree.get_path(std::path::Path::new(&rel_path)) {
                    let checkpoint_content = match blob.to_object(&repo) {
                        Ok(o) => {
                            if let Some(b) = o.as_blob() {
                                String::from_utf8_lossy(b.content()).to_string()
                            } else {
                                String::new()
                            }
                        }
                        Err(_) => String::new(),
                    };
                    if current_content != checkpoint_content {
                        let lines = compute_file_diff(&checkpoint_content, &current_content);
                        let (old_highlighted, new_highlighted) = build_sides(&lines);
                        let old_hl = highlight_content(&old_highlighted, &file_path);
                        let new_hl = highlight_content(&new_highlighted, &file_path);
                        let hunks = vec![DiffHunk {
                            old_start: 1,
                            old_lines: lines.iter().filter(|l| l.line_type == "remove").count(),
                            new_start: 1,
                            new_lines: lines.iter().filter(|l| l.line_type == "add").count(),
                            old_highlighted: old_hl,
                            new_highlighted: new_hl,
                            lines,
                        }];
                        files.push(FileDiff { path: file_path, hunks });
                    }
                } else {
                    let lines = compute_file_diff("", &current_content);
                    let (old_highlighted, new_highlighted) = build_sides(&lines);
                    let old_hl = highlight_content(&old_highlighted, &file_path);
                    let new_hl = highlight_content(&new_highlighted, &file_path);
                    let hunks = vec![DiffHunk {
                        old_start: 0,
                        old_lines: 0,
                        new_start: 1,
                        new_lines: lines.len(),
                        old_highlighted: old_hl,
                        new_highlighted: new_hl,
                        lines,
                    }];
                    files.push(FileDiff { path: file_path, hunks });
                }
            }
        }
    }

    Ok(DiffResult { files })
}

#[cfg(test)]
mod tests {
    use super::*;

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
            DiffLine { line_type: "context".to_string(), content: "same".to_string(), old_num: Some(1), new_num: Some(1) },
            DiffLine { line_type: "remove".to_string(), content: "old".to_string(), old_num: Some(2), new_num: None },
            DiffLine { line_type: "add".to_string(), content: "new".to_string(), old_num: None, new_num: Some(2) },
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
    fn ext_for_path_extracts_extension() {
        assert_eq!(ext_for_path("foo/bar.rs"), "rs");
        assert_eq!(ext_for_path("Cargo.toml"), "toml");
        assert_eq!(ext_for_path("no-extension"), "");
        assert_eq!(ext_for_path("src/main.tsx"), "tsx");
    }

    #[test]
    fn highlight_content_returns_non_empty_for_code() {
        let result = highlight_content("fn main() {}", "test.rs");
        assert!(!result.is_empty());
        assert!(result.contains("main"));
    }

    #[test]
    fn highlight_content_handles_unknown_extension() {
        let result = highlight_content("random text", "file.xyz123");
        assert!(result.contains("random text"));
    }

    fn tempdir_via_env() -> std::path::PathBuf {
        let base = std::env::temp_dir();
        let unique = format!(
            "buildmesh-diff-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = base.join(unique);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn group_into_hunks_empty_when_no_changes() {
        let lines = vec![
            DiffLine { line_type: "context".into(), content: "a".into(), old_num: Some(1), new_num: Some(1) },
            DiffLine { line_type: "context".into(), content: "b".into(), old_num: Some(2), new_num: Some(2) },
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
            lines.push(DiffLine { line_type: "context".into(), content: format!("a{}", i), old_num: Some(i+1), new_num: Some(i+1) });
        }
        lines.push(DiffLine { line_type: "add".into(), content: "NEW".into(), old_num: None, new_num: Some(21) });
        for i in 21..40 {
            lines.push(DiffLine { line_type: "context".into(), content: format!("a{}", i), old_num: Some(i+1), new_num: Some(i+1) });
        }

        let hunks = group_into_hunks(&lines, 3);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].len(), 7, "expected 3 context + 1 add + 3 context = 7");
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
            lines.push(DiffLine { line_type: "context".into(), content: format!("a{}", i), old_num: Some(i+1), new_num: Some(i+1) });
        }
        lines.push(DiffLine { line_type: "add".into(), content: "FIRST".into(), old_num: None, new_num: Some(11) });
        for i in 11..61 {
            lines.push(DiffLine { line_type: "context".into(), content: format!("a{}", i), old_num: Some(i+1), new_num: Some(i+1) });
        }
        lines.push(DiffLine { line_type: "add".into(), content: "SECOND".into(), old_num: None, new_num: Some(62) });
        for i in 62..70 {
            lines.push(DiffLine { line_type: "context".into(), content: format!("a{}", i), old_num: Some(i+1), new_num: Some(i+1) });
        }

        let hunks = group_into_hunks(&lines, 3);
        assert_eq!(hunks.len(), 2, "expected two hunks, got {}: {:#?}", hunks.len(), hunks);
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
            lines.push(DiffLine { line_type: "context".into(), content: format!("a{}", i), old_num: Some(i+1), new_num: Some(i+1) });
        }
        lines.push(DiffLine { line_type: "add".into(), content: "FIRST".into(), old_num: None, new_num: Some(6) });
        for i in 6..10 {
            lines.push(DiffLine { line_type: "context".into(), content: format!("a{}", i), old_num: Some(i+1), new_num: Some(i+1) });
        }
        lines.push(DiffLine { line_type: "add".into(), content: "SECOND".into(), old_num: None, new_num: Some(11) });
        for i in 11..15 {
            lines.push(DiffLine { line_type: "context".into(), content: format!("a{}", i), old_num: Some(i+1), new_num: Some(i+1) });
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
        let tmp = tempdir_via_env();
        let old = tmp.join("old.txt");
        let new = tmp.join("new.txt");
        std::fs::write(&old, &old_content).unwrap();
        std::fs::write(&new, &new_content).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(diff_files(
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
}
