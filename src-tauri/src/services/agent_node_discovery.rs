//! Scans Claude Code's on-disk session storage to find resumable agent nodes
//! that Buildmesh may not already track. The Claude-Code JSONL primitives
//! (path encoding, synthetic-injection skipping, content-text extraction) live
//! in `transcript_reader` so this discovery scan and the coordinator's
//! transcript reader share one source of Claude-Code-format truth
//! (ADR-0008). A format change breaks in one place.

use crate::db;
use crate::env;
use crate::models::EnvType;
use crate::services::commandcode_session;
use crate::services::transcript_reader::{
    cursor_workspace_slug, encode_path, first_text_block, is_synthetic_message, truncate,
};
use serde::Serialize;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
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

    // Command Code stores sessions per-project under
    // `<home>/.commandcode/projects/<encoded-cwd>/<uuid>.jsonl` (issue #1500).
    // Walk every `<encoded-mesh>*` project dir so mesh-root and worktree
    // sessions are both found; the session header cwd determines the worktree.
    let env_type = EnvType::from(env::env_for_path(Path::new(mesh_path)));
    if let Some(projects_dir) = env::commandcode_projects_dir(env_type, mesh_path) {
        if projects_dir.is_dir() {
            // Slugs are lowercased; compare case-insensitively for Windows.
            let encoded_prefix = env::commandcode_project_slug(mesh_path).to_lowercase();
            if let Ok(entries) = fs::read_dir(&projects_dir) {
                for entry in entries.flatten() {
                    let dir_name = entry.file_name().to_string_lossy().to_string();
                    if !dir_name.to_lowercase().starts_with(&encoded_prefix) {
                        continue;
                    }
                    let dir_path = entry.path();
                    if !dir_path.is_dir() {
                        continue;
                    }
                    sessions.extend(discover_commandcode_sessions_in(
                        Some(dir_path.as_path()),
                        mesh_path,
                        &tracked_ids,
                    ));
                }
            }
        }
    }

    // Antigravity (`agy`) stores every conversation globally under
    // `~/.gemini/antigravity-cli/brain/<conversation_id>/` (issue #1284).
    // The scanner walks every conversation directory, reads the
    // `.system_generated/logs/transcript.jsonl` for the first USER_INPUT step,
    // and pulls the conversation_id off the directory name (matches the
    // `conversation id: <UUID>` banner the agy TUI prints, so the captured
    // id in `agent_nodes.cli_session_id` aligns 1:1 with this dir name).
    sessions.extend(discover_agy_sessions_in(
        &env::agy_brain_dir(),
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

/// Discover Command Code's session files below an explicit project sessions
/// directory (`<home>/.commandcode/projects/<encoded-cwd>/`). The session
/// header supplies the working directory and timestamp; the transcript
/// supplies the first user message for the archive label. Sidecars
/// (`<id>.checkpoints.jsonl`, …) are skipped via `read_session_file`.
fn discover_commandcode_sessions_in(
    sessions_dir: Option<&Path>,
    mesh_path: &str,
    tracked_ids: &std::collections::HashSet<String>,
) -> Vec<ArchivedAgentNode> {
    let Some(sessions_dir) = sessions_dir.filter(|path| path.is_dir()) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(sessions_dir) else {
        return Vec::new();
    };

    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }

        let Some(candidate) = commandcode_session::read_session_file(&path) else {
            continue;
        };
        if tracked_ids.contains(&candidate.id) {
            continue;
        }

        let is_mesh_root = env::directories_match(&candidate.directory, mesh_path);
        let worktree_name = if is_mesh_root {
            None
        } else {
            extract_worktree_name_from_path(&candidate.directory, mesh_path)
        };
        if !is_mesh_root && worktree_name.is_none() {
            continue;
        }

        let Some(first_message) = parse_commandcode_first_user_message(&path) else {
            continue;
        };
        let timestamp = (candidate.timestamp_ms > 0)
            .then(|| chrono::DateTime::from_timestamp_millis(candidate.timestamp_ms))
            .flatten()
            .map(|timestamp| timestamp.to_rfc3339())
            .or_else(|| {
                path.metadata()
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .map(|modified| {
                        let timestamp: chrono::DateTime<chrono::Utc> = modified.into();
                        timestamp.to_rfc3339()
                    })
            });

        sessions.push(ArchivedAgentNode {
            session_id: candidate.id,
            first_message,
            branch: None,
            cwd: (!candidate.directory.is_empty()).then_some(candidate.directory),
            timestamp,
            worktree_name,
        });
    }
    sessions
}

/// Read the first genuine user prompt from a Command Code transcript for the
/// archive label. Internal blocks and tool-result envelopes have no title text.
fn parse_commandcode_first_user_message(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    for line in reader.lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(|kind| kind.as_str()) != Some("message") {
            continue;
        }
        let Some(message) = value.get("message") else {
            continue;
        };
        if message.get("role").and_then(|role| role.as_str()) != Some("user") {
            continue;
        }
        let text = first_text_block(message.get("content"));
        if is_synthetic_message(&text) {
            continue;
        }
        let display = truncate(&strip_tags(&text), 80);
        if !display.is_empty() {
            return Some(display);
        }
    }
    None
}

/// Discover Antigravity (`agy`) conversations stored under `<brain_dir>/`.
/// AGY writes every conversation globally — not project-scoped like Claude
/// Code — so the scanner walks each `<conversation_id>/` directory and pulls
/// the first `USER_INPUT` step from `.system_generated/logs/transcript.jsonl`.
/// Issue #1284. The `session_id` we return is the directory name, which is
/// the same UUID the agy TUI prints as `conversation id: <UUID>` and the
/// `AgentProvider::resume_args` path feeds back to `--conversation` on resume.
fn discover_agy_sessions_in(
    brain_dir: &std::path::Path,
    mesh_path: &str,
    tracked_ids: &std::collections::HashSet<String>,
) -> Vec<ArchivedAgentNode> {
    if !brain_dir.is_dir() {
        return Vec::new();
    }

    let mut sessions = Vec::new();
    let Ok(entries) = fs::read_dir(brain_dir) else {
        return sessions;
    };

    for entry in entries.flatten() {
        let conv_dir = entry.path();
        if !conv_dir.is_dir() {
            continue;
        }
        let session_id = entry.file_name().to_string_lossy().to_string();
        if session_id.is_empty() || tracked_ids.contains(&session_id) {
            continue;
        }

        let jsonl_path = conv_dir.join(".system_generated").join("logs").join("transcript.jsonl");
        if !jsonl_path.is_file() {
            continue;
        }

        let Some((first_message, workspace_path, created_at)) =
            parse_agy_transcript(&jsonl_path)
        else {
            continue;
        };
        if first_message.is_empty() {
            continue;
        }

        // Fall back to transcript mtime when no step carried a timestamp —
        // the conversation was written at some point, so the file mtime is
        // always a safe "wall-clock at least this old" anchor for the sort.
        let timestamp = created_at.or_else(|| {
            jsonl_path
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|t| {
                    let dt: chrono::DateTime<chrono::Utc> = t.into();
                    dt.to_rfc3339()
                })
        });

        // AGY conversations live outside the mesh's own `.claude/projects/`
        // tree, so the worktree name comes purely from the transcript's
        // workspace metadata. Only attach a worktree name when the path
        // resolves inside *this* mesh's worktree convention
        // (`.claude/worktrees/<name>` suffix matching `mesh_path`); anything
        // else stays at the mesh root with no worktree_name so the user
        // sees a global session without a misleading association.
        let worktree_name = workspace_path
            .as_deref()
            .and_then(|p| extract_worktree_name_from_path(p, mesh_path));

        sessions.push(ArchivedAgentNode {
            session_id,
            first_message,
            branch: None,
            cwd: workspace_path,
            timestamp,
            worktree_name,
        });
    }
    sessions
}

/// Map an AGY step's `workspace_path` onto a Buildmesh worktree name when the
/// path lives under `<mesh>/.claude/worktrees/<name>`. Mirrors the
/// `extract_worktree_name` / `extract_cursor_worktree_name` rules used by the
/// other scanners — same separator handling, same `<mesh>/.claude/worktrees/`
/// convention. Returns `None` for the mesh root (no worktree) and for paths
/// that don't resolve to a worktree inside the calling mesh (so a foreign
/// conversation doesn't masquerade as ours).
fn extract_worktree_name_from_path(workspace_path: &str, mesh_path: &str) -> Option<String> {
    let mesh = mesh_path.replace('\\', "/");
    let wp = workspace_path.replace('\\', "/");

    let marker = "/.claude/worktrees/";
    let worktree_idx = wp.find(marker)?;
    // The parent of the marker is the mesh root (the buildmesh repo or any
    // equivalent prefix). Match that against the calling mesh *without*
    // including the marker itself, otherwise we'd compare `<mesh>/.claude/worktrees`
    // against `<mesh>` and never match.
    let parent = &wp[..worktree_idx];
    if !same_path(parent.trim_end_matches('/'), mesh.trim_end_matches('/')) {
        return None;
    }
    let name = &wp[worktree_idx + marker.len()..];
    let name = name.split('/').next().unwrap_or("").trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Cross-platform path equality for the worktree-association check above.
/// We can't `Path::canonicalize` here (the AGY session may reference a path
/// that doesn't exist on *this* machine), so we normalize separators and
/// case-fold the trailing component — enough to catch the obvious drift
/// while staying lenient about trailing slashes and Windows drive case.
fn same_path(a: &str, b: &str) -> bool {
    let normalize = |s: &str| -> String {
        let trimmed = s.trim_end_matches('/').trim_end_matches('\\');
        if cfg!(windows) {
            trimmed.to_lowercase()
        } else {
            trimmed.to_string()
        }
    };
    normalize(a) == normalize(b)
}

/// Parse an AGY conversation transcript and return the first real user prompt
/// along with the workspace path the conversation was opened against and the
/// earliest step timestamp. The AGY transcript format isn't formally
/// documented, so we accept a few field-name variants rather than betting on
/// one: a missing field just degrades to `None` instead of dropping the row.
fn parse_agy_transcript(path: &std::path::Path) -> Option<(String, Option<String>, Option<String>)> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    let mut first_prompt: Option<String> = None;
    let mut workspace_path: Option<String> = None;
    let mut earliest_ts: Option<String> = None;

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if line.is_empty() {
            continue;
        }
        let val: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // First-step workspace wins (every step should agree, but the first
        // one is the canonical "where the conversation started" anchor).
        if workspace_path.is_none() {
            workspace_path = extract_agy_workspace_path(&val);
        }

        // Earliest-wins timestamp for sort stability — the first step carries
        // the conversation's true start, later steps are noise for the
        // Archive list.
        if earliest_ts.is_none() {
            earliest_ts = val
                .get("created_at")
                .or_else(|| val.get("timestamp"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
        }

        // First USER_INPUT step: take the opening prompt verbatim (sans tags,
        // truncated to 80 chars to match the Claude/Cursor scanners).
        if first_prompt.is_none() {
            let step_type = val
                .get("step_type")
                .or_else(|| val.get("type"))
                .and_then(|v| v.as_str());
            if step_type == Some("USER_INPUT") {
                let text = extract_agy_user_text(&val);
                let clean = strip_tags(&text);
                let display = truncate(&clean, 80);
                if !display.is_empty() {
                    first_prompt = Some(display);
                }
            }
        }

        // Cheap early-exit once we have all three.
        if first_prompt.is_some() && workspace_path.is_some() && earliest_ts.is_some() {
            break;
        }
    }

    first_prompt.map(|p| (p, workspace_path, earliest_ts))
}

/// Pull the workspace/cwd out of an AGY step, trying the field names the CLI
/// has been observed to use. Tolerates nested shapes (`metadata.workspace_path`)
/// and the leading-`/` Linux form so a Windows-only field name doesn't drop
/// every WSL session.
fn extract_agy_workspace_path(val: &serde_json::Value) -> Option<String> {
    const KEYS: &[&str] = &[
        "workspace_path",
        "working_directory",
        "cwd",
        "workspace",
        "project_path",
    ];
    for key in KEYS {
        if let Some(s) = val.get(*key).and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
    }
    if let Some(meta) = val.get("metadata") {
        for key in KEYS {
            if let Some(s) = meta.get(*key).and_then(|v| v.as_str()) {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// Pull the prompt text out of a USER_INPUT step. AGY has used several
/// shapes — a top-level `text` string, a `content` string, a `content` array
/// of `{type: "text", text: ...}` blocks (Claude-style), and a `message`
/// wrapper. Accept any of them rather than guessing wrong.
fn extract_agy_user_text(val: &serde_json::Value) -> String {
    if let Some(s) = val.get("text").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(s) = val.get("content").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(s) = val.get("prompt").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(content) = val.get("content").and_then(|v| v.as_array()) {
        return first_text_block(Some(&serde_json::Value::Array(content.clone())));
    }
    if let Some(message) = val.get("message") {
        if let Some(s) = message.get("text").and_then(|v| v.as_str()) {
            return s.to_string();
        }
        if let Some(s) = message.get("content").and_then(|v| v.as_str()) {
            return s.to_string();
        }
        if let Some(content) = message.get("content") {
            return first_text_block(Some(content));
        }
    }
    String::new()
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

    #[test]
    fn commandcode_discovery_finds_mesh_sessions_and_filters_foreign_or_tracked() {
        let root = std::env::temp_dir().join(format!(
            "buildmesh_commandcode_discovery_{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();

        let write_session = |id: &str, cwd: &str, prompt: &str, timestamp: &str| {
            std::fs::write(
                root.join(format!("{id}.jsonl")),
                format!(
                    r#"{{"type":"session","id":"{id}","cwd":"{cwd}","timestamp":"{timestamp}"}}
{{"type":"model_change","model":"commandcode-default"}}
{{"type":"message","id":"user-1","message":{{"role":"user","content":"{prompt}"}}}}
"#
                ),
            )
            .unwrap();
        };
        write_session(
            "sess_base",
            "/Users/adam/src/buildmesh",
            "Inspect the base workspace",
            "2026-08-31T10:00:00Z",
        );
        write_session(
            "sess_worktree",
            "/Users/adam/src/buildmesh/.claude/worktrees/fancy-name",
            "Inspect the worktree",
            "2026-08-31T11:00:00Z",
        );
        write_session(
            "sess_foreign",
            "/Users/other/project",
            "Do not include this",
            "2026-08-31T12:00:00Z",
        );
        write_session(
            "sess_tracked",
            "/Users/adam/src/buildmesh",
            "Already tracked",
            "2026-08-31T13:00:00Z",
        );

        let tracked = ["sess_tracked".to_string()].into_iter().collect();
        let discovered = discover_commandcode_sessions_in(
            Some(root.as_path()),
            "/Users/adam/src/buildmesh",
            &tracked,
        );
        std::fs::remove_dir_all(&root).ok();

        assert_eq!(discovered.len(), 2);
        let base = discovered
            .iter()
            .find(|session| session.session_id == "sess_base")
            .expect("mesh-root Command Code session should be discovered");
        assert_eq!(base.first_message, "Inspect the base workspace");
        assert_eq!(base.cwd.as_deref(), Some("/Users/adam/src/buildmesh"));
        assert!(base.worktree_name.is_none());

        let worktree = discovered
            .iter()
            .find(|session| session.session_id == "sess_worktree")
            .expect("worktree Command Code session should be discovered");
        assert_eq!(worktree.first_message, "Inspect the worktree");
        assert_eq!(worktree.worktree_name.as_deref(), Some("fancy-name"));
    }

    #[test]
    fn commandcode_discovery_finds_uuid_sessions_and_skips_sidecars() {
        // Issue #1500: v1.43.0 session IDs are UUIDs in
        // `projects/<slug>/<uuid>.jsonl` with sidecars beside them.
        let root = std::env::temp_dir().join(format!(
            "buildmesh_commandcode_discovery_uuid_{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();

        let id = "3fadada6-e0a3-44a2-ab68-ce1ecf7207a9";
        std::fs::write(
            root.join(format!("{id}.jsonl")),
            format!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-09-02T19:53:20.151Z\",\"cwd\":\"/Users/adam/src/buildmesh\"}}\n\
                 {{\"type\":\"message\",\"id\":\"user-1\",\"message\":{{\"role\":\"user\",\"content\":\"Inspect the UUID workspace\"}}}}\n"
            ),
        )
        .unwrap();
        // Sidecars must not surface as sessions.
        std::fs::write(
            root.join(format!("{id}.checkpoints.jsonl")),
            r#"{"id":"4f729f43-46ed-4721-a885-3f16f26bff5f","turnNumber":1,"createdAt":"2026-09-02T19:53:40.567Z"}"#,
        )
        .unwrap();

        let discovered = discover_commandcode_sessions_in(
            Some(root.as_path()),
            "/Users/adam/src/buildmesh",
            &std::collections::HashSet::new(),
        );
        std::fs::remove_dir_all(&root).ok();

        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].session_id, id);
        assert_eq!(discovered[0].first_message, "Inspect the UUID workspace");
    }

    // --- AGY discovery (issue #1284) -----------------------------------

    /// Build `<brain>/<conv>/.system_generated/logs/transcript.jsonl` with the
    /// provided JSONL body. Returns the conv dir so callers can attach extra
    /// files or directories if they need to.
    fn write_agy_conv(root: &std::path::Path, conv_id: &str, body: &str) -> PathBuf {
        let logs = root
            .join(conv_id)
            .join(".system_generated")
            .join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("transcript.jsonl"), body).unwrap();
        root.join(conv_id)
    }

    #[test]
    fn agy_discovery_finds_present_sessions_with_first_prompt_and_timestamp() {
        let root = std::env::temp_dir().join(format!(
            "buildmesh_agy_discovery_present_{}",
            std::process::id()
        ));
        // Conversation 1: workspace matches the mesh → no worktree.
        // Conversation 2: workspace is a worktree under the mesh → worktree name.
        write_agy_conv(
            &root,
            "conv-aaaa-1111",
            r#"{"step_type":"USER_INPUT","text":"Open the AGY session","workspace_path":"/Users/adam/src/buildmesh","created_at":"2026-07-10T09:00:00Z"}
{"step_type":"MODEL_RESPONSE","text":"Sure thing"}
"#,
        );
        write_agy_conv(
            &root,
            "conv-bbbb-2222",
            r#"{"step_type":"USER_INPUT","text":"Fix the worktree","workspace_path":"/Users/adam/src/buildmesh/.claude/worktrees/fancy-name","created_at":"2026-07-12T11:30:00Z"}
"#,
        );

        let discovered = discover_agy_sessions_in(
            &root,
            "/Users/adam/src/buildmesh",
            &std::collections::HashSet::new(),
        );
        std::fs::remove_dir_all(&root).ok();

        assert_eq!(discovered.len(), 2, "both conversations should be discovered");
        let base = discovered
            .iter()
            .find(|s| s.session_id == "conv-aaaa-1111")
            .expect("mesh-root session present");
        assert_eq!(base.first_message, "Open the AGY session");
        assert_eq!(base.worktree_name, None, "mesh-root workspace → no worktree");
        assert_eq!(base.cwd.as_deref(), Some("/Users/adam/src/buildmesh"));
        assert_eq!(base.timestamp.as_deref(), Some("2026-07-10T09:00:00Z"));
        assert!(base.branch.is_none(), "AGY transcripts don't carry a branch");
        let worktree = discovered
            .iter()
            .find(|s| s.session_id == "conv-bbbb-2222")
            .expect("worktree session present");
        assert_eq!(worktree.first_message, "Fix the worktree");
        assert_eq!(worktree.worktree_name.as_deref(), Some("fancy-name"));
        assert_eq!(
            worktree.timestamp.as_deref(),
            Some("2026-07-12T11:30:00Z")
        );
    }

    #[test]
    fn agy_discovery_skips_absent_sessions() {
        let root = std::env::temp_dir().join(format!(
            "buildmesh_agy_discovery_absent_{}",
            std::process::id()
        ));
        // Brain dir exists but contains no conversation directories → empty
        // result instead of an error. Matches the Cursor/Claude scanners'
        // missing-dir behavior.
        std::fs::create_dir_all(&root).unwrap();

        let discovered = discover_agy_sessions_in(
            &root,
            "/Users/adam/src/buildmesh",
            &std::collections::HashSet::new(),
        );
        std::fs::remove_dir_all(&root).ok();
        assert!(discovered.is_empty());
    }

    #[test]
    fn agy_discovery_skips_missing_brain_dir() {
        // A brain dir that doesn't exist must not crash — same contract as the
        // Claude/Cursor scanners (the user may never have run `agy`, so the
        // path is likely absent on first launch).
        let missing = std::env::temp_dir().join(format!(
            "buildmesh_agy_discovery_missing_{}",
            std::process::id()
        ));
        // Intentionally NOT creating the directory.
        let discovered = discover_agy_sessions_in(
            &missing,
            "/Users/adam/src/buildmesh",
            &std::collections::HashSet::new(),
        );
        assert!(discovered.is_empty());
    }

    #[test]
    fn agy_discovery_filters_already_tracked_session_ids() {
        let root = std::env::temp_dir().join(format!(
            "buildmesh_agy_discovery_tracked_{}",
            std::process::id()
        ));
        write_agy_conv(
            &root,
            "conv-tracked",
            r#"{"step_type":"USER_INPUT","text":"Tracked session","workspace_path":"/Users/adam/src/buildmesh","created_at":"2026-07-10T09:00:00Z"}
"#,
        );
        write_agy_conv(
            &root,
            "conv-orphan",
            r#"{"step_type":"USER_INPUT","text":"Orphan session","workspace_path":"/Users/adam/src/buildmesh","created_at":"2026-07-12T11:30:00Z"}
"#,
        );
        // Mark the first conversation as already tracked by an existing
        // Buildmesh node (so it's expected to be filtered out).
        let tracked: std::collections::HashSet<String> =
            ["conv-tracked".to_string()].into_iter().collect();

        let discovered = discover_agy_sessions_in(
            &root,
            "/Users/adam/src/buildmesh",
            &tracked,
        );
        std::fs::remove_dir_all(&root).ok();

        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].session_id, "conv-orphan");
        assert_eq!(discovered[0].first_message, "Orphan session");
    }

    #[test]
    fn agy_discovery_drops_conversation_without_transcript() {
        // A conversation directory with no `.system_generated/logs/transcript.jsonl`
        // is treated as absent — same as a missing JSONL in the Claude scanner.
        // Probably a half-written session mid-flight; not safe to surface.
        let root = std::env::temp_dir().join(format!(
            "buildmesh_agy_discovery_no_transcript_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("conv-empty/.system_generated/logs")).unwrap();
        // No transcript.jsonl written.

        let discovered = discover_agy_sessions_in(
            &root,
            "/Users/adam/src/buildmesh",
            &std::collections::HashSet::new(),
        );
        std::fs::remove_dir_all(&root).ok();
        assert!(discovered.is_empty());
    }

    #[test]
    fn agy_discovery_carries_timestamps_for_outer_sort() {
        // The scanner itself doesn't sort — `discover()` applies a single
        // unified sort across Claude/Cursor/AGY results. This test only
        // pins that the AGY scanner populates the timestamp field for every
        // discovered session so the outer sort has something to order by.
        let root = std::env::temp_dir().join(format!(
            "buildmesh_agy_discovery_sort_{}",
            std::process::id()
        ));
        write_agy_conv(
            &root,
            "conv-old",
            r#"{"step_type":"USER_INPUT","text":"Old","workspace_path":"/Users/adam/src/buildmesh","created_at":"2026-01-01T00:00:00Z"}
"#,
        );
        write_agy_conv(
            &root,
            "conv-new",
            r#"{"step_type":"USER_INPUT","text":"New","workspace_path":"/Users/adam/src/buildmesh","created_at":"2026-08-01T00:00:00Z"}
"#,
        );
        write_agy_conv(
            &root,
            "conv-mid",
            r#"{"step_type":"USER_INPUT","text":"Mid","workspace_path":"/Users/adam/src/buildmesh","created_at":"2026-05-01T00:00:00Z"}
"#,
        );

        let discovered = discover_agy_sessions_in(
            &root,
            "/Users/adam/src/buildmesh",
            &std::collections::HashSet::new(),
        );
        std::fs::remove_dir_all(&root).ok();

        assert_eq!(discovered.len(), 3);
        // Every session must carry a timestamp so the outer `discover()`
        // sort has something to order by. We don't assert ordering here —
        // read_dir order is filesystem-dependent and the per-scanner
        // contract is just "populate the field"; the sort lives one level
        // up.
        assert!(
            discovered.iter().all(|s| s.timestamp.is_some()),
            "every session must carry a timestamp for the outer discover() sort"
        );
        // Spot-check that each row preserved the source timestamp verbatim.
        for session in &discovered {
            let id = &session.session_id;
            let ts = session.timestamp.as_deref().unwrap();
            match id.as_str() {
                "conv-old" => assert_eq!(ts, "2026-01-01T00:00:00Z"),
                "conv-new" => assert_eq!(ts, "2026-08-01T00:00:00Z"),
                "conv-mid" => assert_eq!(ts, "2026-05-01T00:00:00Z"),
                _ => panic!("unexpected session id {id}"),
            }
        }
    }

    #[test]
    fn agy_discovery_sanitizes_first_prompt_via_strip_tags_and_truncates() {
        let root = std::env::temp_dir().join(format!(
            "buildmesh_agy_discovery_sanitize_{}",
            std::process::id()
        ));
        // First USER_INPUT carries a tag-wrapped prompt and a long body so
        // strip_tags + truncate both get exercised (mirrors the Claude scanner's
        // shape — issue #335).
        write_agy_conv(
            &root,
            "conv-tags",
            &format!(
                "{{\"step_type\":\"USER_INPUT\",\"text\":\"<system>{}</system>\",\"workspace_path\":\"/Users/adam/src/buildmesh\",\"created_at\":\"2026-07-10T09:00:00Z\"}}\n",
                "a".repeat(200)
            ),
        );

        let discovered = discover_agy_sessions_in(
            &root,
            "/Users/adam/src/buildmesh",
            &std::collections::HashSet::new(),
        );
        std::fs::remove_dir_all(&root).ok();

        assert_eq!(discovered.len(), 1);
        let first_message = &discovered[0].first_message;
        assert!(
            !first_message.contains('<'),
            "tags must be stripped, got: {first_message:?}"
        );
        // `truncate(s, 80)` returns `&s[..80] + "…"` — 80 chars of content
        // plus the ellipsis. The 81-char ceiling is the canonical contract
        // shared with the Claude/Cursor scanners (see `truncate_respects_boundary`
        // in transcript_reader for the matching shape).
        assert!(
            first_message.chars().count() <= 81,
            "first message must be truncated to 80 chars + ellipsis, got: {first_message:?} ({} chars)",
            first_message.chars().count()
        );
        assert!(
            first_message.ends_with('…'),
            "truncated first message must end with the ellipsis, got: {first_message:?}"
        );
    }

    #[test]
    fn agy_discovery_worktree_name_only_when_inside_this_mesh() {
        let root = std::env::temp_dir().join(format!(
            "buildmesh_agy_discovery_foreign_{}",
            std::process::id()
        ));
        // Workspace is under a *different* mesh's `.claude/worktrees/` — must
        // NOT be associated with our mesh's worktree (would mislead the user
        // into thinking a foreign conversation was a Buildmesh worktree).
        write_agy_conv(
            &root,
            "conv-foreign",
            r#"{"step_type":"USER_INPUT","text":"Foreign worktree","workspace_path":"/Users/adam/src/other-repo/.claude/worktrees/brave-fox","created_at":"2026-07-10T09:00:00Z"}
"#,
        );
        write_agy_conv(
            &root,
            "conv-mine",
            r#"{"step_type":"USER_INPUT","text":"My worktree","workspace_path":"/Users/adam/src/buildmesh/.claude/worktrees/gentle-fox","created_at":"2026-07-12T11:30:00Z"}
"#,
        );

        let discovered = discover_agy_sessions_in(
            &root,
            "/Users/adam/src/buildmesh",
            &std::collections::HashSet::new(),
        );
        std::fs::remove_dir_all(&root).ok();

        let foreign = discovered
            .iter()
            .find(|s| s.session_id == "conv-foreign")
            .expect("foreign conversation present");
        assert_eq!(
            foreign.worktree_name, None,
            "foreign-mesh worktree must not attach to ours"
        );
        let mine = discovered
            .iter()
            .find(|s| s.session_id == "conv-mine")
            .expect("our-mesh conversation present");
        assert_eq!(mine.worktree_name.as_deref(), Some("gentle-fox"));
    }

    #[test]
    fn agy_discovery_handles_windows_backslash_workspace_paths() {
        // AGY on Windows may store workspace_path with backslashes; the
        // worktree-association rule must normalize them to forward slashes
        // before matching against `mesh_path` (Windows often mixes both).
        let root = std::env::temp_dir().join(format!(
            "buildmesh_agy_discovery_winpath_{}",
            std::process::id()
        ));
        write_agy_conv(
            &root,
            "conv-win",
            r#"{"step_type":"USER_INPUT","text":"Windows worktree","workspace_path":"C:\\Users\\adam\\src\\buildmesh\\.claude\\worktrees\\bold-fox","created_at":"2026-07-10T09:00:00Z"}
"#,
        );

        let discovered = discover_agy_sessions_in(
            &root,
            "C:\\Users\\adam\\src\\buildmesh",
            &std::collections::HashSet::new(),
        );
        std::fs::remove_dir_all(&root).ok();

        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].worktree_name.as_deref(), Some("bold-fox"));
    }

    #[test]
    fn agy_discovery_accepts_alternative_workspace_field_names() {
        // AGY may evolve the field name (`working_directory`, `cwd`, etc.);
        // the scanner must keep discovering sessions across the rename.
        let root = std::env::temp_dir().join(format!(
            "buildmesh_agy_discovery_altkeys_{}",
            std::process::id()
        ));
        write_agy_conv(
            &root,
            "conv-cwd",
            r#"{"step_type":"USER_INPUT","text":"cwd field","cwd":"/Users/adam/src/buildmesh/.claude/worktrees/red-bear","created_at":"2026-07-10T09:00:00Z"}
"#,
        );
        write_agy_conv(
            &root,
            "conv-meta",
            r#"{"step_type":"USER_INPUT","text":"metadata.workspace_path","metadata":{"workspace_path":"/Users/adam/src/buildmesh/.claude/worktrees/blue-owl"},"created_at":"2026-07-12T11:30:00Z"}
"#,
        );

        let discovered = discover_agy_sessions_in(
            &root,
            "/Users/adam/src/buildmesh",
            &std::collections::HashSet::new(),
        );
        std::fs::remove_dir_all(&root).ok();

        let cwd_row = discovered
            .iter()
            .find(|s| s.session_id == "conv-cwd")
            .expect("`cwd` field should be discovered");
        assert_eq!(cwd_row.worktree_name.as_deref(), Some("red-bear"));
        let meta_row = discovered
            .iter()
            .find(|s| s.session_id == "conv-meta")
            .expect("`metadata.workspace_path` field should be discovered");
        assert_eq!(meta_row.worktree_name.as_deref(), Some("blue-owl"));
    }

    #[test]
    fn agy_discovery_falls_back_to_mtime_when_no_step_timestamp() {
        // An AGY transcript that omits created_at/timestamp on every step
        // must still surface a sortable timestamp (the file mtime), so the
        // row lands in the Archive list instead of being dropped silently.
        let root = std::env::temp_dir().join(format!(
            "buildmesh_agy_discovery_mtime_{}",
            std::process::id()
        ));
        write_agy_conv(
            &root,
            "conv-no-ts",
            r#"{"step_type":"USER_INPUT","text":"No timestamps here","workspace_path":"/Users/adam/src/buildmesh"}
"#,
        );

        let discovered = discover_agy_sessions_in(
            &root,
            "/Users/adam/src/buildmesh",
            &std::collections::HashSet::new(),
        );
        std::fs::remove_dir_all(&root).ok();

        assert_eq!(discovered.len(), 1);
        assert!(
            discovered[0].timestamp.is_some(),
            "must fall back to file mtime when no step carries a timestamp"
        );
    }
}
