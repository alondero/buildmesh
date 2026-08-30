//! Command Code session-ID capture from local session transcripts.
//!
//! Command Code self-assigns session IDs of the form `sess_<alphanumeric>`
//! (e.g. `sess_01j6...`) and writes structured transcript records to
//! `%USERPROFILE%/.commandcode/sessions/<session_id>.jsonl` (Windows) or
//! `~/.commandcode/sessions/<session_id>.jsonl` (macOS/Linux/WSL).
//!
//! Because Command Code prints rich TUI output rather than standard UUID
//! banners, PTY UUID capture cannot extract `sess_…` IDs. A bounded
//! post-spawn poller inspects `~/.commandcode/sessions/` and reads the session
//! header record to associate the node with its exact session.
//!
//! Matching both the working directory and the fresh spawn time ensures an
//! older conversation is never resumed by mistake.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::models::EnvType;

pub const CAPTURE_SKEW_MS: i64 = 2_000;
const RETRY_DELAYS_MS: &[u64] = &[400, 800, 1_600, 2_500, 4_000, 6_000];

/// Session header JSON record at line 1 of `<session_id>.jsonl`.
#[derive(Debug, Default, Deserialize)]
struct SessionHeader {
    #[serde(alias = "id", alias = "sessionId")]
    session_id: Option<String>,
    #[serde(alias = "directory", alias = "project_path", alias = "workspace")]
    cwd: Option<String>,
    #[serde(alias = "created_at", alias = "time_created", alias = "createdAt")]
    timestamp: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub directory: String,
    pub timestamp_ms: i64,
}

/// Command Code session IDs start with `sess_`.
pub fn is_commandcode_session_id(id: &str) -> bool {
    id.starts_with("sess_") && id.len() > 5
}

/// Parse timestamp from either ISO 8601 string, integer epoch ms, or integer epoch seconds.
fn parse_timestamp_ms(val: &serde_json::Value) -> Option<i64> {
    match val {
        serde_json::Value::String(s) => chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.timestamp_millis())
            .or_else(|| s.parse::<i64>().ok()),
        serde_json::Value::Number(num) => {
            if let Some(i) = num.as_i64() {
                // If smaller than 10^11, likely seconds -> multiply by 1000
                if i < 100_000_000_000 {
                    Some(i * 1000)
                } else {
                    Some(i)
                }
            } else if let Some(f) = num.as_f64() {
                if f < 100_000_000_000.0 {
                    Some((f * 1000.0) as i64)
                } else {
                    Some(f as i64)
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Read candidate metadata from a `.jsonl` session file.
pub fn read_session_file(path: &Path) -> Option<Candidate> {
    let file = fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let mut first_line = String::new();
    reader.read_line(&mut first_line).ok()?;
    if first_line.trim().is_empty() {
        return None;
    }

    let header: SessionHeader = serde_json::from_str(&first_line).ok()?;
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let id = header
        .session_id
        .filter(|s| is_commandcode_session_id(s))
        .or_else(|| {
            if is_commandcode_session_id(stem) {
                Some(stem.to_string())
            } else {
                None
            }
        })?;

    let directory = header.cwd.unwrap_or_default();
    let timestamp_ms = header
        .timestamp
        .as_ref()
        .and_then(parse_timestamp_ms)
        .or_else(|| {
            path.metadata()
                .ok()?
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_millis() as i64)
        })
        .unwrap_or(0);

    Some(Candidate {
        id,
        directory,
        timestamp_ms,
    })
}

/// Pick the newest session candidate matching the spawn directory created at or after `created_not_before_ms`.
pub fn select_id_for_directory<'a>(
    candidates: &'a [Candidate],
    spawn_directory: &str,
    created_not_before_ms: i64,
) -> Option<&'a str> {
    candidates
        .iter()
        .filter(|c| is_commandcode_session_id(&c.id))
        .filter(|c| crate::env::directories_match(&c.directory, spawn_directory))
        .filter(|c| c.timestamp_ms >= created_not_before_ms)
        .max_by_key(|c| c.timestamp_ms)
        .map(|c| c.id.as_str())
}

/// Scan `sessions_dir` for all `.jsonl` files and find the newest valid candidate.
pub fn find_fresh_id_for_directory_in(
    sessions_dir: &Path,
    spawn_directory: &str,
    created_not_before_ms: i64,
) -> Option<String> {
    if !sessions_dir.exists() {
        return None;
    }
    let entries = fs::read_dir(sessions_dir).ok()?;
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        if let Some(candidate) = read_session_file(&path) {
            candidates.push(candidate);
        }
    }
    select_id_for_directory(&candidates, spawn_directory, created_not_before_ms).map(str::to_string)
}

/// Background poller: read Command Code's session directory until a fresh session
/// created in this node's spawn window appears, then save `cli_session_id`.
pub fn start_capture_poller(node_id: i64, spawn_directory: String, env_type: EnvType) {
    let spawn_epoch_ms = chrono::Utc::now().timestamp_millis();
    tauri::async_runtime::spawn(async move {
        let not_before = spawn_epoch_ms.saturating_sub(CAPTURE_SKEW_MS);
        let Some(sessions_dir) = crate::env::commandcode_sessions_dir(env_type, &spawn_directory) else {
            tracing::warn!("commandcode session capture: no sessions dir for env {env_type:?}");
            return;
        };
        for (attempt, delay) in RETRY_DELAYS_MS.iter().enumerate() {
            tokio::time::sleep(Duration::from_millis(*delay)).await;
            if !crate::agent::process::PROCESS_REGISTRY.contains(&node_id) {
                tracing::debug!("commandcode session capture: node {node_id} gone, stop");
                return;
            }
            if node_has_cli_session_id(node_id) {
                return;
            }
            let path = sessions_dir.clone();
            let dir = spawn_directory.clone();
            let captured = tauri::async_runtime::spawn_blocking(move || {
                find_fresh_id_for_directory_in(&path, &dir, not_before)
            })
            .await
            .ok()
            .flatten();

            if let Some(id) = captured {
                if !crate::agent::process::PROCESS_REGISTRY.contains(&node_id) {
                    return;
                }
                match crate::db::set_cli_session_id_if_missing(node_id, &id) {
                    Ok(true) => tracing::info!(
                        "commandcode session capture: stored {id} for node {node_id} (attempt {})",
                        attempt + 1
                    ),
                    Ok(false) => {}
                    Err(e) => tracing::warn!(
                        "commandcode session capture: db write failed for node {node_id}: {e}"
                    ),
                }
                return;
            }
        }
        tracing::warn!("commandcode session capture: gave up for node {node_id} in {spawn_directory}");
    });
}

fn node_has_cli_session_id(node_id: i64) -> bool {
    crate::db::get_agent_node_by_id(node_id)
        .ok()
        .and_then(|node| node.cli_session_id)
        .is_some_and(|id| !id.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: &str, dir: &str, timestamp_ms: i64) -> Candidate {
        Candidate {
            id: id.into(),
            directory: dir.into(),
            timestamp_ms,
        }
    }

    #[test]
    fn validates_commandcode_session_id_format() {
        assert!(is_commandcode_session_id("sess_01j6xyz890"));
        assert!(is_commandcode_session_id("sess_abc123"));
        assert!(!is_commandcode_session_id("sess_"));
        assert!(!is_commandcode_session_id("01a024d2-7cd6-7ea2-b907-531b0d261be7"));
        assert!(!is_commandcode_session_id("ses_opencode123"));
    }

    #[test]
    fn select_matches_windows_directory_slash_and_case() {
        let candidates = vec![cand(
            "sess_01j6worktree001",
            r"F:\src\buildmesh\.claude\worktrees\commandcode-test",
            200,
        )];
        let id = select_id_for_directory(
            &candidates,
            "f:/src/buildmesh/.claude/worktrees/commandcode-test",
            100,
        );
        assert_eq!(id, Some("sess_01j6worktree001"));
    }

    #[test]
    fn select_prefers_newest_in_window() {
        let candidates = vec![
            cand("sess_01j6older0001", "/tmp/wt", 150),
            cand("sess_01j6newer0002", "/tmp/wt", 250),
        ];
        let id = select_id_for_directory(&candidates, "/tmp/wt", 100);
        assert_eq!(id, Some("sess_01j6newer0002"));
    }

    #[test]
    fn select_rejects_sessions_before_spawn_window() {
        let candidates = vec![cand("sess_01j6old000001", "/tmp/wt", 50)];
        let id = select_id_for_directory(&candidates, "/tmp/wt", 100);
        assert_eq!(id, None);
    }

    #[test]
    fn reads_session_file_with_json_metadata() {
        let temp = tempfile::TempDir::new().unwrap();
        let file_path = temp.path().join("sess_01j6alpha.jsonl");
        let content = serde_json::json!({
            "session_id": "sess_01j6alpha",
            "cwd": "F:\\src\\my-repo",
            "timestamp": "2026-08-30T14:30:00Z"
        });
        fs::write(&file_path, format!("{}\n{{\"type\":\"user_input\"}}", content)).unwrap();

        let c = read_session_file(&file_path).expect("should parse candidate");
        assert_eq!(c.id, "sess_01j6alpha");
        assert_eq!(c.directory, "F:\\src\\my-repo");
        assert!(c.timestamp_ms > 0);
    }

    #[test]
    fn finds_fresh_session_in_directory() {
        let temp = tempfile::TempDir::new().unwrap();
        let file_path = temp.path().join("sess_01j6target.jsonl");
        let content = serde_json::json!({
            "session_id": "sess_01j6target",
            "cwd": "/home/user/src/project",
            "timestamp": 1_787_830_500_000i64
        });
        fs::write(&file_path, content.to_string()).unwrap();

        let found = find_fresh_id_for_directory_in(
            temp.path(),
            "/home/user/src/project",
            1_787_830_400_000,
        );
        assert_eq!(found, Some("sess_01j6target".to_string()));
    }
}
