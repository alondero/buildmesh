//! Codex session-ID capture from rollout metadata.
//!
//! Codex self-assigns a UUID, historically printed it as `session id:` on the
//! interactive PTY, and always writes it to the first `session_meta` rollout
//! record. Recent TUIs can omit the former, so a bounded post-spawn poller
//! reads the latter. Matching both the Node Working Directory and the fresh
//! spawn time prevents an older conversation being resumed by mistake.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::models::EnvType;

pub const CAPTURE_SKEW_MS: i64 = 2_000;
const RETRY_DELAYS_MS: &[u64] = &[500, 1_000, 2_000, 4_000, 6_000];

#[derive(Debug, Deserialize)]
struct RolloutRecord {
    #[serde(rename = "type")]
    kind: String,
    payload: RolloutMetadata,
}

#[derive(Debug, Default, Deserialize)]
struct RolloutMetadata {
    session_id: Option<String>,
    timestamp: Option<String>,
    cwd: Option<String>,
}

#[derive(Debug)]
struct Candidate {
    id: String,
    timestamp_ms: i64,
}

/// Find the newest valid Codex rollout created for this fresh spawn.
///
/// This is intentionally limited to the fixed `YYYY/MM/DD` rollout layout;
/// it does not recursively walk an arbitrary user-controlled directory.
pub fn find_id_for_directory_in(
    sessions_dir: &Path,
    spawn_directory: &str,
    created_not_before_ms: i64,
) -> Option<String> {
    find_candidates(sessions_dir, spawn_directory, created_not_before_ms)
        .into_iter()
        .max_by_key(|candidate| candidate.timestamp_ms)
        .map(|candidate| candidate.id)
}

/// Find an unambiguous historic rollout for a suspended node. The caller only
/// uses this to repair rows created before live capture existed; differing
/// matching conversation IDs are intentionally left suspended for the user.
pub fn find_unique_id_for_directory_in(
    sessions_dir: &Path,
    spawn_directory: &str,
    created_not_before_ms: i64,
) -> Option<String> {
    let mut ids = find_candidates(sessions_dir, spawn_directory, created_not_before_ms)
        .into_iter()
        .map(|candidate| candidate.id)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    (ids.len() == 1).then(|| ids.pop().unwrap())
}

fn find_candidates(
    sessions_dir: &Path,
    spawn_directory: &str,
    created_not_before_ms: i64,
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for year in subdirs(sessions_dir) {
        for month in subdirs(&year) {
            for day in subdirs(&month) {
                let Ok(entries) = fs::read_dir(day) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                        continue;
                    }
                    if let Some(candidate) = read_session_meta(&path, spawn_directory) {
                        if candidate.timestamp_ms >= created_not_before_ms {
                            candidates.push(candidate);
                        }
                    }
                }
            }
        }
    }
    candidates
}

fn subdirs(path: &Path) -> Vec<PathBuf> {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect()
}

fn read_session_meta(path: &Path, spawn_directory: &str) -> Option<Candidate> {
    let mut reader = BufReader::new(fs::File::open(path).ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let record = serde_json::from_str::<RolloutRecord>(&line).ok()?;
    if record.kind != "session_meta" {
        return None;
    }
    let id = uuid::Uuid::parse_str(record.payload.session_id.as_deref()?)
        .ok()?
        .to_string();
    let cwd = record.payload.cwd?;
    if !directories_match(&cwd, spawn_directory) {
        return None;
    }
    let timestamp_ms = chrono::DateTime::parse_from_rfc3339(record.payload.timestamp.as_deref()?)
        .ok()?
        .timestamp_millis();
    Some(Candidate { id, timestamp_ms })
}

fn directories_match(recorded: &str, spawn: &str) -> bool {
    let recorded = normalize_directory(recorded);
    let spawn = normalize_directory(spawn);
    if looks_windows_volume(&recorded) || looks_windows_volume(&spawn) {
        recorded.eq_ignore_ascii_case(&spawn)
    } else {
        recorded == spawn
    }
}

fn normalize_directory(path: &str) -> String {
    let mut normalized = path.replace('\\', "/");
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    normalized
}

fn looks_windows_volume(path: &str) -> bool {
    let bytes = path.as_bytes();
    (bytes.len() >= 2 && bytes[1] == b':') || path.starts_with("//") || path.starts_with("/mnt/")
}

fn sessions_dir(env_type: EnvType) -> Option<PathBuf> {
    match env_type {
        EnvType::Windows => Some(crate::env::codex_dir().join("sessions")),
        EnvType::Wsl => {
            let output = crate::process_util::command_no_window("wsl.exe")
                .args([
                    "--exec",
                    "sh",
                    "-c",
                    "printf %s \"${CODEX_HOME:-$HOME/.codex}\"",
                ])
                .output()
                .ok()?;
            if !output.status.success() {
                return None;
            }
            let home = String::from_utf8_lossy(&output.stdout).trim().to_string();
            (!home.is_empty())
                .then(|| PathBuf::from(crate::env::to_host_path(&home)).join("sessions"))
        }
    }
}

/// Poll briefly for Codex's rollout metadata, then fill an otherwise empty
/// `cli_session_id`. The PTY and hook paths may win first; the DB predicate
/// keeps this delayed fallback from overwriting them.
pub fn start_capture_poller(node_id: i64, spawn_directory: String, env_type: EnvType) {
    let spawn_epoch_ms = chrono::Utc::now().timestamp_millis();
    tauri::async_runtime::spawn(async move {
        let Some(sessions_dir) =
            tauri::async_runtime::spawn_blocking(move || sessions_dir(env_type))
                .await
                .ok()
                .flatten()
        else {
            tracing::warn!("codex session capture: no sessions directory for env {env_type:?}");
            return;
        };
        let not_before = spawn_epoch_ms.saturating_sub(CAPTURE_SKEW_MS);
        for (attempt, delay) in RETRY_DELAYS_MS.iter().enumerate() {
            tokio::time::sleep(Duration::from_millis(*delay)).await;
            if !crate::agent::process::PROCESS_REGISTRY.contains(&node_id) {
                return;
            }
            let path = sessions_dir.clone();
            let directory = spawn_directory.clone();
            let captured = tauri::async_runtime::spawn_blocking(move || {
                find_id_for_directory_in(&path, &directory, not_before)
            })
            .await
            .ok()
            .flatten();
            let Some(id) = captured else {
                continue;
            };
            if !crate::agent::process::PROCESS_REGISTRY.contains(&node_id) {
                return;
            }
            match crate::db::set_cli_session_id_if_missing(node_id, &id) {
                Ok(true) => tracing::info!(
                    "codex session capture: stored {id} for node {node_id} (attempt {})",
                    attempt + 1
                ),
                Ok(false) => {}
                Err(error) => tracing::warn!(
                    "codex session capture: db write failed for node {node_id}: {error}"
                ),
            }
            return;
        }
        tracing::warn!("codex session capture: gave up for node {node_id} in {spawn_directory}");
    });
}

/// Restore a missing pre-capture Codex identity for an existing suspended
/// node. This only accepts a single matching conversation ID, so ambiguity
/// remains visible and never causes a surprise resume.
pub async fn backfill_suspended_node(
    node_id: i64,
    node_directory: String,
    env_type: EnvType,
    created_at_ms: i64,
) -> bool {
    let Some(sessions_dir) = tauri::async_runtime::spawn_blocking(move || sessions_dir(env_type))
        .await
        .ok()
        .flatten()
    else {
        return false;
    };
    let not_before = created_at_ms.saturating_sub(CAPTURE_SKEW_MS);
    let id = tauri::async_runtime::spawn_blocking(move || {
        find_unique_id_for_directory_in(&sessions_dir, &node_directory, not_before)
    })
    .await
    .ok()
    .flatten();
    let Some(id) = id else {
        return false;
    };
    match crate::db::set_cli_session_id_if_missing(node_id, &id) {
        Ok(true) => {
            tracing::info!("codex session capture: backfilled {id} for suspended node {node_id}");
            true
        }
        Ok(false) => false,
        Err(error) => {
            tracing::warn!("codex session capture: backfill failed for node {node_id}: {error}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_rollout(root: &Path, name: &str, id: &str, cwd: &str, timestamp: &str) {
        let day = root.join("2026").join("08").join("27");
        fs::create_dir_all(&day).unwrap();
        let record = serde_json::json!({
            "type": "session_meta",
            "payload": { "session_id": id, "cwd": cwd, "timestamp": timestamp }
        });
        fs::write(day.join(name), record.to_string()).unwrap();
    }

    #[test]
    fn finds_new_rollout_for_matching_worktree_when_pty_has_no_session_banner() {
        let temp = tempfile::TempDir::new().unwrap();
        let wanted = "01a042fe-e7e2-79a2-96bd-a15140478a58";
        write_rollout(
            temp.path(),
            "rollout-other.jsonl",
            "01a042fd-1111-7000-8000-000000000001",
            "F:/src/buildmesh/.claude/worktrees/other",
            "2026-08-27T11:33:19.986Z",
        );
        write_rollout(
            temp.path(),
            "rollout-wanted.jsonl",
            wanted,
            "F:\\src\\buildmesh\\.claude\\worktrees\\rumpled-covert-typhoon",
            "2026-08-27T11:33:19.986Z",
        );

        assert_eq!(
            find_id_for_directory_in(
                temp.path(),
                "F:/src/buildmesh/.claude/worktrees/rumpled-covert-typhoon",
                1_787_830_399_000,
            ),
            Some(wanted.to_string())
        );
    }

    #[test]
    fn ignores_a_rollout_from_before_this_fresh_spawn() {
        let temp = tempfile::TempDir::new().unwrap();
        write_rollout(
            temp.path(),
            "rollout-old.jsonl",
            "01a042fe-e7e2-79a2-96bd-a15140478a58",
            "F:/src/buildmesh/.claude/worktrees/rumpled-covert-typhoon",
            "2026-08-27T11:33:17.010Z",
        );

        assert_eq!(
            find_id_for_directory_in(
                temp.path(),
                "F:/src/buildmesh/.claude/worktrees/rumpled-covert-typhoon",
                1_787_830_400_000,
            ),
            None
        );
    }

    #[test]
    fn historic_backfill_rejects_ambiguous_matching_conversations() {
        let temp = tempfile::TempDir::new().unwrap();
        let directory = "F:/src/buildmesh/.claude/worktrees/rumpled-covert-typhoon";
        write_rollout(
            temp.path(),
            "rollout-first.jsonl",
            "01a042fe-e7e2-79a2-96bd-a15140478a58",
            directory,
            "2026-08-27T11:33:17.010Z",
        );
        write_rollout(
            temp.path(),
            "rollout-second.jsonl",
            "01a04330-f9a2-7c62-9474-347cd72a5e20",
            directory,
            "2026-08-27T12:27:58.367Z",
        );

        assert_eq!(
            find_unique_id_for_directory_in(temp.path(), directory, 1_787_830_000_000),
            None
        );
    }
}
