use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::models::EnvType;

pub const CAPTURE_SKEW_MS: i64 = 2_000;

pub const RETRY_DELAYS_MS: &[u64] = &[400, 800, 1_600, 2_500, 4_000];

const MAX_SLUG_LEN: usize = 128;

pub(crate) fn first_user_request(
    path: &Path,
    session_id: &str,
) -> rusqlite::Result<Option<String>> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(Duration::from_millis(100))?;
    let preview_chars = super::transcript_reader::types::MAX_TURN_TEXT as i64 + 1;
    conn.query_row(
        "SELECT substr(json_extract(chat_message, '$.content'), 1, ?2)
         FROM message_nodes
         WHERE session_id = ?1 AND CASE WHEN json_valid(chat_message) THEN
           json_extract(chat_message, '$.role') = 'user'
           AND json_extract(chat_message, '$.metadata.is_user_input') = 1
           AND json_type(chat_message, '$.content') = 'text'
           AND trim(json_extract(chat_message, '$.content'), char(9, 10, 13, 32)) <> ''
         ELSE 0 END
         ORDER BY node_id LIMIT 1",
        rusqlite::params![session_id, preview_chars],
        |row| row.get(0),
    )
    .optional()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedSession {
    pub session_id: String,
    pub working_directory: String,
    pub created_ms: i64,
}

pub fn is_devin_session_slug(id: &str) -> bool {
    if id.is_empty() || id.len() > MAX_SLUG_LEN {
        return false;
    }
    if uuid::Uuid::parse_str(id).is_ok() {
        return false;
    }
    id.split('-').all(|word| {
        !word.is_empty()
            && word
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    }) && id.contains('-')
}

pub fn is_devin_session_id(id: &str) -> bool {
    is_devin_session_slug(id) || uuid::Uuid::parse_str(id.trim()).is_ok()
}

pub fn select_id_for_directory<'a>(
    sessions: &'a [ListedSession],
    spawn_directory: &str,
    created_not_before_ms: i64,
) -> Option<&'a str> {
    let viable: Vec<&ListedSession> = sessions
        .iter()
        .filter(|s| is_devin_session_id(&s.session_id))
        .filter(|s| crate::env::directories_match(&s.working_directory, spawn_directory))
        .filter(|s| s.created_ms >= created_not_before_ms)
        .collect();
    match viable.len() {
        1 => Some(viable[0].session_id.as_str()),
        _ => None,
    }
}

pub fn list_recent_sessions(
    conn: &Connection,
    created_not_before_ms: i64,
) -> Result<Vec<ListedSession>, String> {
    let not_before_secs = created_not_before_ms.div_euclid(1_000);
    let mut stmt = conn
        .prepare(
            "SELECT id, working_directory, created_at \
             FROM sessions \
             WHERE hidden = 0 AND created_at >= ?1 \
             ORDER BY rowid DESC \
             LIMIT 200",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([not_before_secs], |row| {
            let created_secs: i64 = row.get(2)?;
            Ok(ListedSession {
                session_id: row.get(0)?,
                working_directory: row.get(1)?,
                created_ms: created_secs.saturating_mul(1_000),
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

pub fn list_sessions_in_window(
    conn: &Connection,
    not_before_ms: i64,
    not_after_ms: i64,
) -> Result<Vec<ListedSession>, String> {
    let not_before_secs = not_before_ms.div_euclid(1_000);
    let open_ended = not_after_ms == i64::MAX;
    let not_after_secs = not_after_ms.saturating_add(999).div_euclid(1_000);
    let sql = if open_ended {
        "SELECT id, working_directory, created_at \
         FROM sessions \
         WHERE hidden = 0 AND created_at >= ?1 AND created_at <= ?2 \
         ORDER BY rowid DESC"
    } else {
        "SELECT id, working_directory, created_at \
         FROM sessions \
         WHERE hidden = 0 AND created_at >= ?1 AND created_at <= ?2 \
         ORDER BY rowid DESC \
         LIMIT 500"
    };
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([not_before_secs, not_after_secs], |row| {
            let created_secs: i64 = row.get(2)?;
            Ok(ListedSession {
                session_id: row.get(0)?,
                working_directory: row.get(1)?,
                created_ms: created_secs.saturating_mul(1_000),
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

pub(crate) fn find_historic_id_for_directory(
    env_type: EnvType,
    spawn_path: &str,
    anchor_ms: i64,
    recorded_start: bool,
) -> Option<String> {
    let db_path = crate::env::devin_db_path_for_host(env_type, spawn_path)?;
    find_historic_id_for_db_path(&db_path, spawn_path, anchor_ms, recorded_start)
}

pub(crate) fn find_historic_id_for_db_path(
    db_path: &Path,
    spawn_directory: &str,
    anchor_ms: i64,
    recorded_start: bool,
) -> Option<String> {
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let cutoff = anchor_ms.saturating_sub(crate::services::session_recovery::CLOCK_SKEW_MS);
    let not_after = if recorded_start {
        anchor_ms.saturating_add(crate::services::session_recovery::INITIAL_SPAWN_WINDOW_MS)
    } else {
        i64::MAX
    };
    let candidates = list_sessions_in_window(&conn, cutoff, not_after)
        .ok()?
        .into_iter()
        .filter(|candidate| {
            is_devin_session_id(&candidate.session_id)
                && crate::env::directories_match(&candidate.working_directory, spawn_directory)
        });
    crate::services::session_recovery::select_recovery_identity(
        candidates.map(|candidate| (candidate.session_id, candidate.created_ms)),
        anchor_ms,
        recorded_start,
    )
}

fn try_capture_from_db_path(
    db_path: &Path,
    spawn_directory: &str,
    created_not_before_ms: i64,
) -> Option<String> {
    if !db_path.exists() {
        return None;
    }
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let sessions = list_recent_sessions(&conn, created_not_before_ms).ok()?;
    select_id_for_directory(&sessions, spawn_directory, created_not_before_ms).map(str::to_string)
}

pub fn start_capture_poller(node_id: i64, spawn_directory: String, env_type: EnvType) {
    let spawn_epoch_ms = chrono::Utc::now().timestamp_millis();
    tauri::async_runtime::spawn(async move {
        let not_before = spawn_epoch_ms.saturating_sub(CAPTURE_SKEW_MS);
        let Some(db_path) = crate::env::devin_db_path_for_host(env_type, &spawn_directory) else {
            tracing::warn!("devin session capture: no db path for env {env_type:?}");
            return;
        };
        for (i, delay) in RETRY_DELAYS_MS.iter().enumerate() {
            tokio::time::sleep(Duration::from_millis(*delay)).await;
            if !crate::agent::process::PROCESS_REGISTRY.contains(&node_id) {
                tracing::debug!("devin session capture: node {node_id} gone, stop");
                return;
            }
            let path = db_path.clone();
            let dir = spawn_directory.clone();
            let captured = crate::blocking::run_blocking("devin_capture", move || {
                let Some(id) = try_capture_from_db_path(&path, &dir, not_before) else {
                    return Ok(None);
                };
                crate::db::set_cli_session_id_if_missing(node_id, &id)
                    .map(|_| Some(id))
                    .map_err(|error| error.to_string())
            })
            .await;
            let captured = match captured {
                Ok(captured) => captured,
                Err(error) => {
                    tracing::warn!(
                        "devin session capture: blocking task failed for node {node_id}: {error}"
                    );
                    return;
                }
            };
            if let Some(id) = captured {
                if !crate::agent::process::PROCESS_REGISTRY.contains(&node_id) {
                    return;
                }
                tracing::info!(
                    "devin session capture: stored {id} for node {node_id} (attempt {})",
                    i + 1
                );
                return;
            }
        }
        tracing::warn!("devin session capture: gave up for node {node_id} in {spawn_directory}");
    });
}

#[cfg(test)]
#[path = "devin_session_tests.rs"]
mod tests;
