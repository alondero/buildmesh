use super::*;

#[test]
fn minimax_wake_reads_work_before_tail_window() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join("v2/sessions/2026/10/24/session/messages.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let padding = serde_json::json!({"message":{"role":"custom","content":[
        {"type":"text","text":"x".repeat(WAKEUP_TAIL_BYTES as usize + 1024)}]}});
    let recent = concat!(
        "{\"message\":{\"role\":\"toolResult\",\"timestamp\":200,\"details\":{\"status\":\"auto_promoted\",\"task_id\":\"bg_review_recent\"},\"content\":[]}}\n",
        "{\"message\":{\"role\":\"toolResult\",\"timestamp\":300,\"details\":{\"status\":\"succeeded\",\"task_id\":\"bg_review_recent\"},\"content\":[]}}\n"
    );
    for (pending, settled) in [
        (
            serde_json::json!({"message":{"role":"toolResult","timestamp":50,
                "details":{"status":"started","task_id":"bg_review_older"},"content":[]}}),
            serde_json::json!({"message":{"role":"toolResult","timestamp":100,
                "details":{"status":"succeeded","task_id":"bg_review_older"},"content":[]}}),
        ),
        (
            serde_json::json!({"message":{"role":"assistant","timestamp":50,
                "content":[{"type":"toolCall","id":"foreground","name":"bash","arguments":{}}]}}),
            serde_json::json!({"message":{"role":"toolResult","timestamp":100,
                "toolCallId":"foreground","content":[]}}),
        ),
    ] {
        std::fs::write(&path, format!("{pending}\n{padding}\n{recent}")).unwrap();
        assert_eq!(
            stalled_background_task_at(TranscriptFormat::Mcode, &path, 400),
            None
        );
        std::fs::write(&path, format!("{pending}\n{settled}\n{padding}\n{recent}")).unwrap();
        assert_eq!(
            stalled_background_task_at(TranscriptFormat::Mcode, &path, 400),
            Some("bg_review_recent".into())
        );
    }
}
