use super::*;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// A quiet recovery tick re-reads the same transcript. The parsed failure, and
/// the parsed absence of one, must be reused while the file's length and
/// modification time are unchanged. Replacing the bytes without moving that
/// stamp must not be parsed again.
#[test]
fn unchanged_transcript_is_not_reread_or_reparsed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout.jsonl");
    let cache = interruption_cache::Cache::default();

    let original = failed("server_overloaded");
    fs::write(&path, &original).unwrap();
    pin_mtime(&path, 1_700_000_000);
    let first = read_transcript(TranscriptFormat::Codex, &path, "session-1", &cache)
        .expect("a stable transcript is evidence")
        .expect("the tail reports a failure");
    assert_eq!(first.code, "server_overloaded");
    assert_eq!(cache.misses(), 1, "the first read parses the file");

    let mut replacement = vec![b' '; original.len()];
    replacement[original.len() - 1] = b'\n';
    fs::write(&path, replacement).unwrap();
    pin_mtime(&path, 1_700_000_000);
    let second = read_transcript(TranscriptFormat::Codex, &path, "session-1", &cache);
    assert_eq!(
        cache.misses(),
        1,
        "an unchanged stamp performs no re-read and no re-parse"
    );
    assert_eq!(
        second
            .as_ref()
            .and_then(|parsed| parsed.as_ref())
            .map(|failure| failure.code.as_str()),
        Some("server_overloaded"),
        "bytes written under the same stamp are not parsed"
    );

    let quiet = dir.path().join("quiet.jsonl");
    fs::write(
        &quiet,
        "{\"type\":\"event_msg\",\"timestamp\":\"2026-11-05T08:30:19.103Z\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t\",\"last_agent_message\":null,\"error\":null}}\n",
    )
    .unwrap();
    pin_mtime(&quiet, 1_700_000_001);
    assert_eq!(
        read_transcript(TranscriptFormat::Codex, &quiet, "session-1", &cache),
        Some(None),
        "a completed turn with no error is not a failure"
    );
    assert_eq!(cache.misses(), 2, "the quiet file is parsed once");
    assert_eq!(
        read_transcript(TranscriptFormat::Codex, &quiet, "session-1", &cache),
        Some(None)
    );
    assert_eq!(
        cache.misses(),
        2,
        "a transcript with no failure is not re-parsed on the next tick"
    );
}

#[test]
fn a_changed_stamp_reads_the_appended_failure() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout.jsonl");
    let quiet = "{\"type\":\"event_msg\",\"timestamp\":\"2026-11-05T08:30:19.103Z\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"ok\",\"last_agent_message\":null,\"error\":null}}\n";
    fs::write(&path, quiet).unwrap();
    pin_mtime(&path, 1_700_000_010);
    let cache = interruption_cache::Cache::default();
    assert_eq!(
        read_transcript(TranscriptFormat::Codex, &path, "session-1", &cache),
        Some(None)
    );
    assert_eq!(cache.misses(), 1);

    let mut appended = quiet.to_string();
    appended.push_str(&failed("server_overloaded"));
    fs::write(&path, appended).unwrap();
    pin_mtime(&path, 1_700_000_011);
    let found = read_transcript(TranscriptFormat::Codex, &path, "session-1", &cache)
        .expect("the appended file is stable")
        .expect("the appended turn reports a failure");
    assert_eq!(found.code, "server_overloaded");
    assert_eq!(found.kind, FailureKind::Transient);
    assert_eq!(cache.misses(), 2, "a longer transcript is parsed again");

    let (first, second) = same_length_failures();
    fs::write(&path, &first).unwrap();
    pin_mtime(&path, 1_700_000_012);
    assert_eq!(
        read_transcript(TranscriptFormat::Codex, &path, "session-1", &cache)
            .unwrap()
            .unwrap()
            .code,
        "server_overloaded"
    );
    let misses = cache.misses();
    fs::write(&path, &second).unwrap();
    pin_mtime(&path, 1_700_000_013);
    assert_eq!(first.len(), second.len());
    let replaced = read_transcript(TranscriptFormat::Codex, &path, "session-1", &cache)
        .unwrap()
        .unwrap();
    assert_eq!(replaced.code, "cyber_policy");
    assert_eq!(replaced.kind, FailureKind::Policy);
    assert_eq!(
        cache.misses(),
        misses + 1,
        "the same length with a new modification time is parsed again"
    );

    fs::remove_file(&path).unwrap();
    assert!(
        read_transcript(TranscriptFormat::Codex, &path, "session-1", &cache).is_none(),
        "a removed transcript is unavailable, not the cached failure"
    );
}

/// Evidence is the last 256 KiB only. An older failure is not recovered by
/// scanning the prefix, and a structured failure in that window still counts
/// when the turn has no assistant text.
#[test]
fn a_tail_without_assistant_text_keeps_the_existing_window() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout.jsonl");
    let old_failure = failed("server_overloaded");
    let pad = format!(
        "{}\n",
        serde_json::json!({"type":"event_msg","payload":{"type":"token_count"}})
    );
    let mut body = String::new();
    body.push_str(&old_failure);
    while body.len() <= 256 * 1024 + old_failure.len() {
        body.push_str(&pad);
    }
    assert!(
        !body.contains("\"text\""),
        "the fixture must not carry assistant text"
    );
    fs::write(&path, &body).unwrap();
    pin_mtime(&path, 1_700_000_020);
    let cache = interruption_cache::Cache::default();
    assert_eq!(
        read_transcript(TranscriptFormat::Codex, &path, "session-1", &cache),
        Some(None),
        "a failure older than the last 256 KiB stays outside the read"
    );

    body.push_str(&failed("cyber_policy"));
    fs::write(&path, &body).unwrap();
    pin_mtime(&path, 1_700_000_021);
    let found = read_transcript(TranscriptFormat::Codex, &path, "session-1", &cache)
        .expect("the extended tail is stable")
        .expect("a structured failure needs no assistant text");
    assert_eq!(found.code, "cyber_policy");
    assert_eq!(found.kind, FailureKind::Policy);
    assert_eq!(
        cache.misses(),
        2,
        "appending the tail failure invalidates the stamp"
    );
}

#[test]
fn unchanged_mcode_store_is_not_reread() {
    let dir = tempfile::tempdir().unwrap();
    let path = mcode_tree(dir.path(), "s");
    let cache = interruption_cache::Cache::default();
    let first = read_transcript(TranscriptFormat::Mcode, &path, "s", &cache)
        .expect("the runtime store is readable")
        .expect("the session's failed turn");
    assert_eq!(first.kind, FailureKind::Transient);
    assert_eq!(first.code, "50113");
    assert_eq!(cache.misses(), 1, "the first read queries the store");
    let second = read_transcript(TranscriptFormat::Mcode, &path, "s", &cache)
        .unwrap()
        .unwrap();
    assert_eq!(second, first);
    assert_eq!(
        cache.misses(),
        1,
        "an unchanged MiniMax store is not queried again"
    );
}

#[test]
fn mcode_cache_is_per_session_and_follows_the_store_stamp() {
    let dir = tempfile::tempdir().unwrap();
    let path = mcode_tree(dir.path(), "s");
    let cache = interruption_cache::Cache::default();
    let failure = read_transcript(TranscriptFormat::Mcode, &path, "s", &cache)
        .unwrap()
        .unwrap();
    assert_eq!(failure.code, "50113");
    let misses = cache.misses();
    assert!(
        read_transcript(TranscriptFormat::Mcode, &path, "other", &cache).is_none(),
        "a session the manifest does not name is unavailable, not the cached failure"
    );
    assert!(
        cache.misses() > misses,
        "that session is not served from the cached failure"
    );
    assert_eq!(
        read_transcript(TranscriptFormat::Mcode, &path, "s", &cache)
            .unwrap()
            .unwrap(),
        failure
    );
    assert_eq!(cache.misses(), 2, "the original session stays cached");

    let manifest = path.with_file_name("manifest.json");
    fs::write(&manifest, r#"{"sessionId":"renamed"}"#).unwrap();
    pin_mtime(&manifest, 1_700_000_200);
    assert!(
        read_transcript(TranscriptFormat::Mcode, &path, "s", &cache).is_none(),
        "a manifest that no longer names the session is unavailable"
    );

    fs::write(&manifest, r#"{"sessionId":"s"}"#).unwrap();
    pin_mtime(&manifest, 1_700_000_201);
    let database = dir.path().join("v2/sqlite/runtime-state.sqlite");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute(
            "INSERT INTO local_runtime_turn_ingress VALUES('new','s','accepted',2)",
            [],
        )
        .unwrap();
    drop(connection);
    pin_mtime(&database, 1_700_000_202);
    assert_eq!(
        read_transcript(TranscriptFormat::Mcode, &path, "s", &cache),
        Some(None),
        "an accepted turn clears the cached failure"
    );
}

#[test]
fn a_new_write_ahead_log_invalidates_the_cached_mcode_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = mcode_tree(dir.path(), "s");
    let cache = interruption_cache::Cache::default();
    assert!(read_transcript(TranscriptFormat::Mcode, &path, "s", &cache)
        .unwrap()
        .is_some());
    let misses = cache.misses();
    let wal = dir.path().join("v2/sqlite/runtime-state.sqlite-wal");
    fs::write(&wal, [0_u8; 32]).unwrap();
    pin_mtime(&wal, 1_700_000_210);
    let _ = read_transcript(TranscriptFormat::Mcode, &path, "s", &cache);
    assert!(
        cache.misses() > misses,
        "a write-ahead log that was absent is part of the stamp"
    );
}

#[test]
fn the_interruption_cache_stays_at_its_cap() {
    let dir = tempfile::tempdir().unwrap();
    let cache = interruption_cache::Cache::default();
    for index in 0..=interruption_cache::CAPACITY {
        let path = dir.path().join(format!("{index}.jsonl"));
        fs::write(&path, failed("server_overloaded")).unwrap();
        pin_mtime(&path, 1_700_000_100);
        assert!(
            read_transcript(TranscriptFormat::Codex, &path, "session-1", &cache)
                .unwrap()
                .is_some(),
            "{index}"
        );
    }
    assert_eq!(cache.len(), interruption_cache::CAPACITY);
}

fn mcode_tree(dir: &Path, session: &str) -> std::path::PathBuf {
    let transcript = dir.join("v2/sessions/2026/10/24/session/messages.jsonl");
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::write(&transcript, "{}\n").unwrap();
    fs::write(
        transcript.with_file_name("manifest.json"),
        format!(r#"{{"sessionId":"{session}"}}"#),
    )
    .unwrap();
    let database = dir.join("v2/sqlite/runtime-state.sqlite");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE local_runtime_sessions(session_id TEXT PRIMARY KEY,status TEXT,error_code INTEGER,error_message TEXT,updated_at_ms INTEGER);
             CREATE TABLE local_runtime_session_agent_state(session_id TEXT PRIMARY KEY,turn_id TEXT,terminal_outcome TEXT,updated_at_ms INTEGER);
             CREATE TABLE local_runtime_turn_ingress(turn_id TEXT PRIMARY KEY,session_id TEXT,status TEXT,accepted_sequence INTEGER);
             CREATE INDEX idx_local_runtime_turn_ingress_session_accepted_sequence ON local_runtime_turn_ingress(session_id,accepted_sequence DESC) WHERE status='accepted';
             CREATE UNIQUE INDEX idx_local_runtime_turn_ingress_sequence ON local_runtime_turn_ingress(accepted_sequence) WHERE accepted_sequence IS NOT NULL;
             INSERT INTO local_runtime_turn_ingress VALUES('t','s','failed',1);
             INSERT INTO local_runtime_sessions VALUES('s','error',50113,'Connection error.',100);
             INSERT INTO local_runtime_session_agent_state VALUES('s','t','failed',105);",
        )
        .unwrap();
    transcript
}

fn same_length_failures() -> (String, String) {
    let overloaded = failure_line("server_overloaded", 0);
    let policy = failure_line("cyber_policy", 0);
    if overloaded.len() == policy.len() {
        return (overloaded, policy);
    }
    let (overloaded_len, policy_len) = (overloaded.len(), policy.len());
    if overloaded_len < policy_len {
        (
            failure_line("server_overloaded", policy_len - overloaded_len),
            policy,
        )
    } else {
        (
            overloaded,
            failure_line("cyber_policy", overloaded_len - policy_len),
        )
    }
}

fn failure_line(code: &str, pad: usize) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "type": "event_msg",
            "timestamp": "2026-11-05T08:30:19.103Z",
            "payload": {
                "type": "task_complete",
                "turn_id": "t",
                "last_agent_message": null,
                "pad": "x".repeat(pad),
                "error": {"codex_error_info": code}
            }
        })
    )
}

fn pin_mtime(path: &Path, secs: u64) {
    let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
}

fn failed(code: &str) -> String {
    serde_json::json!({"type":"event_msg","timestamp":"2026-11-05T08:30:19.103Z",
        "payload":{"type":"task_complete","turn_id":"t","last_agent_message":null,
            "error":{"codex_error_info":code}}})
    .to_string()
        + "\n"
}

#[test]
fn native_capacity_retries_but_daybreak_and_unknown_errors_do_not() {
    let failure = codex(&failed("server_overloaded")).unwrap();
    assert_eq!(failure.kind, FailureKind::Transient);
    assert_eq!(
        (0..4).map(|n| failure.delay_ms(n)).collect::<Vec<_>>(),
        vec![Some(30_000), Some(120_000), Some(300_000), None]
    );
    for code in ["cyber_policy", "unauthorized", "new_unknown_error"] {
        assert_eq!(codex(&failed(code)).unwrap().delay_ms(0), None);
    }
}

#[test]
fn newer_work_user_interrupt_damage_and_success_invalidate_failure() {
    for next in [
        r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"new"}}"#,
        r#"{"type":"event_msg","payload":{"type":"turn_aborted"}}"#,
        r#"{"type":"response_item","payload":{"type":"function_call"}}"#,
        r#"{"type":"response_item","payload":{"type":"message","role":"user"}}"#,
        r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"new","error":null}}"#,
        "{broken",
    ] {
        assert!(
            codex(&(failed("server_overloaded") + next)).is_none(),
            "{next}"
        );
    }
    assert!(codex(r#"{"type":"response_item","payload":{"type":"message","role":"assistant","text":"server_overloaded"}}"#).is_none());
}

fn store() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE local_runtime_sessions(session_id TEXT PRIMARY KEY,status TEXT,error_code INTEGER,error_message TEXT,updated_at_ms INTEGER);
        CREATE TABLE local_runtime_session_agent_state(session_id TEXT PRIMARY KEY,turn_id TEXT,terminal_outcome TEXT,updated_at_ms INTEGER);
        CREATE TABLE local_runtime_turn_ingress(turn_id TEXT PRIMARY KEY,session_id TEXT,status TEXT,accepted_sequence INTEGER);
        CREATE INDEX idx_local_runtime_turn_ingress_session_accepted_sequence ON local_runtime_turn_ingress(session_id,accepted_sequence DESC) WHERE status='accepted';
        CREATE UNIQUE INDEX idx_local_runtime_turn_ingress_sequence ON local_runtime_turn_ingress(accepted_sequence) WHERE accepted_sequence IS NOT NULL;
        INSERT INTO local_runtime_turn_ingress VALUES('t','s','failed',1);
        INSERT INTO local_runtime_sessions VALUES('s','error',50113,'Connection error.',100);
        INSERT INTO local_runtime_session_agent_state VALUES('s','t','failed',105);").unwrap();
    c
}

#[test]
fn mcode_requires_matching_failed_session_and_no_newer_or_active_turn() {
    let c = store();
    assert_eq!(
        mcode(&c, "s").unwrap().unwrap().kind,
        FailureKind::Transient
    );
    assert!(mcode(&c, "other").unwrap().is_none());
    c.execute(
        "INSERT INTO local_runtime_turn_ingress VALUES('new','s','accepted',2)",
        [],
    )
    .unwrap();
    assert!(mcode(&c, "s").unwrap().is_none());
    c.execute(
        "UPDATE local_runtime_turn_ingress SET status='completed' WHERE turn_id='new'",
        [],
    )
    .unwrap();
    assert!(mcode(&c, "s").unwrap().is_none());
    c.execute(
        "DELETE FROM local_runtime_turn_ingress WHERE turn_id='new'",
        [],
    )
    .unwrap();
    c.execute(
        "UPDATE local_runtime_session_agent_state SET terminal_outcome='completed'",
        [],
    )
    .unwrap();
    assert!(mcode(&c, "s").unwrap().is_none());
}

#[test]
fn quota_has_slow_bounded_retries_and_unrecognized_errors_need_inspection() {
    let c = store();
    c.execute("UPDATE local_runtime_sessions SET error_code=42212,error_message='The Token Plan usage limit has been reached. (2067)'", []).unwrap();
    let failure = mcode(&c, "s").unwrap().unwrap();
    assert_eq!(failure.kind, FailureKind::Quota);
    assert_eq!(failure.delay_ms(0), Some(900_000));
    assert_eq!(failure.delay_ms(3), None);
    c.execute(
        "UPDATE local_runtime_sessions SET error_message='Different account error'",
        [],
    )
    .unwrap();
    assert_eq!(mcode(&c, "s").unwrap().unwrap().kind, FailureKind::Other);
}

#[test]
fn mcode_probe_uses_native_indexes_and_bounds_newer_ingress() {
    let c = store();
    for sql in [MCODE_FAILURE_SQL, MCODE_ACTIVE_SQL, MCODE_NEWER_SQL] {
        let mut stmt = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let plan = stmt
            .query_map(["s"], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n");
        assert!(!plan.contains("SCAN"), "unbounded native probe: {plan}");
        assert!(plan.contains("SEARCH"), "missing indexed lookup: {plan}");
    }
    for n in 2..=1026 {
        c.execute(
            "INSERT INTO local_runtime_turn_ingress VALUES(?1,'another-session','completed',?2)",
            rusqlite::params![format!("other-{n}"), n],
        )
        .unwrap();
    }
    assert!(
        mcode(&c, "s").is_none(),
        "overflow is unavailable, not proof of a safe retry"
    );
    c.execute(
        "DELETE FROM local_runtime_turn_ingress WHERE accepted_sequence=1026",
        [],
    )
    .unwrap();
    assert!(mcode(&c, "s").unwrap().is_some());
    c.execute("DROP INDEX idx_local_runtime_turn_ingress_sequence", [])
        .unwrap();
    assert!(
        mcode(&c, "s").is_none(),
        "schema drift must not fall back to table scans"
    );
}
