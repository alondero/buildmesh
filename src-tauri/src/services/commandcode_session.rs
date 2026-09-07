//! Command Code session-ID capture from local session transcripts.
//!
//! Command Code self-assigns session IDs as standard UUIDs
//! (e.g. `3fadada6-e0a3-44a2-ab68-ce1ecf7207a9`; `sess_…` is accepted for
//! backward compatibility) and writes structured transcript records to
//! `%USERPROFILE%/.commandcode/projects/<encoded-cwd>/<session_id>.jsonl`
//! (Windows) or `~/.commandcode/projects/<encoded-cwd>/<session_id>.jsonl`
//! (macOS/Linux/WSL) — issue #1500.
//!
//! Because Command Code prints rich TUI output rather than standard UUID
//! banners, PTY UUID capture cannot extract IDs. A bounded
//! post-spawn poller inspects `~/.commandcode/projects/<encoded-cwd>/` and
//! reads the session header record (`{"type":"session","id":"<uuid>",
//! "timestamp":"...","cwd":"..."}`) to associate the node with its exact
//! session.
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
///
/// Observed v1.43.0 shape: `{"type":"session","version":3,"id":"<uuid>",
/// "timestamp":"...","cwd":"..."}`. `session_id`/`directory`/`created_at`
/// aliases cover older transcripts that used those names; the camelCase
/// guesses (`sessionId`, `project_path`, `workspace`, `time_created`,
/// `createdAt`) are deliberately not accepted — `createdAt` is a checkpoint
/// field, and accepting it is what let checkpoint turns masquerade as
/// sessions (issue #1500 review).
#[derive(Debug, Default, Deserialize)]
struct SessionHeader {
    /// Record discriminator. Real session headers carry `"session"`;
    /// checkpoint/message records carry something else (or nothing at all).
    #[serde(rename = "type")]
    record_type: Option<String>,
    #[serde(alias = "id")]
    session_id: Option<String>,
    #[serde(alias = "directory")]
    cwd: Option<String>,
    #[serde(alias = "created_at")]
    timestamp: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub directory: String,
    pub timestamp_ms: i64,
}

/// Command Code session IDs are standard UUIDs (v1.43.0). `sess_…` IDs are
/// accepted for backward compatibility with older transcripts.
pub fn is_commandcode_session_id(id: &str) -> bool {
    if id.starts_with("sess_") && id.len() > 5 {
        return true;
    }
    uuid::Uuid::parse_str(id).is_ok()
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
///
/// Root-cause validation (issue #1500 review), cheapest check first:
/// 1. the extension must be `.jsonl`;
/// 2. the filename stem must itself be a session ID (`<uuid>.jsonl`) — this
///    rejects sidecars (`<id>.checkpoints.jsonl`), auxiliary files
///    (`history.jsonl`, `index.jsonl`), and anything else the CLI did not
///    name after a session, without opening the file;
/// 3. line 1 must parse, and when it carries a `"type"` discriminator it must
///    be `"session"` (checkpoint/message records are rejected here);
/// 4. the header ID (when present and valid) must equal the stem — a mismatch
///    means the file is inconsistent and is never silently preferred.
pub fn read_session_file(path: &Path) -> Option<Candidate> {
    if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
        return None;
    }
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if !is_commandcode_session_id(stem) {
        return None;
    }
    let file = fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let mut first_line = String::new();
    reader.read_line(&mut first_line).ok()?;
    if first_line.trim().is_empty() {
        return None;
    }

    let header: SessionHeader = serde_json::from_str(&first_line).ok()?;
    if header
        .record_type
        .as_deref()
        .is_some_and(|kind| kind != "session")
    {
        return None;
    }
    let header_id = header.session_id.filter(|s| is_commandcode_session_id(s));
    if header_id.as_deref().is_some_and(|id| id != stem) {
        return None;
    }
    let id = header_id.unwrap_or_else(|| stem.to_string());

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
///
/// Every candidate produced by [`read_session_file`] already carries a valid
/// session ID, so this filters only on directory and spawn window.
pub fn select_id_for_directory<'a>(
    candidates: &'a [Candidate],
    spawn_directory: &str,
    created_not_before_ms: i64,
) -> Option<&'a str> {
    candidates
        .iter()
        .filter(|c| crate::env::directories_match(&c.directory, spawn_directory))
        .filter(|c| c.timestamp_ms >= created_not_before_ms)
        .max_by_key(|c| c.timestamp_ms)
        .map(|c| c.id.as_str())
}

/// Scan `sessions_dir` (`<home>/.commandcode/projects/<encoded-cwd>/`) for all
/// session transcripts and find the newest valid candidate. [`read_session_file`]
/// is the single authority on what counts as a transcript (stem + header
/// validation); non-transcripts yield `None` there and are skipped here.
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
        if let Some(candidate) = read_session_file(&entry.path()) {
            candidates.push(candidate);
        }
    }
    select_id_for_directory(&candidates, spawn_directory, created_not_before_ms).map(str::to_string)
}

/// Historic startup recovery entry point used by the Command Code adapter.
/// Transcript validation and directory matching stay in this provider module;
/// the shared service applies the common time and ambiguity rules.
pub(crate) fn find_historic_id_for_directory(
    env_type: EnvType,
    spawn_directory: &str,
    anchor_ms: i64,
    recorded_start: bool,
) -> Option<String> {
    let sessions_dir =
        crate::services::transcript_reader::commandcode_sessions_dir(env_type, spawn_directory)?;
    find_historic_id_for_directory_in(&sessions_dir, spawn_directory, anchor_ms, recorded_start)
}

pub(crate) fn find_historic_id_for_directory_in(
    sessions_dir: &Path,
    spawn_directory: &str,
    anchor_ms: i64,
    recorded_start: bool,
) -> Option<String> {
    let entries = fs::read_dir(sessions_dir).ok()?;
    let candidates = entries
        .flatten()
        .filter_map(|entry| read_session_file(&entry.path()))
        .filter(|candidate| crate::env::directories_match(&candidate.directory, spawn_directory));
    crate::services::session_recovery::select_recovery_identity(
        candidates.map(|candidate| (candidate.id, candidate.timestamp_ms)),
        anchor_ms,
        recorded_start,
    )
}

/// Background poller: read Command Code's session directory until a fresh session
/// created in this node's spawn window appears, then save `cli_session_id`.
pub fn start_capture_poller(
    node_id: i64,
    spawn_directory: String,
    env_type: EnvType,
    app: tauri::AppHandle,
) {
    let spawn_epoch_ms = chrono::Utc::now().timestamp_millis();
    tauri::async_runtime::spawn(async move {
        let not_before = spawn_epoch_ms.saturating_sub(CAPTURE_SKEW_MS);
        let Some(sessions_dir) = crate::services::transcript_reader::commandcode_sessions_dir(
            env_type,
            &spawn_directory,
        ) else {
            tracing::warn!("commandcode session capture: no sessions dir for env {env_type:?}");
            return;
        };
        for (attempt, delay) in RETRY_DELAYS_MS.iter().enumerate() {
            tokio::time::sleep(Duration::from_millis(*delay)).await;
            if !crate::agent::process::PROCESS_REGISTRY.is_alive(&node_id) {
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
                if !crate::agent::process::PROCESS_REGISTRY.is_alive(&node_id) {
                    return;
                }
                match crate::db::set_cli_session_id_if_missing(node_id, &id) {
                    Ok(true) => {
                        tracing::info!(
                            "commandcode session capture: stored {id} for node {node_id} (attempt {})",
                            attempt + 1
                        );
                        let watcher_id = id.clone();
                        let watcher_directory = spawn_directory.clone();
                        let watcher_app = app.clone();
                        let watcher_result =
                            crate::blocking::run_blocking("commandcode watcher start", move || {
                                crate::services::commandcode_watcher::start_for_session(
                                    node_id,
                                    &watcher_id,
                                    &watcher_directory,
                                    env_type,
                                    &watcher_app,
                                )
                            })
                            .await;
                        if let Err(error) = watcher_result {
                            tracing::warn!(
                                "commandcode watcher: could not start for node {node_id}: {error}"
                            );
                        }
                    }
                    Ok(false) => {}
                    Err(e) => tracing::warn!(
                        "commandcode session capture: db write failed for node {node_id}: {e}"
                    ),
                }
                return;
            }
        }
        tracing::warn!(
            "commandcode session capture: gave up for node {node_id} in {spawn_directory}"
        );
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
        // Legacy `sess_…` IDs stay valid for backward compatibility.
        assert!(is_commandcode_session_id("sess_01j6xyz890"));
        assert!(is_commandcode_session_id("sess_abc123"));
        assert!(!is_commandcode_session_id("sess_"));
        // Issue #1500: Command Code v1.43.0 mints standard UUIDs.
        assert!(is_commandcode_session_id(
            "3fadada6-e0a3-44a2-ab68-ce1ecf7207a9"
        ));
        assert!(is_commandcode_session_id(
            "01a024d2-7cd6-7ea2-b907-531b0d261be7"
        ));
        assert!(!is_commandcode_session_id("ses_opencode123"));
        assert!(!is_commandcode_session_id("not-a-uuid"));
        assert!(!is_commandcode_session_id(""));
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
        fs::write(
            &file_path,
            format!("{}\n{{\"type\":\"user_input\"}}", content),
        )
        .unwrap();

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

    #[test]
    fn reads_v143_uuid_session_header() {
        // Exact v1.43.0 header shape from issue #1500.
        let temp = tempfile::TempDir::new().unwrap();
        let id = "3fadada6-e0a3-44a2-ab68-ce1ecf7207a9";
        let file_path = temp.path().join(format!("{id}.jsonl"));
        fs::write(
            &file_path,
            format!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-09-02T19:53:20.151Z\",\"cwd\":\"F:\\\\src\\\\buildmesh\\\\.claude\\\\worktrees\\\\saucy-thunderous-cove\"}}\n"
            ),
        )
        .unwrap();

        let c = read_session_file(&file_path).expect("should parse UUID candidate");
        assert_eq!(c.id, id);
        assert_eq!(
            c.directory,
            r"F:\src\buildmesh\.claude\worktrees\saucy-thunderous-cove"
        );
        assert!(c.timestamp_ms > 0);

        let found = find_fresh_id_for_directory_in(
            temp.path(),
            r"F:\src\buildmesh\.claude\worktrees\saucy-thunderous-cove",
            c.timestamp_ms - 1,
        );
        assert_eq!(found, Some(id.to_string()));
    }

    #[test]
    fn rejects_checkpoint_sidecars() {
        let temp = tempfile::TempDir::new().unwrap();
        let session_id = "3fadada6-e0a3-44a2-ab68-ce1ecf7207a9";
        fs::write(
            temp.path().join(format!("{session_id}.jsonl")),
            format!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"{session_id}\",\"timestamp\":\"2026-09-02T19:53:20.151Z\",\"cwd\":\"/tmp/wt\"}}\n"
            ),
        )
        .unwrap();
        // Checkpoint sidecar carries its own checkpoint UUID — must never be
        // mistaken for a session.
        let checkpoint_path = temp.path().join(format!("{session_id}.checkpoints.jsonl"));
        fs::write(
            &checkpoint_path,
            r#"{"id":"4f729f43-46ed-4721-a885-3f16f26bff5f","messageId":"4f729f43-46ed-4721-a885-3f16f26bff5f","turnNumber":1,"createdAt":"2026-09-02T19:53:40.567Z"}"#,
        )
        .unwrap();

        assert!(read_session_file(&checkpoint_path).is_none());
        let found = find_fresh_id_for_directory_in(temp.path(), "/tmp/wt", 0);
        assert_eq!(found, Some(session_id.to_string()));
    }

    #[test]
    fn rejects_auxiliary_files_and_wrong_types_and_mismatched_ids() {
        // Issue #1500 review: sidecar detection must be schema validation,
        // not filename guessing. Auxiliary files (`history.jsonl`,
        // `index.jsonl`) have non-ID stems; message/checkpoint records carry a
        // non-`session` type; a stem/header mismatch is inconsistent.
        let temp = tempfile::TempDir::new().unwrap();
        for name in ["history.jsonl", "index.jsonl", "active.jsonl"] {
            fs::write(
                temp.path().join(name),
                r#"{"type":"session","id":"3fadada6-e0a3-44a2-ab68-ce1ecf7207a9","cwd":"/tmp/wt","timestamp":"2026-09-02T19:53:20.151Z"}"#,
            )
            .unwrap();
            assert!(
                read_session_file(&temp.path().join(name)).is_none(),
                "{name} must not parse as a session"
            );
        }

        let typed = temp
            .path()
            .join("3fadada6-e0a3-44a2-ab68-ce1ecf7207a9.jsonl");
        fs::write(
            &typed,
            r#"{"type":"message","id":"3fadada6-e0a3-44a2-ab68-ce1ecf7207a9","cwd":"/tmp/wt","timestamp":"2026-09-02T19:53:20.151Z"}"#,
        )
        .unwrap();
        assert!(read_session_file(&typed).is_none());

        let mismatched = temp
            .path()
            .join("aaaaaaaa-1111-2222-3333-444444444444.jsonl");
        fs::write(
            &mismatched,
            r#"{"type":"session","id":"bbbbbbbb-1111-2222-3333-444444444444","cwd":"/tmp/wt","timestamp":"2026-09-02T19:53:20.151Z"}"#,
        )
        .unwrap();
        assert!(read_session_file(&mismatched).is_none());
    }

    #[test]
    fn historic_recovery_accepts_a_transcript_flushed_after_the_poller_expired() {
        const ID: &str = "01a067ef-81ab-7141-a6b4-208e32df59bf";
        const CREATED: i64 = 1_787_830_399_000;
        const TIMESTAMP: &str = "2026-08-27T11:33:19.986Z";
        let temp = tempfile::TempDir::new().unwrap();
        fs::write(
            temp.path().join(format!("{ID}.jsonl")),
            serde_json::json!({
                "type": "session",
                "id": ID,
                "cwd": "F:/repo",
                "timestamp": TIMESTAMP,
            })
            .to_string(),
        )
        .unwrap();

        assert_eq!(
            find_historic_id_for_directory_in(temp.path(), "F:/repo", CREATED - 30_000, true,),
            Some(ID.to_string())
        );
    }

    #[test]
    fn fresh_start_anchor_selects_new_conversation_in_an_old_node_directory() {
        const OLD_ID: &str = "01a067ef-81ab-7141-a6b4-208e32df59bf";
        const NEW_ID: &str = "01a067ec-e952-7830-b3a6-bc16bc79f327";
        const CREATED: i64 = 1_787_830_399_000;
        let temp = tempfile::TempDir::new().unwrap();
        for (id, timestamp) in [
            (OLD_ID, "2026-08-27T10:33:19.986Z"),
            (NEW_ID, "2026-08-27T11:33:19.986Z"),
        ] {
            fs::write(
                temp.path().join(format!("{id}.jsonl")),
                serde_json::json!({
                    "type": "session",
                    "id": id,
                    "cwd": "F:/repo",
                    "timestamp": timestamp,
                })
                .to_string(),
            )
            .unwrap();
        }

        assert_eq!(
            find_historic_id_for_directory_in(temp.path(), "F:/repo", CREATED, true),
            Some(NEW_ID.to_string())
        );
        assert_eq!(
            find_historic_id_for_directory_in(temp.path(), "F:/repo", CREATED - 3_600_000, false,),
            None,
            "legacy nodes with two historic conversations must not revive one by guessing",
        );
    }
}
