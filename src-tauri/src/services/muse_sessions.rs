//! Index-independent discovery of Muse Code session artifacts.
//!
//! Muse publishes `session-index.db` asynchronously, and a session that is
//! still live is **absent** from it: the index gains a session's row when a
//! later Muse process starts (or the session closes), not while it runs.
//! Observed on 2026-09-21 — session `01a0c54b-…`, created 18:47, had no index
//! row an hour later while its
//! `sessions/2026/09/21/01a0c54b-…/session.jsonl` carried a full
//! `runtime.session.metadata` frame.
//!
//! Identity capture (`agent::provider::adapters::muse::find_session`), the turn
//! watcher ([`crate::services::muse_watcher`]) and the transcript reader
//! (`services::transcript_reader::adapters::muse`) all resolved only through
//! that index, so a live Muse node stayed invisible for its whole run. The
//! circuit observer then saw no session identity and no report, and the #1791
//! fast fail ended every wait on it after 15 minutes with *"agent produced no
//! session identity or report within 15 minutes"* (circuit run 183).
//!
//! The per-session log on disk carries the same identity the index would, so
//! each lookup keeps the index as its fast path and falls back to a bounded
//! scan of the `sessions/YYYY/MM/DD/<uuid>/` tree. The JSONL metadata frame is
//! still the authority for the workspace and timestamp — the index row can
//! carry NULL workspace/time columns, and the fresh-session row is missing
//! entirely.

use std::path::{Path, PathBuf};

use crate::env;

/// Muse's data root, relative to the runtime home.
const MUSE_HOME: &str = ".local/share/muse";
const INDEX_DB: &str = "session-index.db";
const SESSIONS_DIR: &str = "sessions";
const SESSION_LOG: &str = "session.jsonl";
/// Bounded so a Coordinator digest poll cannot park a Tokio worker thread
/// indefinitely if the live harness is writing a new line.
const INDEX_BUSY_TIMEOUT_MS: u64 = 200;
/// Upper bound on the day directories the by-id scan visits. A Muse host keeps
/// one directory per active day, so this is far above normal use and only
/// guards a pathological tree.
const MAX_DAY_DIRS: usize = 512;

/// Resolve Muse's data root for a node's spawn path, env-aware (native,
/// Windows+WSL, or Windows interoperability).
pub(crate) fn data_root(spawn_path: &str) -> Option<PathBuf> {
    let native = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)?
        .join(MUSE_HOME);
    env::cli_dir_for_spawn(native, MUSE_HOME, spawn_path)
}

/// Resolve a session's durable log path: the `session-index.db` row when it
/// exists, otherwise a bounded scan of the on-disk session tree. This is the
/// entry point for callers that already know the session id.
pub(crate) fn log_path(root: &Path, session_id: &str) -> Option<PathBuf> {
    if let Some(raw) = index_log_path(root, session_id) {
        return Some(PathBuf::from(env::to_host_path(&raw)));
    }
    disk_log_path(root, session_id)
}

/// Every `(session_id, recorded_ms)` candidate for `workspace`, unioned from
/// the index and the on-disk tree so an anchor can disambiguate. Returning the
/// union (rather than index-first with a disk fallback only when empty) matters
/// when the index still holds a previous session for the same worktree.
pub(crate) fn workspace_candidates(
    root: &Path,
    workspace: &str,
    anchor_ms: i64,
) -> Vec<(String, i64)> {
    let mut candidates = Vec::new();
    for (id, raw) in indexed_sessions(root) {
        let path = PathBuf::from(env::to_host_path(&raw));
        if let Some((recorded_id, cwd, recorded_ms)) = session_metadata(&path) {
            if id == recorded_id && env::directories_match(&cwd, workspace) {
                candidates.push((id, recorded_ms));
            }
        }
    }
    candidates.extend(disk_workspace_candidates(root, workspace, anchor_ms));
    candidates
}

/// Read the identity frame from a session log. Muse's index can leave
/// workspace/timestamp columns NULL, so the JSONL metadata is the authority;
/// only the session metadata frame is read, never transcript text.
pub(crate) fn session_metadata(path: &Path) -> Option<(String, String, i64)> {
    use std::io::{BufRead, Read};
    let file = std::fs::File::open(path).ok()?;
    for line in std::io::BufReader::new(file.take(262_144)).lines().take(64) {
        let line = line.ok()?;
        let frame: serde_json::Value = serde_json::from_str(&line).ok()?;
        if let Some(metadata) = metadata_record(&frame) {
            return Some(metadata);
        }
        let Some(children) = frame.get("children").and_then(|c| c.as_array()) else {
            continue;
        };
        for child in children {
            let Some(json) = child.get("record_json").and_then(|r| r.as_str()) else {
                continue;
            };
            let Ok(record) = serde_json::from_str::<serde_json::Value>(json) else {
                continue;
            };
            if let Some(metadata) = metadata_record(&record) {
                return Some(metadata);
            }
        }
    }
    None
}

fn metadata_record(record: &serde_json::Value) -> Option<(String, String, i64)> {
    if record.get("payload_type").and_then(|p| p.as_str()) != Some("runtime.session.metadata")
        || record.pointer("/stream/kind").and_then(|s| s.as_str()) != Some("session")
    {
        return None;
    }
    let id = record.pointer("/stream/id")?.as_str()?;
    uuid::Uuid::parse_str(id).ok()?;
    let cwd = record.pointer("/payload/record/workspace_root")?.as_str()?;
    let timestamp = record.get("recorded_at")?.as_i64()?;
    Some((id.into(), cwd.into(), timestamp / 1000))
}

/// `(session_id, raw log path)` for the indexed sessions, newest path first.
/// The connection is dropped before the caller reads any file (no filesystem
/// work while a DB connection is held).
fn indexed_sessions(root: &Path) -> Vec<(String, String)> {
    let Ok(connection) = rusqlite::Connection::open_with_flags(
        root.join(INDEX_DB),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) else {
        return Vec::new();
    };
    let _ = connection.busy_timeout(std::time::Duration::from_millis(INDEX_BUSY_TIMEOUT_MS));
    let rows = connection
        .prepare(
            "SELECT session_id, session_log_path FROM sessions \
             ORDER BY session_log_path DESC LIMIT 128",
        )
        .and_then(|mut statement| {
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .filter_map(Result::ok)
                .collect::<Vec<_>>();
            Ok(rows)
        })
        .unwrap_or_default();
    rows
}

/// The raw (guest-side) log path for one session id, or `None` when the index
/// is missing, the id is unknown, or the row's path column is NULL.
fn index_log_path(root: &Path, session_id: &str) -> Option<String> {
    let connection = rusqlite::Connection::open_with_flags(
        root.join(INDEX_DB),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    let _ = connection.busy_timeout(std::time::Duration::from_millis(INDEX_BUSY_TIMEOUT_MS));
    connection
        .query_row(
            "SELECT session_log_path FROM sessions WHERE session_id = ?1 LIMIT 1",
            [session_id],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .filter(|path| !path.is_empty())
}

fn disk_log_path(root: &Path, session_id: &str) -> Option<PathBuf> {
    day_dirs(root).into_iter().find_map(|day| {
        let log = day.join(session_id).join(SESSION_LOG);
        log.is_file().then_some(log)
    })
}

fn disk_workspace_candidates(root: &Path, workspace: &str, anchor_ms: i64) -> Vec<(String, i64)> {
    let mut candidates = Vec::new();
    for day in day_dirs_near(root, anchor_ms) {
        for session_dir in subdirs(&day) {
            let log = session_dir.join(SESSION_LOG);
            if let Some((id, cwd, recorded_ms)) = session_metadata(&log) {
                if env::directories_match(&cwd, workspace) {
                    candidates.push((id, recorded_ms));
                }
            }
        }
    }
    candidates
}

/// Day directories, newest first. Path ordering is chronological because Muse
/// zero-pads `YYYY/MM/DD`.
fn day_dirs(root: &Path) -> Vec<PathBuf> {
    let mut days = Vec::new();
    for year in subdirs(&root.join(SESSIONS_DIR)) {
        for month in subdirs(&year) {
            for day in subdirs(&month) {
                days.push(day);
            }
        }
    }
    days.sort();
    days.reverse();
    days.truncate(MAX_DAY_DIRS);
    days
}

/// The anchor date's day directory and its neighbours. Muse names day
/// directories in UTC; the neighbours absorb a host-timezone disagreement
/// without widening the scan.
fn day_dirs_near(root: &Path, anchor_ms: i64) -> Vec<PathBuf> {
    let Some(anchor) = chrono::DateTime::from_timestamp_millis(anchor_ms) else {
        return Vec::new();
    };
    let sessions = root.join(SESSIONS_DIR);
    (-1i64..=1)
        .filter_map(|offset| {
            let date = anchor.checked_add_signed(chrono::Duration::days(offset))?;
            Some(
                sessions
                    .join(date.format("%Y").to_string())
                    .join(date.format("%m").to_string())
                    .join(date.format("%d").to_string()),
            )
        })
        .filter(|dir| dir.is_dir())
        .collect()
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "01a0c54b-5ed4-7a61-91d7-a7a72c42fe24";
    /// Node 4308's launch anchor (2026-09-21T18:47:21Z) and the session
    /// metadata frame it wrote 8 s later, from circuit run 183.
    const ANCHOR_MS: i64 = 1_790_016_441_436;
    const RECORDED_MS: i64 = 1_790_016_449_082;

    /// Write Muse's on-disk session log with a real metadata frame, the shape
    /// observed for a live session that had no index row (run 183).
    fn write_session(root: &Path, date: [&str; 3], id: &str, workspace: &str, recorded_ms: i64) {
        let dir = root
            .join(SESSIONS_DIR)
            .join(date[0])
            .join(date[1])
            .join(date[2])
            .join(id);
        std::fs::create_dir_all(&dir).unwrap();
        // The metadata frame follows an initial retained-frame envelope, as on
        // disk; a reader that only inspects the first line would miss it.
        let envelope = serde_json::json!({
            "retained_frame": "session_permission_transaction",
            "children": [{"record_json": "{\"payload_type\":\"runtime.session.permission_format_declared\"}"}],
        });
        let metadata = serde_json::json!({
            "schema_version": 1,
            "stream": {"kind": "session", "id": id},
            "recorded_at": recorded_ms * 1000,
            "payload_type": "runtime.session.metadata",
            "payload": {"kind": "metadata", "record": {"workspace_root": workspace}},
        });
        std::fs::write(
            dir.join(SESSION_LOG),
            format!("{envelope}\n{metadata}\n"),
        )
        .unwrap();
    }

    #[test]
    fn workspace_candidates_find_a_live_session_without_an_index() {
        let root = tempfile::tempdir().unwrap();
        write_session(
            root.path(),
            ["2026", "09", "21"],
            SESSION,
            "F:\\src\\buildmesh\\.claude\\worktrees\\gh1816",
            RECORDED_MS,
        );
        let candidates = workspace_candidates(
            root.path(),
            "F:/src/buildmesh/.claude/worktrees/gh1816",
            ANCHOR_MS,
        );
        assert_eq!(candidates, vec![(SESSION.to_string(), RECORDED_MS)]);
    }

    #[test]
    fn workspace_candidates_union_the_index_and_the_tree() {
        let root = tempfile::tempdir().unwrap();
        let connection = rusqlite::Connection::open(root.path().join(INDEX_DB)).unwrap();
        connection
            .execute_batch("CREATE TABLE sessions (session_id TEXT, session_log_path TEXT);")
            .unwrap();
        // Indexed, older session in the same worktree.
        let older = root.path().join("older.jsonl");
        std::fs::write(
            &older,
            format!(
                "{}\n",
                serde_json::json!({
                    "stream": {"kind": "session", "id": "01a0c000-0000-7000-8000-000000000000"},
                    "recorded_at": 1_000_000i64,
                    "payload_type": "runtime.session.metadata",
                    "payload": {"record": {"workspace_root": "F:\\src\\buildmesh\\.claude\\worktrees\\gh1816"}},
                })
            ),
        )
        .unwrap();
        connection
            .execute(
                "INSERT INTO sessions VALUES (?1, ?2)",
                rusqlite::params!["01a0c000-0000-7000-8000-000000000000", older.to_str().unwrap()],
            )
            .unwrap();
        drop(connection);
        write_session(
            root.path(),
            ["2026", "09", "21"],
            SESSION,
            "F:\\src\\buildmesh\\.claude\\worktrees\\gh1816",
            RECORDED_MS,
        );

        let mut ids: Vec<String> = workspace_candidates(
            root.path(),
            "F:/src/buildmesh/.claude/worktrees/gh1816",
            ANCHOR_MS,
        )
        .into_iter()
        .map(|(id, _)| id)
        .collect();
        ids.sort();
        assert_eq!(
            ids,
            vec![
                "01a0c000-0000-7000-8000-000000000000".to_string(),
                SESSION.to_string()
            ],
            "the disk session must surface even when the index still holds an older one"
        );
    }

    #[test]
    fn log_path_prefers_the_index_row_then_falls_back_to_the_tree() {
        let root = tempfile::tempdir().unwrap();
        write_session(
            root.path(),
            ["2026", "09", "21"],
            SESSION,
            "/workspace",
            10_000,
        );
        // No index at all → the tree resolves.
        assert!(log_path(root.path(), SESSION)
            .is_some_and(|path| path.ends_with(SESSION_LOG)));

        // With an index row, the row wins even when the tree also matches.
        let indexed = root.path().join("indexed-elsewhere.jsonl");
        std::fs::write(&indexed, "{}\n").unwrap();
        let connection = rusqlite::Connection::open(root.path().join(INDEX_DB)).unwrap();
        connection
            .execute_batch("CREATE TABLE sessions (session_id TEXT, session_log_path TEXT);")
            .unwrap();
        connection
            .execute(
                "INSERT INTO sessions VALUES (?1, ?2)",
                rusqlite::params![SESSION, indexed.to_str().unwrap()],
            )
            .unwrap();
        drop(connection);
        assert_eq!(log_path(root.path(), SESSION), Some(indexed));

        assert!(log_path(root.path(), "unknown-session").is_none());
    }

    #[test]
    fn metadata_frame_is_read_through_its_retained_envelope() {
        let root = tempfile::tempdir().unwrap();
        write_session(
            root.path(),
            ["2026", "09", "21"],
            SESSION,
            "F:\\src\\buildmesh\\.claude\\worktrees\\gh1816",
            12_345,
        );
        let log = root
            .path()
            .join(SESSIONS_DIR)
            .join("2026/09/21")
            .join(SESSION)
            .join(SESSION_LOG);
        assert_eq!(
            session_metadata(&log),
            Some((
                SESSION.to_string(),
                "F:\\src\\buildmesh\\.claude\\worktrees\\gh1816".to_string(),
                12_345
            ))
        );
    }
}
