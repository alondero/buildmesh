//! Node Glance shaping rules over a parsed window.

use super::*;
use serde_json::json;

fn call(name: &str, input: serde_json::Value) -> ToolCall {
    ToolCall {
        name: name.to_string(),
        input,
    }
}

fn turn(role: &str, text: &str, tool_calls: Vec<ToolCall>) -> Turn {
    Turn {
        role: role.to_string(),
        text: text.to_string(),
        tool_calls,
    }
}

fn parsed(turns: Vec<Turn>) -> Parsed {
    let last_assistant_message = turns
        .iter()
        .rev()
        .find(|turn| turn.role == "assistant" && !turn.text.is_empty())
        .map(|turn| turn.text.clone());
    Parsed {
        turns,
        last_assistant_message,
        saw_malformed: false,
    }
}

#[test]
fn describes_the_common_tool_shapes_of_every_harness() {
    let cases = [
        (
            call(
                "Bash",
                json!({"command": "npm test\n  -- --run", "description": "Run tests"}),
            ),
            Some("npm test -- --run"),
        ),
        (
            call("exec_command", json!({"cmd": "cargo check"})),
            Some("cargo check"),
        ),
        (
            call("shell", json!({"command": ["bash", "-lc", "git status"]})),
            Some("git status"),
        ),
        (
            call("shell", json!({"command": ["rg", "-n", "glance"]})),
            Some("rg -n glance"),
        ),
        (
            call("Edit", json!({"file_path": "src/a.rs", "old_string": "x"})),
            Some("src/a.rs"),
        ),
        (
            call("edit", json!({"filePath": "src/b.ts"})),
            Some("src/b.ts"),
        ),
        (
            call("Grep", json!({"pattern": "fn main", "path": "src"})),
            Some("fn main"),
        ),
        (call("list_dir", json!({"path": "src"})), Some("src")),
        (
            call("WebFetch", json!({"url": "https://example.com"})),
            Some("https://example.com"),
        ),
        (
            call(
                "apply_patch",
                json!("*** Begin Patch\n*** Update File: a.rs\n*** Add File: b.rs\n*** End Patch"),
            ),
            Some("a.rs, b.rs"),
        ),
        (
            call(
                "apply_patch",
                json!({"input": "*** Begin Patch\n*** Delete File: old.rs\n*** End Patch"}),
            ),
            Some("old.rs"),
        ),
        (call("custom", json!({"input": "free text"})), None),
        (call("TodoWrite", json!({"todos": []})), None),
        (call("Task", json!({"description": "  "})), None),
    ];
    for (input, expected) in cases {
        assert_eq!(
            describe_tool_call(&input).target.as_deref(),
            expected,
            "{input:?}"
        );
    }
}

#[test]
fn a_command_outranks_its_description_and_working_directory() {
    let activity = describe_tool_call(&call(
        "Bash",
        json!({"description": "Run tests", "path": "/repo", "command": "npm test"}),
    ));
    assert_eq!(activity.tool, "Bash");
    assert_eq!(activity.target.as_deref(), Some("npm test"));
}

#[test]
fn a_long_target_is_one_bounded_line() {
    let long = "x ".repeat(400);
    let target = describe_tool_call(&call("Bash", json!({"command": long})))
        .target
        .unwrap();
    assert!(!target.contains('\n'));
    assert!(target.len() <= GLANCE_TARGET_BYTES + '…'.len_utf8());
}

#[test]
fn activity_counts_only_tool_calls_since_the_latest_prompt() {
    let window = parsed(vec![
        turn("user", "first task", vec![]),
        turn(
            "assistant",
            "",
            vec![call("Read", json!({"file_path": "old.rs"}))],
        ),
        turn("assistant", "Done with the first task.", vec![]),
        turn("user", "  run the tests  ", vec![]),
        turn(
            "assistant",
            "",
            vec![
                call("Read", json!({"file_path": "a.rs"})),
                call("Read", json!({"file_path": "b.rs"})),
            ],
        ),
        turn(
            "assistant",
            "",
            vec![call("Edit", json!({"file_path": "b.rs"}))],
        ),
        turn(
            "assistant",
            "",
            vec![call("Bash", json!({"command": "npm test"}))],
        ),
    ]);
    let glance = from_parsed(&window, Some(42), describe_tool_call);
    assert_eq!(glance.last_prompt.as_deref(), Some("run the tests"));
    assert_eq!(glance.tool_calls_since_prompt, 4);
    let targets: Vec<_> = glance
        .activity
        .iter()
        .map(|a| a.target.as_deref().unwrap())
        .collect();
    assert_eq!(
        targets,
        ["b.rs", "b.rs", "npm test"],
        "newest last, bounded to three"
    );
    assert_eq!(
        glance.last_message.as_deref(),
        Some("Done with the first task.")
    );
    assert!(
        !glance.message_after_prompt,
        "the message answers the previous prompt"
    );
    assert_eq!(glance.updated_at_ms, Some(42));
}

#[test]
fn an_answer_after_the_prompt_is_marked_current() {
    let window = parsed(vec![
        turn("user", "fix it", vec![]),
        turn("assistant", "Fixed in a.rs.", vec![]),
    ]);
    let glance = from_parsed(&window, None, describe_tool_call);
    assert!(glance.message_after_prompt);
    assert!(glance.activity.is_empty());
}

#[test]
fn a_window_without_its_prompt_still_reports_activity() {
    let window = parsed(vec![turn(
        "assistant",
        "Working through the files.",
        vec![call("Read", json!({"file_path": "a.rs"}))],
    )]);
    let glance = from_parsed(&window, None, describe_tool_call);
    assert_eq!(glance.last_prompt, None);
    assert_eq!(glance.tool_calls_since_prompt, 1);
    assert!(glance.message_after_prompt);
}

#[test]
fn the_reader_describer_applies_to_every_activity_line() {
    let window = parsed(vec![turn("assistant", "", vec![call("x", json!({}))])]);
    let glance = from_parsed(&window, None, |call| GlanceActivity {
        tool: format!("custom:{}", call.name),
        target: None,
    });
    assert_eq!(glance.activity[0].tool, "custom:x");
}
