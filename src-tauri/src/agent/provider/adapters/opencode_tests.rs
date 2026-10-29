//! OpenCode child sessions as Circuit owned work (issue #2636).
use super::*;

// Real ids from a controlled OpenCode 1.18.3 run: a `task` tool call created
// the child session under the parent, then the child went idle.
const PARENT: &str = "ses_e77a04977ffea25PMbbvyAZJYU";
const CHILD: &str = "ses_e77a02cc6ffeHjnk2f6NIW0LE8";

fn parse_hook(value: serde_json::Value) -> Option<crate::circuit::strategy::NativeHook> {
    OPENCODE.circuit_observation().parse_hook(&value)
}

#[test]
fn child_sessions_are_declared_owned_work_without_a_foreground_source() {
    use crate::circuit::strategy::{OwnedWorkKind, PushKind, TurnIdentity};
    let strategy = OPENCODE.circuit_observation();
    let hooks = strategy.hooks().expect("child-session hooks are declared");
    assert!(
        !hooks.foreground,
        "the parser reports child sessions only; it never reports a foreground turn"
    );
    assert!(
        !strategy.has_foreground_source(),
        "a child-only parser must not make the foreground read as available"
    );
    assert!(
        hooks.ownership_gap.is_none(),
        "child events can name owned work, so a missing registry is unknown, not a validated gap"
    );
    let coverage = strategy.coverage();
    assert_eq!(coverage.push, PushKind::Hooks);
    assert_eq!(coverage.owned_work, OwnedWorkKind::HookRegistry);
    assert_eq!(coverage.turn_identity, TurnIdentity::None);
}

#[test]
fn child_start_and_stop_become_owned_work_receipts_fenced_to_the_parent() {
    for (wire, native) in [
        ("subagent_start", "SubagentStart"),
        ("subagent_stop", "SubagentStop"),
    ] {
        let hook = parse_hook(serde_json::json!({
            "hook_event_name": wire,
            "sessionID": PARENT,
            "agent_id": CHILD,
        }))
        .unwrap_or_else(|| panic!("{wire} parses"));
        assert_eq!(hook.event, native);
        assert_eq!(hook.session_id.as_deref(), Some(PARENT), "{wire}");
        assert_eq!(hook.child_id.as_deref(), Some(CHILD), "{wire}");
        assert_eq!(hook.provider.as_deref(), Some("opencode"), "{wire}");
        assert_eq!(hook.turn_id, None, "{wire}: OpenCode names no turn");
        assert_eq!(hook.active_work, None, "{wire}: no registry snapshot");
        assert_eq!(hook.final_report, None, "{wire}");
        assert!(!hook.background_busy, "{wire}");
    }
}

#[test]
fn child_payloads_without_a_valid_parent_and_child_identity_are_rejected() {
    let wrong_shape = [
        // No child id: nothing to own.
        serde_json::json!({"hook_event_name":"subagent_start","sessionID":PARENT}),
        serde_json::json!({"hook_event_name":"subagent_start","sessionID":PARENT,"agent_id":""}),
        // No parent: the receipt could not be fenced to a session.
        serde_json::json!({"hook_event_name":"subagent_start","agent_id":CHILD}),
        // Not OpenCode session ids.
        serde_json::json!({"hook_event_name":"subagent_start","sessionID":"not-a-session","agent_id":CHILD}),
        serde_json::json!({"hook_event_name":"subagent_start","sessionID":PARENT,"agent_id":"550e8400-e29b-41d4-a716-446655440000"}),
        // A session cannot own itself.
        serde_json::json!({"hook_event_name":"subagent_start","sessionID":PARENT,"agent_id":PARENT}),
    ];
    for payload in wrong_shape {
        assert!(parse_hook(payload.clone()).is_none(), "{payload}");
    }
}

#[test]
fn only_child_events_parse_never_a_foreground_turn() {
    // The foreground stays unwired: none of the lifecycle events the plugin
    // already forwards, nor another harness's vocabulary, may become a native
    // receipt.
    for event in [
        "session.created",
        "session.idle",
        "session.busy",
        "permission.asked",
        "question.asked",
        "Stop",
        "UserPromptSubmit",
    ] {
        assert!(
            parse_hook(serde_json::json!({
                "hook_event_name": event,
                "sessionID": PARENT,
                "agent_id": CHILD,
            }))
            .is_none(),
            "{event} must not become an OpenCode native receipt"
        );
    }
}

#[test]
fn the_receipt_parser_reads_event_names_the_way_the_attention_route_does() {
    // The route lowercases and drops underscores before it tracks a child in
    // `HookState`. If the receipt parser were stricter, the route could mark a
    // child running for a spelling that never reaches the Circuit.
    for spelling in ["subagent_start", "SubagentStart", "SUBAGENT_START"] {
        let hook = parse_hook(serde_json::json!({
            "hook_event_name": spelling,
            "sessionID": PARENT,
            "agent_id": CHILD,
        }))
        .unwrap_or_else(|| panic!("{spelling} parses"));
        assert_eq!(hook.event, "SubagentStart", "{spelling}");
    }
}
