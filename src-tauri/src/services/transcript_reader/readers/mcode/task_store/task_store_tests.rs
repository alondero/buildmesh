use super::super::{LaunchedTask, TaskStoreFact};
use super::reconcile;

#[test]
fn native_task_evidence_requires_owner_terminal_status_and_end_time() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("messages.jsonl");
    let database = dir.path().join("v2/sqlite/runtime-state.sqlite");
    std::fs::create_dir_all(database.parent().unwrap()).unwrap();
    std::fs::write(
        transcript.with_file_name("manifest.json"),
        r#"{"sessionId":"mvs_owned"}"#,
    )
    .unwrap();
    let conn = rusqlite::Connection::open(&database).unwrap();
    conn.execute_batch(
        "CREATE TABLE local_runtime_background_tasks (
        task_id TEXT PRIMARY KEY, owner_session_id TEXT NOT NULL,
        status TEXT NOT NULL, created_at_ms INTEGER NOT NULL, ended_at_ms INTEGER);
        INSERT INTO local_runtime_background_tasks VALUES ('bg_task','mvs_owned','failed',40,300);",
    )
    .unwrap();
    let launched = [LaunchedTask {
        task_id: "bg_task".into(),
        started_at_ms: 100,
    }];
    let mut facts = vec![];
    reconcile(&transcript, &database, &launched, &mut facts).unwrap();
    assert_eq!(
        facts,
        vec![TaskStoreFact {
            task_id: "bg_task".into(),
            settled_at_ms: Some(300),
            finished: true,
        }]
    );
    // Native startup recovery ends interrupted tasks as lost, rather than
    // leaving them running or publishing another transcript tool receipt.
    conn.execute(
        "UPDATE local_runtime_background_tasks SET status='lost'",
        [],
    )
    .unwrap();
    reconcile(&transcript, &database, &launched, &mut facts).unwrap();
    assert!(facts[0].finished, "a dated lost task has terminated");
    for (status, ended) in [
        ("running", Some(300)),
        ("unknown", Some(300)),
        ("lost", None),
        ("lost", Some(30)),
        ("failed", None),
        ("failed", Some(30)),
    ] {
        conn.execute(
            "UPDATE local_runtime_background_tasks SET status=?1,ended_at_ms=?2",
            rusqlite::params![status, ended],
        )
        .unwrap();
        // A stale summary must not overrule the live registry's running row.
        facts[0].finished = true;
        reconcile(&transcript, &database, &launched, &mut facts).unwrap();
        assert!(!facts[0].finished, "{status}/{ended:?}");
    }
    // A fast task can finish before the transcript acknowledgement at t=100.
    conn.execute(
        "UPDATE local_runtime_background_tasks SET status='failed',ended_at_ms=50",
        [],
    )
    .unwrap();
    reconcile(&transcript, &database, &launched, &mut facts).unwrap();
    assert!(facts[0].finished);
    conn.execute("UPDATE local_runtime_background_tasks SET owner_session_id='mvs_other',status='failed',ended_at_ms=300", []).unwrap();
    facts = vec![TaskStoreFact {
        task_id: "bg_task".into(),
        settled_at_ms: Some(900),
        finished: true,
    }];
    let summary_fact = facts.clone();
    reconcile(&transcript, &database, &launched, &mut facts).unwrap();
    assert_eq!(
        facts, summary_fact,
        "another session cannot erase summary evidence"
    );
    drop(conn);
    std::fs::write(&database, b"not sqlite").unwrap();
    assert_eq!(
        reconcile(&transcript, &database, &launched, &mut facts),
        None
    );
}

#[test]
fn an_existing_database_without_the_task_table_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("v2/sqlite/runtime-state.sqlite");
    std::fs::create_dir_all(database.parent().unwrap()).unwrap();
    drop(rusqlite::Connection::open(&database).unwrap());
    let transcript = dir.path().join("messages.jsonl");
    std::fs::write(
        transcript.with_file_name("manifest.json"),
        r#"{"sessionId":"mvs_owned"}"#,
    )
    .unwrap();
    let launched = [LaunchedTask {
        task_id: "bg_task".into(),
        started_at_ms: 100,
    }];
    let mut facts = vec![TaskStoreFact {
        task_id: "bg_task".into(),
        settled_at_ms: Some(300),
        finished: true,
    }];
    assert_eq!(
        reconcile(
            &transcript,
            &dir.path().join("v2/sqlite/runtime-state.sqlite"),
            &launched,
            &mut facts
        ),
        None
    );
}

#[test]
fn missing_native_store_preserves_legacy_evidence_without_creating_a_database() {
    let dir = tempfile::tempdir().unwrap();
    let launched = [LaunchedTask {
        task_id: "bg_task".into(),
        started_at_ms: 100,
    }];
    let mut facts = vec![TaskStoreFact {
        task_id: "bg_task".into(),
        settled_at_ms: Some(300),
        finished: true,
    }];
    let previous = facts.clone();
    reconcile(
        &dir.path().join("messages.jsonl"),
        &dir.path().join("v2/sqlite/runtime-state.sqlite"),
        &launched,
        &mut facts,
    )
    .unwrap();
    assert_eq!(facts, previous);
    assert!(!dir.path().join("v2").exists());
    let database = dir.path().join("v2/sqlite/runtime-state.sqlite");
    std::fs::create_dir_all(&database).unwrap();
    assert_eq!(
        reconcile(
            &dir.path().join("messages.jsonl"),
            &dir.path().join("v2/sqlite/runtime-state.sqlite"),
            &launched,
            &mut facts
        ),
        None,
        "an existing invalid store must not permit the legacy fallback"
    );
}
