use super::*;

#[test]
fn messages_map_text_and_tools_onto_turns() {
    let messages = vec![
        serde_json::json!({
            "info": {"role": "user"},
            "parts": [{"type": "text", "text": "Fix login"}],
        }),
        serde_json::json!({
            "info": {"role": "assistant"},
            "parts": [
                {"type": "reasoning", "text": "thinking"},
                {"type": "text", "text": "Looking"},
                {"type": "tool", "state": {"title": "read_file", "input": {"path": "a.rs"}}},
            ],
        }),
    ];

    let parsed = parse_opencode_messages(&messages, 10);

    assert!(!parsed.saw_malformed);
    assert_eq!(parsed.turns.len(), 2);
    assert_eq!(parsed.turns[0].role, "user");
    assert_eq!(parsed.turns[0].text, "Fix login");
    assert_eq!(parsed.turns[1].text, "Looking");
    assert_eq!(parsed.turns[1].tool_calls.len(), 1);
    assert_eq!(parsed.turns[1].tool_calls[0].name, "read_file");
    assert_eq!(parsed.last_assistant_message.as_deref(), Some("Looking"));
}

#[test]
fn structural_breaks_flag_malformed_and_unknown_parts_stay_quiet() {
    let messages = vec![
        serde_json::json!({"info": {"role": "system"}, "parts": []}),
        serde_json::json!({"parts": []}),
        serde_json::json!({"info": {"role": "assistant"}, "parts": [{"type": "step-start"}]}),
    ];

    let parsed = parse_opencode_messages(&messages, 10);

    assert!(parsed.saw_malformed);
    assert!(parsed.turns.is_empty());
    assert!(parsed.last_assistant_message.is_none());
}
