//! Node Glance reads across the reader seam: every wired harness fixture, the
//! change-stamped cache, and the typed degrade for a store with no turns.
use super::test_support::fixture;
use super::*;

fn claude_line(role: &str, content: serde_json::Value, id: &str) -> String {
    let message = if role == "assistant" {
        serde_json::json!({"id": id, "role": role, "content": content})
    } else {
        serde_json::json!({"role": role, "content": content})
    };
    format!(
        "{}\n",
        serde_json::json!({"type": role, "message": message})
    )
}

#[test]
fn every_wired_file_reader_serves_a_glance_from_its_fixture() {
    let cases = [
        (
            TranscriptFormat::ClaudeCode,
            fixture("claude_code", "claude_code_transcript.jsonl"),
        ),
        (
            TranscriptFormat::Codex,
            fixture("codex", "codex_rollout_transcript.jsonl"),
        ),
        (
            TranscriptFormat::Cursor,
            fixture("cursor", "transcript.jsonl"),
        ),
        (
            TranscriptFormat::CommandCode,
            fixture("commandcode", "commandcode_transcript.jsonl"),
        ),
        (
            TranscriptFormat::Agy,
            fixture("agy", "agy_transcript.jsonl"),
        ),
        (
            TranscriptFormat::Grok,
            fixture("grok", "grok_chat_history.jsonl"),
        ),
        (
            TranscriptFormat::Muse,
            fixture("muse", "muse_transcript.jsonl"),
        ),
        (
            TranscriptFormat::Mcode,
            fixture("mcode", "transcript.jsonl"),
        ),
        (
            TranscriptFormat::Cline,
            fixture("cline", "cline_messages.json"),
        ),
    ];
    for (format, path) in cases {
        let glance = read_glance_at(format, &path, "session")
            .unwrap_or_else(|reason| panic!("{format:?} glance degraded: {reason:?}"));
        assert!(
            glance.last_message.is_some() || glance.last_prompt.is_some(),
            "{format:?} glance names neither a prompt nor a message"
        );
        assert!(
            glance.updated_at_ms.is_some(),
            "{format:?} glance has no write time"
        );
        // The glance uses the same parser as the log tail, so it must agree
        // with it on the latest assistant text.
        let tail = reader(format).read_tail(&path, "session", 200).unwrap();
        assert_eq!(
            glance.last_message,
            tail.last_assistant_message
                .map(|text| types::truncate(text.trim(), glance::GLANCE_MESSAGE_BYTES)),
            "{format:?}"
        );
    }
}

#[test]
fn a_claude_session_glance_names_the_prompt_activity_and_answer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let body = [
        claude_line("user", serde_json::json!("Run the test suite"), ""),
        claude_line(
            "assistant",
            serde_json::json!([{"type": "tool_use", "id": "t1", "name": "Bash",
                "input": {"command": "npm test", "description": "Run tests"}}]),
            "m1",
        ),
        claude_line(
            "user",
            serde_json::json!([{"type": "tool_result", "tool_use_id": "t1", "content": "ok"}]),
            "",
        ),
        claude_line(
            "assistant",
            serde_json::json!([{"type": "text", "text": "All 42 tests pass."}]),
            "m2",
        ),
    ]
    .concat();
    std::fs::write(&path, body).unwrap();
    let glance = read_glance_at(TranscriptFormat::ClaudeCode, &path, "s").unwrap();
    assert_eq!(glance.last_prompt.as_deref(), Some("Run the test suite"));
    assert_eq!(glance.last_message.as_deref(), Some("All 42 tests pass."));
    assert!(glance.message_after_prompt);
    assert_eq!(glance.tool_calls_since_prompt, 1);
    assert_eq!(
        glance.activity,
        vec![GlanceActivity {
            tool: "Bash".to_string(),
            target: Some("npm test".to_string()),
        }]
    );
}

#[test]
fn an_unchanged_store_is_served_from_the_cache_and_a_write_refreshes_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    std::fs::write(
        &path,
        claude_line("user", serde_json::json!("first task"), ""),
    )
    .unwrap();
    let first = read_glance_at(TranscriptFormat::ClaudeCode, &path, "s").unwrap();
    assert_eq!(first.last_prompt.as_deref(), Some("first task"));

    let key = (TranscriptFormat::ClaudeCode, path.clone(), "s".to_string());
    assert_eq!(
        glance_cache::get(&key, &glance_cache::Stamp::of(&path)),
        Some(first.clone()),
        "an unchanged store is a cache hit"
    );

    let mut body = std::fs::read_to_string(&path).unwrap();
    body.push_str(&claude_line("user", serde_json::json!("second task"), ""));
    std::fs::write(&path, body).unwrap();
    assert_eq!(
        glance_cache::get(&key, &glance_cache::Stamp::of(&path)),
        None,
        "a longer store is a cache miss"
    );
    let second = read_glance_at(TranscriptFormat::ClaudeCode, &path, "s").unwrap();
    assert_eq!(second.last_prompt.as_deref(), Some("second task"));
}

#[test]
fn a_store_without_turns_degrades_with_the_typed_reason() {
    assert_eq!(
        read_glance_at(
            TranscriptFormat::ClaudeCode,
            &fixture("claude_code", "claude_code_transcript_empty.jsonl"),
            "s"
        ),
        Err(UnavailableReason::Empty)
    );
    assert_eq!(
        read_glance_at(
            TranscriptFormat::ClaudeCode,
            &fixture("claude_code", "claude_code_transcript_shape_changed.jsonl"),
            "s"
        ),
        Err(UnavailableReason::ShapeChanged)
    );
    assert_eq!(
        read_glance(TranscriptFormat::ClaudeCode, None, "X:\\src\\buildmesh"),
        Err(UnavailableReason::NoSession)
    );
}

#[test]
fn a_long_session_is_read_through_a_bounded_window() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let old = claude_line("user", serde_json::json!("x".repeat(300 * 1024)), "");
    let recent = [
        claude_line("user", serde_json::json!("latest task"), ""),
        claude_line(
            "assistant",
            serde_json::json!([{"type": "text", "text": "On it."}]),
            "m9",
        ),
    ]
    .concat();
    std::fs::write(&path, format!("{old}{recent}")).unwrap();
    let glance = read_glance_at(TranscriptFormat::ClaudeCode, &path, "s").unwrap();
    assert_eq!(glance.last_prompt.as_deref(), Some("latest task"));
    assert_eq!(glance.last_message.as_deref(), Some("On it."));
}
