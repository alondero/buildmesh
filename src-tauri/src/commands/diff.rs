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

static SYNTAX_SET: Lazy<SyntaxSet> = Lazy::new(|| SyntaxSet::load_defaults_newlines());
static THEME_SET: Lazy<ThemeSet> = Lazy::new(|| ThemeSet::load_defaults());

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

/// Diff two files on disk
#[command]
pub async fn diff_files(
    old_path: String,
    new_path: String,
) -> Result<DiffResult, String> {
    let old_content = fs::read_to_string(&old_path).unwrap_or_default();
    let new_content = fs::read_to_string(&new_path).unwrap_or_default();
    let lines = compute_file_diff(&old_content, &new_content);
    let (old_highlighted, new_highlighted) = build_sides(&lines);
    let old_hl = highlight_content(&old_highlighted, &new_path);
    let new_hl = highlight_content(&new_highlighted, &new_path);

    let hunks = vec![DiffHunk {
        old_start: 1,
        old_lines: lines.iter().filter(|l| l.line_type == "remove").count(),
        new_start: 1,
        new_lines: lines.iter().filter(|l| l.line_type == "add").count(),
        old_highlighted: old_hl,
        new_highlighted: new_hl,
        lines,
    }];

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
}