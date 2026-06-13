//! Diff computation using difference-rs with syntect syntax highlighting for Buildmesh

use crate::db;
use crate::env::to_host_path;
use crate::models::{DiffHunk, DiffLine, FileDiff, DiffResult};
use difference_rs::{Changeset, Difference};
use once_cell::sync::Lazy;
use syntect::highlighting::ThemeSet;
use syntect::html::highlighted_html_for_string;
use syntect::parsing::SyntaxSet;
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

/// Minimal HTML escape for the highlighting fallback path, so a line we
/// couldn't tokenise still renders as text rather than injecting markup.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Highlight each line of a hunk independently, returning inline HTML aligned
/// 1:1 with `group`. Two stateful highlighters (one per side) are advanced in
/// lockstep so multi-line constructs — block comments, multi-line strings —
/// stay coloured correctly: a context line advances both sides, an add advances
/// only the new side, a remove only the old side.
fn highlight_group_lines(group: &[DiffLine], path: &str) -> Vec<String> {
    use syntect::easy::HighlightLines;
    use syntect::html::{styled_line_to_highlighted_html, IncludeBackground};

    let ext = ext_for_path(path);
    let syntax = SYNTAX_SET
        .find_syntax_by_extension(&ext)
        .unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text());
    let theme = &THEME_SET.themes["base16-ocean.dark"];

    let mut old_h = HighlightLines::new(syntax, theme);
    let mut new_h = HighlightLines::new(syntax, theme);

    let render = |line: &DiffLine,
                  ranges: Result<Vec<(syntect::highlighting::Style, &str)>, _>| {
        ranges
            .ok()
            .and_then(|r| styled_line_to_highlighted_html(&r, IncludeBackground::No).ok())
            .unwrap_or_else(|| html_escape(&line.content))
    };

    group
        .iter()
        .map(|line| {
            // HighlightLines expects a trailing newline to terminate the line;
            // the stray `\n` lands inside a span and collapses harmlessly in
            // the single-line row the frontend renders.
            let with_nl = format!("{}\n", line.content);
            match line.line_type.as_str() {
                "add" => render(line, new_h.highlight_line(&with_nl, &SYNTAX_SET)),
                "remove" => render(line, old_h.highlight_line(&with_nl, &SYNTAX_SET)),
                _ => {
                    // context: keep both sides' parser state in sync, emit new.
                    let _ = old_h.highlight_line(&with_nl, &SYNTAX_SET);
                    render(line, new_h.highlight_line(&with_nl, &SYNTAX_SET))
                }
            }
        })
        .collect()
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
    let lines_highlighted = highlight_group_lines(group, highlight_path);
    DiffHunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
        old_highlighted: old_hl,
        new_highlighted: new_hl,
        lines: group.to_vec(),
        lines_highlighted,
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
            ..Default::default()
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
            ..Default::default()
        }],
    })
}

/// Map a libgit2 delta status onto the frontend status vocabulary
/// (`GitStatus.status`), so the changed-files list and tree badges share one
/// set of labels.
fn delta_status_str(status: git2::Delta) -> &'static str {
    use git2::Delta;
    match status {
        Delta::Added => "added",
        Delta::Deleted => "deleted",
        Delta::Renamed | Delta::Copied => "renamed",
        Delta::Untracked => "untracked",
        // Typechange (e.g. file → symlink) and anything unforeseen read as a
        // modification — the safe, least-surprising default.
        _ => "modified",
    }
}

/// Read a path's blob out of a tree as a String (empty if absent). NUL bytes
/// survive (valid UTF-8), so binary detection downstream still works.
fn tree_blob_string(repo: &git2::Repository, tree: Option<&git2::Tree>, path: &str) -> String {
    tree.and_then(|t| t.get_path(std::path::Path::new(path)).ok())
        .and_then(|e| e.to_object(repo).ok())
        .and_then(|o| o.as_blob().map(|b| String::from_utf8_lossy(b.content()).to_string()))
        .unwrap_or_default()
}

/// Resolve the baseline tree to diff a worktree against: the merge-base of the
/// worktree `HEAD` and `base_ref`. Fallback chain (see ADR 0005): merge-base →
/// the `base_ref` commit (unrelated histories) → `HEAD` (no resolvable base) →
/// `None`, i.e. the empty tree (a repo with no commits, where everything in the
/// working dir reads as an addition).
fn resolve_base_tree<'r>(repo: &'r git2::Repository, base_ref: &str) -> Option<git2::Tree<'r>> {
    let head_commit = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let base_commit = repo
        .revparse_single(base_ref)
        .ok()
        .and_then(|o| o.peel_to_commit().ok());

    match (head_commit, base_commit) {
        (Some(head), Some(base)) => {
            if let Ok(mb) = repo.merge_base(head.id(), base.id()) {
                if let Some(tree) = repo.find_commit(mb).ok().and_then(|c| c.tree().ok()) {
                    return Some(tree);
                }
            }
            base.tree().ok()
        }
        (Some(head), None) => head.tree().ok(),
        (None, Some(base)) => base.tree().ok(),
        (None, None) => None,
    }
}

/// Core diff used by the desktop review panel and the mobile changes view.
/// Diffs the working directory against the merge-base with `base_ref`, so the
/// result is everything the agent changed since branching — committed *and*
/// uncommitted (ADR 0005). `only`, when set, restricts to one repo-relative
/// path. Files are returned sorted by path for a stable review order.
fn diff_against_base(
    repo_path: &str,
    base_ref: &str,
    only: Option<&str>,
) -> Result<DiffResult, String> {
    let host = to_host_path(repo_path);
    let repo = git2::Repository::open(&host).map_err(|e| e.to_string())?;
    let base_tree = resolve_base_tree(&repo, base_ref);

    let mut opts = git2::DiffOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    if let Some(p) = only {
        opts.pathspec(p);
    }

    let mut diff = repo
        .diff_tree_to_workdir_with_index(base_tree.as_ref(), Some(&mut opts))
        .map_err(|e| e.to_string())?;

    // Collapse a delete+add of the same content into a single "renamed" entry.
    let mut find = git2::DiffFindOptions::new();
    find.renames(true);
    let _ = diff.find_similar(Some(&mut find));

    let root = std::path::Path::new(&host);
    let mut files = Vec::new();

    for delta in diff.deltas() {
        let Some(path) = delta.new_file().path().or_else(|| delta.old_file().path()) else {
            continue;
        };
        let rel = path.to_string_lossy().to_string();
        let status = delta_status_str(delta.status());

        let old_rel = delta.old_file().path().map(|p| p.to_string_lossy().to_string());
        let old_path = if status == "renamed" { old_rel.clone() } else { None };

        // Old side comes from the base tree (under the pre-rename path); a brand
        // new file has no old side.
        let old_content = if matches!(status, "added" | "untracked") {
            String::new()
        } else {
            tree_blob_string(&repo, base_tree.as_ref(), old_rel.as_deref().unwrap_or(&rel))
        };

        // New side is the current bytes on disk (absent for deletions).
        let raw = std::fs::read(root.join(&rel)).ok();
        let is_binary = delta.new_file().is_binary()
            || delta.old_file().is_binary()
            || raw.as_ref().map(|b| b.contains(&0)).unwrap_or(false)
            || old_content.as_bytes().contains(&0);

        if is_binary {
            files.push(FileDiff {
                path: rel,
                status: status.to_string(),
                old_path,
                binary: true,
                ..Default::default()
            });
            continue;
        }

        let new_content = raw
            .map(|b| String::from_utf8_lossy(&b).to_string())
            .unwrap_or_default();

        let lines = compute_file_diff(&old_content, &new_content);
        let additions = lines.iter().filter(|l| l.line_type == "add").count();
        let deletions = lines.iter().filter(|l| l.line_type == "remove").count();
        let hunks = group_into_hunks(&lines, CONTEXT_LINES)
            .iter()
            .map(|g| build_hunk(g, &rel))
            .collect();

        files.push(FileDiff {
            path: rel,
            hunks,
            status: status.to_string(),
            old_path,
            additions,
            deletions,
            binary: false,
        });
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(DiffResult { files })
}

/// Resolve an Agent Node's mesh `base_ref`, defaulting to `origin/main` if the
/// mesh row can't be read.
fn node_base_ref(node: &crate::models::AgentNode) -> String {
    db::get_mesh_by_id(node.mesh_id)
        .map(|m| m.base_ref)
        .unwrap_or_else(|_| "origin/main".to_string())
}

/// The worktree-name part to feed `resolve_agent_path`: the node's worktree name
/// when it runs in a worktree, else `None` (run in the repo root). Trimmed/empty
/// names collapse to `None`. Mirrors the frontend `getNodeGitPath` and the
/// backend `worktree_path_for_node` so every layer agrees on the node's dir.
fn node_diff_worktree_name(use_worktree: bool, worktree_name: Option<&str>) -> Option<&str> {
    if !use_worktree {
        return None;
    }
    worktree_name.map(str::trim).filter(|n| !n.is_empty())
}

/// Resolve the directory a node's diff should inspect. A worktree node's edits
/// live in `{path}/.claude/worktrees/{worktree_name}`, NOT the project root, so
/// diffing `node.path` directly inspects the wrong tree (the root reads clean
/// while the agent's work sits in the worktree). This realigns the diff with the
/// title-bar git-summary chip, which already resolves the worktree path.
fn node_repo_path(node: &crate::models::AgentNode) -> String {
    let wt = node_diff_worktree_name(node.use_worktree, node.worktree_name.as_deref());
    crate::env::resolve_agent_path(&node.path, wt).host_path
}

/// Diff every file an Agent Node changed since it branched (ADR 0005). One call
/// returns the whole change set the review panel renders as a stacked view.
#[command]
pub async fn diff_node_against_base(node_id: i64) -> Result<DiffResult, String> {
    let node = db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())?;
    let base_ref = node_base_ref(&node);
    diff_against_base(&node_repo_path(&node), &base_ref, None)
}

/// Diff a single file of an Agent Node against its merge-base — the per-file
/// entry point the mobile changes view taps into.
#[command]
pub async fn diff_node_file_against_base(
    node_id: i64,
    file_path: String,
) -> Result<DiffResult, String> {
    let node = db::get_agent_node_by_id(node_id).map_err(|e| e.to_string())?;
    let base_ref = node_base_ref(&node);
    diff_against_base(&node_repo_path(&node), &base_ref, Some(&file_path))
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
            let oid = repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[]).unwrap();
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
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &[&parent]).unwrap();
    }

    #[test]
    fn diff_against_base_shows_committed_and_uncommitted_work() {
        let tmp = tempdir_via_env();
        let repo = init_repo_with_base(&tmp);

        // The agent commits a new file after branching...
        std::fs::write(tmp.join("committed.txt"), "a\nb\n").unwrap();
        commit_all(&repo, "agent work");
        // ...and leaves an uncommitted edit to an existing file.
        std::fs::write(tmp.join("base.txt"), "one\nTWO\nthree\n").unwrap();

        let result = diff_against_base(tmp.to_str().unwrap(), "base", None).unwrap();
        let paths: Vec<&str> = result.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"committed.txt"), "committed file missing: {:?}", paths);
        assert!(paths.contains(&"base.txt"), "uncommitted edit missing: {:?}", paths);

        let committed = result.files.iter().find(|f| f.path == "committed.txt").unwrap();
        assert_eq!(committed.status, "added");
        assert_eq!(committed.additions, 2);
        assert_eq!(committed.deletions, 0);
    }

    #[test]
    fn diff_against_base_detects_rename() {
        let tmp = tempdir_via_env();
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

        let result = diff_against_base(tmp.to_str().unwrap(), "base", None).unwrap();
        let renamed = result
            .files
            .iter()
            .find(|f| f.path == "renamed.txt")
            .unwrap_or_else(|| panic!("renamed file missing: {:?}",
                result.files.iter().map(|f| (&f.path, &f.status)).collect::<Vec<_>>()));
        assert_eq!(renamed.status, "renamed");
        assert_eq!(renamed.old_path.as_deref(), Some("base.txt"));
    }

    #[test]
    fn diff_against_base_falls_back_to_head_when_base_unresolvable() {
        let tmp = tempdir_via_env();
        let _repo = init_repo_with_base(&tmp);

        std::fs::write(tmp.join("base.txt"), "one\ntwo\nthree\nfour\n").unwrap();

        // A base_ref that doesn't resolve must fall back to HEAD, not error.
        let result =
            diff_against_base(tmp.to_str().unwrap(), "origin/does-not-exist", None).unwrap();
        let f = result.files.iter().find(|f| f.path == "base.txt").unwrap();
        assert_eq!(f.status, "modified");
        assert_eq!(f.additions, 1);
        assert_eq!(f.deletions, 0);
    }

    #[test]
    fn diff_against_base_flags_binary_without_hunks() {
        let tmp = tempdir_via_env();
        let _repo = init_repo_with_base(&tmp);

        std::fs::write(tmp.join("blob.bin"), [0u8, 1, 2, 0, 255, 7]).unwrap();

        let result = diff_against_base(tmp.to_str().unwrap(), "base", None).unwrap();
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
    fn html_escape_neutralises_markup() {
        assert_eq!(html_escape("a < b && c > d"), "a &lt; b &amp;&amp; c &gt; d");
    }

    #[test]
    fn diff_against_base_only_restricts_to_one_path() {
        let tmp = tempdir_via_env();
        let repo = init_repo_with_base(&tmp);

        std::fs::write(tmp.join("a.txt"), "x\n").unwrap();
        std::fs::write(tmp.join("b.txt"), "y\n").unwrap();
        commit_all(&repo, "two files");

        let result = diff_against_base(tmp.to_str().unwrap(), "base", Some("a.txt")).unwrap();
        let paths: Vec<&str> = result.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt"], "only the filtered path should appear");
    }

    // ----- node → directory resolution (worktree vs root) ----------------

    fn make_node(path: &str, use_worktree: bool, worktree_name: Option<&str>) -> crate::models::AgentNode {
        use crate::models::{AgentNode, EnvType, Provider, SessionStatus};
        AgentNode {
            id: 1,
            mesh_id: 1,
            name: "n".to_string(),
            path: path.to_string(),
            branch: "base".to_string(),
            env: EnvType::Windows,
            provider: Provider::Anthropic,
            status: SessionStatus::Idle,
            cli_session_id: None,
            worktree_name: worktree_name.map(|s| s.to_string()),
            use_worktree,
            source_issue: None,
            position: 0,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn node_diff_worktree_name_selects_only_for_worktree_nodes() {
        assert_eq!(node_diff_worktree_name(true, Some("wt")), Some("wt"));
        // Blank / whitespace names are not a worktree.
        assert_eq!(node_diff_worktree_name(true, Some("   ")), None);
        assert_eq!(node_diff_worktree_name(true, None), None);
        // Root-mode node ignores any stored name.
        assert_eq!(node_diff_worktree_name(false, Some("wt")), None);
    }

    // Regression: the review surface diffed `node.path` (the project root), but a
    // worktree agent's edits live in `{path}/.claude/worktrees/{name}`. The root
    // read clean while the agent's work sat in the worktree — so the panel showed
    // "no changes" while the title-bar chip (which resolves the worktree) showed a
    // count. The diff must follow the same resolved worktree path.
    #[test]
    fn diff_node_inspects_worktree_not_project_root() {
        // Project root: a clean repo with a `base` branch, nothing pending.
        let root = tempdir_via_env();
        let _root_repo = init_repo_with_base(&root);

        let node = make_node(root.to_str().unwrap(), true, Some("wt"));
        let wt_path = node_repo_path(&node);
        assert!(
            wt_path.replace('\\', "/").ends_with("/.claude/worktrees/wt"),
            "expected the worktree path, got {wt_path}"
        );

        // Materialise the worktree as its own repo with an uncommitted edit.
        std::fs::create_dir_all(&wt_path).unwrap();
        let wt_dir = std::path::Path::new(&wt_path);
        let _wt_repo = init_repo_with_base(wt_dir);
        std::fs::write(wt_dir.join("base.txt"), "one\nTWO\nthree\n").unwrap();

        // Following the resolved worktree path surfaces the agent's edit...
        let via_worktree = diff_against_base(&wt_path, "base", None).unwrap();
        assert!(
            via_worktree.files.iter().any(|f| f.path == "base.txt"),
            "worktree edit missing: {:?}",
            via_worktree.files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );

        // ...while the old behaviour — diffing the project root — never surfaces
        // the agent's edit (the root's own `base.txt` is untouched; it can only
        // see the worktree as an opaque untracked subdir).
        let via_root = diff_against_base(node.path.as_str(), "base", None).unwrap();
        assert!(
            !via_root.files.iter().any(|f| f.path == "base.txt"),
            "project-root diff must not surface the worktree's edit, got {:?}",
            via_root.files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
    }
}
