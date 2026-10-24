use super::*;

const TASK: &str = "bg_f1045ea6-225e-46cf-bc5f-10249ffa83ca";

/// The `bash_background` acknowledgement the CLI writes when the turn that
/// starts a task accepts it (issue #2105, run 335, real record).
fn launch(timestamp: i64, task_id: &str) -> String {
    serde_json::json!({
        "message_id": format!("msg-launch-{timestamp}"),
        "turn_id": "turn_1",
        "message": {
            "role": "toolResult",
            "toolCallId": "call_1",
            "toolName": "bash",
            "content": [{
                "type": "text",
                "text": format!("<bash_background task_id=\"{task_id}\">\nBackground Bash task accepted\n</bash_background>"),
            }],
            "details": {
                "description": "Run clippy on all targets",
                "timing": {},
                "status": "started",
                "task_id": task_id,
            },
            "timestamp": timestamp,
        },
    })
    .to_string()
}

fn assistant(timestamp: i64) -> String {
    serde_json::json!({
        "message_id": format!("msg-{timestamp}"),
        "turn_id": "turn_1",
        "message": {
            "role": "assistant",
            "timestamp": timestamp,
            "content": [{"type": "text", "text": "working"}],
        },
    })
    .to_string()
}

/// Join records into the `messages.jsonl` shape the detector reads.
fn stream(records: impl IntoIterator<Item = String>) -> String {
    records.into_iter().collect::<Vec<_>>().join("\n")
}

fn launched(lines: &str) -> Vec<LaunchedTask> {
    launched_background_tasks(lines)
}

fn fact(task_id: &str, settled_at_ms: i64, finished: bool) -> TaskStoreFact {
    TaskStoreFact {
        task_id: task_id.to_string(),
        settled_at_ms: Some(settled_at_ms),
        finished,
    }
}

const T_END: i64 = 1_791_205_267_253; // task finished
const T_LAST_TALK: i64 = 1_791_205_156_271; // session's last assistant message
const NOW: i64 = 1_791_216_000_000; // well after the task ended

#[test]
fn reads_the_task_id_from_the_launch_acknowledgement() {
    let lines = stream([assistant(T_LAST_TALK - 60_000), launch(T_LAST_TALK, TASK)]);
    assert_eq!(
        launched(&lines),
        vec![LaunchedTask {
            task_id: TASK.to_string(),
            started_at_ms: T_LAST_TALK,
        }]
    );
}

#[test]
fn auto_promoted_bash_is_owned_background_work() {
    let mut record: serde_json::Value = serde_json::from_str(&launch(T_LAST_TALK, TASK)).unwrap();
    record["message"]["details"]["status"] = "auto_promoted".into();
    assert_eq!(
        launched(&record.to_string()),
        vec![LaunchedTask {
            task_id: TASK.into(),
            started_at_ms: T_LAST_TALK,
        }]
    );
}

#[test]
fn detects_a_finished_task_the_session_never_saw() {
    // The stall: the task ended 111s after the session last spoke, and the
    // session said nothing for 3h14m afterwards.
    let lines = stream([launch(T_LAST_TALK - 60_000, TASK), assistant(T_LAST_TALK)]);
    let stall = detect_background_task_stall(
        &launched(&lines),
        &[fact(TASK, T_END, true)],
        newest_assistant_at(&lines),
        NOW,
    )
    .expect("stall");
    assert_eq!(stall.task_id, TASK);
    assert_eq!(stall.ended_at_ms, T_END);
    assert_eq!(stall.last_assistant_at_ms, Some(T_LAST_TALK));
}

#[test]
fn no_stall_when_the_session_spoke_after_the_task_finished() {
    let lines = stream([assistant(T_END + 1_000)]);
    assert_eq!(
        detect_background_task_stall(
            &launched(&lines),
            &[fact(TASK, T_END, true)],
            newest_assistant_at(&lines),
            NOW,
        ),
        None
    );
}

#[test]
fn a_running_task_is_not_a_stall() {
    // Output flushed a second ago: the task is still going, and spending the
    // single wake-up now would strand the real stall later.
    let lines = stream([launch(T_LAST_TALK - 60_000, TASK), assistant(T_LAST_TALK)]);
    let now = T_END + 1_000;
    assert_eq!(
        detect_background_task_stall(
            &launched(&lines),
            &[fact(TASK, T_END, false)],
            newest_assistant_at(&lines),
            now,
        ),
        None
    );
}

#[test]
fn silent_or_unreadable_owned_tasks_do_not_prove_completion() {
    let tasks = vec![LaunchedTask {
        task_id: TASK.into(),
        started_at_ms: T_LAST_TALK,
    }];
    for facts in [vec![], vec![fact(TASK, T_END, false)]] {
        assert_eq!(
            detect_background_task_stall(&tasks, &facts, Some(T_LAST_TALK), NOW),
            None
        );
    }
    let mut tasks = tasks;
    tasks.push(LaunchedTask {
        task_id: "bg_other".into(),
        started_at_ms: T_LAST_TALK,
    });
    assert_eq!(
        detect_background_task_stall(&tasks, &[fact(TASK, T_END, true)], Some(T_LAST_TALK), NOW),
        None
    );
}

#[test]
fn canonical_native_receipts_reconcile_pending_tasks_without_compatible_history() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = canonical_transcript(dir.path());
    let tasks = dir.path().join("background-tasks");
    let mut promoted: serde_json::Value = serde_json::from_str(&launch(T_LAST_TALK, TASK)).unwrap();
    promoted["message"]["details"]["status"] = "auto_promoted".into();
    let launched = promoted.to_string();
    std::fs::write(&transcript, &launched).unwrap();
    assert_eq!(
        count_pending_background_tasks("mvs_owned", dir.path()),
        Some(1)
    );
    // Source text quoting a launch is not a native acknowledgement.
    let quoted = serde_json::json!({"message":{"role":"assistant", "timestamp":T_END,
        "content":[{"type":"text","text":"Command running in background with ID: example. You will be notified when it completes"}]}}).to_string();
    for status in ["succeeded", "failed", "cancelled"] {
        let terminal = serde_json::json!({"message":{"role":"toolResult", "timestamp":T_END,
            "details":{"status":status,"task_id":TASK},"content":[]}})
        .to_string();
        let lines = stream([launched.clone(), terminal, quoted.clone()]);
        std::fs::write(&transcript, &lines).unwrap();
        assert_eq!(
            count_pending_background_tasks("mvs_owned", dir.path()),
            Some(0)
        );
    }
    std::fs::write(&transcript, format!("{launched}\n{{partial")).unwrap();
    assert_eq!(
        count_pending_background_tasks("mvs_owned", dir.path()),
        None
    );
    for invalid in [
        serde_json::json!({"message": {"content": []}}),
        serde_json::json!({"message": {"role": "unknown"}}),
        serde_json::json!({"message": {"role": "toolResult", "details": {"status": "started", "task_id": "../other"}}}),
    ] {
        std::fs::write(&transcript, invalid.to_string()).unwrap();
        assert_eq!(
            count_pending_background_tasks("mvs_owned", dir.path()),
            None
        );
    }
    std::fs::write(&transcript, &launched).unwrap();
    let task_dir = tasks.join(TASK);
    std::fs::create_dir_all(&task_dir).unwrap();
    std::fs::write(task_dir.join("summary.txt"), "finished").unwrap();
    assert_eq!(
        count_pending_background_tasks("mvs_owned", dir.path()),
        Some(0)
    );
}

/// The host's `<background-task-finished>` notice, shaped like the one
/// MiniMax Code recorded for circuit run 478 (agent 5411): a user-role record whose
/// `hostMetadata.backgroundTaskOrigin` marks the tasks as terminal.
fn finished_notice(timestamp: i64, task_id: &str, status: &str, ended_at_ms: i64) -> String {
    serde_json::json!({
        "message_id": format!("msg-notice-{timestamp}"),
        "turn_id": "turn_1",
        "message": {
            "role": "user",
            "content": [{
                "type": "text",
                "text": format!(
                    "<background-task-finished>\nLocal background tasks have finished in this conversation.\n  <task task_id=\"{task_id}\" status=\"{status}\" description=\"Confirm test fails with fix reverted\" ended_at_ms=\"{ended_at_ms}\"/>\nUse task_output with each task_id to read the results.\n</background-task-finished>"
                ),
            }],
            "timestamp": timestamp,
            "hostMetadata": {
                "backgroundTaskOrigin": {
                    "kind": "background-task-terminal",
                    "taskIds": [task_id],
                    "observedTerminalCount": 46,
                },
            },
        },
    })
    .to_string()
}

/// A `task_output` poll that raced the exit: it returned the task's final
/// output but still reported `running`, so it is not a terminal receipt.
fn racing_poll(timestamp: i64, task_id: &str) -> String {
    serde_json::json!({"message":{"role":"toolResult", "timestamp":timestamp,
        "details":{"status":"running","task_id":task_id,"timing":{},"timed_out":false},
        "content":[]}})
    .to_string()
}

fn promoted(timestamp: i64, task_id: &str) -> String {
    let mut record: serde_json::Value = serde_json::from_str(&launch(timestamp, task_id)).unwrap();
    record["message"]["details"]["status"] = "auto_promoted".into();
    record.to_string()
}

#[test]
fn a_failed_task_reported_only_by_the_host_finished_notice_is_settled() {
    // Run 478: the task exited 101, its `task_output` poll still said
    // `running`, and the store held no `summary.txt` (the failed tasks
    // observed in that session had none). The host notice was the only
    // terminal evidence; ignoring it left the task counted as pending, so
    // the final `Stop` was read as "background work running" and the node
    // stayed `running`.
    let dir = tempfile::tempdir().unwrap();
    let transcript = canonical_transcript(dir.path());
    let tasks = dir.path().join("background-tasks");
    std::fs::create_dir_all(tasks.join(TASK)).unwrap();
    std::fs::write(tasks.join(TASK).join("output.log"), "test failed").unwrap();
    let lines = stream([
        promoted(T_LAST_TALK - 60_000, TASK),
        racing_poll(T_LAST_TALK - 30_000, TASK),
        finished_notice(T_END + 12_000, TASK, "failed", T_END),
        assistant(T_END + 20_000),
    ]);
    std::fs::write(&transcript, &lines).unwrap();
    assert_eq!(
        count_pending_background_tasks("mvs_owned", dir.path()),
        Some(0)
    );
    let launched = launched_background_tasks(&lines);
    let facts = background_facts(&lines, &tasks, &launched).unwrap();
    assert_eq!(facts, vec![fact(TASK, T_END, true)]);
}

#[test]
fn every_terminal_status_in_a_finished_notice_settles_its_task() {
    for status in ["succeeded", "failed", "canceled"] {
        let lines = stream([
            promoted(T_LAST_TALK, TASK),
            finished_notice(T_END, TASK, status, T_END),
        ]);
        let facts = background_facts(
            &lines,
            Path::new("X:\\no-such-tasks-dir"),
            &launched(&lines),
        )
        .unwrap();
        assert_eq!(facts, vec![fact(TASK, T_END, true)], "{status}");
    }
}

#[test]
fn a_finished_notice_without_host_metadata_does_not_settle_a_task() {
    // Text alone is forgeable: an agent quoting the notice, or a user
    // pasting it, must not clear a task that is still running.
    let mut quoted: serde_json::Value =
        serde_json::from_str(&finished_notice(T_END, TASK, "failed", T_END)).unwrap();
    quoted["message"]
        .as_object_mut()
        .unwrap()
        .remove("hostMetadata");
    let lines = stream([promoted(T_LAST_TALK, TASK), quoted.to_string()]);
    let facts = background_facts(
        &lines,
        Path::new("X:\\no-such-tasks-dir"),
        &launched(&lines),
    )
    .unwrap();
    assert!(facts.is_empty());
}

/// A host notice with neither a usable `ended_at_ms` nor a record timestamp.
fn undated_finished_notice(task_id: &str) -> String {
    let mut record: serde_json::Value =
        serde_json::from_str(&finished_notice(0, task_id, "failed", 0)).unwrap();
    let message = record["message"].as_object_mut().unwrap();
    message.remove("timestamp");
    message["content"][0]["text"] = format!(
        "<background-task-finished>\n  <task task_id=\"{task_id}\" status=\"failed\" ended_at_ms=\"soon\"/>\n</background-task-finished>"
    )
    .into();
    record.to_string()
}

#[test]
fn an_undated_finished_notice_settles_the_task_without_inventing_a_time() {
    let lines = stream([
        promoted(T_LAST_TALK - 60_000, TASK),
        undated_finished_notice(TASK),
    ]);
    let facts = background_facts(
        &lines,
        Path::new("X:\\no-such-tasks-dir"),
        &launched(&lines),
    )
    .unwrap();
    assert_eq!(
        facts,
        vec![TaskStoreFact {
            task_id: TASK.into(),
            settled_at_ms: None,
            finished: true,
        }]
    );
    let dir = tempfile::tempdir().unwrap();
    let transcript = canonical_transcript(dir.path());
    std::fs::write(&transcript, &lines).unwrap();
    assert_eq!(
        count_pending_background_tasks("mvs_owned", dir.path()),
        Some(0)
    );
}

#[test]
fn an_undated_notice_does_not_erase_dated_terminal_evidence() {
    // Dated evidence first (a terminal receipt, then a store summary), the
    // undated notice after it. Replacing the dated fact with `None` hid a
    // valid stall: the task had been dated all along.
    let receipt = serde_json::json!({"message":{"role":"toolResult", "timestamp":T_END,
        "details":{"status":"failed","task_id":TASK},"content":[]}})
    .to_string();
    let dir = tempfile::tempdir().unwrap();
    let tasks = dir.path().join("background-tasks");
    std::fs::create_dir_all(tasks.join(TASK)).unwrap();
    std::fs::write(tasks.join(TASK).join("summary.txt"), "finished").unwrap();
    let summary_mtime = mtime_ms(&tasks.join(TASK).join("summary.txt")).unwrap();
    let tasks_dir = dir.path().join("none");
    for (dir_for_facts, expected) in [(&tasks_dir, T_END), (&tasks, summary_mtime)] {
        let mut records = vec![promoted(T_LAST_TALK - 60_000, TASK)];
        if expected == T_END {
            records.push(receipt.clone());
        }
        records.push(undated_finished_notice(TASK));
        let lines = stream(records);
        let facts = background_facts(&lines, dir_for_facts, &launched(&lines)).unwrap();
        assert_eq!(facts, vec![fact(TASK, expected, true)]);
    }
    // And the consequence the reviewer named: the dated fact still yields a
    // wake-up for a session that has not spoken since.
    let lines = stream([
        promoted(T_LAST_TALK - 60_000, TASK),
        assistant(T_LAST_TALK),
        receipt,
        undated_finished_notice(TASK),
    ]);
    let facts = background_facts(&lines, &tasks_dir, &launched(&lines)).unwrap();
    let stall =
        detect_background_task_stall(&launched(&lines), &facts, newest_assistant_at(&lines), NOW)
            .expect("stall");
    assert_eq!(stall.ended_at_ms, T_END);
}

#[test]
fn a_dated_notice_still_replaces_an_earlier_terminal_date() {
    // Only an undated update is refused the chance to erase a date; a dated
    // one keeps the existing latest-record-wins behaviour.
    let receipt = serde_json::json!({"message":{"role":"toolResult", "timestamp":T_END,
        "details":{"status":"failed","task_id":TASK},"content":[]}})
    .to_string();
    let lines = stream([
        promoted(T_LAST_TALK, TASK),
        receipt,
        finished_notice(T_END + 9_000, TASK, "failed", T_END + 8_000),
    ]);
    let facts = background_facts(
        &lines,
        Path::new("X:\\no-such-tasks-dir"),
        &launched(&lines),
    )
    .unwrap();
    assert_eq!(facts, vec![fact(TASK, T_END + 8_000, true)]);
}

#[test]
fn an_undated_terminal_fact_is_never_reported_as_a_stall() {
    // An undated fact has no ordering against the assistant. Dated `0` it
    // would be reported as a stall that "ended" at epoch 0 whenever the
    // session had said nothing, and hidden whenever it had; either way the
    // date was invented. Declining is the only claim the evidence supports.
    for assistant_records in [vec![], vec![assistant(T_LAST_TALK)]] {
        let mut records = vec![promoted(T_LAST_TALK - 60_000, TASK)];
        records.extend(assistant_records);
        records.push(undated_finished_notice(TASK));
        let lines = stream(records);
        let facts = background_facts(
            &lines,
            Path::new("X:\\no-such-tasks-dir"),
            &launched(&lines),
        )
        .unwrap();
        assert_eq!(
            detect_background_task_stall(
                &launched(&lines),
                &facts,
                newest_assistant_at(&lines),
                NOW
            ),
            None
        );
    }
}

#[test]
fn a_dated_notice_falls_back_to_the_record_timestamp_without_ended_at() {
    let mut record: serde_json::Value =
        serde_json::from_str(&finished_notice(T_END + 5_000, TASK, "failed", T_END)).unwrap();
    record["message"]["content"][0]["text"] = "<background-task-finished/>".into();
    let lines = stream([promoted(T_LAST_TALK, TASK), record.to_string()]);
    let facts = background_facts(
        &lines,
        Path::new("X:\\no-such-tasks-dir"),
        &launched(&lines),
    )
    .unwrap();
    assert_eq!(facts, vec![fact(TASK, T_END + 5_000, true)]);
}

#[test]
fn a_finished_notice_for_another_sessions_task_is_ignored() {
    let lines = stream([
        promoted(T_LAST_TALK, TASK),
        finished_notice(T_END, "bg_someone_elses", "failed", T_END),
    ]);
    let facts = background_facts(
        &lines,
        Path::new("X:\\no-such-tasks-dir"),
        &launched(&lines),
    )
    .unwrap();
    assert!(facts.is_empty());
}

#[test]
fn a_finished_notice_leaves_other_running_tasks_pending() {
    let other = "bg_still_running";
    let lines = stream([
        promoted(T_LAST_TALK - 2_000, TASK),
        promoted(T_LAST_TALK - 1_000, other),
        finished_notice(T_END, TASK, "failed", T_END),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let transcript = canonical_transcript(dir.path());
    std::fs::write(&transcript, &lines).unwrap();
    assert_eq!(
        count_pending_background_tasks("mvs_owned", dir.path()),
        Some(1)
    );
}

#[test]
fn a_finished_summary_settles_a_task_whose_output_is_still_recent() {
    // `summary.txt` is the CLI's own terminal marker, so no waiting for
    // quiescence is needed.
    let lines = stream([launch(T_LAST_TALK - 60_000, TASK), assistant(T_LAST_TALK)]);
    assert!(detect_background_task_stall(
        &launched(&lines),
        &[fact(TASK, T_END, true)],
        newest_assistant_at(&lines),
        T_END + 1_000,
    )
    .is_some());
}

#[test]
fn a_task_this_session_never_launched_is_ignored() {
    let lines = stream([assistant(T_LAST_TALK)]);
    assert_eq!(
        detect_background_task_stall(
            &launched(&lines),
            &[fact("bg_someone_elses", T_END, true)],
            newest_assistant_at(&lines),
            NOW,
        ),
        None
    );
}

#[test]
fn newest_finished_task_wins() {
    let lines = stream([launch(T_LAST_TALK - 60_000, TASK), assistant(T_LAST_TALK)]);
    let facts = [
        fact("bg_older", T_END, true),
        fact(TASK, T_END + 60_000, true),
    ];
    let stall =
        detect_background_task_stall(&launched(&lines), &facts, newest_assistant_at(&lines), NOW)
            .expect("stall");
    assert_eq!(stall.task_id, TASK);
}

#[test]
fn a_duplicate_launch_acknowledgement_is_one_task() {
    let lines = stream([launch(T_LAST_TALK, TASK), launch(T_LAST_TALK + 1, TASK)]);
    assert_eq!(launched(&lines).len(), 1);
}

#[test]
fn a_stall_with_no_assistant_message_at_all_is_reported() {
    let lines = launch(T_LAST_TALK, TASK);
    let stall = detect_background_task_stall(
        &launched(&lines),
        &[fact(TASK, T_END, true)],
        newest_assistant_at(&lines),
        NOW,
    )
    .expect("stall");
    assert_eq!(stall.last_assistant_at_ms, None);
}

#[test]
fn malformed_lines_are_skipped_without_panicking() {
    let lines = stream([
        "not json at all".to_string(),
        launch(T_LAST_TALK - 60_000, TASK),
        "{ broken".to_string(),
        assistant(T_LAST_TALK),
    ]);
    assert_eq!(launched(&lines).len(), 1);
    assert!(detect_background_task_stall(
        &launched(&lines),
        &[fact(TASK, T_END, true)],
        newest_assistant_at(&lines),
        NOW,
    )
    .is_some());
}

#[test]
fn unfinished_foreground_tool_calls_prevent_a_background_wake() {
    let call = serde_json::json!({"message":{"role":"assistant", "timestamp":T_LAST_TALK,
        "content":[{"type":"toolCall","id":"foreground","name":"bash","arguments":{}}]}})
    .to_string();
    assert_eq!(has_pending_tool_call(&call), Some(true));
    let result =
        serde_json::json!({"message":{"role":"toolResult","toolCallId":"foreground","content":[]}})
            .to_string();
    assert_eq!(has_pending_tool_call(&stream([call, result])), Some(false));
    assert_eq!(has_pending_tool_call("{partial"), None);
}

fn canonical_transcript(root: &Path) -> PathBuf {
    let path = root.join("v2/sessions/2026/10/24/session/messages.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path.with_file_name("manifest.json"),
        r#"{"sessionId":"mvs_owned"}"#,
    )
    .unwrap();
    path
}
