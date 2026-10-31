//! Codex session discovery across Harness Login homes: a session written by
//! an additional login is found exactly like one in the default home.

use super::*;

const NOT_BEFORE_MS: i64 = 1_787_830_399_000;

fn write_rollout(sessions: &Path, id: &str, cwd: &str, timestamp: &str) {
    let day = sessions.join("2026").join("08").join("27");
    fs::create_dir_all(&day).unwrap();
    let record = serde_json::json!({
        "type": "session_meta",
        "payload": { "session_id": id, "cwd": cwd, "timestamp": timestamp }
    });
    fs::write(day.join(format!("rollout-{id}.jsonl")), record.to_string()).unwrap();
}

#[test]
fn native_runtimes_search_every_login_home_once() {
    let default_dir = PathBuf::from("C:/u/.codex/sessions");
    let homes = vec![
        PathBuf::from("C:/u/.codex-accounts/work"),
        PathBuf::from("C:/u/.codex"),
    ];
    let dirs = sessions_dirs_with(Some(default_dir.clone()), EnvType::Windows, homes.clone());
    assert_eq!(
        dirs,
        vec![
            default_dir.clone(),
            PathBuf::from("C:/u/.codex-accounts/work/sessions")
        ],
        "the default home is searched first and never twice"
    );
    assert_eq!(
        sessions_dirs_with(Some(default_dir.clone()), EnvType::Wsl, homes),
        vec![default_dir],
        "a WSL guest cannot run an additional login, so only its own home is read"
    );
}

#[test]
fn a_fresh_session_in_a_login_home_is_captured() {
    let default_home = tempfile::TempDir::new().unwrap();
    let login_home = tempfile::TempDir::new().unwrap();
    let wanted = "01a042fe-e7e2-79a2-96bd-a15140478a58";
    write_rollout(
        login_home.path(),
        wanted,
        "F:/repo",
        "2026-08-27T11:33:19.986Z",
    );
    let dirs = vec![
        default_home.path().to_path_buf(),
        login_home.path().to_path_buf(),
    ];
    assert_eq!(
        find_fresh_id_in_dirs(&dirs, "F:/repo", NOT_BEFORE_MS).as_deref(),
        Some(wanted)
    );
}

#[test]
fn two_candidates_split_across_homes_stay_ambiguous() {
    let default_home = tempfile::TempDir::new().unwrap();
    let login_home = tempfile::TempDir::new().unwrap();
    write_rollout(
        default_home.path(),
        "01a042fe-e7e2-79a2-96bd-a15140478a58",
        "F:/repo",
        "2026-08-27T11:33:19.986Z",
    );
    write_rollout(
        login_home.path(),
        "01a042fd-1111-7000-8000-000000000001",
        "F:/repo",
        "2026-08-27T11:33:20.986Z",
    );
    let dirs = vec![
        default_home.path().to_path_buf(),
        login_home.path().to_path_buf(),
    ];
    assert_eq!(
        find_fresh_id_in_dirs(&dirs, "F:/repo", NOT_BEFORE_MS),
        None,
        "one selection over every home, so a split cannot hide ambiguity"
    );
}

#[test]
fn historic_recovery_finds_a_session_written_by_a_login() {
    let default_home = tempfile::TempDir::new().unwrap();
    let login_home = tempfile::TempDir::new().unwrap();
    let wanted = "01a042fe-e7e2-79a2-96bd-a15140478a58";
    write_rollout(
        login_home.path(),
        wanted,
        "F:/repo",
        "2026-08-27T11:33:19.986Z",
    );
    let dirs = vec![
        default_home.path().to_path_buf(),
        login_home.path().to_path_buf(),
    ];
    assert_eq!(
        find_historic_id_in_dirs(&dirs, "F:/repo", NOT_BEFORE_MS, true).as_deref(),
        Some(wanted)
    );
}
