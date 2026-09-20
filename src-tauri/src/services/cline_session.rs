//! Cline session-ID capture from the local SQLite store.
//!
//! Cline 3.x self-assigns session IDs of the form `<epochms>_<5 chars>` (for
//! example `1789757012702_7of3e`, captured by the issue #1769 research
//! against a locally installed `cline 3.0.62`). The TUI never prints that
//! ID live — it only surfaces it inside the end-of-run summary, so the PTY
//! UUID regex in `session_capture` cannot bind a fresh spawn. Capture runs
//! from [`Cline::after_fresh_spawn`] against the authoritative store at
//! `<cline home>/data/db/sessions.db` (issue #1769) instead.
//!
//! ## Why SQLite and not the on-disk `~/.cline/data/sessions/<id>/` tree?
//!
//! The on-disk tree is only written after the first user message lands, and
//! `cwd` is not part of the directory name. The SQLite `sessions` row is
//! written when the TUI starts and carries the exact `cwd` plus an
//! `interactive` flag, which gives us both the freshness gate (row creation
//! timestamp) and the directory match (the `cwd` column) without ever
//! walking the tree. The `~/.cline/data/sessions/<id>/` fallback only
//! applies in `find_historic_id_for_directory` when SQLite is missing or
//! corrupt (the issue #1769 research confirmed both paths surface the same
//! IDs).
//!
//! ## Fresh-spawn capture
//!
//! [`start_capture_poller`] runs a short retry loop over the SQLite store
//! until either a row created at or after `spawn_epoch_ms -
//! CAPTURE_SKEW_MS` appears for the spawn directory, or the retry budget
//! is exhausted. A captured ID is written via
//! `db::set_cli_session_id_if_missing` so a later (more authoritative)
//! capture cannot be clobbered.
//!
//! ## Historic recovery
//!
//! [`find_historic_id_for_directory`] reads the same SQLite store without
//! a spawn-anchor window when called by the suspended-session sweep
//! (`services::session_recovery` → `db::recover_suspended_cli_session_id`).
//! The session_recovery helper applies its own clock-skew and
//! spawn-window rules — this function only owns the SQLite read and the
//! session-id validity check.
//!
//! ## Session-ID shape
//!
//! `is_cline_session_id` pins the exact format `<13+ digits>_<5 chars from
//! [0-9a-z]>`. Anything else (a partial write, a temp file) is rejected so
//! the DB predicate stays tight.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::models::EnvType;

/// Wall-clock ms subtracted from spawn time so a TUI that minted the row
/// a few ms before we sampled still matches. Must stay small: a large
/// window would admit a session the user closed seconds earlier in the
/// same directory.
pub const CAPTURE_SKEW_MS: i64 = 2_000;

/// Retry delays for the fresh-spawn capture loop. Total budget ≈ 9.3s —
/// long enough to absorb a slow SQLite flush on a cold start, short
/// enough that a failed capture does not delay the user. Cline's TUI
/// persists the `sessions` row in the first few hundred ms of boot per
/// the issue #1769 evidence, so the early retries are the ones that win.
pub const RETRY_DELAYS_MS: &[u64] = &[400, 800, 1_600, 2_500, 4_000];

/// One Cline session row, in the shape the SQLite `sessions` table
/// exposes (issue #1769). The fields we read are exactly what the
/// capture and recovery helpers need; the rest of the row is left to
/// the Cline process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedSession {
    pub session_id: String,
    pub cwd: String,
    pub created_ms: i64,
    pub interactive: bool,
}

/// Validate a Cline session id (issue #1769 observed shape). Cline
/// mints `<epochms>_<5 chars>` where the suffix is `[0-9a-z]{5}` — no
/// upper case, no separator. The leading epoch is at least 13 digits
/// (year 2026+) so a 10-digit legacy epoch or a 0-prefixed placeholder
/// never binds a node.
pub fn is_cline_session_id(id: &str) -> bool {
    let Some((epoch, suffix)) = id.split_once('_') else {
        return false;
    };
    if epoch.len() < 13 || !epoch.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if suffix.len() != 5 || !suffix.chars().all(|c| c.is_ascii_digit() || c.is_ascii_lowercase()) {
        return false;
    }
    epoch.parse::<i64>().is_ok()
}

/// Pick the session ID to store for a freshly spawned node. Mirrors
/// [`crate::services::opencode_session::select_id_for_directory`] —
/// only rows that (a) match `spawn_directory`, (b) carry a valid
/// `<epochms>_<suffix>` id, (c) were created at or after the not-before
/// timestamp, and (d) are tagged interactive are eligible. The
/// `interactive = 1` filter is what excludes one-shot prompt runs (no
/// `-i`, positional prompt — they exit immediately per issue #1769).
/// Returns the newest match (created, then updated tiebreaker).
pub fn select_id_for_directory<'a>(
    sessions: &'a [ListedSession],
    spawn_directory: &str,
    created_not_before_ms: i64,
) -> Option<&'a str> {
    sessions
        .iter()
        .filter(|s| is_cline_session_id(&s.session_id))
        .filter(|s| s.interactive)
        .filter(|s| crate::env::directories_match(&s.cwd, spawn_directory))
        .filter(|s| s.created_ms >= created_not_before_ms)
        .max_by_key(|s| s.created_ms)
        .map(|s| s.session_id.as_str())
}

/// List interactive sessions created at or after `created_not_before_ms`.
/// Directory matching stays in Rust so slash/case rules can apply
/// uniformly across Windows, WSL, and macOS/Linux spawn paths. The
/// `LIMIT 50` is a safety net for a corrupted store that mints a row
/// per millisecond — the `time_created DESC` ordering and the
/// `created_ms` filter above still pick the right one.
pub fn list_recent_interactive_sessions(
    conn: &Connection,
    created_not_before_ms: i64,
) -> Result<Vec<ListedSession>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT session_id, cwd, time_created, interactive \
             FROM sessions \
             WHERE interactive = 1 \
               AND time_created >= ?1 \
             ORDER BY time_created DESC \
             LIMIT 50",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([created_not_before_ms], |row| {
            let interactive: i64 = row.get(3)?;
            Ok(ListedSession {
                session_id: row.get(0)?,
                cwd: row.get(1)?,
                created_ms: row.get(2)?,
                interactive: interactive != 0,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// Bounded read for historic recovery: list interactive sessions whose
/// `time_created` is inside the issue #1224 / issue #1769 spawn window.
/// Same predicate as the fresh poller minus the `not_before` clamp —
/// the recovery caller owns the window and passes the bounds in.
pub fn list_sessions_in_window(
    conn: &Connection,
    not_before: i64,
    not_after: i64,
) -> Result<Vec<ListedSession>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT session_id, cwd, time_created, interactive \
             FROM sessions \
             WHERE interactive = 1 \
               AND time_created BETWEEN ?1 AND ?2 \
             ORDER BY time_created DESC, session_id DESC \
             LIMIT 100",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([not_before, not_after], |row| {
            let interactive: i64 = row.get(3)?;
            Ok(ListedSession {
                session_id: row.get(0)?,
                cwd: row.get(1)?,
                created_ms: row.get(2)?,
                interactive: interactive != 0,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// Historic startup recovery entry point used by the Cline adapter.
/// Reads `<cline home>/data/db/sessions.db` (issue #1769) and selects the
/// best fit for `spawn_path` (also the directory we match against
/// `sessions.cwd`) inside the spawn window. Mirrors
/// [`crate::services::opencode_session::find_historic_id_for_directory`].
pub(crate) fn find_historic_id_for_directory(
    env_type: EnvType,
    spawn_path: &str,
    anchor_ms: i64,
    recorded_start: bool,
) -> Option<String> {
    let db_path = crate::env::cline_db_path_for_env(env_type, spawn_path)?;
    find_historic_id_for_db_path(
        &db_path,
        spawn_path,
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
    let conn =
        Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
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
            is_cline_session_id(&candidate.session_id)
                && crate::env::directories_match(&candidate.cwd, spawn_directory)
        });
    crate::services::session_recovery::select_recovery_identity(
        candidates.map(|candidate| (candidate.session_id, candidate.created_ms)),
        anchor_ms,
        recorded_start,
    )
}

/// Read the SQLite store once and pick a session id for the spawn
/// directory. Returns `None` when (a) the store is missing — Cline has
/// not yet flushed its first row, the caller should retry — or (b) no
/// matching row exists.
fn try_capture_from_db_path(
    db_path: &Path,
    spawn_directory: &str,
    created_not_before_ms: i64,
) -> Option<String> {
    if !db_path.exists() {
        return None;
    }
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let sessions = list_recent_interactive_sessions(&conn, created_not_before_ms).ok()?;
    select_id_for_directory(&sessions, spawn_directory, created_not_before_ms)
        .map(str::to_string)
}

/// Background poller (issue #1774): read Cline's local SQLite until a
/// session created in this spawn's time window appears, then write
/// `cli_session_id` via the "fill-only" predicate so a later capture
/// (e.g. one driven by the `~/.cline/data/sessions/<id>/` fallback in a
/// future patch) cannot clobber it. Cancels if the node is no longer in
/// the process registry (killed / crashed before the TUI flushed).
///
/// The primary capture path is this poller — Cline 3.x has no attention
/// hook surface that surfaces the session id (issue #1770 research is
/// still pending a follow-up), so the SQLite read is the only route
/// until a hook lands.
pub fn start_capture_poller(node_id: i64, spawn_directory: String, env_type: EnvType) {
    let spawn_epoch_ms = chrono::Utc::now().timestamp_millis();
    tauri::async_runtime::spawn(async move {
        let not_before = spawn_epoch_ms.saturating_sub(CAPTURE_SKEW_MS);
        let Some(db_path) = crate::env::cline_db_path_for_env(env_type, &spawn_directory) else {
            tracing::warn!("cline session capture: no db path for env {env_type:?}");
            return;
        };
        for (i, delay) in RETRY_DELAYS_MS.iter().enumerate() {
            tokio::time::sleep(Duration::from_millis(*delay)).await;
            if !crate::agent::process::PROCESS_REGISTRY.contains(&node_id) {
                tracing::debug!("cline session capture: node {node_id} gone, stop");
                return;
            }
            let path = db_path.clone();
            let dir = spawn_directory.clone();
            // Read the provider DB and conditionally persist the captured ID
            // in one blocking task. A separate DB hop after every scan adds
            // needless pool churn and leaves a race between the two steps.
            let captured = crate::blocking::run_blocking("cline_capture", move || {
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
                    tracing::warn!("cline session capture: blocking task failed for node {node_id}: {error}");
                    return;
                }
            };
            if let Some(id) = captured {
                if !crate::agent::process::PROCESS_REGISTRY.contains(&node_id) {
                    return;
                }
                tracing::info!(
                    "cline session capture: stored {id} for node {node_id} (attempt {})",
                    i + 1
                );
                return;
            }
        }
        tracing::warn!(
            "cline session capture: gave up for node {node_id} in {spawn_directory}"
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn sess(id: &str, dir: &str, created: i64, interactive: bool) -> ListedSession {
        ListedSession {
            session_id: id.into(),
            cwd: dir.into(),
            created_ms: created,
            interactive,
        }
    }

    // ── is_cline_session_id (issue #1769 observed shape) ──────────────

    #[test]
    fn accepts_canonical_epochms_suffix_form() {
        assert!(is_cline_session_id("1789757012702_7of3e"));
        assert!(is_cline_session_id("1789757012702_00000"));
        assert!(is_cline_session_id("1700000000000_abcde"));
    }

    #[test]
    fn rejects_non_cline_shapes() {
        // UUID (Anthropic / Codex) — different prefix, no underscore.
        assert!(!is_cline_session_id("550e8400e29b41d4a716446655440000"));
        // OpenCode `ses_…` form.
        assert!(!is_cline_session_id("ses_fc52ccfb9ffek1jl23ZwpRuSP7"));
        // CommandCode `sess_…` form.
        assert!(!is_cline_session_id("sess_abc123"));
        // Wrong suffix length.
        assert!(!is_cline_session_id("1789757012702_abc"));
        assert!(!is_cline_session_id("1789757012702_abcdef"));
        // Upper-case suffix not allowed (issue #1769 sample is lowercase).
        assert!(!is_cline_session_id("1789757012702_ABCDE"));
        // Non-digit epoch.
        assert!(!is_cline_session_id("17a9757012702_7of3e"));
        // Empty / partial.
        assert!(!is_cline_session_id(""));
        assert!(!is_cline_session_id("_7of3e"));
        assert!(!is_cline_session_id("1789757012702_"));
        // Legacy 10-digit epoch (must be ≥13 per issue #1769 minimum).
        assert!(!is_cline_session_id("1570123456_abcde"));
    }

    // ── select_id_for_directory ────────────────────────────────────────

    #[test]
    fn select_matches_windows_directory_slash_and_case() {
        let sessions = vec![sess(
            "1789757012702_7of3e",
            r"F:\src\buildmesh\.claude\worktrees\high-crisp-buttercup",
            100,
            true,
        )];
        let id = select_id_for_directory(
            &sessions,
            r"f:/src/buildmesh/.claude/worktrees/high-crisp-buttercup",
            50,
        );
        assert_eq!(id, Some("1789757012702_7of3e"));
    }

    #[test]
    fn select_matches_unc_wsl_path() {
        let sessions = vec![sess(
            "1789757012702_unc01",
            r"\\wsl$\Ubuntu\home\adam\src",
            100,
            true,
        )];
        assert_eq!(
            select_id_for_directory(&sessions, "//wsl$/Ubuntu/home/adam/src", 50),
            Some("1789757012702_unc01")
        );
    }

    #[test]
    fn select_matches_wsl_mnt_drive_case() {
        let sessions = vec![sess(
            "1789757012702_mnt01",
            "/mnt/f/src/buildmesh",
            100,
            true,
        )];
        assert_eq!(
            select_id_for_directory(&sessions, "/mnt/F/src/buildmesh", 50),
            Some("1789757012702_mnt01")
        );
    }

    #[test]
    fn select_rejects_one_shot_runs() {
        // Cline without `-i` and a positional prompt is one-shot and exits;
        // its row has `interactive = 0` and must never bind a node.
        let sessions = vec![
            sess("1789757012702_inter", "/repo", 100, true),
            sess("1789757012702_one_sh", "/repo", 101, false),
        ];
        assert_eq!(
            select_id_for_directory(&sessions, "/repo", 50),
            Some("1789757012702_inter")
        );
    }

    #[test]
    fn select_rejects_malformed_ids() {
        let sessions = vec![
            sess("not-a-cline-id", "/repo", 100, true),
            sess("ses_abc", "/repo", 100, true),
            sess("550e8400e29b41d4a716446655440000", "/repo", 100, true),
        ];
        assert!(select_id_for_directory(&sessions, "/repo", 50).is_none());
    }

    #[test]
    fn select_prefers_newest_in_time_window_for_same_directory() {
        let sessions = vec![
            sess("1789757012702_old00", "/repo", 100, true),
            sess("1789757012702_newer", "/repo", 200, true),
            sess("1789757012702_newst", "/repo", 300, true),
        ];
        let id = select_id_for_directory(&sessions, "/repo", 50);
        assert_eq!(id, Some("1789757012702_newst"));
    }

    #[test]
    fn select_rejects_rows_outside_time_window() {
        // Row 150 is fresh enough but its created_ms < not_before. Without
        // this gate, a user opening the same directory twice would bind the
        // older session and resume it incorrectly.
        let sessions = vec![sess("1789757012702_oldie", "/repo", 150, true)];
        assert!(select_id_for_directory(&sessions, "/repo", 500).is_none());
    }

    #[test]
    fn select_returns_none_for_no_directory_match() {
        let sessions = vec![sess("1789757012702_match", "/other", 100, true)];
        assert!(select_id_for_directory(&sessions, "/repo", 50).is_none());
    }

    // ── list_recent_interactive_sessions (SQLite read) ────────────────

    fn open_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE sessions (
                session_id TEXT PRIMARY KEY,
                pid INTEGER,
                status TEXT,
                interactive INTEGER NOT NULL,
                cwd TEXT,
                time_created INTEGER NOT NULL,
                time_updated INTEGER
             )",
            [],
        )
        .unwrap();
        conn
    }

    fn insert_session(
        conn: &Connection,
        id: &str,
        cwd: &str,
        interactive: bool,
        created: i64,
    ) {
        conn.execute(
            "INSERT INTO sessions (session_id, pid, status, interactive, cwd, time_created, time_updated) \
             VALUES (?1, 1, 'running', ?2, ?3, ?4, ?4)",
            params![id, interactive as i64, cwd, created],
        )
        .unwrap();
    }

    #[test]
    fn list_recent_filters_out_non_interactive_sessions() {
        let conn = open_test_db();
        insert_session(&conn, "1789757012702_aaaaa", "/repo", true, 100);
        insert_session(&conn, "1789757012702_bbbbb", "/repo", false, 110);
        insert_session(&conn, "1789757012702_ccccc", "/other", true, 120);
        let sessions = list_recent_interactive_sessions(&conn, 0).unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.iter().all(|s| s.interactive));
        let ids: Vec<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
        assert!(ids.contains(&"1789757012702_aaaaa"));
        assert!(ids.contains(&"1789757012702_ccccc"));
        assert!(!ids.contains(&"1789757012702_bbbbb"));
    }

    #[test]
    fn list_recent_orders_newest_first() {
        let conn = open_test_db();
        insert_session(&conn, "1789757012702_older", "/repo", true, 100);
        insert_session(&conn, "1789757012702_newer", "/repo", true, 200);
        let sessions = list_recent_interactive_sessions(&conn, 0).unwrap();
        assert_eq!(sessions.first().unwrap().session_id, "1789757012702_newer");
        assert_eq!(sessions.last().unwrap().session_id, "1789757012702_older");
    }

    #[test]
    fn list_recent_honours_not_before_floor() {
        let conn = open_test_db();
        insert_session(&conn, "1789757012702_oldie", "/repo", true, 50);
        insert_session(&conn, "1789757012702_newer", "/repo", true, 200);
        let sessions = list_recent_interactive_sessions(&conn, 150).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "1789757012702_newer");
    }

    // ── find_historic_id_for_db_path ──────────────────────────────────

    fn write_test_db(dir: &Path, rows: &[(String, String, i64, bool)]) -> std::path::PathBuf {
        let db_path = dir.join("sessions.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE sessions (
                session_id TEXT PRIMARY KEY,
                pid INTEGER,
                status TEXT,
                interactive INTEGER NOT NULL,
                cwd TEXT,
                time_created INTEGER NOT NULL,
                time_updated INTEGER
             )",
            [],
        )
        .unwrap();
        for (id, cwd, created, interactive) in rows {
            conn.execute(
                "INSERT INTO sessions (session_id, pid, status, interactive, cwd, time_created, time_updated) \
                 VALUES (?1, 1, 'running', ?2, ?3, ?4, ?4)",
                params![id, i64::from(*interactive), cwd, created],
            )
            .unwrap();
        }
        db_path
    }

    #[test]
    fn historic_recovery_picks_newest_in_window_for_directory() {
        let temp = crate::env::test_helpers::TestDir::new("cline_session_historic");
        // One matching row inside the window — no ambiguity, recovery
        // must bind it. The 250 ms row is a one-shot and the `ses_…` row
        // is the wrong shape; both must be excluded. The oldie row sits
        // *before* the spawn-window cutoff so it does not count as a
        // candidate either, leaving `match` as the only viable id.
        let rows = vec![
            // Oldest: before the spawn window (cutoff = anchor - 2000 ms).
            // `select_recovery_identity` excludes it via the `>= cutoff`
            // filter when anchor = 1000, cutoff = -1000 — wait, that's
            // IN the window. Use an anchor that drops oldie entirely:
            // anchor = 100, cutoff = -1900; oldie@50 still in window.
            // Move oldie below cutoff by anchoring close enough that
            // its creation time sits outside the lower bound. With
            // CLOCK_SKEW_MS = 2_000 we need anchor - 2_000 > 50.
            ("1789757012702_oldie".into(), "/repo".into(), 50, true),
            ("1789757012702_match".into(), "/repo".into(), 2200, true),
            ("1789757012702_onesh".into(), "/repo".into(), 2300, false),
            ("ses_fc52ccfb9ffek1jl23ZwpRuSP7".into(), "/repo".into(), 2200, true),
        ];
        let db_path = write_test_db(temp.path(), &rows);
        // Anchor 2300 → cutoff = 300. oldie@50 is below the cutoff; only
        // the 2200 ms row survives. Anchor recorded_start=true so the
        // upper bound is 2300 + 300_000 = 302_300; the 2200 ms row sits
        // inside.
        let id = find_historic_id_for_db_path(&db_path, "/repo", 2300, true)
            .expect("historic recovery must pick the only matching interactive row");
        assert_eq!(id, "1789757012702_match");
    }

    #[test]
    fn historic_recovery_skips_one_shot_and_malformed_rows() {
        let temp = crate::env::test_helpers::TestDir::new("cline_session_filter");
        // Anchor 250, no recorded start. select_recovery_identity still
        // requires uniqueness — the 250 ms row is non-interactive so the
        // SQLite read already excludes it; only the 200 ms row remains.
        let rows = vec![
            ("1789757012702_match".into(), "/repo".into(), 200, true),
            ("1789757012702_onesh".into(), "/repo".into(), 250, false),
            ("ses_fc52ccfb9ffek1jl23ZwpRuSP7".into(), "/repo".into(), 200, true),
        ];
        let db_path = write_test_db(temp.path(), &rows);
        let id = find_historic_id_for_db_path(&db_path, "/repo", 250, false)
            .expect("must still pick the 200 ms interactive row");
        assert_eq!(id, "1789757012702_match");
    }

    #[test]
    fn historic_recovery_returns_none_for_missing_db() {
        let temp = crate::env::test_helpers::TestDir::new("cline_session_missing");
        let db_path = temp.path().join("does_not_exist.db");
        assert!(find_historic_id_for_db_path(&db_path, "/repo", 1000, true).is_none());
    }

    #[test]
    fn historic_recovery_returns_none_for_no_directory_match() {
        let temp = crate::env::test_helpers::TestDir::new("cline_session_other");
        let rows = vec![(
            "1789757012702_match".into(),
            "/repo".into(),
            1000,
            true,
        )];
        let db_path = write_test_db(temp.path(), &rows);
        assert!(find_historic_id_for_db_path(&db_path, "/elsewhere", 250, true).is_none());
    }

    #[test]
    fn historic_recovery_refuses_when_two_candidates_share_the_window() {
        // Two valid interactive rows in the same directory inside the
        // spawn window — `select_recovery_identity` must refuse to guess.
        // This is the deliberate fail-safe that prevents resuming the
        // wrong conversation when the user opened the same dir twice
        // (issue #1774 acceptance: no cross-binding).
        let temp = crate::env::test_helpers::TestDir::new("cline_session_ambiguous");
        let rows = vec![
            ("1789757012702_a0001".into(), "/repo".into(), 900, true),
            ("1789757012702_a0002".into(), "/repo".into(), 950, true),
        ];
        let db_path = write_test_db(temp.path(), &rows);
        assert!(
            find_historic_id_for_db_path(&db_path, "/repo", 1000, true).is_none(),
            "two viable candidates in the spawn window must not bind a node"
        );
    }
}
