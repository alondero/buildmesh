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

use chrono::{Datelike, Days, NaiveDate};
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
    id: Option<String>,
    timestamp: Option<String>,
    cwd: Option<String>,
    #[serde(default)]
    thread_source: Option<String>,
    source: Option<serde_json::Value>,
}

#[derive(Debug)]
pub(crate) struct Candidate {
    pub id: String,
    pub timestamp_ms: i64,
}

enum CaptureAttempt {
    AlreadyStored,
    NotFound,
    Stored(String),
}

/// Find the newest valid Codex rollout created for this fresh spawn.
///
/// This is intentionally limited to the fixed `YYYY/MM/DD` rollout layout;
/// it does not recursively walk an arbitrary user-controlled directory.
#[cfg(test)]
pub fn find_id_for_directory_in(
    sessions_dir: &Path,
    spawn_directory: &str,
    created_not_before_ms: i64,
) -> Option<String> {
    find_candidates(sessions_dir, spawn_directory, created_not_before_ms, None)
        .into_iter()
        .max_by_key(|candidate| candidate.timestamp_ms)
        .map(|candidate| candidate.id)
}

fn find_fresh_id_for_directory_in(
    sessions_dir: &Path,
    spawn_directory: &str,
    created_not_before_ms: i64,
) -> Option<String> {
    find_candidates(
        sessions_dir,
        spawn_directory,
        created_not_before_ms,
        Some(created_not_before_ms),
    )
    .into_iter()
    .max_by_key(|candidate| candidate.timestamp_ms)
    .map(|candidate| candidate.id)
}

pub(crate) fn find_candidates(
    sessions_dir: &Path,
    spawn_directory: &str,
    created_not_before_ms: i64,
    written_not_before_ms: Option<i64>,
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for day in rollout_days_newest_first(sessions_dir, created_not_before_ms) {
        let Ok(entries) = fs::read_dir(day) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            if written_not_before_ms.is_some_and(|not_before| !was_written_since(&path, not_before)) {
                continue;
            }
            if let Some(candidate) = read_session_meta(&path, spawn_directory) {
                if candidate.timestamp_ms >= created_not_before_ms {
                    candidates.push(candidate);
                }
            }
        }
    }
    candidates
}

/// Historic startup recovery entry point used by the Codex adapter. The
/// adapter owns the Codex rollout layout; the shared recovery service owns
/// only the time-window and ambiguity policy.
pub(crate) fn find_historic_id_for_directory(
    env_type: EnvType,
    spawn_directory: &str,
    anchor_ms: i64,
    recorded_start: bool,
) -> Option<String> {
    let sessions_dir = crate::env::codex_sessions_dir(env_type, spawn_directory)?;
    find_historic_id_for_directory_in(
        &sessions_dir,
        spawn_directory,
        anchor_ms,
        recorded_start,
    )
}

pub(crate) fn find_historic_id_for_directory_in(
    sessions_dir: &Path,
    spawn_directory: &str,
    anchor_ms: i64,
    recorded_start: bool,
) -> Option<String> {
    let cutoff = anchor_ms.saturating_sub(crate::services::session_recovery::CLOCK_SKEW_MS);
    let candidates = find_candidates(&sessions_dir, spawn_directory, cutoff, None);
    crate::services::session_recovery::select_recovery_identity(
        candidates.into_iter().map(|candidate| (candidate.id, candidate.timestamp_ms)),
        anchor_ms,
        recorded_start,
    )
}

fn was_written_since(path: &Path, not_before_ms: i64) -> bool {
    path.metadata()
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .is_some_and(|time| time.as_millis() >= not_before_ms as u128)
}

fn rollout_days_newest_first(sessions_dir: &Path, created_not_before_ms: i64) -> Vec<PathBuf> {
    let cutoff = chrono::DateTime::from_timestamp_millis(created_not_before_ms)
        .and_then(|time| time.date_naive().checked_sub_days(Days::new(1)))
        .unwrap_or(NaiveDate::MIN);
    let mut days = Vec::new();
    for year in subdirs_sorted_desc(sessions_dir) {
        let Some(year_number) = directory_number(&year) else { continue; };
        if year_number < cutoff.year() { break; }
        for month in subdirs_sorted_desc(&year) {
            let Some(month_number) = directory_number(&month) else { continue; };
            if year_number == cutoff.year() && month_number < cutoff.month() as i32 { break; }
            for day in subdirs_sorted_desc(&month) {
                let Some(day_number) = directory_number(&day) else { continue; };
                let Some(date) = NaiveDate::from_ymd_opt(year_number, month_number as u32, day_number as u32) else { continue; };
                if date < cutoff { break; }
                days.push(day);
            }
        }
    }
    days
}

fn subdirs_sorted_desc(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new(); };
    let mut dirs = entries.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()).collect::<Vec<_>>();
    dirs.sort_by(|left, right| right.file_name().cmp(&left.file_name()));
    dirs
}

fn directory_number(path: &Path) -> Option<i32> {
    path.file_name()?.to_str()?.parse().ok()
}

fn read_session_meta(path: &Path, spawn_directory: &str) -> Option<Candidate> {
    let mut reader = BufReader::new(fs::File::open(path).ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let record = serde_json::from_str::<RolloutRecord>(&line).ok()?;
    if record.kind != "session_meta" {
        return None;
    }
    if record
        .payload
        .thread_source
        .as_deref()
        .is_some_and(|source| source != "user")
    {
        return None;
    }
    if record.payload.source.as_ref().is_some_and(|source| source.get("subagent").is_some()) {
        return None;
    }
    let id = uuid::Uuid::parse_str(record.payload.session_id.as_deref().or(record.payload.id.as_deref())?)
        .ok()?
        .to_string();
    let cwd = record.payload.cwd?;
    if !crate::env::directories_match(&cwd, spawn_directory) {
        return None;
    }
    let timestamp_ms = chrono::DateTime::parse_from_rfc3339(record.payload.timestamp.as_deref()?)
        .ok()?
        .timestamp_millis();
    Some(Candidate { id, timestamp_ms })
}

/// Poll briefly for Codex's rollout metadata, then fill an otherwise empty
/// `cli_session_id`. The PTY and hook paths may win first; the DB predicate
/// keeps this delayed fallback from overwriting them.
pub fn start_capture_poller(node_id: i64, spawn_directory: String, env_type: EnvType) {
    let spawn_epoch_ms = chrono::Utc::now().timestamp_millis();
    tauri::async_runtime::spawn(async move {
        let not_before = spawn_epoch_ms.saturating_sub(CAPTURE_SKEW_MS);
        for (attempt, delay) in RETRY_DELAYS_MS.iter().enumerate() {
            tokio::time::sleep(Duration::from_millis(*delay)).await;
            if !crate::agent::process::PROCESS_REGISTRY.contains(&node_id) {
                return;
            }
            let Some(sessions_dir) = crate::env::codex_sessions_dir(env_type, &spawn_directory) else {
                tracing::warn!("codex session capture: no sessions directory for env {env_type:?}");
                return;
            };
            let path = sessions_dir.clone();
            let directory = spawn_directory.clone();
            // Keep the DB predicate, disk scan, and conditional write in one
            // blocking task. Splitting these into three dispatches on every
            // retry needlessly thrashes the blocking pool and widens the race
            // window between finding a rollout and claiming the row.
            let captured = crate::blocking::run_blocking("codex_capture", move || {
                if node_has_cli_session_id(node_id) {
                    return Ok(CaptureAttempt::AlreadyStored);
                }
                let Some(id) = find_fresh_id_for_directory_in(&path, &directory, not_before) else {
                    return Ok(CaptureAttempt::NotFound);
                };
                match crate::db::set_cli_session_id_if_missing(node_id, &id) {
                    Ok(true) => Ok(CaptureAttempt::Stored(id)),
                    Ok(false) => Ok(CaptureAttempt::AlreadyStored),
                    Err(error) => Err(error.to_string()),
                }
            })
            .await;
            let capture = match captured {
                Ok(attempt) => attempt,
                Err(error) => {
                    tracing::warn!("codex session capture: blocking task failed for node {node_id}: {error}");
                    return;
                }
            };
            let id = match capture {
                CaptureAttempt::AlreadyStored => return,
                CaptureAttempt::NotFound => continue,
                CaptureAttempt::Stored(id) => id,
            };
            if !crate::agent::process::PROCESS_REGISTRY.contains(&node_id) {
                return;
            }
            tracing::info!(
                "codex session capture: stored {id} for node {node_id} (attempt {})",
                attempt + 1
            );
            return;
        }
        tracing::warn!("codex session capture: gave up for node {node_id} in {spawn_directory}");
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
    fn traversal_is_descending_and_prunes_old_rollout_days() {
        let temp = tempfile::TempDir::new().unwrap();
        for date in [("2024", "01", "01"), ("2026", "08", "26"), ("2026", "08", "27")] {
            fs::create_dir_all(temp.path().join(date.0).join(date.1).join(date.2)).unwrap();
        }
        let names = rollout_days_newest_first(temp.path(), 1_787_830_399_000)
            .iter()
            .map(|day| day.file_name().unwrap().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(names, ["27", "26"]);
    }

    #[test]
    fn shared_directory_matcher_accepts_windows_and_wsl_spawn_forms() {
        assert!(crate::env::directories_match(
            r"F:\src\buildmesh\.claude\worktrees\node",
            "f:/src/buildmesh/.claude/worktrees/node/"
        ));
        assert!(crate::env::directories_match(
            "/home/alond/src/buildmesh/.claude/worktrees/node",
            "/home/alond/src/buildmesh/.claude/worktrees/node/"
        ));
    }

    fn write_rollout_with_source(
        root: &Path,
        name: &str,
        id: &str,
        cwd: &str,
        timestamp: &str,
        thread_source: &str,
    ) {
        let day = root.join("2026").join("08").join("27");
        fs::create_dir_all(&day).unwrap();
        let record = serde_json::json!({
            "type": "session_meta",
            "payload": {
                "session_id": id,
                "cwd": cwd,
                "timestamp": timestamp,
                "thread_source": thread_source
            }
        });
        fs::write(day.join(name), record.to_string()).unwrap();
    }

    #[test]
    fn fresh_capture_ignores_subagent_rollouts_in_the_same_worktree() {
        let temp = tempfile::TempDir::new().unwrap();
        let directory = "F:/src/buildmesh/.claude/worktrees/rumpled-covert-typhoon";
        let parent = "01a042fe-e7e2-79a2-96bd-a15140478a58";
        write_rollout_with_source(
            temp.path(),
            "rollout-parent.jsonl",
            parent,
            directory,
            "2026-08-27T11:33:19.986Z",
            "user",
        );
        write_rollout_with_source(
            temp.path(),
            "rollout-subagent.jsonl",
            "01a04330-f9a2-7c62-9474-347cd72a5e20",
            directory,
            "2026-08-27T11:34:19.986Z",
            "subagent",
        );

        assert_eq!(
            find_fresh_id_for_directory_in(temp.path(), directory, 1_787_830_399_000),
            Some(parent.to_string())
        );
    }

    #[test]
    fn historic_recovery_reads_metadata_without_banner_and_excludes_child_threads() {
        const ID: &str = "01a067ef-81ab-7141-a6b4-208e32df59bf";
        const CREATED: i64 = 1_787_830_399_000;
        const TIMESTAMP: &str = "2026-08-27T11:33:19.986Z";
        let temp = tempfile::TempDir::new().unwrap();
        let day = temp.path().join("2026/08/27");
        fs::create_dir_all(&day).unwrap();
        for (file, payload) in [
            (
                "root.jsonl",
                serde_json::json!({
                    "id": ID,
                    "cwd": "F:\\repo",
                    "timestamp": TIMESTAMP,
                    "thread_source": "user"
                }),
            ),
            (
                "child.jsonl",
                serde_json::json!({
                    "id": "01a067ec-e952-7830-b3a6-bc16bc79f327",
                    "cwd": "F:\\repo",
                    "timestamp": TIMESTAMP,
                    "source": {"subagent": {}}
                }),
            ),
        ] {
            fs::write(
                day.join(file),
                serde_json::json!({"type": "session_meta", "payload": payload}).to_string(),
            )
            .unwrap();
        }

        assert_eq!(
            find_historic_id_for_directory_in(temp.path(), "f:/repo", CREATED, true),
            Some(ID.to_string())
        );
        assert_eq!(
            find_historic_id_for_directory_in(temp.path(), "f:/other", CREATED, true),
            None
        );
        assert_eq!(
            find_historic_id_for_directory_in(temp.path(), "f:/repo", CREATED + 10_000, true),
            None
        );
    }
}
