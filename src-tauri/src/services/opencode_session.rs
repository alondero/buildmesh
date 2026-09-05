//! OpenCode session-ID capture from the local SQLite store.
//!
//! OpenCode self-assigns IDs of the form `ses_<hex+base62>` and does not
//! accept a caller-chosen UUID (`docs/learning/opencode-harness-capabilities.md`).
//! The TUI does not print that ID, and the PTY UUID regex in
//! `session_capture` cannot match `ses_…`. After a fresh spawn we read
//! `%USERPROFILE%/.local/share/opencode/opencode.db` (the same file the
//! usage meter already opens) and pick the newest **root** session whose
//! `directory` matches the node's spawn path **and** whose `time_created`
//! is at or after spawn (minus a small clock skew).
//!
//! Historical rows in that directory are never used as a fallback: if the
//! TUI has not flushed yet, the poller retries. Poisoning `cli_session_id`
//! with last week's conversation would resume the wrong session.
//!
//! Select/match helpers are pure so the rules are unit-tested without a
//! live `opencode` binary. SQLite listing is tested against an in-memory
//! schema matching OpenCode 1.18.3.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::models::EnvType;

/// One OpenCode session row (JSON list shape or SQLite `session` table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedSession {
    pub id: String,
    pub directory: String,
    pub created: i64,
    pub updated: i64,
}

/// Pick the session ID to store for a freshly spawned node.
///
/// Only rows that (a) match `spawn_directory`, (b) look like an OpenCode
/// `ses_…` id, and (c) were created at or after `created_not_before_ms`
/// are eligible. If none qualify, returns `None` — the caller retries.
/// Never falls back to an older row in the same directory.
pub fn select_id_for_directory<'a>(
    sessions: &'a [ListedSession],
    spawn_directory: &str,
    created_not_before_ms: i64,
) -> Option<&'a str> {
    sessions
        .iter()
        .filter(|s| is_opencode_session_id(&s.id))
        .filter(|s| crate::env::directories_match(&s.directory, spawn_directory))
        .filter(|s| s.created >= created_not_before_ms)
        .max_by_key(|s| (s.created, s.updated))
        .map(|s| s.id.as_str())
}

/// OpenCode session IDs start with `ses_` (schema `SessionID`).
/// `pub(crate)` so the transcript reader (`services::transcript_reader`) can
/// share the same gate without duplicating the prefix check — both modules
/// live inside the private `services` tree, so crate-private visibility is
/// the right seam for a sibling call (the two readers, transcript + session-id
/// capture poller, must agree on what an OpenCode session id looks like).
pub(crate) fn is_opencode_session_id(id: &str) -> bool {
    id.starts_with("ses_") && id.len() > 4
}

#[allow(dead_code)]
/// Wall-clock ms subtracted from spawn time so a TUI that minted the row
/// a few ms before we sampled still matches. Must stay small: a large
/// window would admit a session the user closed seconds earlier in the
/// same directory.
pub const CAPTURE_SKEW_MS: i64 = 2_000;

const RETRY_DELAYS_MS: &[u64] = &[400, 800, 1_600, 2_500, 4_000];

/// Resolve the on-disk SQLite path OpenCode uses for its session + message
/// store. Mirrors the env handling in `services::usage` (which opens the same
/// DB for the billing rollup); on WSL the Linux-side path is converted to the
/// Windows-side UNC form so a Rust reader can `Connection::open` it directly.
/// `pub(crate)` so the transcript reader (`services::transcript_reader`) can
/// resolve the same DB without duplicating the env↔host mapping.
pub(crate) fn opencode_db_path(env_type: EnvType) -> Option<PathBuf> {
    match env_type {
        EnvType::Wsl => {
            let user = std::env::var("USERNAME")
                .ok()
                .or_else(|| std::env::var("USER").ok())?;
            let linux = format!("/home/{user}/.local/share/opencode/opencode.db");
            Some(PathBuf::from(crate::env::to_host_path(&linux)))
        }
        EnvType::Windows => {
            let home = std::env::var("USERPROFILE")
                .ok()
                .or_else(|| std::env::var("HOME").ok())?;
            Some(
                PathBuf::from(home)
                    .join(".local")
                    .join("share")
                    .join("opencode")
                    .join("opencode.db"),
            )
        }
    }
}

/// List root (non-child) sessions created at or after `created_not_before_ms`.
/// Directory matching stays in Rust so slash/case rules can apply.
pub fn list_recent_root_sessions(
    conn: &Connection,
    created_not_before_ms: i64,
) -> Result<Vec<ListedSession>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, directory, time_created, time_updated \
             FROM session \
             WHERE parent_id IS NULL \
               AND time_archived IS NULL \
               AND time_created >= ?1 \
             ORDER BY time_created DESC, time_updated DESC \
             LIMIT 50",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([created_not_before_ms], |row| {
            Ok(ListedSession {
                id: row.get(0)?,
                directory: row.get(1)?,
                created: row.get(2)?,
                updated: row.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// Historic recovery must constrain time before applying any result limit;
/// the 50 most recent sessions across all projects can hide the wanted row.
pub(crate) fn list_root_sessions_in_window(
    conn: &Connection,
    not_before: i64,
    not_after: i64,
) -> Result<Vec<ListedSession>, String> {
    let mut stmt = conn.prepare("SELECT id, directory, time_created, time_updated
        FROM session WHERE parent_id IS NULL AND time_archived IS NULL
        AND time_created BETWEEN ?1 AND ?2").map_err(|e| e.to_string())?;
    let rows = stmt.query_map([not_before, not_after], |row| Ok(ListedSession {
        id: row.get(0)?, directory: row.get(1)?, created: row.get(2)?, updated: row.get(3)?,
    })).map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// Historic startup recovery entry point used by the OpenCode adapter. The
/// bounded SQL window is applied before any result selection, so old sessions
/// in other projects cannot hide a matching row behind a global LIMIT.
pub(crate) fn find_historic_id_for_directory(
    env_type: EnvType,
    spawn_directory: &str,
    anchor_ms: i64,
    recorded_start: bool,
) -> Option<String> {
    let db_path = opencode_db_path(env_type)?;
    find_historic_id_for_db_path(
        &db_path,
        spawn_directory,
        anchor_ms,
        recorded_start,
    )
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
    let candidates = list_root_sessions_in_window(&conn, cutoff, not_after)
        .ok()?
        .into_iter()
        .filter(|candidate| {
            is_opencode_session_id(&candidate.id)
                && crate::env::directories_match(&candidate.directory, spawn_directory)
        });
    crate::services::session_recovery::select_recovery_identity(
        candidates.map(|candidate| (candidate.id, candidate.created)),
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
    let sessions = list_recent_root_sessions(&conn, created_not_before_ms).ok()?;
    select_id_for_directory(&sessions, spawn_directory, created_not_before_ms).map(str::to_string)
}

/// Background poller (fallback path, issue #1294): read OpenCode's local
/// SQLite until a session created in this spawn's time window appears,
/// then write `cli_session_id`. Cancels if the node is no longer in the
/// process registry (killed / crashed before the TUI flushed).
///
/// **The primary capture path is the project's plugin** (see
/// `provider::opencode` and `opencode_attention_plugin.js`): the
/// plugin's `session.created` event reaches the attention route's
/// `Decision::Ignore` arm and persists the `ses_<…>` id via
/// `set_cli_session_id_if_missing`. This poller is the fallback when
/// the plugin is missing or blocked — it matches the spawn
/// `directory` against the opencode.db session table (no node_id
/// binding, so two Root Nodes in one mesh root are
/// indistinguishable — but the plugin has already disambiguated by then).
///
/// Production reproduction from issue #1294's investigation comment:
/// the SQLite row landed an hour after spawn (the TUI minted the id
/// on first interactive prompt, not at boot), so a 9.3s retry window
/// gives up too early for the typical cold-spawn case. The plugin
/// closes that gap; this poller remains as a safety net for hosts where
/// the plugin is absent.
pub fn start_capture_poller(node_id: i64, spawn_directory: String, env_type: EnvType) {
    let spawn_epoch_ms = chrono::Utc::now().timestamp_millis();
    tauri::async_runtime::spawn(async move {
        let not_before = spawn_epoch_ms.saturating_sub(CAPTURE_SKEW_MS);
        let Some(db_path) = opencode_db_path(env_type) else {
            tracing::warn!("opencode session capture: no db path for env {env_type:?}");
            return;
        };
        for (i, delay) in RETRY_DELAYS_MS.iter().enumerate() {
            tokio::time::sleep(Duration::from_millis(*delay)).await;
            if !crate::agent::process::PROCESS_REGISTRY.contains(&node_id) {
                tracing::debug!("opencode session capture: node {node_id} gone, stop");
                return;
            }
            let path = db_path.clone();
            let dir = spawn_directory.clone();
            // Read the provider DB and conditionally persist the captured ID
            // in one blocking task. A separate DB hop after every scan adds
            // needless pool churn and leaves a race between the two steps.
            let captured = crate::blocking::run_blocking("opencode_capture", move || {
                let Some(id) = try_capture_from_db_path(&path, &dir, not_before) else {
                    return Ok(None);
                };
                crate::db::update_cli_session_id(node_id, &id)
                    .map(|()| Some(id))
                    .map_err(|error| error.to_string())
            })
            .await;
            let captured = match captured {
                Ok(captured) => captured,
                Err(error) => {
                    tracing::warn!("opencode session capture: blocking task failed for node {node_id}: {error}");
                    return;
                }
            };
            if let Some(id) = captured {
                if !crate::agent::process::PROCESS_REGISTRY.contains(&node_id) {
                    return;
                }
                tracing::info!(
                    "opencode session capture: stored {id} for node {node_id} (attempt {})",
                    i + 1
                );
                return;
            }
        }
        tracing::warn!(
            "opencode session capture: gave up for node {node_id} in {spawn_directory}"
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn sess(id: &str, dir: &str, created: i64) -> ListedSession {
        ListedSession {
            id: id.into(),
            directory: dir.into(),
            created,
            updated: created,
        }
    }

    #[test]
    fn select_matches_windows_directory_slash_and_case() {
        let sessions = vec![sess(
            "ses_fc52ccfb9ffek1jl23ZwpRuSP7",
            r"F:\src\buildmesh\.claude\worktrees\high-crisp-buttercup",
            100,
        )];
        let id = select_id_for_directory(
            &sessions,
            r"f:/src/buildmesh/.claude/worktrees/high-crisp-buttercup",
            50,
        );
        assert_eq!(id, Some("ses_fc52ccfb9ffek1jl23ZwpRuSP7"));
    }

    #[test]
    fn select_matches_unc_wsl_path() {
        let sessions = vec![sess(
            "ses_uncpathid000000000000001",
            r"\\wsl$\Ubuntu\home\adam\src",
            100,
        )];
        assert_eq!(
            select_id_for_directory(&sessions, "//wsl$/Ubuntu/home/adam/src", 50),
            Some("ses_uncpathid000000000000001")
        );
    }

    #[test]
    fn select_matches_wsl_mnt_drive_case() {
        let sessions = vec![sess(
            "ses_mntpathid000000000000001",
            "/mnt/f/src/buildmesh",
            100,
        )];
        assert_eq!(
            select_id_for_directory(&sessions, "/mnt/F/src/buildmesh", 50),
            Some("ses_mntpathid000000000000001")
        );
    }

    #[test]
    fn select_prefers_newest_in_time_window_for_same_directory() {
        let sessions = vec![
            sess("ses_oldersessionid0000000001", "/tmp/worktree", 100),
            sess("ses_newersessionid0000000002", "/tmp/worktree", 200),
        ];
        assert_eq!(
            select_id_for_directory(&sessions, "/tmp/worktree", 150),
            Some("ses_newersessionid0000000002")
        );
    }

    #[test]
    fn select_does_not_fall_back_to_historical_row_outside_window() {
        let sessions = vec![sess(
            "ses_fc256eca5ffeGxygWliQQsJGNP",
            r"F:\src\buildmesh",
            100,
        )];
        assert_eq!(
            select_id_for_directory(&sessions, r"F:\src\buildmesh", 9_000_000_000_000),
            None,
            "empty time window must not bind a brand-new node to last week's session"
        );
    }

    #[test]
    fn select_ignores_other_directories() {
        let sessions = vec![sess(
            "ses_fc52ccfb9ffek1jl23ZwpRuSP7",
            r"F:\src\buildmesh",
            100,
        )];
        assert_eq!(
            select_id_for_directory(&sessions, r"F:\src\other-repo", 0),
            None
        );
    }

    #[test]
    fn select_rejects_non_ses_ids() {
        let sessions = vec![sess(
            "01a024d2-7cd6-7ea2-b907-531b0d261be7",
            "/tmp/worktree",
            1,
        )];
        assert_eq!(
            select_id_for_directory(&sessions, "/tmp/worktree", 0),
            None
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn linux_home_directories_are_case_sensitive() {
        let sessions = vec![sess("ses_unixpathid00000000000001", "/home/Adam/src", 1)];
        assert_eq!(
            select_id_for_directory(&sessions, "/home/adam/src", 0),
            None
        );
        assert_eq!(
            select_id_for_directory(&sessions, "/home/Adam/src/", 0),
            Some("ses_unixpathid00000000000001")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_home_directories_are_case_insensitive() {
        let sessions = vec![sess("ses_unixpathid00000000000001", "/Users/Adam/src", 1)];
        assert_eq!(
            select_id_for_directory(&sessions, "/users/adam/src", 0),
            Some("ses_unixpathid00000000000001")
        );
    }

    fn memory_session_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE session (
                id text PRIMARY KEY,
                directory text NOT NULL,
                time_created integer NOT NULL,
                time_updated integer NOT NULL,
                time_archived integer,
                parent_id text
            );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn sqlite_list_skips_archived_and_child_and_old_rows() {
        let conn = memory_session_db();
        conn.execute(
            "INSERT INTO session (id, directory, time_created, time_updated, time_archived, parent_id)
             VALUES
               ('ses_currentroot0000000000001', '/tmp/wt', 200, 200, NULL, NULL),
               ('ses_historical0000000000001', '/tmp/wt', 50, 50, NULL, NULL),
               ('ses_archived000000000000001', '/tmp/wt', 250, 250, 251, NULL),
               ('ses_childsess00000000000001', '/tmp/wt', 260, 260, NULL, 'ses_currentroot0000000000001')",
            [],
        )
        .unwrap();
        let listed = list_recent_root_sessions(&conn, 100).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "ses_currentroot0000000000001");
        assert_eq!(
            select_id_for_directory(&listed, "/tmp/wt", 100),
            Some("ses_currentroot0000000000001")
        );
    }

    #[test]
    fn sqlite_list_empty_when_nothing_in_window() {
        let conn = memory_session_db();
        conn.execute(
            "INSERT INTO session (id, directory, time_created, time_updated, time_archived, parent_id)
             VALUES ('ses_oldonly0000000000000001', '/tmp/wt', 10, 10, NULL, NULL)",
            params![],
        )
        .unwrap();
        let listed = list_recent_root_sessions(&conn, 100).unwrap();
        assert!(listed.is_empty());
    }

    #[test]
    fn historic_recovery_finds_a_delayed_session_before_other_projects_fill_the_window() {
        const CREATED: i64 = 1_787_830_399_000;
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("opencode.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (
                id TEXT,
                directory TEXT,
                time_created INTEGER,
                time_updated INTEGER,
                parent_id TEXT,
                time_archived INTEGER
            );",
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
                params![format!("ses_other{i}"), CREATED + 600_000 + i],
            )
            .unwrap();
        }
        drop(conn);

        assert_eq!(
            find_historic_id_for_db_path(&path, "F:/repo", CREATED, true),
            Some("ses_wanted".to_string())
        );
    }
}
