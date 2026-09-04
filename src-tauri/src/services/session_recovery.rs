//! Recover missed harness identities before startup resume (#1555).
//!
//! This reads historic metadata, not fresh-spawn pollers: those require a live
//! process and use today's clock. Never substitute a new conversation or guess
//! between two IDs. A missing identity is retried on the next startup.

use super::{agy_session, codex_session, commandcode_session, opencode_session};
use crate::models::{AgentNode, Provider};

const CLOCK_SKEW_MS: i64 = 2_000;
const INITIAL_SPAWN_WINDOW_MS: i64 = 300_000;

/// Legacy rows have a creation time but no durable process-start time. Limit
/// recovery to the initial launch window so a later conversation in a reused
/// directory cannot be mistaken for this node. Regenerated/ambiguous rows need
/// explicit recovery if their original identity was never captured.
fn select_identity(candidates: Vec<(String, i64)>, created_at_ms: i64) -> Option<String> {
    let mut ids = candidates
        .into_iter()
        .filter(|(_, time)| {
            *time >= created_at_ms.saturating_sub(CLOCK_SKEW_MS)
                && *time <= created_at_ms.saturating_add(INITIAL_SPAWN_WINDOW_MS)
        })
        .map(|(id, _)| id)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    (ids.len() == 1).then(|| ids.remove(0))
}

fn find_identity(node: &AgentNode, created: i64, recorded_start: bool) -> Option<String> {
    let directory = crate::env::node_working_path(node).spawn_path;
    let provider = crate::preferences::resolve_harness_provider(&node.provider);
    let root = match provider {
        Provider::Codex => crate::env::codex_sessions_dir(node.env, &directory)?,
        Provider::Agy => crate::env::agy_brain_dir_for_env(node.env, &directory)?,
        Provider::OpenCode => opencode_session::opencode_db_path(node.env)?,
        Provider::CommandCode => {
            super::transcript_reader::commandcode_sessions_dir(node.env, &directory)?
        }
        _ => return None,
    };
    find_identity_in(provider, &root, &directory, created, recorded_start)
}

fn find_identity_in(
    provider: Provider,
    root: &std::path::Path,
    directory: &str,
    created: i64,
    recorded_start: bool,
) -> Option<String> {
    let cutoff = created.saturating_sub(CLOCK_SKEW_MS);
    let candidates: Vec<(String, i64)> = match provider {
        Provider::Codex => codex_session::find_candidates(root, directory, cutoff, None)
            .into_iter()
            .map(|c| (c.id, c.timestamp_ms))
            .collect(),
        Provider::Agy => {
            agy_session::collect_candidates(root, cutoff)
                .into_iter()
                // Timing alone is insufficient for historic global brain entries.
                .filter(|c| {
                    c.workspace
                        .as_deref()
                        .is_some_and(|cwd| crate::env::directories_match(cwd, directory))
                })
                .map(|c| (c.id, c.created_ms))
                .collect()
        }
        Provider::OpenCode => {
            let conn = rusqlite::Connection::open_with_flags(
                root,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .ok()?;
            opencode_session::list_root_sessions_in_window(
                &conn,
                cutoff,
                if recorded_start {
                    created.saturating_add(INITIAL_SPAWN_WINDOW_MS)
                } else {
                    i64::MAX
                },
            )
            .ok()?
            .into_iter()
            .filter(|c| {
                crate::env::directories_match(&c.directory, directory)
                    && opencode_session::is_opencode_session_id(&c.id)
            })
            .map(|c| (c.id, c.created))
            .collect()
        }
        Provider::CommandCode => std::fs::read_dir(root)
            .ok()?
            .flatten()
            .filter_map(|entry| commandcode_session::read_session_file(&entry.path()))
            .filter(|c| crate::env::directories_match(&c.directory, directory))
            .map(|c| (c.id, c.timestamp_ms))
            .collect(),
        _ => return None,
    };
    if !recorded_start {
        // Legacy regeneration has no durable launch timestamp. Discarding a
        // later ID before checking ambiguity could revive a replaced session.
        let ids: std::collections::HashSet<&String> = candidates
            .iter()
            .filter(|(_, time)| *time >= cutoff)
            .map(|(id, _)| id)
            .collect();
        if ids.len() != 1 {
            return None;
        }
    }
    select_identity(candidates, created)
}

pub async fn recover_suspended_node(node: AgentNode) -> Result<bool, String> {
    crate::blocking::run_blocking("recover_suspended_session", move || {
        if node.cli_session_id.as_deref().is_some_and(|id| !id.is_empty())
            || !crate::preferences::resolve_harness_provider(&node.provider).adapter().auto_resume_on_startup()
            // A suspended Autopilot node without an identity may be awaiting
            // sandbox approval and must never be started by transcript matching.
            || crate::db::get_autopilot_run(node.id).map_err(|e| e.to_string())?.is_some()
        {
            return Ok(false);
        }
        let generation = crate::db::session_started_at_ms(node.id).map_err(|e| e.to_string())?;
        let started = generation.unwrap_or_else(|| node.created_at.timestamp_millis());
        let Some(id) = find_identity(&node, started, generation.is_some()) else { return Ok(false); };
        let recovered = crate::db::recover_suspended_cli_session_id(&node, &id, generation)
            .map_err(|e| e.to_string())?;
        if recovered {
            tracing::info!("session recovery: restored identity for node {} ({})", node.id, node.provider);
        }
        Ok(recovered)
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "01a067ef-81ab-7141-a6b4-208e32df59bf";
    const CREATED: i64 = 1_787_830_399_000;
    const TIMESTAMP: &str = "2026-08-27T11:33:19.986Z";

    #[test]
    fn recovers_codex_metadata_without_banner_and_excludes_child_threads() {
        let temp = tempfile::tempdir().unwrap();
        let day = temp.path().join("2026/08/27");
        std::fs::create_dir_all(&day).unwrap();
        for (file, payload) in [
            (
                "root.jsonl",
                serde_json::json!({"id": ID, "session_id": ID,
                "cwd": "F:\\repo", "timestamp": TIMESTAMP, "thread_source": "user"}),
            ),
            (
                "child.jsonl",
                serde_json::json!({"id": "01a067ec-e952-7830-b3a6-bc16bc79f327",
                "cwd": "F:\\repo", "timestamp": TIMESTAMP, "source": {"subagent": {}}}),
            ),
        ] {
            std::fs::write(
                day.join(file),
                serde_json::json!({"type": "session_meta", "payload": payload}).to_string(),
            )
            .unwrap();
        }
        assert_eq!(
            find_identity_in(Provider::Codex, temp.path(), "f:/repo", CREATED, true),
            Some(ID.into())
        );
        assert_eq!(
            find_identity_in(Provider::Codex, temp.path(), "f:/other", CREATED, true),
            None
        );
        assert_eq!(
            find_identity_in(
                Provider::Codex,
                temp.path(),
                "f:/repo",
                CREATED + 10_000,
                true
            ),
            None
        );
    }

    #[test]
    fn recovers_commandcode_transcript_flushed_after_the_poller_expired() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join(format!("{ID}.jsonl")),
            serde_json::json!({
                "type": "session", "id": ID, "cwd": "F:/repo", "timestamp": TIMESTAMP,
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            find_identity_in(
                Provider::CommandCode,
                temp.path(),
                "F:/repo",
                CREATED - 30_000,
                true
            ),
            Some(ID.into())
        );
    }

    #[test]
    fn recovers_agy_json_encoded_windows_anchor_but_not_unknown_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let logs = temp.path().join(ID).join(".system_generated/logs");
        std::fs::create_dir_all(&logs).unwrap();
        let transcript = logs.join("transcript.jsonl");
        let metadata = serde_json::json!({"created_at": TIMESTAMP,
            "tool_calls": [{"args": {"Cwd": serde_json::to_string(r"F:\repo").unwrap()}}]});
        std::fs::write(&transcript, metadata.to_string()).unwrap();
        assert_eq!(
            find_identity_in(Provider::Agy, temp.path(), "F:/repo", CREATED, true),
            Some(ID.into())
        );
        std::fs::write(
            &transcript,
            serde_json::json!({"created_at": TIMESTAMP}).to_string(),
        )
        .unwrap();
        assert_eq!(
            find_identity_in(Provider::Agy, temp.path(), "F:/repo", CREATED, true),
            None
        );
    }

    #[test]
    fn recovers_opencode_after_two_minutes_even_with_fifty_newer_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("opencode.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id TEXT, directory TEXT, time_created INTEGER,
            time_updated INTEGER, parent_id TEXT, time_archived INTEGER);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session VALUES ('ses_wanted', 'F:/repo', ?1, ?1, NULL, NULL)",
            [CREATED + 133_000],
        )
        .unwrap();
        for i in 0..55 {
            conn.execute(
                "INSERT INTO session VALUES (?1, 'F:/other', ?2, ?2, NULL, NULL)",
                rusqlite::params![format!("ses_other{i}"), CREATED + 600_000 + i],
            )
            .unwrap();
        }
        assert_eq!(
            find_identity_in(Provider::OpenCode, &path, "F:/repo", CREATED, true),
            Some("ses_wanted".into())
        );
    }

    #[test]
    fn historic_recovery_ignores_later_conversations_and_accepts_duplicate_rollouts() {
        assert_eq!(
            select_identity(
                vec![
                    ("original".into(), 10_005),
                    ("original".into(), 10_006),
                    ("later".into(), 400_000),
                    ("old".into(), 1_000),
                ],
                10_000
            ),
            Some("original".into())
        );
    }

    #[test]
    fn historic_recovery_does_not_guess_between_simultaneous_conversations() {
        assert_eq!(
            select_identity(vec![("a".into(), 10_005), ("b".into(), 10_010)], 10_000),
            None
        );
        assert_eq!(select_identity(vec![], 10_000), None);
    }

    #[test]
    fn fresh_start_anchor_selects_new_conversation_in_an_old_node_directory() {
        let temp = tempfile::tempdir().unwrap();
        let new_id = "01a067ec-e952-7830-b3a6-bc16bc79f327";
        for (id, timestamp) in [(ID, "2026-08-27T10:33:19.986Z"), (new_id, TIMESTAMP)] {
            std::fs::write(
                temp.path().join(format!("{id}.jsonl")),
                serde_json::json!({
                    "type": "session", "id": id, "cwd": "F:/repo", "timestamp": timestamp,
                })
                .to_string(),
            )
            .unwrap();
        }
        assert_eq!(
            find_identity_in(Provider::CommandCode, temp.path(), "F:/repo", CREATED, true),
            Some(new_id.into())
        );
        assert_eq!(
            find_identity_in(
                Provider::CommandCode,
                temp.path(),
                "F:/repo",
                CREATED - 3_600_000,
                false
            ),
            None,
            "legacy node with two historic conversations must not revive the original",
        );
    }
}
