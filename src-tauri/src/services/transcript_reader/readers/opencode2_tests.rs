use super::*;
use crate::services::transcript_reader::{
    adapter::TranscriptAdapter,
    report_snapshot::{read_opencode2_file, ReportReadError},
};

fn store() -> (tempfile::TempDir, PathBuf, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("opencode.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE session_message(id TEXT PRIMARY KEY,session_id TEXT,type TEXT,seq INTEGER,data TEXT); CREATE TABLE session_part(message_id TEXT,session_id TEXT,ordinal INTEGER,type TEXT,call_id TEXT,data TEXT); CREATE TABLE message(id TEXT,session_id TEXT,time_created INTEGER,data TEXT); CREATE TABLE part(id TEXT,message_id TEXT,session_id TEXT,data TEXT);").unwrap();
    (dir, path, conn)
}

fn message(conn: &Connection, id: &str, typ: &str, seq: i64, data: Value) {
    conn.execute(
        "INSERT INTO session_message VALUES(?1,'ses_testSession',?2,?3,?4)",
        rusqlite::params![id, typ, seq, data.to_string()],
    )
    .unwrap();
}

#[test]
fn v2_inline_and_native_parts_are_ordered_and_v1_rows_never_leak() {
    let (_dir, path, conn) = store();
    message(&conn, "u", "user", 0, json!({"text":"hello"}));
    message(
        &conn,
        "a",
        "assistant",
        1,
        json!({"content":[{"type":"text","text":"old inline"}],"time":{"completed":100}}),
    );
    for (ordinal, text) in [(1, "second"), (0, "first")] {
        conn.execute(
            "INSERT INTO session_part VALUES('a','ses_testSession',?1,'text',NULL,?2)",
            rusqlite::params![ordinal, json!({"text":text}).to_string()],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO message VALUES('v1','ses_testSession',500,'{}')",
        [],
    )
    .unwrap();
    let parsed = OpenCode2Adapter
        .read_tail(&path, "ses_testSession", 10)
        .unwrap();
    assert!(!parsed.saw_malformed);
    assert_eq!(parsed.turns[0].text, "hello");
    assert!(
        parsed.turns[1].text.find("first").unwrap() < parsed.turns[1].text.find("second").unwrap()
    );
    assert!(!parsed.turns[1].text.contains("inline"));
    assert!(OpenCode2Adapter
        .read_tail(&path, "ses_otherSession", 10)
        .unwrap()
        .turns
        .is_empty());
}

#[test]
fn idle_is_not_a_report_and_newer_input_tools_or_corruption_fence_older_answers() {
    let (_dir, path, conn) = store();
    message(
        &conn,
        "a",
        "assistant",
        1,
        json!({"content":[{"type":"text","text":"Done"}],"time":{"completed":100}}),
    );
    message(
        &conn,
        "i",
        "idle",
        2,
        json!({"outcome":"succeeded","time":{"created":101}}),
    );
    let report = read_opencode2_file(&path, "ses_testSession").unwrap();
    assert_eq!(report.text, "Done");
    assert!(!report.turn_finished);
    message(&conn, "u", "user", 3, json!({"text":"next"}));
    assert!(!report.is_current());
    assert_eq!(
        read_opencode2_file(&path, "ses_testSession").unwrap_err(),
        ReportReadError::WorkInProgress
    );
    conn.execute("DELETE FROM session_message WHERE id='u'", [])
        .unwrap();
    message(
        &conn,
        "tool",
        "assistant",
        3,
        json!({"content":[{"type":"tool","name":"shell","state":{"input":{}}}],"time":{"completed":102}}),
    );
    assert_eq!(
        read_opencode2_file(&path, "ses_testSession").unwrap_err(),
        ReportReadError::WorkInProgress
    );
    conn.execute(
        "UPDATE session_message SET data='broken' WHERE id='tool'",
        [],
    )
    .unwrap();
    assert!(
        OpenCode2Adapter
            .read_tail(&path, "ses_testSession", 10)
            .unwrap()
            .saw_malformed
    );
    assert_eq!(
        read_opencode2_file(&path, "ses_testSession").unwrap_err(),
        ReportReadError::MalformedRecord
    );
    conn.execute("DELETE FROM session_message WHERE type!='idle'", [])
        .unwrap();
    assert!(read_opencode2_file(&path, "ses_testSession").is_err());
}

#[test]
fn unavailable_v2_schema_does_not_fall_back_to_v1() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v1.db");
    Connection::open(&path)
        .unwrap()
        .execute_batch("CREATE TABLE message(id TEXT);")
        .unwrap();
    assert!(matches!(
        OpenCode2Adapter.read_tail(&path, "ses_testSession", 1),
        Err(UnavailableReason::Unreadable)
    ));
}

#[test]
fn archive_discovery_is_scoped_and_stamps_v2_provenance() {
    let (_dir, path, conn) = store();
    conn.execute_batch("CREATE TABLE session_v2(id TEXT,directory TEXT,time_updated INTEGER,title TEXT,parent_id TEXT);").unwrap();
    for (id, directory) in [
        ("ses_root", "/repo"),
        ("ses_worktree", "/repo/.worktrees/task"),
        ("ses_sibling", "/repo-other"),
        ("ses_tracked", "/repo"),
    ] {
        conn.execute(
            "INSERT INTO session_v2 VALUES(?1,?2,1000,'title',NULL)",
            rusqlite::params![id, directory],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_message VALUES(?1,?2,'user',0,?3)",
            rusqlite::params![format!("u_{id}"), id, json!({"text":"hello"}).to_string()],
        )
        .unwrap();
    }
    let rows = discover(
        &path,
        "/repo",
        "/repo/.worktrees",
        &["ses_tracked".into()].into_iter().collect(),
    )
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .all(|r| r.provider.as_deref() == Some("opencode2") && r.resumable));
    assert!(rows
        .iter()
        .any(|r| r.worktree_name.as_deref() == Some("task")));
}
