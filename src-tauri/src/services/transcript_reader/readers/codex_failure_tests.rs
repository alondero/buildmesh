use super::*;

#[test]
fn failed_native_turn_is_never_successful_completion() {
    for code in ["server_overloaded", "cyber_policy"] {
        let record = serde_json::json!({
            "timestamp":"2026-11-05T08:30:19.103Z", "type":"event_msg",
            "payload":{"type":"task_complete","turn_id":"turn-6559",
                "last_agent_message":null,
                "error":{"message":"native failure", "codex_error_info":code}}
        });
        assert!(
            CodexAdapter.completed_turn(&record.to_string()).is_none(),
            "{code} must not satisfy a Circuit's successful completion"
        );
    }
}
