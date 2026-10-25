use super::*;

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir
        .path()
        .join("v2/sessions/2026/10/24/session/messages.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(
        transcript.with_file_name("manifest.json"),
        r#"{"sessionId":"mvs_owned"}"#,
    )
    .unwrap();
    (dir, transcript)
}

fn publish(path: &Path, records: &[serde_json::Value]) {
    let lines = records
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(path, format!("{lines}\n")).unwrap();
}

fn launch(task: &str, timestamp: i64) -> serde_json::Value {
    serde_json::json!({"message":{"role":"toolResult","timestamp":timestamp,
        "details":{"status":"started","task_id":task},"content":[]}})
}

fn assistant(timestamp: i64) -> serde_json::Value {
    serde_json::json!({"message":{"role":"assistant","timestamp":timestamp,
        "content":[{"type":"text","text":"Waiting for background work."}]}})
}

#[test]
fn native_spawn_keeps_the_supplied_data_directory_separate_from_the_worktree() {
    let (dir, transcript) = fixture();
    let worktree = tempfile::tempdir().unwrap();
    let spawn_path = worktree.path().to_str().unwrap();
    assert_eq!(
        minimax_data_dir_for_spawn(spawn_path, dir.path().to_path_buf()),
        Some(dir.path().to_path_buf())
    );
    publish(&transcript, &[launch("bg_pending", 100)]);
    assert_eq!(
        count_pending_background_tasks_for_spawn("mvs_owned", spawn_path, dir.path().to_path_buf()),
        Some(1)
    );
}

#[test]
fn native_spawn_does_not_fall_back_to_a_repository_session_store() {
    let (worktree, transcript) = fixture();
    publish(&transcript, &[launch("bg_repository", 100)]);
    let native_store = tempfile::tempdir().unwrap();
    assert_eq!(
        count_pending_background_tasks("mvs_owned", worktree.path()),
        Some(1),
        "the repository contains a valid pending session that a fallback would find"
    );
    assert_eq!(
        count_pending_background_tasks_for_spawn(
            "mvs_owned",
            worktree.path().to_str().unwrap(),
            native_store.path().to_path_buf()
        ),
        None,
        "an empty native store must not borrow the repository's pending session"
    );
}

#[test]
fn production_counter_locates_only_the_requested_session_in_the_data_directory() {
    let (dir, transcript) = fixture();
    publish(
        &transcript,
        &[
            launch("bg_done", 100),
            serde_json::json!({"message":{"role":"toolResult","timestamp":200,
            "details":{"task_id":"bg_done","status":"succeeded"},"content":[]}}),
        ],
    );
    let pending = transcript
        .parent()
        .unwrap()
        .with_file_name("other-session")
        .join("messages.jsonl");
    std::fs::create_dir_all(pending.parent().unwrap()).unwrap();
    std::fs::write(
        pending.with_file_name("manifest.json"),
        r#"{"sessionId":"mvs_pending"}"#,
    )
    .unwrap();
    publish(&pending, &[launch("bg_pending", 100)]);

    assert_eq!(
        count_pending_background_tasks("mvs_owned", dir.path()),
        Some(0)
    );
    assert_eq!(
        count_pending_background_tasks("mvs_pending", dir.path()),
        Some(1)
    );
    assert_eq!(
        count_pending_background_tasks("mvs_absent", dir.path()),
        None
    );
}

#[test]
fn stop_and_wake_reconcile_summary_success_plus_native_failed_or_lost_task() {
    let (dir, transcript) = fixture();
    let tasks = dir.path().join("background-tasks");
    let success = tasks.join("bg_success");
    std::fs::create_dir_all(&success).unwrap();
    std::fs::write(success.join("summary.txt"), "success").unwrap();
    let database = dir.path().join("v2/sqlite/runtime-state.sqlite");
    std::fs::create_dir_all(database.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&database).unwrap();
    conn.execute_batch(
        "CREATE TABLE local_runtime_background_tasks (
        task_id TEXT PRIMARY KEY, owner_session_id TEXT NOT NULL,
        status TEXT NOT NULL, created_at_ms INTEGER NOT NULL, ended_at_ms INTEGER,
        record_json TEXT NOT NULL DEFAULT '{}');
        INSERT INTO local_runtime_background_tasks VALUES
        ('bg_failed','mvs_owned','failed',100,300,'{}');",
    )
    .unwrap();
    let records = [
        launch("bg_success", 100),
        launch("bg_failed", 110),
        assistant(200),
    ];
    publish(&transcript, &records);
    let lines = std::fs::read_to_string(&transcript).unwrap();
    let launched = launched_background_tasks(&lines);
    let legacy = background_facts(&lines, &tasks, &launched).unwrap();
    assert_eq!(detect_background_task_stall(&launched, &legacy, Some(200), i64::MAX), None,
        "the original transcript/summary path misses the native failed task and suppresses the wake");
    let wake = || {
        McodeAdapter.stalled_background_task(
            &std::fs::read_to_string(&transcript).unwrap(),
            &transcript,
            i64::MAX,
        )
    };
    for status in ["failed", "lost"] {
        conn.execute(
            "UPDATE local_runtime_background_tasks SET status=?1",
            [status],
        )
        .unwrap();
        assert_eq!(
            count_pending_background_tasks("mvs_owned", dir.path()),
            Some(0)
        );
        assert_eq!(wake(), Some("bg_success".into()), "{status}");
    }
    for status in ["running", "unknown"] {
        conn.execute(
            "UPDATE local_runtime_background_tasks SET status=?1",
            [status],
        )
        .unwrap();
        assert_eq!(
            count_pending_background_tasks("mvs_owned", dir.path()),
            Some(1)
        );
        assert_eq!(wake(), None, "{status}");
    }
    conn.execute(
        "UPDATE local_runtime_background_tasks SET status='failed',owner_session_id='mvs_other'",
        [],
    )
    .unwrap();
    assert_eq!(
        count_pending_background_tasks("mvs_owned", dir.path()),
        Some(1)
    );
    assert_eq!(
        wake(),
        None,
        "a foreign task cannot settle the pending launch"
    );

    conn.execute(
        "UPDATE local_runtime_background_tasks SET owner_session_id='mvs_owned'",
        [],
    )
    .unwrap();
    let mut foreground = records.to_vec();
    foreground.push(
        serde_json::json!({"message":{"role":"assistant","timestamp":400,
        "content":[{"type":"toolCall","id":"fg","name":"bash","arguments":{}}]}}),
    );
    publish(&transcript, &foreground);
    assert_eq!(
        count_pending_background_tasks("mvs_owned", dir.path()),
        None
    );
    assert_eq!(
        wake(),
        None,
        "terminal background work does not finish a foreground call"
    );
}

#[test]
fn a_native_lost_tool_receipt_settles_without_a_database() {
    let (dir, transcript) = fixture();
    publish(
        &transcript,
        &[
            launch("bg_lost", 100),
            serde_json::json!({"message":{"role":"toolResult","timestamp":300,
            "details":{"task_id":"bg_lost","status":"lost",
                "lastError":{"code":"TASK_LOST_ON_STARTUP"}},"content":[]}}),
            assistant(400),
        ],
    );
    assert_eq!(
        count_pending_background_tasks("mvs_owned", dir.path()),
        Some(0)
    );
    assert_eq!(
        McodeAdapter.stalled_background_task(
            &std::fs::read_to_string(&transcript).unwrap(),
            &transcript,
            500
        ),
        None,
        "a terminal task already seen by the assistant does not require a wake"
    );
}

#[test]
fn a_missing_native_table_leaves_stop_and_wake_unavailable() {
    let (dir, transcript) = fixture();
    let database = dir.path().join("v2/sqlite/runtime-state.sqlite");
    std::fs::create_dir_all(database.parent().unwrap()).unwrap();
    drop(rusqlite::Connection::open(database).unwrap());
    publish(&transcript, &[launch("bg_task", 100), assistant(200)]);
    assert_eq!(
        count_pending_background_tasks("mvs_owned", dir.path()),
        None
    );
    assert_eq!(
        McodeAdapter.stalled_background_task(
            &std::fs::read_to_string(&transcript).unwrap(),
            &transcript,
            500
        ),
        None
    );
}

#[test]
fn a_summary_does_not_authorize_a_wake_when_an_existing_registry_is_unavailable() {
    let (dir, transcript) = fixture();
    let summary = dir.path().join("background-tasks/bg_task/summary.txt");
    std::fs::create_dir_all(summary.parent().unwrap()).unwrap();
    std::fs::write(&summary, "task completed").unwrap();
    publish(&transcript, &[launch("bg_task", 100), assistant(200)]);
    let lines = std::fs::read_to_string(&transcript).unwrap();
    let launched = launched_background_tasks(&lines);
    let legacy = background_facts(
        &lines,
        summary.parent().unwrap().parent().unwrap(),
        &launched,
    )
    .unwrap();
    assert!(
        detect_background_task_stall(&launched, &legacy, Some(200), i64::MAX).is_some(),
        "the summary alone would authorize a wake"
    );
    let database = dir.path().join("v2/sqlite/runtime-state.sqlite");
    std::fs::create_dir_all(database.parent().unwrap()).unwrap();
    for corrupt in [false, true] {
        if corrupt {
            std::fs::write(&database, b"not sqlite").unwrap();
        } else {
            drop(rusqlite::Connection::open(&database).unwrap());
        }
        assert_eq!(
            count_pending_background_tasks("mvs_owned", dir.path()),
            None
        );
        assert_eq!(McodeAdapter.stalled_background_task(&lines, &transcript, i64::MAX), None,
            "unavailable registry is a conservative veto, including with a summary; corrupt={corrupt}");
    }
}
