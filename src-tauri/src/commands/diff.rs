//! Diff computation using difference-rs

use crate::db;
use crate::models::{DiffHunk, DiffLine, FileDiff, DiffResult};
use difference_rs::{Changeset, Difference};
use std::path::PathBuf;
use std::fs;
use tauri::command;

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

/// Diff two files on disk
#[command]
pub async fn diff_files(
    old_path: String,
    new_path: String,
) -> Result<DiffResult, String> {
    let old_content = fs::read_to_string(&old_path).unwrap_or_default();
    let new_content = fs::read_to_string(&new_path).unwrap_or_default();
    let lines = compute_file_diff(&old_content, &new_content);

    let hunks = vec![DiffHunk {
        old_start: 1,
        old_lines: lines.iter().filter(|l| l.line_type == "remove").count(),
        new_start: 1,
        new_lines: lines.iter().filter(|l| l.line_type == "add").count(),
        lines,
    }];

    Ok(DiffResult {
        files: vec![FileDiff {
            path: new_path,
            hunks,
        }],
    })
}

/// Diff workspace files against a checkpoint
#[command]
pub async fn diff_workspace_checkpoint(
    workspace_id: i64,
    checkpoint_id: i64,
) -> Result<DiffResult, String> {
    let workspace = db::get_workspace_by_id(workspace_id)
        .map_err(|e| e.to_string())?;
    let checkpoint = db::get_checkpoint_by_id(checkpoint_id)
        .map_err(|e| e.to_string())?;
    let ref_name = format!("refs/heads/conductor/checkpoints/c{}", checkpoint.turn_index);

    let repo = git2::Repository::open(&workspace.path)
        .map_err(|e| e.to_string())?;

    let reference = repo.find_reference(&ref_name)
        .map_err(|e| e.to_string())?;
    let commit = reference.peel_to_commit()
        .map_err(|e| e.to_string())?;
    let tree = commit.as_object().peel_to_tree()
        .map_err(|e| e.to_string())?;

    let mut files = Vec::new();
    let current_path = PathBuf::from(&workspace.path);

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
                        let hunks = vec![DiffHunk {
                            old_start: 1,
                            old_lines: lines.iter().filter(|l| l.line_type == "remove").count(),
                            new_start: 1,
                            new_lines: lines.iter().filter(|l| l.line_type == "add").count(),
                            lines,
                        }];
                        files.push(FileDiff { path: file_path, hunks });
                    }
                } else {
                    let lines = compute_file_diff("", &current_content);
                    let hunks = vec![DiffHunk {
                        old_start: 0,
                        old_lines: 0,
                        new_start: 1,
                        new_lines: lines.len(),
                        lines,
                    }];
                    files.push(FileDiff { path: file_path, hunks });
                }
            }
        }
    }

    Ok(DiffResult { files })
}
