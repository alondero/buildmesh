//! Session discovery — scans Claude Code's on-disk session storage to find
//! resumable sessions that Buildmesh may not already track.

use crate::db;
use crate::env;
use serde::Serialize;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredSession {
    pub session_id: String,
    pub first_message: String,
    pub branch: Option<String>,
    pub cwd: Option<String>,
    pub timestamp: Option<String>,
    pub worktree_name: Option<String>,
}

/// Encode a filesystem path the same way Claude Code does: replace every
/// non-alphanumeric character with `-`. On Windows this includes the drive
/// colon (`:`) and `\` separators; on Unix it covers `/`. It also matches
/// Claude Code's handling of `.` in worktree paths (e.g. `.claude` →
/// `-claude`), so `X:\src\buildmesh\.claude\worktrees\foo` round-trips to
/// `X--src-buildmesh--claude-worktrees-foo`.
fn encode_path(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Extract the worktree name from an encoded project directory name.
/// e.g. "-Users-adam-repo--claude-worktrees-fancy-name" → Some("fancy-name")
fn extract_worktree_name(dir_name: &str, base_prefix: &str) -> Option<String> {
    let suffix = dir_name.strip_prefix(base_prefix)?;
    let wt_marker = "--claude-worktrees-";
    suffix.strip_prefix(wt_marker).map(|s| s.to_string())
}

/// Strip XML/HTML-like tags from a string for clean display.
fn strip_tags(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut inside_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            _ if !inside_tag => result.push(ch),
            _ => {}
        }
    }
    result.trim().to_string()
}

/// Truncate a string to at most `max` characters, appending "…" if truncated.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

/// Parse a JSONL session file to extract the first user message and metadata.
/// Reads lines until it finds a `type: "user"` entry, then stops.
fn parse_session_file(path: &PathBuf) -> Option<(String, Option<String>, Option<String>, Option<String>)> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if line.is_empty() {
            continue;
        }
        let Some(val) = serde_json::from_str::<serde_json::Value>(&line).ok() else { continue };

        if val.get("type").and_then(|t| t.as_str()) == Some("user") {
            if let Some(msg) = val.get("message") {
                if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
                    continue;
                }
                let content = msg.get("content");
                let text = match content {
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(serde_json::Value::Array(arr)) => {
                        arr.iter()
                            .filter_map(|block| {
                                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                                    block.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                                } else {
                                    None
                                }
                            })
                            .next()
                            .unwrap_or_default()
                    }
                    _ => String::new(),
                };

                let clean = strip_tags(&text);
                let display = truncate(&clean, 80);
                let branch = val.get("gitBranch").and_then(|b| b.as_str()).map(|s| s.to_string());
                let cwd = val.get("cwd").and_then(|c| c.as_str()).map(|s| s.to_string());
                let timestamp = val.get("timestamp").and_then(|t| t.as_str()).map(|s| s.to_string());

                return Some((display, branch, cwd, timestamp));
            }
        }
    }
    None
}

/// Discover Claude Code sessions on disk for the given mesh path.
/// Returns sessions that are NOT already tracked by active/idle/suspended Buildmesh nodes.
pub fn discover(mesh_id: i64, mesh_path: &str) -> Result<Vec<DiscoveredSession>, String> {
    let claude_dir = env::claude_dir();
    let projects_dir = claude_dir.join("projects");

    if !projects_dir.exists() {
        return Ok(vec![]);
    }

    let encoded_prefix = encode_path(mesh_path);

    // Get session IDs already tracked by non-archived Buildmesh nodes for this mesh
    let tracked_ids: std::collections::HashSet<String> = db::list_agent_nodes_by_mesh(mesh_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|n| n.status != crate::models::SessionStatus::Archived)
        .filter_map(|n| n.cli_session_id)
        .collect();

    let mut sessions: Vec<DiscoveredSession> = Vec::new();

    let entries = fs::read_dir(&projects_dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let dir_name = entry.file_name().to_string_lossy().to_string();
        if !dir_name.starts_with(&encoded_prefix) {
            continue;
        }

        let worktree_name = extract_worktree_name(&dir_name, &encoded_prefix);

        let dir_path = entry.path();
        if !dir_path.is_dir() {
            continue;
        }

        let jsonl_files = fs::read_dir(&dir_path)
            .map_err(|e| e.to_string())?
            .flatten()
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext == "jsonl")
                    .unwrap_or(false)
            });

        for jsonl_entry in jsonl_files {
            let jsonl_path = jsonl_entry.path();
            let session_id = jsonl_path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();

            if session_id.is_empty() || tracked_ids.contains(&session_id) {
                continue;
            }

            if let Some((first_message, branch, cwd, timestamp)) = parse_session_file(&jsonl_path) {
                if first_message.is_empty() {
                    continue;
                }

                // Use file mtime as fallback timestamp
                let ts = timestamp.or_else(|| {
                    jsonl_entry
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .map(|t| {
                            let dt: chrono::DateTime<chrono::Utc> = t.into();
                            dt.to_rfc3339()
                        })
                });

                sessions.push(DiscoveredSession {
                    session_id,
                    first_message,
                    branch,
                    cwd,
                    timestamp: ts,
                    worktree_name: worktree_name.clone(),
                });
            }
        }
    }

    // Sort by timestamp descending (most recent first)
    sessions.sort_by(|a, b| {
        let ta = a.timestamp.as_deref().unwrap_or("");
        let tb = b.timestamp.as_deref().unwrap_or("");
        tb.cmp(ta)
    });

    Ok(sessions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_path_replaces_separators() {
        assert_eq!(
            encode_path("/Users/adam/src/buildmesh"),
            "-Users-adam-src-buildmesh"
        );
    }

    #[test]
    fn encode_path_replaces_windows_drive_colon() {
        // Matches Claude Code's on-disk form: ~/.claude/projects/X--src-buildmesh
        // The drive colon and every backslash both collapse to `-`.
        assert_eq!(
            encode_path("X:\\src\\buildmesh"),
            "X--src-buildmesh"
        );
        assert_eq!(
            encode_path("C:\\Users\\adam\\src\\buildmesh"),
            "C--Users-adam-src-buildmesh"
        );
    }

    #[test]
    fn encode_path_replaces_dot_for_worktrees() {
        // `.claude\worktrees\foo` → `--claude-worktrees-foo` on Windows.
        assert_eq!(
            encode_path("X:\\src\\buildmesh\\.claude\\worktrees\\foo"),
            "X--src-buildmesh--claude-worktrees-foo"
        );
    }

    #[test]
    fn extract_worktree_name_works() {
        let base = "-Users-adam-src-buildmesh";
        assert_eq!(
            extract_worktree_name("-Users-adam-src-buildmesh--claude-worktrees-fancy-name", base),
            Some("fancy-name".to_string())
        );
        assert_eq!(extract_worktree_name("-Users-adam-src-buildmesh", base), None);
        assert_eq!(
            extract_worktree_name("-Users-adam-src-buildmesh-src-tauri", base),
            None
        );
    }

    #[test]
    fn extract_worktree_name_works_on_windows_encoded_base() {
        let base = "X--src-buildmesh";
        assert_eq!(
            extract_worktree_name("X--src-buildmesh--claude-worktrees-bold-live-plume", base),
            Some("bold-live-plume".to_string())
        );
        assert_eq!(extract_worktree_name("X--src-buildmesh", base), None);
    }

    #[test]
    fn strip_tags_removes_xml() {
        assert_eq!(
            strip_tags("<command-message>grill-me</command-message>"),
            "grill-me"
        );
        assert_eq!(strip_tags("hello world"), "hello world");
        assert_eq!(strip_tags("<b>bold</b> and <i>italic</i>"), "bold and italic");
    }

    #[test]
    fn truncate_respects_boundary() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world this is long", 11), "hello world…");
    }
}
