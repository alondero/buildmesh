use super::*;
use rusqlite::params;

fn sess(id: &str, dir: &str, created_ms: i64) -> ListedSession {
    ListedSession {
        session_id: id.into(),
        working_directory: dir.into(),
        created_ms,
    }
}

fn store() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
          id TEXT PRIMARY KEY,
          working_directory TEXT NOT NULL,
          backend_type TEXT NOT NULL,
          model TEXT NOT NULL,
          agent_mode TEXT NOT NULL,
          created_at INTEGER NOT NULL,
          last_activity_at INTEGER NOT NULL,
          title TEXT, main_chain_id INTEGER, shell_last_seen_index INTEGER DEFAULT 0,
          cogs_json TEXT, workspace_dirs TEXT, hidden INTEGER NOT NULL DEFAULT 0,
          metadata TEXT
        )",
    )
    .unwrap();
    conn
}

fn insert(conn: &Connection, id: &str, dir: &str, created_secs: i64, hidden: i64) {
    conn.execute(
        "INSERT INTO sessions (id, working_directory, backend_type, model, agent_mode, created_at, last_activity_at, hidden)
         VALUES (?1, ?2, 'windsurf', '', '', ?3, ?3, ?4)",
        params![id, dir, created_secs, hidden],
    )
    .unwrap();
}

#[test]
fn accepts_the_live_observed_slugs() {
    for id in [
        "tangy-peony",
        "exciting-ghost",
        "spangled-click",
        "phantom-jellyfish",
        "orchid-ragamuffin",
    ] {
        assert!(is_devin_session_id(id), "{id} must validate");
        assert!(is_devin_session_slug(id), "{id} must be a slug");
    }
}

#[test]
fn accepts_multiword_and_digit_bearing_slugs() {
    assert!(is_devin_session_slug("tempted-wondrous-vortex"));
    assert!(is_devin_session_slug("steady-otter2"));
}

#[test]
fn rejects_non_devin_and_injection_shapes() {
    for bad in [
        "",
        "ses_fc52ccfb9ffek1jl23ZwpRuSP7",
        "mvs_d66c5fa695294e2abe936c242fb43c76",
        "session_1789901791099_yrvad",
        "sess_abc123",
        "--permission-mode",
        "-r",
        "tangy-peony;rm -rf /",
        "$(whoami)",
        "a/b",
        "a b",
        "Tangy-Peony",
        "-leading",
        "trailing-",
        "double--hyphen",
        "single",
        "under_score",
        "tangy.peony",
    ] {
        assert!(!is_devin_session_slug(bad), "{bad} must not be a slug");
        assert!(!is_devin_session_id(bad), "{bad} must not validate");
    }
}

#[test]
fn accepts_uuid_ids_for_forward_compatibility() {
    let uuid = "3f8d1c2a-1cb0-408c-b29c-9f0f68f2982b";
    assert!(is_devin_session_id(uuid));
    assert!(!is_devin_session_slug(uuid));
}

#[test]
fn enforces_the_length_bound() {
    assert!(is_devin_session_slug(&format!(
        "{}-{}",
        "a".repeat(63),
        "b".repeat(62)
    )));
    assert!(!is_devin_session_slug(&format!(
        "{}-{}",
        "a".repeat(64),
        "b".repeat(64)
    )));
}

#[test]
fn selects_the_unique_fresh_row_for_the_spawn_directory() {
    let sessions = vec![
        sess("tangy-peony", r"F:\src\mesh", 10_000),
        sess("older-row", r"F:\src\mesh", 5_000),
        sess("other-dir", r"F:\src\other", 10_000),
    ];
    assert_eq!(
        select_id_for_directory(&sessions, r"F:\src\mesh", 8_000),
        Some("tangy-peony")
    );
}

#[test]
fn refuses_ambiguous_and_stale_rows() {
    let sessions = vec![
        sess("first-row", r"F:\src\mesh", 10_000),
        sess("second-row", r"F:\src\mesh", 10_001),
    ];
    assert_eq!(
        select_id_for_directory(&sessions, r"F:\src\mesh", 8_000),
        None
    );
    assert_eq!(
        select_id_for_directory(&sessions, r"F:\src\mesh", 11_000),
        None
    );
    assert_eq!(
        select_id_for_directory(&sessions, r"F:\src\other", 8_000),
        None
    );
}

#[test]
fn directory_matching_tolerates_separator_and_case_drift() {
    let sessions = vec![sess("tangy-peony", "F:/src/mesh", 10_000)];
    assert_eq!(
        select_id_for_directory(&sessions, r"f:\SRC\MESH", 8_000),
        Some("tangy-peony")
    );
}

#[test]
fn recent_sessions_filters_freshness_and_hidden_rows() {
    let conn = store();
    insert(&conn, "fresh-row", r"F:\src\mesh", 10, 0);
    insert(&conn, "hidden-row", r"F:\src\mesh", 10, 1);
    insert(&conn, "old-row", r"F:\src\mesh", 5, 0);
    let rows = list_recent_sessions(&conn, 8_000).unwrap();
    assert_eq!(
        rows.iter()
            .map(|r| r.session_id.as_str())
            .collect::<Vec<_>>(),
        vec!["fresh-row"]
    );
}

#[test]
fn window_read_bounds_created_at_in_seconds() {
    let conn = store();
    insert(&conn, "in-window", r"F:\src\mesh", 11, 0);
    insert(&conn, "too-early", r"F:\src\mesh", 8, 0);
    insert(&conn, "too-late", r"F:\src\mesh", 20, 0);
    let rows = list_sessions_in_window(&conn, 10_000, 15_000).unwrap();
    assert_eq!(
        rows.iter()
            .map(|r| r.session_id.as_str())
            .collect::<Vec<_>>(),
        vec!["in-window"]
    );
}

#[test]
fn db_row_for_spawn_dir_resolves_to_the_resume_id() {
    let dir = std::env::temp_dir().join(format!("devin-session-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("sessions.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
              id TEXT PRIMARY KEY,
              working_directory TEXT NOT NULL,
              backend_type TEXT NOT NULL,
              model TEXT NOT NULL,
              agent_mode TEXT NOT NULL,
              created_at INTEGER NOT NULL,
              last_activity_at INTEGER NOT NULL,
              title TEXT, main_chain_id INTEGER, shell_last_seen_index INTEGER DEFAULT 0,
              cogs_json TEXT, workspace_dirs TEXT, hidden INTEGER NOT NULL DEFAULT 0,
              metadata TEXT
            )",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, working_directory, backend_type, model, agent_mode, created_at, last_activity_at, hidden)
             VALUES ('phantom-jellyfish', ?1, 'windsurf', '', '', ?2, ?2, 0)",
            params![dir.to_string_lossy().to_string(), 1_800_000_000i64],
        )
        .unwrap();
    }
    let spawn = dir.to_string_lossy().to_string();
    assert_eq!(
        try_capture_from_db_path(&db_path, &spawn, 1_800_000_000_000 - CAPTURE_SKEW_MS),
        Some("phantom-jellyfish".to_string()),
        "the captured id is the value --resume <id> consumes"
    );
    std::fs::remove_dir_all(&dir).ok();
}
