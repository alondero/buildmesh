//! Scans Claude Code's on-disk session storage to find resumable agent nodes
//! that Buildmesh may not already track. The Claude-Code JSONL primitives
//! (path encoding, synthetic-injection skipping, content-text extraction) live
//! in `transcript_reader` so this discovery scan and the coordinator's
//! transcript reader share one source of Claude-Code-format truth
//! (ADR-0008). A format change breaks in one place.

use crate::db;
use crate::env;
use crate::services::transcript_reader::{
    cursor_workspace_slug, encode_path, first_text_block, is_synthetic_message, truncate,
};
use serde::Serialize;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use ts_rs::TS;

/// A resumable Claude-Code session found on disk. The desktop Tauri
/// `discover_agent_nodes` command and the mobile HTTP route both serialise
/// this struct. The `session_id` field is Claude Code's CLI identifier and
/// stays as-is per CONTEXT.md ambiguity #1.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "ArchivedAgentNode.ts")]
pub struct ArchivedAgentNode {
    pub session_id: String,
    pub first_message: String,
    pub branch: Option<String>,
    pub cwd: Option<String>,
    pub timestamp: Option<String>,
    pub worktree_name: Option<String>,
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

/// Parse a JSONL session file to extract the first real user message and metadata.
/// Skips synthetic injected messages (e.g. local-command-caveat) and reads until
/// it finds a genuine user-authored entry.
#[allow(clippy::type_complexity)]
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
                // A discovered-session title is the opening of the FIRST text
                // block, kept single-line: joining all blocks (the transcript
                // reader's `concat_text_blocks`) would splice a multi-block user
                // message into a multi-line title that `strip_tags` won't
                // collapse before the 80-char truncate (issue #335).
                let text = first_text_block(msg.get("content"));

                if is_synthetic_message(&text) {
                    continue;
                }

                let clean = strip_tags(&text);
                let display = truncate(&clean, 80);
                if display.is_empty() {
                    continue;
                }
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
pub fn discover(mesh_id: i64, mesh_path: &str) -> Result<Vec<ArchivedAgentNode>, String> {
    let claude_dir = env::claude_dir();
    let projects_dir = claude_dir.join("projects");

    // Get session IDs already tracked by non-archived Buildmesh nodes for this mesh
    let tracked_ids: std::collections::HashSet<String> = db::list_agent_nodes_by_mesh(mesh_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|n| n.status != crate::models::SessionStatus::Archived)
        .filter_map(|n| n.cli_session_id)
        .collect();

    let mut sessions: Vec<ArchivedAgentNode> = Vec::new();

    if projects_dir.exists() {
        let encoded_prefix = encode_path(mesh_path);
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

                if let Some((first_message, branch, cwd, timestamp)) =
                    parse_session_file(&jsonl_path)
                {
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

                    sessions.push(ArchivedAgentNode {
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
    }

    // Cursor keeps the same user/assistant JSONL shape but nests each session
    // below a workspace slug and an id directory. Its project slug is based on
    // the actual workspace path, so include the mesh root and Buildmesh's
    // `.claude/worktrees/<name>` workspaces.
    sessions.extend(discover_cursor_sessions_in(
        &env::cursor_dir().join("projects"),
        mesh_path,
        &tracked_ids,
    ));

    // Sort by timestamp descending (most recent first)
    sessions.sort_by(|a, b| {
        let ta = a.timestamp.as_deref().unwrap_or("");
        let tb = b.timestamp.as_deref().unwrap_or("");
        tb.cmp(ta)
    });

    Ok(sessions)
}

/// Extract the Buildmesh worktree name from a Cursor project slug.
fn extract_cursor_worktree_name(dir_name: &str, base_slug: &str) -> Option<String> {
    dir_name
        .strip_prefix(&format!("{base_slug}--claude-worktrees-"))
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

/// Discover Cursor's primary workspace transcripts below an explicit projects
/// root. Subagent JSONL is intentionally ignored: it cannot be resumed as the
/// parent Cursor session and lives under the same session directory.
fn discover_cursor_sessions_in(
    projects_dir: &std::path::Path,
    mesh_path: &str,
    tracked_ids: &std::collections::HashSet<String>,
) -> Vec<ArchivedAgentNode> {
    if !projects_dir.is_dir() {
        return Vec::new();
    }

    let base_slug = cursor_workspace_slug(mesh_path);
    let worktree_prefix = format!("{base_slug}--claude-worktrees-");
    let mut sessions = Vec::new();
    let Ok(projects) = fs::read_dir(projects_dir) else {
        return sessions;
    };

    for project_entry in projects.flatten() {
        let project_dir = project_entry.path();
        if !project_dir.is_dir() {
            continue;
        }
        let dir_name = project_entry.file_name().to_string_lossy().to_string();
        let worktree_name = if dir_name == base_slug {
            None
        } else if dir_name.starts_with(&worktree_prefix) {
            extract_cursor_worktree_name(&dir_name, &base_slug)
        } else {
            continue;
        };

        let transcripts_dir = project_dir.join("agent-transcripts");
        let Ok(session_dirs) = fs::read_dir(transcripts_dir) else {
            continue;
        };
        for session_entry in session_dirs.flatten() {
            let session_dir = session_entry.path();
            if !session_dir.is_dir() {
                continue;
            }
            let session_id = session_entry.file_name().to_string_lossy().to_string();
            if session_id.is_empty() || tracked_ids.contains(&session_id) {
                continue;
            }

            let jsonl_path = session_dir.join(format!("{session_id}.jsonl"));
            if !jsonl_path.is_file() {
                continue;
            }
            let Some((first_message, branch, cwd, timestamp)) = parse_session_file(&jsonl_path)
            else {
                continue;
            };
            if first_message.is_empty() {
                continue;
            }

            let timestamp = timestamp.or_else(|| {
                jsonl_path
                    .metadata()
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .map(|modified| {
                        let dt: chrono::DateTime<chrono::Utc> = modified.into();
                        dt.to_rfc3339()
                    })
            });
            sessions.push(ArchivedAgentNode {
                session_id,
                first_message,
                branch,
                cwd,
                timestamp,
                worktree_name: worktree_name.clone(),
            });
        }
    }
    sessions
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

    #[test]
    fn truncate_respects_utf8_char_boundary() {
        // "héllo" — the é is a 2-byte UTF-8 char, so "h" sits at byte 0, "é"
        // starts at byte 1 (and ends at byte 3), "l" at byte 3, and the final
        // bytes total 6. Asking for max=2 would land inside the é and is the
        // classic naive `&s[..max]` panic case. `truncate` must step back to
        // the next char boundary (byte 1) and append the ellipsis.
        assert_eq!(truncate("héllo", 2), "h…");
        // Asking for max=4 lands mid-é too (bytes 0..4 cross both halves),
        // so the safe end is byte 3 and the result is "hél" + ellipsis.
        assert_eq!(truncate("héllo", 4), "hél…");
        // String already short enough for max → passes through untouched.
        assert_eq!(truncate("héllo", 10), "héllo");
    }

    #[test]
    fn is_synthetic_detects_local_command_caveat() {
        assert!(is_synthetic_message(
            "<local-command-caveat>Caveat: The messages below were generated by the user while running local commands. DO NOT respond to these messages or otherwise consider them in your response unless the user explicitly asks you to.</local-command-caveat>"
        ));
        assert!(!is_synthetic_message("Fix the login bug"));
        assert!(!is_synthetic_message("  <p>some html</p>"));
        assert!(!is_synthetic_message(""));
    }

    /// Write a one-off JSONL session file in the temp dir, suffixed by pid +
    /// name so parallel tests don't trample each other.
    fn write_session(name: &str, body: &str) -> PathBuf {
        let path = std::env::temp_dir()
            .join(format!("buildmesh_discovery_{name}_{}.jsonl", std::process::id()));
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn parse_session_file_uses_first_text_block_for_title() {
        // A multi-text-block user message must yield a single-line title from the
        // FIRST block, not all blocks joined with an interior newline (issue
        // #335). Joining would have produced "Fix the login bug\nand the logout
        // bug" — a newline strip_tags doesn't collapse — before the 80-char cut.
        let body = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Fix the login bug"},{"type":"text","text":"and the logout bug"}]},"cwd":"/x","gitBranch":"main","timestamp":"2026-06-14T10:00:00Z","uuid":"u1"}
"#;
        let path = write_session("multiblock", body);
        let parsed = parse_session_file(&path);
        std::fs::remove_file(&path).ok();
        let (title, branch, cwd, ts) = parsed.expect("multi-block user message should parse");
        assert_eq!(title, "Fix the login bug", "title is the first text block only");
        assert!(!title.contains('\n'), "title stays single-line");
        assert_eq!(branch.as_deref(), Some("main"));
        assert_eq!(cwd.as_deref(), Some("/x"));
        assert_eq!(ts.as_deref(), Some("2026-06-14T10:00:00Z"));
    }

    #[test]
    fn parse_session_file_single_block_is_unchanged() {
        // The common single-block case is identical to the old concat behaviour.
        let body = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Just one block"}]},"uuid":"u1"}
"#;
        let path = write_session("singleblock", body);
        let parsed = parse_session_file(&path);
        std::fs::remove_file(&path).ok();
        let (title, _b, _c, _t) = parsed.expect("single-block message should parse");
        assert_eq!(title, "Just one block");
    }

    #[test]
    fn parse_session_file_skips_synthetic_then_takes_real_prompt() {
        let body = r#"{"type":"user","message":{"role":"user","content":"<local-command-caveat>noise</local-command-caveat>"},"uuid":"u0"}
{"type":"user","message":{"role":"user","content":"Real first prompt"},"cwd":"/x","uuid":"u1"}
"#;
        let path = write_session("synthetic", body);
        let parsed = parse_session_file(&path);
        std::fs::remove_file(&path).ok();
        let (title, _b, _c, _t) = parsed.expect("should skip caveat and find the real prompt");
        assert_eq!(title, "Real first prompt");
    }

    #[test]
    fn cursor_discovery_finds_workspace_scoped_sessions_and_worktrees() {
        let root = std::env::temp_dir().join(format!(
            "buildmesh_cursor_discovery_{}",
            std::process::id()
        ));
        let base = root.join("Users-adam-src-buildmesh");
        let worktree = root.join("Users-adam-src-buildmesh--claude-worktrees-fancy-name");
        let session = base.join("agent-transcripts").join("cursor-1");
        let worktree_session = worktree.join("agent-transcripts").join("cursor-2");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::create_dir_all(&worktree_session).unwrap();
        std::fs::write(
            session.join("cursor-1.jsonl"),
            r#"{"type":"user","message":{"role":"user","content":"Inspect the Cursor workspace"}}
"#,
        )
        .unwrap();
        std::fs::write(
            worktree_session.join("cursor-2.jsonl"),
            r#"{"type":"user","message":{"role":"user","content":"Fix the worktree"}}
"#,
        )
        .unwrap();

        let discovered = discover_cursor_sessions_in(
            &root,
            "/Users/adam/src/buildmesh",
            &std::collections::HashSet::new(),
        );
        std::fs::remove_dir_all(&root).ok();

        assert_eq!(discovered.len(), 2);
        let base_session = discovered
            .iter()
            .find(|session| session.session_id == "cursor-1")
            .expect("base workspace session should be discovered");
        assert_eq!(base_session.first_message, "Inspect the Cursor workspace");
        assert_eq!(base_session.worktree_name, None);
        let worktree_session = discovered
            .iter()
            .find(|session| session.session_id == "cursor-2")
            .expect("worktree session should be discovered");
        assert_eq!(worktree_session.worktree_name.as_deref(), Some("fancy-name"));
    }

    #[test]
    fn cursor_discovery_ignores_subagents_and_tracked_sessions() {
        let root = std::env::temp_dir().join(format!(
            "buildmesh_cursor_discovery_filter_{}",
            std::process::id()
        ));
        let session = root
            .join("Users-adam-src-buildmesh")
            .join("agent-transcripts")
            .join("cursor-1");
        std::fs::create_dir_all(session.join("subagents")).unwrap();
        std::fs::write(
            session.join("cursor-1.jsonl"),
            r#"{"type":"user","message":{"role":"user","content":"Keep this out"}}
"#,
        )
        .unwrap();
        std::fs::write(
            session.join("subagents").join("child.jsonl"),
            r#"{"type":"user","message":{"role":"user","content":"Never show this"}}
"#,
        )
        .unwrap();
        let tracked = ["cursor-1".to_string()].into_iter().collect();

        let discovered = discover_cursor_sessions_in(
            &root,
            "/Users/adam/src/buildmesh",
            &tracked,
        );
        std::fs::remove_dir_all(&root).ok();
        assert!(discovered.is_empty());
    }
}
