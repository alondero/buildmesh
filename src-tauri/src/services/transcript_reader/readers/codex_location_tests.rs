//! Tests for the positive Codex rollout location cache.
//!
//! Run with: cd src-tauri && cargo test services::transcript_reader::readers::codex::location_cache_tests

use super::*;

fn write_rollout(sessions: &Path, day: &str, session_id: &str) -> PathBuf {
    let dir = sessions.join(day);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-07-18T10-00-00-{session_id}.jsonl"));
    std::fs::write(&path, "{}\n").unwrap();
    path
}

#[test]
fn repeated_lookup_reuses_a_valid_location_and_rediscovers_after_deletion() {
    let root = tempfile::tempdir().unwrap();
    let sessions = root.path().join("sessions");
    let id = "01a076ee-6c95-7c82-9e5f-928e9f43ad01";
    let old = write_rollout(&sessions, "2026/07/18", id);
    let dirs = vec![sessions.clone()];
    let cache = codex_location_cache::CodexLocationCache::default();

    assert_eq!(locate_codex_rollout(&cache, &dirs, id), Some(old.clone()));
    assert!(cache.contains(id));

    // A newer duplicate is only reachable by walking. The cached path wins
    // while it still exists, proving the second lookup did not walk.
    let newer = write_rollout(&sessions, "2026/07/19", id);
    assert_eq!(
        locate_codex_rollout(&cache, &dirs, id),
        Some(old.clone()),
        "a valid cached location must not be rediscovered"
    );

    // Deletion invalidates the cached path, so the walk finds the newer
    // duplicate.
    std::fs::remove_file(&old).unwrap();
    assert_eq!(locate_codex_rollout(&cache, &dirs, id), Some(newer));
}

#[test]
fn location_cache_is_scoped_to_the_runtime_login_set() {
    let first_root = tempfile::tempdir().unwrap();
    let second_root = tempfile::tempdir().unwrap();
    let id = "01a076ee-6c95-7c82-9e5f-928e9f43ad02";
    let first_sessions = first_root.path().join("sessions");
    let second_sessions = second_root.path().join("sessions");
    let first = write_rollout(&first_sessions, "2026/07/18", id);
    let second = write_rollout(&second_sessions, "2026/07/18", id);
    let cache = codex_location_cache::CodexLocationCache::default();

    assert_eq!(
        locate_codex_rollout(&cache, std::slice::from_ref(&first_sessions), id),
        Some(first.clone())
    );
    assert_eq!(
        locate_codex_rollout(&cache, std::slice::from_ref(&second_sessions), id),
        Some(second),
        "a changed runtime/login set must rediscover, not reuse the first root"
    );
    assert!(first.is_file(), "the first path still exists");
}

#[test]
fn missing_transcripts_are_not_cached() {
    let root = tempfile::tempdir().unwrap();
    let sessions = root.path().join("sessions");
    let id = "01a076ee-6c95-7c82-9e5f-928e9f43ad03";
    let dirs = vec![sessions.clone()];
    let cache = codex_location_cache::CodexLocationCache::default();

    assert_eq!(locate_codex_rollout(&cache, &dirs, id), None);
    assert!(!cache.contains(id));

    // The CLI creates the rollout moments after the id is captured; the
    // earlier negative lookup must not hide it.
    let created = write_rollout(&sessions, "2026/07/18", id);
    assert_eq!(locate_codex_rollout(&cache, &dirs, id), Some(created));
}
