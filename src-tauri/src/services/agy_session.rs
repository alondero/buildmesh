//! Antigravity session-ID capture from the brain directory.
//!
//! Antigravity (`agy`) self-assigns a UUIDv4 conversation ID and stores every
//! conversation globally under `~/.gemini/antigravity-cli/brain/
//! <conversation-id>/.system_generated/logs/transcript.jsonl` (issue #1283).
//! The interactive TUI does **not** print that UUID to stdout, so the PTY
//! UUID regex in `session_capture` can never match it (issue #1499). After a
//! fresh spawn we scan the brain directory for a conversation directory
//! created in this spawn's time window whose transcript workspace matches the
//! node's spawn path, then persist it with
//! `db::set_cli_session_id_if_missing` so `auto_resume_agent_nodes` and manual
//! "Resume" (`agy --conversation <uuid>`) keep working across app restarts.
//!
//! Matching both the Node Working Directory and the fresh spawn time prevents
//! an older conversation being resumed by mistake. The `Stop` attention hook
//! (`conversationId` extraction in `http/routes/attention.rs`) stays as a
//! resilient secondary capture path.
//!
//! Select/match helpers are pure so the rules are unit-tested without a live
//! `agy` binary. Filesystem scanning is tested against temp brain roots.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::models::EnvType;

pub const CAPTURE_SKEW_MS: i64 = 2_000;
const RETRY_DELAYS_MS: &[u64] = &[500, 1_000, 2_000, 4_000, 6_000];

/// One Antigravity conversation candidate found under the brain root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub directory: String,
    pub timestamp_ms: i64,
}

/// Antigravity conversation IDs are UUIDs (the same UUID the `Stop` hook
/// delivers as `conversationId` and `resume_args` feeds to `--conversation`).
/// Reject anything else so temp dirs or partial writes never bind a node.
pub fn is_agy_conversation_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok()
}

/// Resolve the transcript for a conversation dir: the token-efficient
/// `transcript.jsonl` first, falling back to `transcript_full.jsonl` when the
/// short variant is missing (issue #1283 shape, shared with
/// `services::transcript_reader::agy_locator_in`).
fn transcript_path_in(conv_dir: &Path) -> Option<PathBuf> {
    let logs = conv_dir.join(".system_generated").join("logs");
    let short = logs.join("transcript.jsonl");
    if short.is_file() {
        return Some(short);
    }
    let full = logs.join("transcript_full.jsonl");
    if full.is_file() {
        return Some(full);
    }
    None
}

/// Pull the workspace/cwd out of an AGY transcript step, trying the field
/// names the CLI has been observed to use. Mirrors
/// `services::agent_node_discovery::extract_agy_workspace_path` (same key
/// order, same `metadata` nesting) so the poller and the archive scanner
/// agree on "where the conversation started".
fn extract_workspace_from_step(val: &serde_json::Value) -> Option<String> {
    const KEYS: &[&str] = &[
        "workspace_path",
        "working_directory",
        "cwd",
        "workspace",
        "project_path",
    ];
    for key in KEYS {
        if let Some(s) = val.get(*key).and_then(|v| v.as_str()) {
            if !s.trim().is_empty() {
                return Some(s.to_string());
            }
        }
    }
    if let Some(meta) = val.get("metadata") {
        for key in KEYS {
            if let Some(s) = meta.get(*key).and_then(|v| v.as_str()) {
                if !s.trim().is_empty() {
                    return Some(s.to_string());
                }
            }
        }
    }
    None
}

/// Read the workspace path from the first transcript step that carries one.
/// Scans up to the first 50 non-empty lines — the workspace anchor is always
/// in the opening steps; bounding the read keeps a huge resumed transcript
/// from stalling the poller on every retry.
pub fn read_workspace_from_transcript(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut scanned = 0usize;
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        scanned += 1;
        if scanned > 50 {
            break;
        }
        let val: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(workspace) = extract_workspace_from_step(&val) {
            return Some(workspace);
        }
    }
    None
}

fn transcript_mtime_ms(path: &Path) -> Option<i64> {
    path.metadata()
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as i64)
}

/// Build a candidate from a conversation directory: the directory name is the
/// conversation ID, the transcript supplies the workspace, and the transcript
/// mtime is the freshness/order anchor. Returns `None` when the directory is
/// not a conversation (non-UUID name, no transcript) so callers skip it.
fn read_conversation_candidate(conv_dir: &Path, conv_id: &str) -> Option<Candidate> {
    if !is_agy_conversation_id(conv_id) {
        return None;
    }
    let transcript = transcript_path_in(conv_dir)?;
    let mtime_ms = transcript_mtime_ms(&transcript)?;
    // A transcript without a workspace anchor can't prove which node it
    // belongs to — but the freshness window below is tight (spawn ± 2s
    // skew + 13.5s poll), so an anchor-less transcript created in this
    // spawn's window is still almost certainly ours. Record an empty
    // directory so `select_id_for_directory` can accept it as a fallback
    // when nothing with a matching workspace exists.
    let directory = read_workspace_from_transcript(&transcript).unwrap_or_default();
    Some(Candidate {
        id: conv_id.to_string(),
        directory,
        timestamp_ms: mtime_ms,
    })
}

/// Pick the conversation ID to store for a freshly spawned node.
///
/// Only candidates created at or after `created_not_before_ms` are eligible.
/// A candidate with a workspace anchor must match `spawn_directory`; a
/// candidate without one (empty `directory`) is accepted as a fallback so a
/// transcript shape that omits the workspace field doesn't permanently wedge
/// capture — the tight time window is the safety net there. Prefers a
/// workspace-matched candidate over an anchor-less one; newest wins within
/// each tier. Never falls back to an older row outside the window.
pub fn select_id_for_directory<'a>(
    candidates: &'a [Candidate],
    spawn_directory: &str,
    created_not_before_ms: i64,
) -> Option<&'a str> {
    let fresh: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| is_agy_conversation_id(&c.id))
        .filter(|c| c.timestamp_ms >= created_not_before_ms)
        .collect();
    // Tier 1: workspace-anchored match (the precise case).
    if let Some(best) = fresh
        .iter()
        .filter(|c| !c.directory.trim().is_empty())
        .filter(|c| crate::env::directories_match(&c.directory, spawn_directory))
        .max_by_key(|c| c.timestamp_ms)
    {
        return Some(best.id.as_str());
    }
    // Tier 2: anchor-less transcript created in this spawn's window.
    fresh
        .iter()
        .filter(|c| c.directory.trim().is_empty())
        .max_by_key(|c| c.timestamp_ms)
        .map(|c| c.id.as_str())
}

fn collect_candidates(brain_dir: &Path) -> Vec<Candidate> {
    let Ok(entries) = fs::read_dir(brain_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let conv_dir = entry.path();
        if !conv_dir.is_dir() {
            continue;
        }
        let conv_id = entry.file_name().to_string_lossy().to_string();
        if let Some(candidate) = read_conversation_candidate(&conv_dir, &conv_id) {
            out.push(candidate);
        }
    }
    out
}

/// Scan `brain_dir` for the newest conversation created in this spawn's time
/// window. Workspace-anchored matches win; anchor-less transcripts in the
/// window are a fallback (see `select_id_for_directory`).
pub fn find_fresh_id_for_directory_in(
    brain_dir: &Path,
    spawn_directory: &str,
    created_not_before_ms: i64,
) -> Option<String> {
    if !brain_dir.is_dir() {
        return None;
    }
    let candidates = collect_candidates(brain_dir);
    select_id_for_directory(&candidates, spawn_directory, created_not_before_ms)
        .map(str::to_string)
}

/// Find an unambiguous historic conversation for a suspended node. The caller
/// only uses this to repair rows created before live capture existed;
/// differing matching conversation IDs are intentionally left suspended for
/// the user (mirrors `codex_session::find_unique_id_for_directory_in`).
pub fn find_unique_id_for_directory_in(
    brain_dir: &Path,
    spawn_directory: &str,
    created_not_before_ms: i64,
) -> Option<String> {
    if !brain_dir.is_dir() {
        return None;
    }
    let candidates = collect_candidates(brain_dir);
    let mut ids: Vec<String> = candidates
        .iter()
        .filter(|c| c.timestamp_ms >= created_not_before_ms)
        .filter(|c| {
            if c.directory.trim().is_empty() {
                true
            } else {
                crate::env::directories_match(&c.directory, spawn_directory)
            }
        })
        .map(|c| c.id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    (ids.len() == 1).then(|| ids.pop().unwrap())
}

enum CaptureAttempt {
    AlreadyStored,
    NotFound,
    Stored(String),
}

/// Poll briefly for Antigravity's brain-directory conversation, then fill an
/// otherwise empty `cli_session_id`. The hook path may win first; the DB
/// predicate keeps this delayed fallback from overwriting it.
pub fn start_capture_poller(node_id: i64, spawn_directory: String, env_type: EnvType) {
    let spawn_epoch_ms = chrono::Utc::now().timestamp_millis();
    tauri::async_runtime::spawn(async move {
        let not_before = spawn_epoch_ms.saturating_sub(CAPTURE_SKEW_MS);
        for (attempt, delay) in RETRY_DELAYS_MS.iter().enumerate() {
            tokio::time::sleep(Duration::from_millis(*delay)).await;
            if !crate::agent::process::PROCESS_REGISTRY.contains(&node_id) {
                return;
            }
            let Some(brain_dir) =
                crate::env::agy_brain_dir_for_env(env_type, &spawn_directory)
            else {
                tracing::warn!("agy session capture: no brain directory for env {env_type:?}");
                return;
            };
            let path = brain_dir.clone();
            let directory = spawn_directory.clone();
            // Keep the DB predicate, disk scan, and conditional write in one
            // blocking task. Splitting these into three dispatches on every
            // retry needlessly thrashes the blocking pool and widens the race
            // window between finding a conversation and claiming the row.
            let captured = crate::blocking::run_blocking("agy_capture", move || {
                if node_has_cli_session_id(node_id) {
                    return Ok(CaptureAttempt::AlreadyStored);
                }
                let Some(id) =
                    find_fresh_id_for_directory_in(&path, &directory, not_before)
                else {
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
                    tracing::warn!(
                        "agy session capture: blocking task failed for node {node_id}: {error}"
                    );
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
                "agy session capture: stored {id} for node {node_id} (attempt {})",
                attempt + 1
            );
            return;
        }
        tracing::warn!("agy session capture: gave up for node {node_id} in {spawn_directory}");
    });
}

fn node_has_cli_session_id(node_id: i64) -> bool {
    crate::db::get_agent_node_by_id(node_id)
        .ok()
        .and_then(|node| node.cli_session_id)
        .is_some_and(|id| !id.trim().is_empty())
}

/// Restore a missing pre-capture Antigravity identity for an existing
/// suspended node. This only accepts a single matching conversation ID, so
/// ambiguity remains visible and never causes a surprise resume.
pub async fn backfill_suspended_node(
    node_id: i64,
    node_directory: String,
    env_type: EnvType,
    created_at_ms: i64,
) -> bool {
    let Some(brain_dir) = crate::env::agy_brain_dir_for_env(env_type, &node_directory)
    else {
        return false;
    };
    let not_before = created_at_ms.saturating_sub(CAPTURE_SKEW_MS);
    let stored = crate::blocking::run_blocking("agy_backfill", move || {
        let Some(id) =
            find_unique_id_for_directory_in(&brain_dir, &node_directory, not_before)
        else {
            return Ok(None);
        };
        match crate::db::set_cli_session_id_if_missing(node_id, &id) {
            Ok(true) => Ok(Some(id)),
            Ok(false) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    })
    .await
    .unwrap_or_else(|error| {
        tracing::warn!("agy session capture: backfill failed for node {node_id}: {error}");
        None
    });
    if let Some(id) = stored {
        tracing::info!("agy session capture: backfilled {id} for suspended node {node_id}");
        true
    } else {
        false
    }
}

/// One-time recovery for rows created before Antigravity brain capture
/// existed (issue #1499). Mirrors
/// `codex_session::backfill_legacy_suspended_nodes_once`.
pub async fn backfill_legacy_suspended_nodes_once() -> Result<(), String> {
    let nodes = crate::blocking::run_blocking("agy_backfill_list_nodes", || {
        if crate::db::agy_legacy_session_backfill_completed().map_err(|error| error.to_string())? {
            return Ok(None);
        }
        crate::db::list_suspended_agy_nodes_without_cli_session_id()
            .map(Some)
            .map_err(|error| error.to_string())
    })
    .await?;
    let Some(nodes) = nodes else {
        return Ok(());
    };
    let mut inspected_all_sources = true;
    for node in nodes {
        let spawn_path = crate::env::node_working_path(&node).spawn_path;
        if crate::env::agy_brain_dir_for_env(node.env, &spawn_path).is_none() {
            inspected_all_sources = false;
            continue;
        }
        let _ = backfill_suspended_node(
            node.id,
            spawn_path,
            node.env,
            node.created_at.timestamp_millis(),
        )
        .await;
    }
    if !inspected_all_sources {
        return Ok(());
    }
    crate::db::mark_agy_legacy_session_backfill_completed().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn cand(id: &str, dir: &str, timestamp_ms: i64) -> Candidate {
        Candidate {
            id: id.into(),
            directory: dir.into(),
            timestamp_ms,
        }
    }

    const UUID_A: &str = "550e8400-e29b-41d4-a716-446655440000";
    const UUID_B: &str = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";

    #[test]
    fn validates_agy_conversation_id_as_uuid() {
        assert!(is_agy_conversation_id(UUID_A));
        assert!(is_agy_conversation_id(UUID_B));
        assert!(!is_agy_conversation_id("conv-aaaa-1111"));
        assert!(!is_agy_conversation_id("sess_01j6xyz890"));
        assert!(!is_agy_conversation_id(""));
        assert!(!is_agy_conversation_id("not-a-uuid"));
    }

    #[test]
    fn select_prefers_workspace_match_over_anchor_less_fallback() {
        let candidates = vec![
            cand(UUID_A, "", 300),
            cand(UUID_B, "/tmp/wt", 200),
        ];
        assert_eq!(
            select_id_for_directory(&candidates, "/tmp/wt", 100),
            Some(UUID_B)
        );
    }

    #[test]
    fn select_falls_back_to_anchor_less_transcript_in_window() {
        // Older transcript shapes (or fixtures) omit the workspace field.
        // A tight time window is the safety net — accept the newest
        // anchor-less candidate rather than wedging capture at NULL.
        let candidates = vec![cand(UUID_A, "", 200)];
        assert_eq!(
            select_id_for_directory(&candidates, "/tmp/wt", 100),
            Some(UUID_A)
        );
    }

    #[test]
    fn select_does_not_fall_back_to_historical_row_outside_window() {
        let candidates = vec![cand(UUID_A, "/tmp/wt", 100)];
        assert_eq!(
            select_id_for_directory(&candidates, "/tmp/wt", 9_000_000_000_000),
            None,
            "empty time window must not bind a brand-new node to last week's session"
        );
        let anchor_less = vec![cand(UUID_A, "", 100)];
        assert_eq!(
            select_id_for_directory(&anchor_less, "/tmp/wt", 9_000_000_000_000),
            None
        );
    }

    #[test]
    fn select_ignores_other_directories() {
        let candidates = vec![cand(UUID_A, "/tmp/other", 200)];
        assert_eq!(select_id_for_directory(&candidates, "/tmp/wt", 100), None);
    }

    #[test]
    fn select_rejects_non_uuid_ids() {
        let candidates = vec![cand("conv-aaaa-1111", "/tmp/wt", 200)];
        assert_eq!(select_id_for_directory(&candidates, "/tmp/wt", 100), None);
    }

    #[test]
    fn select_matches_windows_directory_slash_and_case() {
        let candidates = vec![cand(
            UUID_A,
            r"F:\src\buildmesh\.claude\worktrees\agy-test",
            200,
        )];
        assert_eq!(
            select_id_for_directory(
                &candidates,
                "f:/src/buildmesh/.claude/worktrees/agy-test",
                100
            ),
            Some(UUID_A)
        );
    }

    fn write_conv(brain: &Path, conv_id: &str, body: &str) -> PathBuf {
        let logs = brain
            .join(conv_id)
            .join(".system_generated")
            .join("logs");
        fs::create_dir_all(&logs).unwrap();
        let path = logs.join("transcript.jsonl");
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        file.sync_all().unwrap();
        brain.join(conv_id)
    }

    #[test]
    fn finds_fresh_session_matching_worktree() {
        let temp = tempfile::TempDir::new().unwrap();
        write_conv(
            temp.path(),
            UUID_A,
            r#"{"step_type":"USER_INPUT","text":"other","workspace_path":"/tmp/other","created_at":"2026-08-30T10:00:00Z"}"#,
        );
        write_conv(
            temp.path(),
            UUID_B,
            r#"{"step_type":"USER_INPUT","text":"wanted","workspace_path":"/tmp/wt","created_at":"2026-08-30T10:00:01Z"}"#,
        );
        let found = find_fresh_id_for_directory_in(temp.path(), "/tmp/wt", 0);
        assert_eq!(found, Some(UUID_B.to_string()));
    }

    #[test]
    fn ignores_conversation_from_before_this_fresh_spawn() {
        let temp = tempfile::TempDir::new().unwrap();
        write_conv(
            temp.path(),
            UUID_A,
            r#"{"step_type":"USER_INPUT","text":"old","workspace_path":"/tmp/wt"}"#,
        );
        assert_eq!(
            find_fresh_id_for_directory_in(temp.path(), "/tmp/wt", 9_000_000_000_000),
            None
        );
    }

    #[test]
    fn skips_non_uuid_directories_and_missing_transcripts() {
        let temp = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("conv-aaaa-1111")).unwrap();
        fs::create_dir_all(temp.path().join(UUID_A)).unwrap();
        assert_eq!(find_fresh_id_for_directory_in(temp.path(), "/tmp/wt", 0), None);
    }

    #[test]
    fn reads_metadata_nested_workspace() {
        let temp = tempfile::TempDir::new().unwrap();
        let logs = temp.path().join("conv").join(".system_generated").join("logs");
        fs::create_dir_all(&logs).unwrap();
        let path = logs.join("transcript.jsonl");
        fs::write(
            &path,
            r#"{"type":"USER_INPUT","metadata":{"cwd":"/tmp/nested"}}"#,
        )
        .unwrap();
        assert_eq!(
            read_workspace_from_transcript(&path).as_deref(),
            Some("/tmp/nested")
        );
    }

    #[test]
    fn transcript_full_fallback_resolves_when_short_missing() {
        let temp = tempfile::TempDir::new().unwrap();
        let conv = temp.path().join(UUID_A);
        let logs = conv.join(".system_generated").join("logs");
        fs::create_dir_all(&logs).unwrap();
        fs::write(
            logs.join("transcript_full.jsonl"),
            r#"{"step_type":"USER_INPUT","text":"hi","workspace_path":"/tmp/wt"}"#,
        )
        .unwrap();
        let found = find_fresh_id_for_directory_in(temp.path(), "/tmp/wt", 0);
        assert_eq!(found, Some(UUID_A.to_string()));
    }

    #[test]
    fn historic_backfill_rejects_ambiguous_matching_conversations() {
        let temp = tempfile::TempDir::new().unwrap();
        write_conv(
            temp.path(),
            UUID_A,
            r#"{"step_type":"USER_INPUT","text":"first","workspace_path":"/tmp/wt"}"#,
        );
        // Ensure distinct mtimes so both are "fresh" relative to epoch 0.
        std::thread::sleep(std::time::Duration::from_millis(10));
        write_conv(
            temp.path(),
            UUID_B,
            r#"{"step_type":"USER_INPUT","text":"second","workspace_path":"/tmp/wt"}"#,
        );
        assert_eq!(find_unique_id_for_directory_in(temp.path(), "/tmp/wt", 0), None);
    }

    #[test]
    fn historic_backfill_accepts_single_match() {
        let temp = tempfile::TempDir::new().unwrap();
        write_conv(
            temp.path(),
            UUID_A,
            r#"{"step_type":"USER_INPUT","text":"only","workspace_path":"/tmp/wt"}"#,
        );
        assert_eq!(
            find_unique_id_for_directory_in(temp.path(), "/tmp/wt", 0),
            Some(UUID_A.to_string())
        );
    }
}
