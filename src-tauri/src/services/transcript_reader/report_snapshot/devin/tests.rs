use super::*;

#[test]
fn a_tool_call_at_the_main_chain_head_is_not_a_final_reply() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sessions.db");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            r#"CREATE TABLE sessions (id TEXT PRIMARY KEY, main_chain_id INTEGER);
               CREATE TABLE message_nodes (
                   session_id TEXT, node_id INTEGER, chat_message TEXT,
                   UNIQUE(session_id, node_id)
               );
               INSERT INTO sessions VALUES ('active-fox', 2);
               INSERT INTO message_nodes VALUES
                 ('active-fox', 1, '{"role":"assistant","message_id":"earlier",
                     "content":"The previous task is done.",
                     "metadata":{"finish_reason":"stop","created_at":"2026-11-26T23:12:00Z"}}'),
                 ('active-fox', 2, '{"role":"assistant","message_id":"current",
                     "content":"Running a check","tool_calls":[{"id":"call_1"}],
                     "metadata":{"finish_reason":"stop","created_at":"2026-11-26T23:12:01Z"}}');"#,
        )
        .unwrap();
    assert_eq!(read(&path, "active-fox"), Err(E::WorkInProgress));
}
