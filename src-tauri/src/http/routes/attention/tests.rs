fn apply_status_fixture(
    state: &mut crate::agent::hook_state::HookState,
    value: serde_json::Value,
) -> Decision {
    let body = value.to_string();
    let payload = HookPayload::parse(body.as_bytes()).unwrap();
    let classified = classify(body.as_bytes(), "anthropic", |_| Some(0));
    let accept = accept_hook(state, &payload, &classified);
    if accept.accepted {
        lifecycle_decision(classified.decision, state, false, accept)
    } else {
        Decision::Ignore
    }
}

#[test]
fn node_waits_for_native_children_after_foreground_stop() {
    let mut state = crate::agent::hook_state::HookState::default();
    assert_eq!(
        apply_status_fixture(
            &mut state,
            serde_json::json!({"hook_event_name":"UserPromptSubmit", "prompt_id":"turn-1"})
        ),
        Decision::Running
    );
    for child in ["child-a", "child-b"] {
        apply_status_fixture(
            &mut state,
            serde_json::json!({"hook_event_name":"SubagentStart", "agent_id":child, "prompt_id":"turn-1"}),
        );
    }
    assert_eq!(
        apply_status_fixture(
            &mut state,
            serde_json::json!({"hook_event_name":"Stop", "prompt_id":"turn-1"})
        ),
        Decision::SuppressPendingBackground
    );
    assert_eq!(
        apply_status_fixture(
            &mut state,
            serde_json::json!({"hook_event_name":"SubagentStop", "agent_id":"child-a", "prompt_id":"turn-1"})
        ),
        Decision::Ignore
    );
    assert_eq!(
        apply_status_fixture(
            &mut state,
            serde_json::json!({"hook_event_name":"SubagentStop", "agent_id":"child-b", "prompt_id":"turn-1"})
        ),
        Decision::Ready
    );
}

#[test]
fn old_child_completion_does_not_end_a_new_foreground_turn() {
    let mut state = crate::agent::hook_state::HookState::default();
    for value in [
        serde_json::json!({"hook_event_name":"UserPromptSubmit", "prompt_id":"turn-1"}),
        serde_json::json!({"hook_event_name":"SubagentStart", "agent_id":"child", "prompt_id":"turn-1"}),
        serde_json::json!({"hook_event_name":"Stop", "prompt_id":"turn-1"}),
        serde_json::json!({"hook_event_name":"UserPromptSubmit", "prompt_id":"turn-2"}),
    ] {
        apply_status_fixture(&mut state, value);
    }
    assert_eq!(
        apply_status_fixture(
            &mut state,
            serde_json::json!({"hook_event_name":"SubagentStop", "agent_id":"child", "prompt_id":"turn-1"})
        ),
        Decision::Ignore
    );
    assert_eq!(
        apply_status_fixture(
            &mut state,
            serde_json::json!({"hook_event_name":"Stop", "prompt_id":"turn-2"})
        ),
        Decision::Ready
    );
}

#[test]
fn codex_answer_resumes_after_question_tool_result() {
    let mut state = crate::agent::hook_state::HookState::default();
    for (event, expected) in [
        ("PreToolUse", Decision::MarkInput),
        ("PostToolUse", Decision::Running),
    ] {
        let body = serde_json::json!({"hook_event_name":event,
            "tool_name":"request_user_input", "tool_use_id":"question-1"})
        .to_string();
        let payload = HookPayload::parse(body.as_bytes()).unwrap();
        let classified = classify(body.as_bytes(), "codex", |_| Some(0));
        let permission_pending = state.has_permission_requests();
        let accepted = accept_hook(&mut state, &payload, &classified);
        assert!(accepted.accepted);
        assert_eq!(
            lifecycle_decision(classified.decision, &state, permission_pending, accepted),
            expected
        );
    }
    assert!(!state.has_questions());
}

use super::common::*;
use super::normalizers::resolve_attention_node;
use super::ordering::*;
use super::*;
use crate::agent::session_lifecycle::LifecycleKind;
use std::path::Path;

#[test]
fn stale_and_current_native_stops_follow_the_turn_fence() {
    use crate::services::circuit_worker::native_hooks::{NativeHook, NativeReceipt};

    let _db = crate::db::test_support::isolated();
    let unique = format!(
        "issue1905-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let (node_id, run_id) = {
        let db = crate::db::write_conn();
        db.execute(
            "INSERT INTO meshes (name, path) VALUES (?1, ?2)",
            rusqlite::params![unique, format!("/tmp/{unique}")],
        )
        .unwrap();
        let mesh_id = db.last_insert_rowid();
        db.execute(
            "INSERT INTO agent_nodes (mesh_id, name, path, provider, status, session_started_at)
             VALUES (?1, ?2, ?3, 'codex', 'awaiting_input', 1000)",
            rusqlite::params![mesh_id, format!("node-{unique}"), format!("/tmp/{unique}")],
        )
        .unwrap();
        let node_id = db.last_insert_rowid();
        let graph = crate::circuit::model::CircuitGraph::walking_skeleton("work")
            .to_json()
            .unwrap();
        db.execute(
            "INSERT INTO autopilot_circuits (mesh_id, name, graph_json) VALUES (?1, ?2, ?3)",
            rusqlite::params![mesh_id, format!("circuit-{unique}"), graph],
        )
        .unwrap();
        let circuit_id = db.last_insert_rowid();
        db.execute(
            "INSERT INTO autopilot_circuit_runs
                (circuit_id, mesh_id, trigger_identity, state, source_agent_node_id)
             VALUES (?1, ?2, ?3, 'running', ?4)",
            rusqlite::params![circuit_id, mesh_id, unique, node_id],
        )
        .unwrap();
        let run_id = db.last_insert_rowid();
        db.execute(
            "INSERT INTO autopilot_circuit_run_steps
                (run_id, node_id, attempt, status, agent_node_id)
             VALUES (?1, 'spawn', 1, 'running', ?2)",
            rusqlite::params![run_id, node_id],
        )
        .unwrap();
        (node_id, run_id)
    };

    let body = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "codex-session",
        "turn_id": "old-turn",
        "last_assistant_message": "stale completion must not update the current node"
    })
    .to_string()
    .into_bytes();
    let payload = HookPayload::parse(&body).unwrap();
    let classified = classify(&body, "codex", |_| Some(0));
    assert_eq!(classified.decision, Decision::Ready);
    let native_hook = NativeHook::parse("codex", &body).unwrap();
    let mut state = crate::agent::hook_state::HookState::default();
    assert!(state.accepts(Some("current-turn"), true));

    let mut lifecycle_projection_called = false;
    let applied = apply_hook_after_turn_fence(
        node_id,
        &mut state,
        Some(&payload),
        &classified,
        Some(&native_hook),
        |_, _, _| {
            lifecycle_projection_called = true;
            Ok(Applied::Applied)
        },
    )
    .unwrap();
    assert!(matches!(applied, Applied::StaleDropped));
    assert!(
        !lifecycle_projection_called,
        "a rejected turn must return before accepted-hook projection"
    );

    let history = crate::db::circuit::evidence::native_hook_receipts(run_id, 0).unwrap();
    assert_eq!(history.len(), 1, "the rejected native event is durable");
    let receipt: NativeReceipt = serde_json::from_str(&history[0].detail).unwrap();
    assert_eq!(receipt.hook.event, "Stop");
    assert!(!receipt.turn_fenced);
    assert!(receipt.explicit_turn_mismatch);

    let current_body = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "codex-session",
        "turn_id": "current-turn",
        "last_assistant_message": "current turn completion"
    })
    .to_string()
    .into_bytes();
    let current_payload = HookPayload::parse(&current_body).unwrap();
    let current_classified = classify(&current_body, "codex", |_| Some(0));
    let current_native_hook = NativeHook::parse("codex", &current_body).unwrap();
    let mut accepted_projection_called = false;
    let accepted = apply_hook_after_turn_fence(
        node_id,
        &mut state,
        Some(&current_payload),
        &current_classified,
        Some(&current_native_hook),
        |state, hook, _| {
            accepted_projection_called = true;
            let hook = hook.expect("matching Stop has a native receipt");
            crate::services::circuit_worker::native_hooks::receive(
                node_id,
                hook.clone(),
                state.matches_turn(hook.turn_id.as_deref()),
                state.mismatches_turn(hook.turn_id.as_deref()),
            )?;
            Ok(Applied::Applied)
        },
    )
    .unwrap();
    assert!(matches!(accepted, Applied::Applied));
    assert!(
        accepted_projection_called,
        "a matching current-turn Stop reaches accepted-hook projection"
    );

    let history = crate::db::circuit::evidence::native_hook_receipts(run_id, 0).unwrap();
    assert_eq!(
        history.len(),
        2,
        "both stale and matching Stop receipts are durable"
    );
    let current_receipt: NativeReceipt = serde_json::from_str(&history[1].detail).unwrap();
    assert_eq!(current_receipt.hook.event, "Stop");
    assert!(current_receipt.turn_fenced);
    assert!(!current_receipt.explicit_turn_mismatch);

    let db = crate::db::read_conn();
    let status: String = db
        .query_row(
            "SELECT status FROM agent_nodes WHERE id = ?1",
            [node_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        status, "awaiting_input",
        "stale Stop must skip Ready projection"
    );
}

#[test]
fn native_question_resolution_unblocks_completion_and_old_turns_stay_stale() {
    let mut state = crate::agent::hook_state::HookState::default();
    let apply = |state: &mut crate::agent::hook_state::HookState, value: serde_json::Value| {
        let body = value.to_string();
        let payload = HookPayload::parse(body.as_bytes()).unwrap();
        let classified = classify(body.as_bytes(), "opencode", |_| Some(0));
        accept_hook(state, &payload, &classified).accepted
    };
    assert!(apply(
        &mut state,
        serde_json::json!({"hook_event_name":"UserPromptSubmit", "promptId":"first"})
    ));
    assert!(apply(
        &mut state,
        serde_json::json!({"hook_event_name":"question.asked", "request_id":"one"})
    ));
    assert!(apply(
        &mut state,
        serde_json::json!({"hook_event_name":"question.asked", "request_id":"two"})
    ));
    assert!(!apply(
        &mut state,
        serde_json::json!({"hook_event_name":"session.idle"})
    ));
    let reply_body = serde_json::json!({
        "hook_event_name":"question.replied", "request_id":"one"
    })
    .to_string();
    let reply_payload = HookPayload::parse(reply_body.as_bytes()).unwrap();
    let reply_classified = classify(reply_body.as_bytes(), "opencode", |_| Some(0));
    let reply = accept_hook(&mut state, &reply_payload, &reply_classified);
    let published = lifecycle_decision(reply_classified.decision, &state, false, reply);
    assert!(
        reply.accepted,
        "the reply is accepted rather than dropped as stale"
    );
    assert!(reply.preserve_attention);
    assert_eq!(published, Decision::Ignore);
    assert!(state.has_foreground_question("two"));
    assert!(
        !apply(
            &mut state,
            serde_json::json!({"hook_event_name":"session.idle"})
        ),
        "turn completion stays fenced while question two is open"
    );
    assert!(apply(
        &mut state,
        serde_json::json!({"hook_event_name":"question.rejected", "request_id":"two"})
    ));
    assert!(apply(
        &mut state,
        serde_json::json!({"hook_event_name":"session.idle"})
    ));
    assert!(apply(
        &mut state,
        serde_json::json!({"hook_event_name":"UserPromptSubmit", "promptId":"second"})
    ));
    assert!(!apply(
        &mut state,
        serde_json::json!({"hook_event_name":"Stop", "promptId":"first"})
    ));
    assert!(apply(
        &mut state,
        serde_json::json!({"hook_event_name":"Stop"})
    ));
}

#[test]
fn opencode_permission_reply_releases_the_same_request() {
    let mut state = crate::agent::hook_state::HookState::default();
    let apply = |state: &mut crate::agent::hook_state::HookState, value: serde_json::Value| {
        let body = value.to_string();
        let payload = HookPayload::parse(body.as_bytes()).unwrap();
        let classified = classify(body.as_bytes(), "opencode", |_| Some(0));
        (
            accept_hook(state, &payload, &classified).accepted,
            classified.decision,
        )
    };
    assert_eq!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"UserPromptSubmit", "promptId":"turn-1"
            })
        ),
        (true, Decision::Running)
    );
    assert_eq!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"permission.asked", "request_id":"perm-1", "tool_name":"Bash"
            })
        ),
        (true, Decision::MarkInput)
    );
    assert_eq!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"permission.replied", "request_id":"perm-1"
            })
        ),
        (true, Decision::Running)
    );
    assert_eq!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"session.idle"
            })
        ),
        (true, Decision::Ready)
    );
}

#[test]
fn opencode_auto_reply_that_arrives_before_the_ask_does_not_raise_a_prompt() {
    let mut state = crate::agent::hook_state::HookState::default();
    let apply = |state: &mut crate::agent::hook_state::HookState, value: serde_json::Value| {
        let body = value.to_string();
        let payload = HookPayload::parse(body.as_bytes()).unwrap();
        let classified = classify(body.as_bytes(), "opencode", |_| Some(0));
        let accept = accept_hook(state, &payload, &classified);
        (accept, classified.decision)
    };
    assert!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"permission.replied", "request_id":"per_external"
            })
        )
        .0
        .accepted
    );
    let (accept, decision) = apply(
        &mut state,
        serde_json::json!({
            "hook_event_name":"permission.asked",
            "request_id":"per_external",
            "message":"OpenCode is asking for permission: external_directory (F:\\tmp\\*)"
        }),
    );
    assert!(accept.accepted);
    assert_eq!(
        decision,
        Decision::MarkInput,
        "the ask itself is still a permission event"
    );
    assert!(
        !state.has_foreground_questions(),
        "the earlier reply already satisfied this ask"
    );
    assert_eq!(
        lifecycle_decision(decision, &state, false, accept),
        Decision::Ignore,
        "an ask that was already answered must not change the node's lifecycle"
    );
}

#[test]
fn late_permission_ask_after_idle_does_not_resume_a_finished_turn() {
    let mut state = crate::agent::hook_state::HookState::default();
    let apply = |state: &mut crate::agent::hook_state::HookState, value: serde_json::Value| {
        let body = value.to_string();
        let payload = HookPayload::parse(body.as_bytes()).unwrap();
        let classified = classify(body.as_bytes(), "opencode", |_| Some(0));
        let accept = accept_hook(state, &payload, &classified);
        (
            accept.accepted,
            classified.decision,
            lifecycle_decision(classified.decision, state, false, accept),
        )
    };
    assert_eq!(
        apply(
            &mut state,
            serde_json::json!({"hook_event_name":"permission.replied", "request_id":"per_external"})
        ),
        (true, Decision::Running, Decision::Running)
    );
    assert!(state.has_early_reply("per_external"));
    assert_eq!(
        apply(
            &mut state,
            serde_json::json!({"hook_event_name":"session.idle"})
        ),
        (true, Decision::Ready, Decision::Ready)
    );
    assert!(
        !state.has_early_reply("per_external"),
        "idle completes the turn and drops its unmatched reply"
    );
    let (accepted, decision, published) = apply(
        &mut state,
        serde_json::json!({"hook_event_name":"permission.asked", "request_id":"per_external"}),
    );
    assert!(accepted);
    assert_eq!(decision, Decision::MarkInput);
    assert_eq!(published, Decision::MarkInput);
    assert!(!matches!(published, Decision::Running));
}

#[test]
fn early_permission_reply_does_not_dismiss_a_different_question() {
    let mut state = crate::agent::hook_state::HookState::default();
    // The fence is what production uses. Its closure sees the `Accept`
    // `accept_hook` returned, and the decision is computed from that
    // value. A rejected callback never enters the closure.
    let publish = |state: &mut crate::agent::hook_state::HookState, value: serde_json::Value| {
        let body = value.to_string();
        let payload = HookPayload::parse(body.as_bytes()).unwrap();
        let classified = classify(body.as_bytes(), "opencode", |_| Some(0));
        let mut seen = None;
        let applied = apply_hook_after_turn_fence(
            1,
            state,
            Some(&payload),
            &classified,
            None,
            |state, _, accept| {
                seen = Some((
                    accept,
                    lifecycle_decision(classified.decision, state, false, accept),
                ));
                Ok(Applied::Applied)
            },
        )
        .unwrap();
        let (accept, published) = seen.expect("a rejected hook must not be described as published");
        (applied, accept, published)
    };
    let (asked_applied, asked, asked_published) = publish(
        &mut state,
        serde_json::json!({"hook_event_name":"question.asked", "request_id":"one"}),
    );
    assert!(matches!(asked_applied, Applied::Applied));
    assert!(asked.accepted);
    assert_eq!(asked_published, Decision::MarkInput);
    assert!(state.has_foreground_question("one"));

    let (reply_applied, reply, reply_published) = publish(
        &mut state,
        serde_json::json!({"hook_event_name":"permission.replied", "request_id":"per_external"}),
    );
    assert!(
        matches!(reply_applied, Applied::Applied),
        "the reply is not StaleDropped"
    );
    assert!(reply.accepted);
    assert!(reply.preserve_attention);
    assert_eq!(reply_published, Decision::Ignore);
    assert!(state.has_foreground_question("one"));
    assert!(
        state.has_early_reply("per_external"),
        "the stored id belongs to an accepted reply"
    );

    let (ask_applied, ask, ask_published) = publish(
        &mut state,
        serde_json::json!({"hook_event_name":"permission.asked", "request_id":"per_external"}),
    );
    assert!(matches!(ask_applied, Applied::Applied));
    assert!(ask.accepted);
    assert!(ask.ask_already_resolved);
    assert_eq!(ask_published, Decision::Ignore);
    assert!(state.has_foreground_question("one"));
    assert!(!state.has_early_reply("per_external"));

    // The resolved-ask bit must not survive into the next callback. An
    // empty body is the degraded "mark attention" path and never enters
    // accept_hook, so a sticky flag would silence it.
    let classified = classify(b"", "opencode", |_| Some(0));
    let mut published = None;
    let applied = apply_hook_after_turn_fence(
        1,
        &mut state,
        None,
        &classified,
        None,
        |state, _, ask_already_resolved| {
            published = Some(lifecycle_decision(
                classified.decision,
                state,
                false,
                ask_already_resolved,
            ));
            Ok(Applied::Applied)
        },
    )
    .unwrap();
    assert!(matches!(applied, Applied::Applied));
    assert_eq!(published, Some(Decision::MarkInput));
}

#[test]
fn codex_permission_reply_without_id_releases_a_sole_request() {
    let mut state = crate::agent::hook_state::HookState::default();
    let apply = |state: &mut crate::agent::hook_state::HookState, value: serde_json::Value| {
        let body = value.to_string();
        let payload = HookPayload::parse(body.as_bytes()).unwrap();
        let classified = classify(body.as_bytes(), "codex", |_| Some(0));
        (
            accept_hook(state, &payload, &classified).accepted,
            classified.decision,
        )
    };
    assert!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"UserPromptSubmit", "promptId":"turn-1"
            })
        )
        .0
    );
    assert!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"PermissionRequest", "tool_name":"Bash"
            })
        )
        .0
    );
    assert_eq!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"PermissionResult"
            })
        ),
        (true, Decision::Running)
    );
    assert_eq!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"Stop"
            })
        ),
        (true, Decision::Ready)
    );
}

#[test]
fn codex_permission_is_released_by_the_following_tool_result() {
    let mut state = crate::agent::hook_state::HookState::default();
    let apply = |state: &mut crate::agent::hook_state::HookState, value: serde_json::Value| {
        let body = value.to_string();
        let payload = HookPayload::parse(body.as_bytes()).unwrap();
        let classified = classify(body.as_bytes(), "codex", |_| Some(0));
        let permission_pending =
            classified.decision == Decision::CodexToolResult && state.has_permission_requests();
        let accepted = accept_hook(state, &payload, &classified).accepted;
        (
            accepted,
            normalize_decision(classified.decision, state, permission_pending),
        )
    };
    assert!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"UserPromptSubmit", "turn_id":"turn-1"
            })
        )
        .0
    );
    assert!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"PermissionRequest", "tool_name":"Bash"
            })
        )
        .0
    );
    // Codex has no PermissionResult hook; the tool result is the
    // observable approval/denial boundary and carries the tool name.
    assert_eq!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"PostToolUse", "tool_name":"Bash"
            })
        ),
        (true, Decision::Running)
    );
    assert_eq!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"Stop", "turn_id":"turn-1"
            })
        ),
        (true, Decision::Ready)
    );
}

#[test]
fn codex_ordinary_tool_result_is_lifecycle_neutral() {
    let body = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
    })
    .to_string();
    let classified = classify(body.as_bytes(), "codex", |_| Some(0));
    assert_eq!(
        classified.decision,
        Decision::CodexToolResult,
        "ordinary Codex tool output must remain a correlation-only decision"
    );
    assert_eq!(
        normalize_decision(
            classified.decision,
            &crate::agent::hook_state::HookState::default(),
            false
        ),
        Decision::Ignore
    );
}

#[test]
fn codex_failed_tool_result_requires_degraded_review() {
    let body = serde_json::json!({
        "hook_event_name": "PostToolUseFailure",
        "tool_name": "Bash",
    })
    .to_string();
    let classified = classify(body.as_bytes(), "codex", |_| Some(0));
    assert_eq!(classified.decision, Decision::MarkInput);
    assert_eq!(
        classified.detail.signal_health,
        crate::agent::session_lifecycle::SignalHealth::Degraded
    );
}

#[test]
fn dropped_stop_does_not_end_the_active_turn() {
    let mut state = crate::agent::hook_state::HookState::default();
    let apply = |state: &mut crate::agent::hook_state::HookState, value: serde_json::Value| {
        let body = value.to_string();
        let payload = HookPayload::parse(body.as_bytes()).unwrap();
        let classified = classify(body.as_bytes(), "codex", |_| Some(0));
        (
            accept_hook(state, &payload, &classified).accepted,
            classified.decision,
        )
    };
    assert!(
        apply(
            &mut state,
            serde_json::json!({"hook_event_name":"UserPromptSubmit", "turn_id":"turn-1"})
        )
        .0
    );
    assert!(
        apply(
            &mut state,
            serde_json::json!({"hook_event_name":"PermissionRequest", "tool_name":"Bash"})
        )
        .0
    );

    // A Stop without a turn token cannot prove that it belongs to the
    // active turn. The pending approval fence must reject it without
    // ending the foreground turn or clearing its marker.
    assert_eq!(
        apply(&mut state, serde_json::json!({"hook_event_name":"Stop"})),
        (false, Decision::Ready)
    );
    assert!(state.is_turn_active());
    assert!(state.has_permission_requests());
}

#[test]
fn codex_denied_permission_does_not_wedge_the_terminal_stop() {
    let mut state = crate::agent::hook_state::HookState::default();
    let apply = |state: &mut crate::agent::hook_state::HookState, value: serde_json::Value| {
        let body = value.to_string();
        let payload = HookPayload::parse(body.as_bytes()).unwrap();
        let classified = classify(body.as_bytes(), "codex", |_| Some(0));
        (
            accept_hook(state, &payload, &classified).accepted,
            classified.decision,
        )
    };
    assert!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"UserPromptSubmit", "turn_id":"turn-1"
            })
        )
        .0
    );
    assert!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"PermissionRequest", "tool_name":"Bash"
            })
        )
        .0
    );
    // A denied approval has no PostToolUse callback in Codex. Stop is the
    // terminal fallback and clears only the permission marker.
    assert_eq!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"Stop", "turn_id":"turn-1"
            })
        ),
        (true, Decision::Ready)
    );
}

#[test]
fn kimi_background_question_waits_for_terminal_task_notification() {
    let mut state = crate::agent::hook_state::HookState::default();
    let apply = |state: &mut crate::agent::hook_state::HookState, value: serde_json::Value| {
        let body = value.to_string();
        let payload = HookPayload::parse(body.as_bytes()).unwrap();
        let classified = classify(body.as_bytes(), "kimi", |_| Some(0));
        let accepted = accept_hook(state, &payload, &classified).accepted;
        (
            accepted,
            classified.decision,
            effective_decision(classified.decision, state),
        )
    };

    assert_eq!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"UserPromptSubmit", "promptId":"turn-1"
            })
        ),
        (true, Decision::Running, Decision::Running)
    );
    assert!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"PreToolUse", "tool_name":"AskUserQuestion",
                "tool_call_id":"call-1", "tool_input":{"background":true}
            })
        )
        .0
    );
    // PostToolUse only migrates the question to its detached task. It is
    // correlation-only and must not complete the foreground turn.
    assert_eq!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"PostToolUse", "tool_name":"AskUserQuestion",
                "tool_call_id":"call-1", "tool_input":{"background":true},
                "tool_output":"task_id: task-1\ndescription: choose\nstatus: running"
            })
        ),
        (true, Decision::Ignore, Decision::Ignore)
    );
    assert!(!apply(&mut state, serde_json::json!({"hook_event_name":"Stop"})).0);

    // The terminal task notification is authoritative only after Stop.
    assert_eq!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"Notification", "notification_type":"task.completed",
                "source_kind":"background_task", "source_id":"task-1"
            })
        ),
        (true, Decision::BackgroundTaskCompleted, Decision::Ready)
    );
}

#[test]
fn kimi_task_notification_before_post_result_is_not_reopened() {
    let mut state = crate::agent::hook_state::HookState::default();
    let apply = |state: &mut crate::agent::hook_state::HookState, value: serde_json::Value| {
        let body = value.to_string();
        let payload = HookPayload::parse(body.as_bytes()).unwrap();
        let classified = classify(body.as_bytes(), "kimi", |_| Some(0));
        let accepted = accept_hook(state, &payload, &classified).accepted;
        (
            accepted,
            classified.decision,
            effective_decision(classified.decision, state),
        )
    };
    assert_eq!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"Notification", "notification_type":"task.failed",
                "source_kind":"background_task", "source_id":"task-early"
            })
        ),
        (false, Decision::BackgroundTaskCompleted, Decision::Ready)
    );
    assert_eq!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"PostToolUse", "tool_name":"AskUserQuestion",
                "tool_call_id":"call-early", "tool_input":{"background":true},
                "tool_output":"task_id: task-early\nstatus: running"
            })
        ),
        (true, Decision::Ignore, Decision::Ignore)
    );
    assert_eq!(
        apply(&mut state, serde_json::json!({"hook_event_name":"Stop"})),
        (true, Decision::Ready, Decision::Ready)
    );
}

#[test]
fn kimi_failed_background_question_post_hook_still_correlates_task() {
    let mut state = crate::agent::hook_state::HookState::default();
    let apply = |state: &mut crate::agent::hook_state::HookState, value: serde_json::Value| {
        let body = value.to_string();
        let payload = HookPayload::parse(body.as_bytes()).unwrap();
        let classified = classify(body.as_bytes(), "kimi", |_| Some(0));
        let accepted = accept_hook(state, &payload, &classified).accepted;
        (
            accepted,
            classified.decision,
            effective_decision(classified.decision, state),
        )
    };
    assert!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"PreToolUse", "tool_name":"AskUserQuestion",
                "tool_call_id":"call-failed", "tool_input":{"background":true}
            })
        )
        .0
    );
    assert_eq!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"PostToolUseFailure", "tool_name":"AskUserQuestion",
                "tool_call_id":"call-failed", "tool_input":{"background":true},
                "tool_output":"task_id: task-failed\nstatus: failed"
            })
        ),
        (true, Decision::Ignore, Decision::Ignore)
    );
    assert_eq!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"Notification", "notification_type":"task.failed",
                "source_kind":"background_task", "source_id":"task-failed"
            })
        ),
        (true, Decision::BackgroundTaskCompleted, Decision::Ready)
    );
}

#[test]
fn user_prompt_submission_is_not_fenced_by_detached_background_question() {
    let mut state = crate::agent::hook_state::HookState::default();
    let apply = |state: &mut crate::agent::hook_state::HookState, value: serde_json::Value| {
        let body = value.to_string();
        let payload = HookPayload::parse(body.as_bytes()).unwrap();
        let classified = classify(body.as_bytes(), "kimi", |_| Some(0));
        let accepted = accept_hook(state, &payload, &classified).accepted;
        (
            accepted,
            classified.decision,
            effective_decision(classified.decision, state),
        )
    };
    assert!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"PreToolUse", "tool_name":"AskUserQuestion",
                "tool_call_id":"call-detached", "tool_input":{"background":true}
            })
        )
        .0
    );
    assert_eq!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"PostToolUse", "tool_name":"AskUserQuestion",
                "tool_call_id":"call-detached", "tool_input":{"background":true},
                "tool_output":"task_id: task-detached\nstatus: running"
            })
        ),
        (true, Decision::Ignore, Decision::Ignore)
    );

    // A new prompt is explicit user activity and must remain accepted.
    assert_eq!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"UserPromptSubmit", "promptId":"foreground-2"
            })
        ),
        (true, Decision::Running, Decision::Running)
    );
    // The background callback cannot complete this active foreground turn.
    assert_eq!(
        apply(
            &mut state,
            serde_json::json!({
                "hook_event_name":"Notification", "notification_type":"task.completed",
                "source_kind":"background_task", "source_id":"task-detached"
            })
        ),
        (true, Decision::BackgroundTaskCompleted, Decision::Ignore)
    );
    assert_eq!(
        apply(&mut state, serde_json::json!({"hook_event_name":"Stop"})),
        (true, Decision::Ready, Decision::Ready)
    );
}

#[test]
fn elicitation_result_releases_the_exact_request() {
    let mut state = crate::agent::hook_state::HookState::default();
    for (event, accepted) in [
        ("Elicitation", true),
        ("Stop", false),
        ("ElicitationResult", true),
        ("Stop", true),
    ] {
        let body =
            serde_json::json!({"hook_event_name":event,"elicitation_id":"request-1"}).to_string();
        assert_eq!(
            accept_hook(
                &mut state,
                &HookPayload::parse(body.as_bytes()).unwrap(),
                &classify(body.as_bytes(), "anthropic", |_| Some(0))
            )
            .accepted,
            accepted,
            "{event}"
        );
    }
}

#[test]
fn permission_resolution_releases_only_its_matching_request() {
    let mut state = crate::agent::hook_state::HookState::default();
    let apply = |state: &mut crate::agent::hook_state::HookState, value: serde_json::Value| {
        let body = value.to_string();
        let payload = HookPayload::parse(body.as_bytes()).unwrap();
        let classified = classify(body.as_bytes(), "kimi", |_| Some(0));
        accept_hook(state, &payload, &classified).accepted
    };
    assert!(apply(
        &mut state,
        serde_json::json!({"hook_event_name":"PermissionRequest", "toolUseId":"p1", "tool_name":"Bash"})
    ));
    assert!(apply(
        &mut state,
        serde_json::json!({"hook_event_name":"PermissionRequest", "toolUseId":"p2", "tool_name":"Write"})
    ));
    let reply_body = serde_json::json!({
        "hook_event_name":"PermissionResult", "toolUseId":"p1"
    })
    .to_string();
    let reply_payload = HookPayload::parse(reply_body.as_bytes()).unwrap();
    let reply_classified = classify(reply_body.as_bytes(), "kimi", |_| Some(0));
    let reply = accept_hook(&mut state, &reply_payload, &reply_classified);
    let published = lifecycle_decision(reply_classified.decision, &state, false, reply);
    assert!(
        reply.accepted,
        "the matched reply is accepted while p2 remains open"
    );
    assert!(reply.preserve_attention);
    assert_eq!(published, Decision::Ignore);
    assert!(state.has_foreground_question("p2"));
    assert!(
        !apply(&mut state, serde_json::json!({"hook_event_name":"Stop"})),
        "turn completion stays fenced while p2 is open"
    );
    assert!(apply(
        &mut state,
        serde_json::json!({"hook_event_name":"PermissionResult", "toolUseId":"p2"})
    ));
    assert!(apply(
        &mut state,
        serde_json::json!({"hook_event_name":"Stop"})
    ));
}

#[test]
fn grok_dual_case_envelope_keeps_the_native_event_and_session() {
    let body = br#"{"hook_event_name":"PreToolUse","hookEventName":"pre_tool_use","session_id":"12345678-1234-1234-1234-123456789abc","sessionId":"12345678-1234-1234-1234-123456789abc","tool_name":"ask_user_question","toolName":"ask_user_question"}"#;
    let classified = classify(body, "grok", |_| Some(0));
    assert_eq!(classified.decision, Decision::MarkInput);
    assert_eq!(
        classified.detail.kind,
        Some(crate::agent::session_lifecycle::LifecycleKind::QuestionRequested)
    );
    assert_eq!(
        classified.detail.signal_health,
        crate::agent::session_lifecycle::SignalHealth::Ok
    );
    assert_eq!(
        hook_session_id(body, "grok").as_deref(),
        Some("12345678-1234-1234-1234-123456789abc")
    );
}

#[test]
fn failed_and_cancelled_turns_need_review_and_never_claim_success() {
    for event in ["StopFailure", "StopCancelled", "stop_failure", "Interrupt"] {
        let body =
            serde_json::json!({"hook_event_name":event,"last_assistant_message":"partial result"});
        assert_eq!(
            classify_decision(body.to_string().as_bytes(), "grok", |_| Some(0)),
            Decision::MarkInput
        );
        assert_eq!(
            semantic_turn(&HookPayload::parse(body.to_string().as_bytes()).unwrap()),
            None
        );
    }
}

#[test]
fn resolved_native_input_returns_to_running_without_completing_a_turn() {
    for body in [
        serde_json::json!({"hook_event_name":"UserPromptSubmit"}),
        serde_json::json!({"hook_event_name":"PermissionResult"}),
        serde_json::json!({"hook_event_name":"PostToolUse", "tool_name":"AskUserQuestion"}),
    ] {
        assert_eq!(
            classify_decision(body.to_string().as_bytes(), "kimi", |_| Some(0)),
            Decision::Running
        );
    }
}

#[test]
fn questions_are_not_turn_completions() {
    for (provider, payload) in [
        (
            "anthropic",
            serde_json::json!({"hook_event_name":"Notification", "notification_type":"elicitation_dialog"}),
        ),
        (
            "anthropic",
            serde_json::json!({"hook_event_name":"Elicitation", "message":"Choose an account"}),
        ),
        (
            "anthropic",
            serde_json::json!({"hook_event_name":"PreToolUse", "tool_name":"AskUserQuestion"}),
        ),
        (
            "codex",
            serde_json::json!({"hook_event_name":"PreToolUse", "tool_name":"request_user_input"}),
        ),
        (
            "opencode",
            serde_json::json!({"hook_event_name":"question.asked"}),
        ),
    ] {
        let classified = classify(payload.to_string().as_bytes(), provider, |_| Some(3));
        assert_eq!(
            classified.decision,
            Decision::MarkInput,
            "{provider}: {payload}"
        );
        assert_eq!(
            classified.detail.kind,
            Some(crate::agent::session_lifecycle::LifecycleKind::QuestionRequested),
            "{provider}: {payload}"
        );
    }
}

#[test]
fn informational_events_do_not_complete_a_turn_or_request_permission() {
    for event in ["PreToolUse", "PostToolUse", "SubagentStop"] {
        let body = serde_json::json!({"hook_event_name":event, "tool_name":"Bash"});
        assert_eq!(
            classify_decision(body.to_string().as_bytes(), "anthropic", |_| Some(0)),
            Decision::Ignore,
            "{event}"
        );
    }
    let body = br#"{"hook_event_name":"Notification","notification_type":"auth_success"}"#;
    assert_eq!(
        classify_decision(body, "anthropic", |_| Some(0)),
        Decision::Ignore
    );
}

#[test]
fn unknown_named_events_degrade_instead_of_claiming_completion() {
    let result = classify(
        br#"{"hook_event_name":"future_event"}"#,
        "anthropic",
        |_| Some(0),
    );
    assert_eq!(result.decision, Decision::MarkInput);
    assert_eq!(
        result.detail.signal_health,
        crate::agent::session_lifecycle::SignalHealth::Degraded
    );
}

#[test]
fn semantic_turn_normalizes_permission_command_and_finished_payloads() {
    let permission: HookPayload = serde_json::from_value(serde_json::json!({
        "hook_event_name": "PermissionRequest",
        "tool_name": "Edit",
        "tool_input": { "file_path": "src/lib/auth.ts" }
    }))
    .unwrap();
    assert_eq!(
        semantic_turn(&permission),
        Some(SemanticTurn {
            kind: SemanticTurnKind::PermissionRequest,
            description: "Allow edit: src/lib/auth.ts".into(),
        })
    );

    let command: HookPayload = serde_json::from_value(serde_json::json!({
        "hook_event_name": "PreToolUse",
        "toolCall": { "name": "run_command", "args": { "cmd": "npm test -- --coverage" } }
    }))
    .unwrap();
    assert_eq!(
        semantic_turn(&command),
        Some(SemanticTurn {
            kind: SemanticTurnKind::CommandConfirmation,
            description: "Run: npm test -- --coverage".into(),
        })
    );

    let finished: HookPayload = serde_json::from_value(serde_json::json!({
        "hookEventName": "Stop",
        "lastAssistantMessage": "Implemented the auth guard."
    }))
    .unwrap();
    assert_eq!(
        semantic_turn(&finished),
        Some(SemanticTurn {
            kind: SemanticTurnKind::TurnFinished,
            description: "Implemented the auth guard.".into(),
        })
    );

    assert_eq!(semantic_turn(&HookPayload::default()), None);
}

/// Issue #1295 (round-2 review): `permission.asked` must round-trip
/// through `semantic_turn` so the route can extract a description.
/// Without this branch, `extract_semantic_turn` returns None and the
/// downstream Node Turn collapses `PermissionRequest → InputRequired`
/// (issue #1364 §1 — never silently downgrade a permission to a
/// bare input request). Mirrors the Codex `PermissionRequest` arm.
#[test]
fn semantic_turn_recognizes_opencode_permission_asked() {
    let permission: HookPayload = serde_json::from_value(serde_json::json!({
        "hook_event_name": "permission.asked",
        "notification_type": "permission_prompt",
        "tool_name": "Bash",
        "message": "OpenCode is asking for permission: Bash",
    }))
    .unwrap();
    let turn = semantic_turn(&permission).expect("permission.asked must yield a SemanticTurn");
    assert_eq!(turn.kind, SemanticTurnKind::PermissionRequest);
    assert!(
        turn.description.contains("Bash"),
        "tool name must surface in the description; got {:?}",
        turn.description
    );
}

/// A representative Stop-hook stdin payload.
fn stop_body(transcript_path: &str) -> Vec<u8> {
    serde_json::json!({
        "session_id": "abc-123",
        "transcript_path": transcript_path,
        "cwd": "F:\\src\\repo",
        "hook_event_name": "Stop",
        "stop_hook_active": false,
    })
    .to_string()
    .into_bytes()
}

/// Test shim: classify and return just the [`Decision`].
fn classify_decision(
    body: &[u8],
    provider: &str,
    count_pending: impl FnOnce(&Path) -> Option<usize>,
) -> Decision {
    classify(body, provider, count_pending).decision
}

#[test]
fn empty_or_garbage_body_marks_input_with_degraded_health() {
    // Pre-#878 hooks post no body at all; a broken payload must degrade to
    // the old always-mark behaviour, never to silence — but the signal is
    // unknown, so the health is Degraded, never a high-confidence mark.
    for body in [&b""[..], b"not json".as_slice()] {
        let classified = classify(body, "anthropic", |_| Some(5));
        assert_eq!(classified.decision, Decision::MarkInput);
        assert_eq!(
            classified.detail.signal_health,
            crate::agent::session_lifecycle::SignalHealth::Degraded,
            "an unparseable payload must record degraded signal health (issue #1364)"
        );
    }
}

#[test]
fn review_handoff_stop_after_reading_launch_examples_is_ready() {
    // The fixture transcript intentionally contains a tool_result whose
    // body *mentions* a background-task launch pattern in prose. The
    // shared `count_pending_background_tasks` parser correctly extracts
    // the launch id from the pattern (the same parser that handles
    // real launches); a Stop hook firing with that transcript must
    // therefore surface as `SuppressPendingBackground`, not `Ready`.
    // The test name preserves the original intent (the parser should
    // not be tricked by prose mentions of the launch pattern); the
    // expectation was wrong, not the parser.
    let transcript = tempfile::NamedTempFile::new().unwrap();
    let record = serde_json::json!({"type":"user", "message":{"content":[{
        "type":"tool_result", "content":"659\t// Command running in background with ID: xyz.\n681\tconst LAUNCH_MARKER: &str = \"You will be notified when it completes\";"
    }]}});
    std::fs::write(transcript.path(), format!("{record}\n")).unwrap();
    let body = serde_json::json!({"hook_event_name":"Stop", "transcript_path":transcript.path()})
        .to_string();
    assert_eq!(classify_decision(body.as_bytes(), "claude", crate::services::transcript_reader::adapters::claude_code::count_pending_background_tasks), Decision::SuppressPendingBackground);
}

#[test]
fn fieldless_json_body_marks_input_with_degraded_health() {
    // A parseable `{}` with no recognized fields is an unknown signal —
    // not "turn completed". Mark with degraded health (issue #1364).
    let classified = classify(b"{}", "anthropic", |_| Some(0));
    assert_eq!(classified.decision, Decision::MarkInput);
    assert_eq!(
        classified.detail.signal_health,
        crate::agent::session_lifecycle::SignalHealth::Degraded
    );
}

#[test]
fn structured_hook_exposes_canonical_session_uuid() {
    let body = serde_json::json!({
        "session_id": "C1234567-89AB-CDEF-0123-456789ABCDEF",
        "hook_event_name": "Stop",
    })
    .to_string();
    assert_eq!(
        hook_session_id(body.as_bytes(), "claude").as_deref(),
        Some("c1234567-89ab-cdef-0123-456789abcdef")
    );
}

#[test]
fn missing_malformed_or_non_uuid_session_id_is_ignored() {
    assert_eq!(hook_session_id(b"{}", "claude"), None);
    assert_eq!(hook_session_id(b"not json", "claude"), None);
    assert_eq!(
        hook_session_id(br#"{"session_id":"most-recent"}"#, "claude"),
        None
    );
}

#[test]
fn stop_with_pending_background_tasks_suppresses() {
    let body = stop_body("/tmp/session.jsonl");
    assert_eq!(
        classify_decision(&body, "anthropic", |_| Some(2)),
        Decision::SuppressPendingBackground
    );
}

#[test]
fn stop_with_no_pending_tasks_is_ready() {
    // Issue #1364 — a clean turn completion is NOT a user-input request;
    // the node lands in Ready, never AwaitingInput.
    let body = stop_body("/tmp/session.jsonl");
    assert_eq!(
        classify_decision(&body, "anthropic", |_| Some(0)),
        Decision::Ready
    );
}

#[test]
fn unreadable_transcript_is_degraded_not_a_confirmed_completion() {
    let body = stop_body("/tmp/session.jsonl");
    let result = classify(&body, "anthropic", |_| None);
    assert_eq!(result.decision, Decision::MarkInput);
    assert_eq!(
        result.detail.signal_health,
        crate::agent::session_lifecycle::SignalHealth::Degraded
    );
}

#[test]
fn missing_transcript_path_is_ready() {
    let body = serde_json::json!({"hook_event_name": "Stop"})
        .to_string()
        .into_bytes();
    assert_eq!(
        classify_decision(&body, "anthropic", |_| Some(3)),
        Decision::Ready
    );
}

#[test]
fn permission_prompt_notification_marks_input_even_with_pending_tasks() {
    // A tool-approval question blocks the whole turn — background work
    // running in parallel doesn't make the user less needed.
    let body = serde_json::json!({
        "hook_event_name": "Notification",
        "transcript_path": "/tmp/session.jsonl",
        "message": "Claude needs your permission to use Bash",
    })
    .to_string()
    .into_bytes();
    let classified = classify(&body, "anthropic", |_| Some(2));
    assert_eq!(classified.decision, Decision::MarkInput);
    assert_eq!(
        classified.detail.signal_health,
        crate::agent::session_lifecycle::SignalHealth::Ok,
        "a structured permission payload is a high-confidence signal"
    );
}

#[test]
fn codex_permission_request_marks_input_even_with_pending_tasks() {
    // Codex raises a dedicated PermissionRequest hook event when a tool
    // needs approval (issue #884) — the user is needed, same as a Claude
    // permission Notification, regardless of background work.
    let body = serde_json::json!({
        "hook_event_name": "PermissionRequest",
        "transcript_path": "/tmp/session.jsonl",
        "tool_name": "Bash",
        "message": "Codex needs your permission to run Bash",
    })
    .to_string()
    .into_bytes();
    assert_eq!(
        classify_decision(&body, "codex", |_| Some(2)),
        Decision::MarkInput
    );
}

#[test]
fn idle_notification_with_pending_tasks_suppresses() {
    // The 60s idle notification fires while the agent sits at its input
    // box waiting for a long background build — same false yield as Stop.
    let body = serde_json::json!({
        "hook_event_name": "Notification",
        "transcript_path": "/tmp/session.jsonl",
        "message": "Claude is waiting for your input",
    })
    .to_string()
    .into_bytes();
    assert_eq!(
        classify_decision(&body, "anthropic", |_| Some(1)),
        Decision::SuppressPendingBackground
    );
}

// -------------------------------------------------------------------
// Grok Code (issue #1282) — camelCase wire, no transcript_path,
// Notification carries structured `notificationType`.
// -------------------------------------------------------------------

/// Grok's permission prompt: the matcher fires `Notification` with
/// `notificationType = "permission_prompt"`. We mark the node
/// regardless of any background-task count (defensive — Grok has
/// no transcript reader yet, so the closure is unused, but
/// keeping the signature uniform guards against a future
/// transcript reader silently swallowing the permission yield).
#[test]
fn grok_notification_with_permission_type_marks_input() {
    let body = serde_json::json!({
        "hookEventName": "notification",
        "sessionId": "550e8400-e29b-41d4-a716-446655440000",
        "cwd": "/Users/you/project",
        "workspaceRoot": "/Users/you/project",
        "notificationType": "permission_prompt",
        "message": "Grok needs your permission to run Bash",
    })
    .to_string()
    .into_bytes();
    assert_eq!(
        classify_decision(&body, "grok", |_| Some(2)),
        Decision::MarkInput
    );
}

/// Grok's idle prompt (`notificationType = "idle_prompt"`) carries
/// no transcript path. With no pending work it reads as a clean turn
/// completion → Ready (issue #1364); it must never show the amber
/// "Needs attention" for a turn that finished normally.
#[test]
fn grok_idle_notification_without_transcript_is_ready() {
    let body = serde_json::json!({
        "hookEventName": "notification",
        "sessionId": "550e8400-e29b-41d4-a716-446655440000",
        "notificationType": "idle_prompt",
    })
    .to_string()
    .into_bytes();
    assert_eq!(
        classify_decision(&body, "grok", |_| Some(2)),
        Decision::Ready
    );
}

/// Grok's `task_complete` notification is an explicit completion signal
/// → Ready (issue #1364).
#[test]
fn grok_task_complete_notification_is_ready() {
    let body = serde_json::json!({
        "hookEventName": "notification",
        "sessionId": "550e8400-e29b-41d4-a716-446655440000",
        "notificationType": "task_complete",
        "message": "Task finished",
    })
    .to_string()
    .into_bytes();
    assert_eq!(
        classify_decision(&body, "grok", |_| Some(2)),
        Decision::Ready
    );
}

// -- OpenCode plugin (issue #1294) ---------------------------------------

/// OpenCode's `session.created` plugin event fires once at TUI boot
/// carrying the freshly minted `ses_<…>` id. The classifier treats
/// it as lifecycle-neutral (`Ignore`); the session id itself is
/// captured by `set_cli_session_id_if_missing` above the
/// classifier's apply pass — the very property that makes this the
/// primary capture path for `agent_nodes.cli_session_id`. A regression
/// that lands this event on `Ready` or `MarkInput` would either fire
/// naming/autopilot on an empty session (Ready) or pop an
/// "awaiting_input" badge on a node that just booted (MarkInput).
#[test]
fn opencode_session_created_is_lifecycle_neutral() {
    let body = serde_json::json!({
        "hook_event_name": "session.created",
        "sessionID": "ses_fc52ccfb9ffek1jl23ZwpRuSP7",
    })
    .to_string()
    .into_bytes();
    assert_eq!(
        classify_decision(&body, "opencode", |_| Some(5)),
        Decision::Ignore
    );
    // The classifier uses `provider_event` for downstream telemetry;
    // `Ignore` callers still record the upstream name verbatim so
    // future diagnostics can correlate.
    let classified = classify(&body, "opencode", |_| Some(5));
    assert_eq!(
        classified.detail.provider_event.as_deref(),
        Some("session.created")
    );
    // The id must round-trip through the provider-aware extractor so
    // `set_cli_session_id_if_missing` (called outside the classifier)
    // has a valid `ses_…` string to write. Without the OpenCode
    // gate the UUID parser would silently drop it — the exact symptom
    // this issue set out to fix. Base62 casing is preserved (issue
    // #1294 round-2 review): the live `ses_…ZwpRuSP7` round-trips
    // case-sensitively because OpenCode's CLI looks ids up that way.
    assert_eq!(
        hook_session_id(&body, "opencode").as_deref(),
        Some("ses_fc52ccfb9ffek1jl23ZwpRuSP7"),
    );
    // Same body, non-OpenCode provider — the legacy UUID gate
    // rejects the `ses_…` shape (a UUID-shaped field is what Claude/
    // Codex/AGY/Grok/Cursor carry). Pins the per-provider
    // dispatcher.
    assert_eq!(hook_session_id(&body, "claude"), None);
}

/// Issue #1294 — `session.created` is case-folded the same way as
/// `session.idle` so an upstream plugin version that emits
/// `SESSION.CREATED` still hits the rule. Regression pin matching
/// `opencode_session_idle_is_case_insensitive` above.
#[test]
fn opencode_session_created_is_case_insensitive() {
    for casing in ["session.created", "SESSION.CREATED", "Session.Created"] {
        let body = serde_json::json!({
            "hook_event_name": casing,
            "sessionID": "ses_fc52ccfb9ffek1jl23ZwpRuSP7",
        })
        .to_string()
        .into_bytes();
        assert_eq!(
            classify_decision(&body, "opencode", |_| Some(5)),
            Decision::Ignore,
            "casing {casing:?} must hit the session.created rule"
        );
    }
}

/// Issue #1294 negative AC: a plugin payload that claims to be a
/// `session.created` event but carries a UUID-shaped id (instead of
/// the documented `ses_…` shape) must NOT be captured. The provider-
/// aware extractor drops it on the OpenCode gate, so the underlying
/// `set_cli_session_id_if_missing` call sees `None` and writes
/// nothing. The classifier still returns `Ignore` so a malformed
/// payload can't fake a "needs attention" land via a side channel.
#[test]
fn opencode_session_created_with_uuid_is_not_captured() {
    let body = serde_json::json!({
        "hook_event_name": "session.created",
        "sessionID": "550e8400-e29b-41d4-a716-446655440000",
    })
    .to_string()
    .into_bytes();
    assert_eq!(
        classify_decision(&body, "opencode", |_| Some(5)),
        Decision::Ignore
    );
    assert_eq!(hook_session_id(&body, "opencode"), None);
}

/// Issue #1294 round-2 review: `classify()` must use the
/// provider-aware extractor (not the legacy UUID-only gate) so the
/// `detail.provider_session_id` survives into the `agent-lifecycle`
/// event for OpenCode. The original implementation hard-coded
/// `parse_cli_session_id` here, which silently dropped every
/// OpenCode callback's id and left the frontend telemetry with a
/// blank `provider_session_id` field. Pin that the provider reaches
/// the extractor by checking the field on the classified detail.
#[test]
fn classify_populates_provider_session_id_for_opencode() {
    let body = serde_json::json!({
        "hook_event_name": "session.created",
        "sessionID": "ses_fc52ccfb9ffek1jl23ZwpRuSP7",
    })
    .to_string()
    .into_bytes();
    // Provider is "opencode" — the Base62 tail must round-trip
    // with original casing preserved (issue #1294 round-2 review).
    let classified = classify(&body, "opencode", |_| Some(5));
    assert_eq!(
        classified.detail.provider_session_id.as_deref(),
        Some("ses_fc52ccfb9ffek1jl23ZwpRuSP7"),
        "opencode provider must route through parse_opencode_session_id, not the UUID gate",
    );
    // Same body, provider = "" — the legacy UUID gate rejects the
    // `ses_…` shape, so `provider_session_id` is None. Pins that
    // the dispatcher truly switches on provider.
    let classified_unknown = classify(&body, "", |_| Some(5));
    assert_eq!(
        classified_unknown.detail.provider_session_id, None,
        "unknown harness must not capture another harness session",
    );
}

// -- OpenCode plugin (issue #1295) ---------------------------------------

/// OpenCode's `session.idle` plugin event — agent finished its turn
/// and is at the prompt. Must land as MarkInput with kind =
/// `InputRequired` so the node reaches `awaiting_input`. The rule
/// fires before the transcript-scan fallback (OpenCode has no
/// transcript), so even a hypothetical "no pending tasks" caller
/// does NOT land this on `Ready`.
#[test]
fn opencode_session_idle_is_a_clean_completion() {
    let body = serde_json::json!({
        "hook_event_name": "session.idle",
        "message": "OpenCode session idle — agent ready for input",
    })
    .to_string()
    .into_bytes();
    let classified = classify(&body, "opencode", |_| Some(0));
    assert_eq!(classified.decision, Decision::Ready);
    assert_eq!(
        classified.detail.kind,
        Some(LifecycleKind::TurnCompleted),
        "ordinary idle must not claim the user is needed"
    );
    assert_eq!(
        classified.detail.signal_health,
        crate::agent::session_lifecycle::SignalHealth::Ok
    );
    assert_eq!(
        classified.detail.provider_event.as_deref(),
        Some("session.idle")
    );
}

/// Issue #1294 round-2 review: the OpenCode plugin now attaches
/// `sessionID` to EVERY lifecycle event (session.created /
/// session.idle / permission.asked) so the route's ordering-token
/// fence (`attention.rs:690-702`) can drop stale callbacks from a
/// previous OpenCode incarnation. Pin that `session.idle`'s id flows
/// through the provider-aware extractor and lands on
/// `detail.provider_session_id` — without this, the fence's
/// `if let Some(hook) = hook_uuid.as_deref()` short-circuits and a
/// second OpenCode process can poison the first's UI state.
#[test]
fn opencode_session_idle_carries_session_id_for_fencing() {
    let body = serde_json::json!({
        "hook_event_name": "session.idle",
        "sessionID": "ses_fc52ccfb9ffek1jl23ZwpRuSP7",
        "message": "idle",
    })
    .to_string()
    .into_bytes();
    let classified = classify(&body, "opencode", |_| Some(0));
    assert_eq!(
        classified.detail.provider_session_id.as_deref(),
        Some("ses_fc52ccfb9ffek1jl23ZwpRuSP7"),
        "session.idle must propagate sessionID with Base62 casing preserved \
         (issue #1294 round-2 — fence must fire on idle callbacks too)",
    );
    assert_eq!(classified.decision, Decision::Ready);
}

/// Same regression pin for `permission.asked`. Without sessionID on
/// the permission callback, an old OpenCode process can re-issue a
/// stale permission request and the route has no way to distinguish
/// it from the live process's callback.
#[test]
fn opencode_permission_asked_carries_session_id_for_fencing() {
    let body = serde_json::json!({
        "hook_event_name": "permission.asked",
        "sessionID": "ses_fc52ccfb9ffek1jl23ZwpRuSP7",
        "notification_type": "permission_prompt",
    })
    .to_string()
    .into_bytes();
    let classified = classify(&body, "opencode", |_| Some(0));
    assert_eq!(
        classified.detail.provider_session_id.as_deref(),
        Some("ses_fc52ccfb9ffek1jl23ZwpRuSP7"),
        "permission.asked must propagate sessionID with Base62 casing preserved \
         (issue #1294 round-2 — fence must fire on permission callbacks too)",
    );
    assert_eq!(classified.decision, Decision::MarkInput);
}

/// OpenCode's `session.idle` is case-folded the same way as the
/// other event names (`SESSION.IDLE` and `session.idle` must hit the
/// same rule). Regression: a future refactor that switches to
/// `eq_ignore_ascii_case` only on the `permissionrequest` arm would
/// silently regress this rule.
#[test]
fn opencode_session_idle_is_case_insensitive() {
    for casing in ["session.idle", "SESSION.IDLE", "Session.Idle"] {
        let body = serde_json::json!({
            "hook_event_name": casing,
            "message": "idle",
        })
        .to_string()
        .into_bytes();
        let classified = classify(&body, "opencode", |_| Some(0));
        assert_eq!(
            classified.decision,
            Decision::Ready,
            "casing {casing:?} must hit the rule"
        );
        assert_eq!(classified.detail.kind, Some(LifecycleKind::TurnCompleted));
    }
}

/// OpenCode's `permission.asked` plugin event — agent blocked on a
/// tool approval decision. Must always mark input (rule 2-style
/// permission handling), regardless of transcript path. The
/// classifier does NOT do a transcript scan on permission events —
/// the harness signalled "user is needed", full stop.
#[test]
fn opencode_permission_asked_marks_input_regardless_of_transcript() {
    let body = serde_json::json!({
        "hook_event_name": "permission.asked",
        "notification_type": "permission_prompt",
        "message": "OpenCode is asking for permission: Bash",
        "tool_name": "Bash",
    })
    .to_string()
    .into_bytes();
    let classified = classify(&body, "opencode", |_| Some(5));
    assert_eq!(classified.decision, Decision::MarkInput);
    assert_eq!(
        classified.detail.provider_event.as_deref(),
        Some("permission.asked")
    );
    // Forwarded tool name rides on the structured `tool_name` field;
    // it does NOT change the lifecycle kind — the harness already
    // labelled this as a permission event upstream.
    assert_eq!(
        classified.detail.kind,
        Some(LifecycleKind::PermissionRequested)
    );
}

/// Regression: pre-#1295 the plugin borrowed Codex's
/// `hook_event_name: "PermissionRequest"`. A payload with that exact
/// shape still classifies correctly (Codex's own rule still handles
/// it) — so any historical migration to the honest
/// `permission.asked` event name does NOT break in-flight Codex
/// callbacks. The two rules are independent.
#[test]
fn codex_permissionrequest_still_marks_independent_of_opencode_branch() {
    let body = serde_json::json!({
        "hook_event_name": "PermissionRequest",
        "message": "Bash wants to run",
    })
    .to_string()
    .into_bytes();
    let classified = classify(&body, "opencode", |_| Some(0));
    assert_eq!(classified.decision, Decision::MarkInput);
    assert_eq!(
        classified.detail.provider_event.as_deref(),
        Some("PermissionRequest")
    );
}

/// Regression: an OpenCode permission payload that arrives with the
/// Grok-style `notification` event name + `notification_type` shape
/// must still mark input. This is the cross-harness safety net: if
/// a future plugin version forgets to set `hook_event_name` but
/// carries the structured `notification_type`, the route still
/// recognises it via rule 2b. (Pure "missing event_name" payloads
/// without `notification_type` would land on `Ready` — that's the
/// documented "no transcript, no signal" semantics.)
#[test]
fn opencode_permission_payload_with_notification_event_marks_via_notification_type() {
    let body = serde_json::json!({
        // Grok-style notification envelope used by an upstream
        // plugin draft that hadn't migrated to the honest
        // `permission.asked` event name yet.
        "hook_event_name": "notification",
        "notification_type": "permission_prompt",
        "message": "asking for permission",
    })
    .to_string()
    .into_bytes();
    let classified = classify(&body, "opencode", |_| Some(0));
    assert_eq!(
        classified.decision,
        Decision::MarkInput,
        "notification + notification_type=permission_prompt must still mark via rule 2b"
    );
}

/// Codex SessionStart fires as soon as the TUI boots, carrying the
/// conversation UUID. That is the structured capture we want. It is
/// not a turn completion — treating it as Ready would fire naming and
/// Autopilot on an empty session.
#[test]
fn codex_session_start_is_lifecycle_neutral() {
    let body = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "550e8400-e29b-41d4-a716-446655440000",
        "cwd": r"F:\src\buildmesh\.claude\worktrees\node",
        "source": "startup",
    })
    .to_string()
    .into_bytes();
    assert_eq!(
        classify_decision(&body, "codex", |_| Some(0)),
        Decision::Ignore
    );
    assert_eq!(
        hook_session_id(&body, "codex").as_deref(),
        Some("550e8400-e29b-41d4-a716-446655440000")
    );
}

/// Grok's `Stop` event is a clean turn completion → Ready. The runner
/// treats the route's empty 200 OK as "allow the stop" (we never return
/// a `decision: "block"` JSON), so the agent doesn't loop on the gate.
#[test]
fn grok_stop_event_is_ready() {
    let body = serde_json::json!({
        "hookEventName": "stop",
        "sessionId": "550e8400-e29b-41d4-a716-446655440000",
        "stopHookActive": false,
        "lastAssistantMessage": "Done.",
        "reason": "end_turn",
    })
    .to_string()
    .into_bytes();
    assert_eq!(
        classify_decision(&body, "grok", |_| Some(0)),
        Decision::Ready
    );
}

/// Grok's Stop carries `reason` and `stopHookActive` — parse them into
/// the envelope's completion reason so the lifecycle event preserves
/// the provider detail (issue #1364 §1).
#[test]
fn grok_stop_envelope_preserves_completion_reason() {
    let body = serde_json::json!({
        "hookEventName": "stop",
        "sessionId": "550e8400-e29b-41d4-a716-446655440000",
        "stopHookActive": false,
        "lastAssistantMessage": "Done.",
        "reason": "end_turn",
    })
    .to_string()
    .into_bytes();
    let classified = classify(&body, "grok", |_| Some(0));
    assert_eq!(classified.decision, Decision::Ready);
    assert_eq!(
        classified.detail.completion_reason.as_deref(),
        Some("end_turn")
    );
    assert_eq!(
        classified.detail.provider_session_id.as_deref(),
        Some("550e8400-e29b-41d4-a716-446655440000")
    );
}

/// Issue #1366 — Grok's structured `notificationType` is
/// preserved into the classified detail end-to-end. The
/// lifecycle event carries both the normalized decision
/// (`MarkInput` / `Ready`) AND the harness's own string
/// (`permission_prompt`, `task_complete`, …) so the UI can
/// render the harness's own classification. A future refactor
/// that drops this field trips here before the wire shape
/// drifts.
#[test]
fn grok_notification_type_surfaces_in_signal_detail() {
    for (notification_type, expected_decision) in [
        ("permission_prompt", Decision::MarkInput),
        ("task_complete", Decision::Ready),
        ("idle_prompt", Decision::Ready),
        ("question", Decision::MarkInput),
        ("question_prompt", Decision::MarkInput),
        ("ask_user", Decision::MarkInput),
    ] {
        let body = serde_json::json!({
            "hookEventName": "notification",
            "sessionId": "550e8400-e29b-41d4-a716-446655440000",
            "notificationType": notification_type,
        })
        .to_string()
        .into_bytes();
        let classified = classify(&body, "grok", |_| Some(0));
        assert_eq!(
            classified.detail.notification_type.as_deref(),
            Some(notification_type),
            "{notification_type}: notificationType must round-trip into the signal detail"
        );
        assert_eq!(
            classified.decision, expected_decision,
            "{notification_type}: lifecycle decision must match the shared contract"
        );
    }
}

/// `notificationType` absent means "the harness did not
/// structure the notification" — the route falls through to the
/// transcript-scan path. Pin so a future refactor that
/// conflates "no notification_type" with "no event" trips here.
#[test]
fn grok_notification_without_type_falls_through() {
    let body = serde_json::json!({
        "hookEventName": "notification",
        "sessionId": "550e8400-e29b-41d4-a716-446655440000",
        "transcriptPath": "/tmp/session.jsonl",
    })
    .to_string()
    .into_bytes();
    let classified = classify(&body, "grok", |_| Some(2));
    assert_eq!(classified.detail.notification_type, None);
    // 2 pending tasks → suppress (the prose-only fallback is
    // strict on "needs your permission"; untyped notifications
    // match that pattern only if the harness's prose says so).
    assert_eq!(classified.decision, Decision::SuppressPendingBackground);
}

/// The payload parser accepts Grok's camelCase `sessionId` and
/// canonicalises it to the same UUID string Claude payloads do.
/// `hook_session_id` is the only consumer of the field on the
/// session-id side of the route — both casings must round-trip.
#[test]
fn hook_payload_reads_camel_case_session_id() {
    let body = serde_json::json!({
        "hookEventName": "notification",
        "sessionId": "550E8400-E29B-41D4-A716-446655440000",
        "notificationType": "idle_prompt",
    })
    .to_string();
    assert_eq!(
        hook_session_id(body.as_bytes(), "grok").as_deref(),
        Some("550e8400-e29b-41d4-a716-446655440000")
    );
}

/// Grok wires `notificationType` as a separate field, distinct
/// from Claude's message-substring convention — both styles must
/// classify as a permission yield so a hook that emits either
/// shape gets marked.
#[test]
fn grok_notification_type_via_snake_case_alias_also_marks_input() {
    // The grok-agent-sdk converts camelCase top-level keys to
    // snake_case — accept both so the same parser handles both
    // delivery surfaces.
    let body = serde_json::json!({
        "hook_event_name": "Notification",
        "session_id": "550e8400-e29b-41d4-a716-446655440000",
        "notification_type": "permission_prompt",
    })
    .to_string()
    .into_bytes();
    assert_eq!(
        classify_decision(&body, "grok", |_| Some(2)),
        Decision::MarkInput
    );
}

// -------------------------------------------------------------------
// Antigravity (agy) payload shape — issue #1285.
//
// AGY sends camelCase fields (`conversationId`, `transcriptPath`,
// `fullyIdle`, `terminationReason`) and two event kinds (`Stop` and
// `PreToolUse`). The route's `HookPayload` accepts both via serde
// aliases; `decide` short-circuits `PreToolUse` to always-mark and
// `Stop` with `fullyIdle: false` to always-suppress.
// -------------------------------------------------------------------

/// AGY's `Stop` with `fullyIdle: false` is a direct false-yield signal
/// from the harness — the turn ended but background work is still
/// running. Short-circuits before the transcript scan so an AGY node
/// (which has no transcript reader) gets correct suppression. Even
/// when `count_pending` reports zero tasks (the harness said so), the
/// harness's own signal wins — AGY's view is authoritative for AGY.
#[test]
fn agy_stop_with_fully_idle_false_suppresses() {
    let body = serde_json::json!({
        "conversationId": "abc-123",
        "transcriptPath": "/tmp/session.jsonl",
        "hook_event_name": "Stop",
        "fullyIdle": false,
        "terminationReason": "model_stop",
    })
    .to_string()
    .into_bytes();
    assert_eq!(
        classify_decision(&body, "agy", |_| Some(0)),
        Decision::SuppressPendingBackground
    );
}

/// AGY's `Stop` with `fullyIdle: true` is a genuine turn completion —
/// falls through to the transcript-scan path. No pending tasks → Ready
/// (issue #1364: the user is NOT needed).
#[test]
fn agy_stop_with_fully_idle_true_is_ready() {
    let body = serde_json::json!({
        "conversationId": "abc-123",
        "transcriptPath": "/tmp/session.jsonl",
        "hook_event_name": "Stop",
        "fullyIdle": true,
    })
    .to_string()
    .into_bytes();
    assert_eq!(
        classify_decision(&body, "agy", |_| Some(0)),
        Decision::Ready
    );
}

/// `fullyIdle: true` with pending tasks still suppresses — the
/// transcript scan is consulted, not skipped, when the harness says
/// the turn actually settled.
#[test]
fn agy_stop_with_fully_idle_true_and_pending_tasks_suppresses() {
    let body = serde_json::json!({
        "conversationId": "abc-123",
        "transcriptPath": "/tmp/session.jsonl",
        "hook_event_name": "Stop",
        "fullyIdle": true,
    })
    .to_string()
    .into_bytes();
    assert_eq!(
        classify_decision(&body, "agy", |_| Some(2)),
        Decision::SuppressPendingBackground
    );
}

/// An older AGY payload that omits `fullyIdle` entirely (or any
/// future harness that doesn't set it) falls through to the
/// transcript-scan path — `Stop` with no transcript path → Ready,
/// matching the issue #1364 clean-turn-completion semantics. The
/// field is additive, not breaking.
#[test]
fn agy_stop_without_fully_idle_uses_transcript_scan() {
    let body = serde_json::json!({
        "conversationId": "abc-123",
        "transcriptPath": "/tmp/session.jsonl",
        "hook_event_name": "Stop",
    })
    .to_string()
    .into_bytes();
    assert_eq!(
        classify_decision(&body, "agy", |_| Some(0)),
        Decision::Ready
    );

    let missing_transcript = serde_json::json!({
        "conversationId": "abc-123",
        "hook_event_name": "Stop",
    })
    .to_string()
    .into_bytes();
    assert_eq!(
        classify_decision(&missing_transcript, "agy", |_| Some(3)),
        Decision::Ready
    );
}

/// AGY's `PreToolUse` fires before a tool call — analogous to Codex's
/// `PermissionRequest`. The agent is at a tool-approval decision, so
/// the user is needed regardless of background work. Always marks input.
#[test]
fn agy_pre_tool_use_is_not_evidence_of_a_permission_prompt() {
    let body = serde_json::json!({
        "conversationId": "abc-123",
        "transcriptPath": "/tmp/session.jsonl",
        "hook_event_name": "PreToolUse",
        "toolCall": {"name": "run_command", "args": {"cmd": "ls"}},
        "stepIdx": 5,
    })
    .to_string()
    .into_bytes();
    assert_eq!(
        classify_decision(&body, "agy", |_| Some(5)),
        Decision::Ignore
    );
}

/// `hook_session_id` extracts the AGY UUID from `conversationId` via
/// the alias, just like Claude Code's `session_id`. Lower-cased so
/// the value matches what the orchestrator's spawn pipeline writes
/// into `agent_nodes.cli_session_id`.
#[test]
fn hook_session_id_reads_mcode_mvs_id() {
    // Issue #1797: mcode's `mvs_<hex>` id is not a UUID. Without the
    // provider arm it fell through to the UUID validator, so the fill-only
    // capture dropped it and `agent_nodes.cli_session_id` stayed NULL —
    // breaking `--session <id>` resume and the `TranscriptFormat::Mcode`
    // manifest scan. The body is the real envelope from a live 0.4.12 TUI.
    let body = serde_json::json!({
        "stop_hook_active": false,
        "last_assistant_message": "OK",
        "hook_event_name": "Stop",
        "session_id": "mvs_d66c5fa695294e2abe936c242fb43c76",
        "transcript_path": "/tmp/x.jsonl",
        "permission_mode": "auto",
    })
    .to_string();
    assert_eq!(
        hook_session_id(body.as_bytes(), "mcode").as_deref(),
        Some("mvs_d66c5fa695294e2abe936c242fb43c76")
    );
    assert_eq!(
        hook_session_id(body.as_bytes(), "anthropic"),
        None,
        "another provider must not adopt the mvs_ shape"
    );
}

#[test]
fn mcode_native_attention_routes_legacy_and_shared_callbacks_before_capture() {
    use crate::models::{AgentNode, SessionStatus};
    let nodes = vec![
        AgentNode {
            id: 4724,
            provider: "mcode".into(),
            path: "F:/repo/implementation".into(),
            status: SessionStatus::Running,
            ..Default::default()
        },
        AgentNode {
            id: 4725,
            provider: "mcode".into(),
            path: "F:/repo/other".into(),
            status: SessionStatus::Running,
            ..Default::default()
        },
        AgentNode {
            id: 4715,
            provider: "mcode".into(),
            path: "F:/unrelated".into(),
            cli_session_id: Some("mvs_11111111111111111111111111111111".into()),
            status: SessionStatus::Running,
            ..Default::default()
        },
    ];
    let body =
        |event: &str, id: &str, cwd: &str| {
            serde_json::json!({
        "hook_event_name": event, "session_id": id, "cwd": cwd,
        "transcript_path": "F:/native/messages.jsonl", "permission_mode": "bypassPermissions"
    }).to_string().into_bytes()
        };
    let id = "mvs_22222222222222222222222222222222";
    let start = body("SessionStart", id, "f:\\repo\\implementation");
    let start_payload = HookPayload::parse(&start);
    let resolve = |id: &str, cwd: &str| {
        crate::services::mcode_session::select_hook_target(&nodes, id, cwd, |_| true)
    };
    let legacy = resolve_attention_node(
        Some(nodes[1].clone()),
        start_payload.as_ref(),
        false,
        resolve,
    )
    .unwrap();
    let shared = resolve_attention_node(None, start_payload.as_ref(), true, resolve).unwrap();
    assert_eq!(
        legacy.id, 4724,
        "last-writer numeric URL cannot select node4725"
    );
    assert_eq!(shared.id, legacy.id);
    assert_eq!(
        classify(&start, "mcode", |_| Some(0)).decision,
        Decision::Ignore
    );
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE agent_nodes(id INTEGER PRIMARY KEY, provider TEXT, path TEXT, env TEXT,
        worktree_name TEXT, worktree_path TEXT, use_worktree INTEGER, status TEXT, session_started_at INTEGER, cli_session_id TEXT);
        INSERT INTO agent_nodes VALUES(4724,'mcode','F:/repo/implementation','windows',NULL,NULL,0,'running',100,NULL);
        INSERT INTO agent_nodes VALUES(4725,'mcode','F:/repo/other','windows',NULL,NULL,0,'running',101,NULL);").unwrap();
    assert!(
        !crate::db::agent_node::recover_live_cli_session_id_inner(&conn, &legacy, id, 99).unwrap(),
        "stale generation cannot capture"
    );
    assert!(
        crate::db::agent_node::recover_live_cli_session_id_inner(&conn, &legacy, id, 100).unwrap()
    );
    assert!(
        !crate::db::set_cli_session_id_if_missing_inner(&conn, 4725, id).unwrap(),
        "another node cannot claim the conversation"
    );
    let stop = body("Stop", id, "F:/repo/implementation");
    assert_eq!(
        classify(&stop, "mcode", |_| Some(0)).decision,
        Decision::Ready
    );
    let unrelated = body(
        "Stop",
        "mvs_11111111111111111111111111111111",
        "F:/repo/implementation",
    );
    let unrelated_payload = HookPayload::parse(&unrelated);
    let standalone_payload = HookPayload::parse(&body("Stop", id, "F:/standalone"));
    let missing_cwd_payload = HookPayload::parse(&body("Stop", id, ""));
    assert!(resolve_attention_node(
        Some(nodes[0].clone()),
        unrelated_payload.as_ref(),
        false,
        resolve
    )
    .is_none());
    assert!(resolve_attention_node(None, standalone_payload.as_ref(), true, resolve).is_none());
    assert!(resolve_attention_node(None, missing_cwd_payload.as_ref(), true, resolve).is_none());
    assert!(
        resolve_attention_node(None, start_payload.as_ref(), true, |id, cwd| {
            crate::services::mcode_session::select_hook_target(&nodes, id, cwd, |_| false)
        })
        .is_none()
    );
}

#[test]
fn hook_session_id_reads_agy_conversation_id() {
    let body = serde_json::json!({
        "conversationId": "C1234567-89AB-CDEF-0123-456789ABCDEF",
        "transcriptPath": "/tmp/session.jsonl",
        "hook_event_name": "Stop",
        "fullyIdle": true,
    })
    .to_string();
    assert_eq!(
        hook_session_id(body.as_bytes(), "agy").as_deref(),
        Some("c1234567-89ab-cdef-0123-456789abcdef")
    );
}

/// A payload that mixes snake_case (Claude Code shape) and
/// camelCase (AGY shape) for different fields still parses — the
/// alias is per-field, not per-payload. Both Grok's `sessionId` and
/// AGY's `conversationId` flow through the same `session_id` field
/// via stacked aliases.
#[test]
fn hook_payload_tolerates_mixed_case_field_names() {
    let body = serde_json::json!({
        "session_id": "C1234567-89AB-CDEF-0123-456789ABCDEF",
        "transcriptPath": "/tmp/session.jsonl",
        "hook_event_name": "Stop",
        "fullyIdle": true,
    })
    .to_string();
    assert_eq!(
        hook_session_id(body.as_bytes(), "agy").as_deref(),
        Some("c1234567-89ab-cdef-0123-456789abcdef")
    );
}

#[test]
fn hook_payload_parsing_preserves_the_shared_envelope() {
    let raw = serde_json::json!({
        "session_id": "canonical-session",
        "sessionId": "alias-session",
        "hook_event_name": "Stop",
        "hookEventName": "Notification",
        "toolInput": {"nested": [1, 2, 3]},
    });
    let original = raw.clone();

    let payload = HookPayload::parse_value(&raw).unwrap();

    assert_eq!(payload.session_id.as_deref(), Some("canonical-session"));
    assert_eq!(payload.hook_event_name.as_deref(), Some("Stop"));
    assert_eq!(
        raw, original,
        "other classifiers must see the original envelope"
    );
}

/// Issue #1367: Fixture for a complete AGY Stop payload emitted by current releases (1.0.0-1.1.22+).
/// Covers all standard metadata fields, camelCase keys, and verifies exact parsing.
#[test]
fn agy_full_release_fixture_parses_all_fields() {
    let json_body = serde_json::json!({
        "conversationId": "550e8400-e29b-41d4-a716-446655440000",
        "executionNum": 4,
        "terminationReason": "model_stop",
        "error": "",
        "fullyIdle": true,
        "workspacePaths": ["/Users/dev/project"],
        "transcriptPath": "/Users/dev/project/.gemini/antigravity/transcript.jsonl",
        "artifactDirectoryPath": "/Users/dev/project/.gemini/antigravity/artifacts",
        "modelName": "gemini-3.7-flash"
    });
    let body = json_body.to_string().into_bytes();

    let parsed = normalizers::parse(&json_body, "agy").expect("must parse full AGY fixture");
    assert_eq!(
        parsed.session_id.as_deref(),
        Some("550e8400-e29b-41d4-a716-446655440000")
    );
    assert_eq!(parsed.execution_num, Some(4));
    assert_eq!(parsed.termination_reason.as_deref(), Some("model_stop"));
    assert!(parsed.error.as_deref().is_some_and(|e| e.is_empty()) || parsed.error.is_none());
    assert_eq!(parsed.fully_idle, Some(true));
    assert_eq!(
        parsed.workspace_paths.as_deref(),
        Some(&["/Users/dev/project".to_string()][..])
    );
    assert_eq!(parsed.model_name.as_deref(), Some("gemini-3.7-flash"));

    assert_eq!(
        hook_session_id(&body, "agy").as_deref(),
        Some("550e8400-e29b-41d4-a716-446655440000")
    );
    assert_eq!(
        classify_decision(&body, "agy", |_| Some(0)),
        Decision::Ready
    );
}

/// Issue #1367: Background yield fixture (`fullyIdle: false`) with no hook_event_name
/// (the natural AGY Stop hook stdin payload). Must suppress attention.
#[test]
fn agy_background_yield_without_hook_event_name_suppresses() {
    let json_body = serde_json::json!({
        "conversationId": "550e8400-e29b-41d4-a716-446655440000",
        "executionNum": 2,
        "terminationReason": "model_stop",
        "fullyIdle": false,
        "workspacePaths": ["/work/proj"],
        "transcriptPath": "/work/proj/transcript.jsonl"
    });
    let body = json_body.to_string().into_bytes();
    assert_eq!(
        classify_decision(&body, "agy", |_| Some(0)),
        Decision::SuppressPendingBackground
    );
}

/// Issue #1367: Various termination reasons emitted by AGY releases.
#[test]
fn agy_termination_reasons_classification() {
    for reason in [
        "model_stop",
        "tool_execution_limit_reached",
        "max_steps_exceeded",
        "user_interrupt",
        "error",
    ] {
        let body_idle = serde_json::json!({
            "conversationId": "550e8400-e29b-41d4-a716-446655440000",
            "terminationReason": reason,
            "fullyIdle": true,
        })
        .to_string()
        .into_bytes();
        assert_eq!(
            classify_decision(&body_idle, "agy", |_| Some(0)),
            Decision::Ready,
            "reason={reason} with fullyIdle=true must be Ready"
        );

        let body_busy = serde_json::json!({
            "conversationId": "550e8400-e29b-41d4-a716-446655440000",
            "terminationReason": reason,
            "fullyIdle": false,
        })
        .to_string()
        .into_bytes();
        assert_eq!(
            classify_decision(&body_busy, "agy", |_| Some(0)),
            Decision::SuppressPendingBackground,
            "reason={reason} with fullyIdle=false must Suppress"
        );
    }
}

/// Issue #1367: Malformed or unexpected JSON payloads degrade safely to
/// MarkInput (never to a high-confidence completion or silence), with a
/// degraded signal health.
#[test]
fn agy_malformed_payload_degrades_to_mark_input() {
    let malformed = b"{not: valid, json";
    let classified = classify(malformed, "agy", |_| Some(0));
    assert_eq!(classified.decision, Decision::MarkInput);
    assert_eq!(
        classified.detail.signal_health,
        crate::agent::session_lifecycle::SignalHealth::Degraded
    );

    let empty_obj = b"{}";
    let classified = classify(empty_obj, "agy", |_| Some(0));
    assert_eq!(classified.decision, Decision::MarkInput);
    assert_eq!(
        classified.detail.signal_health,
        crate::agent::session_lifecycle::SignalHealth::Degraded
    );
}

/// A structured question notification type maps to the normalized
/// `QuestionRequested` kind, still a MarkInput decision (issue #1364
/// §1). Unstructured prose is NEVER classified as a question.
#[test]
fn structured_question_notification_classifies_as_question_requested() {
    let body = serde_json::json!({
        "hookEventName": "notification",
        "sessionId": "550e8400-e29b-41d4-a716-446655440000",
        "notificationType": "question",
        "message": "Which database should I use?",
    })
    .to_string()
    .into_bytes();
    let classified = classify(&body, "grok", |_| Some(2));
    assert_eq!(classified.decision, Decision::MarkInput);
    assert_eq!(
        classified.detail.kind,
        Some(crate::agent::session_lifecycle::LifecycleKind::QuestionRequested)
    );
}

/// Issue #1966 — a question tool that enumerated answers carries the
/// choice list on the observation, so a client can offer those answers
/// instead of permission-shaped yes/no chips.
#[test]
fn structured_question_carries_its_answer_choices() {
    let body = serde_json::json!({
        "hookEventName": "PreToolUse",
        "sessionId": "550e8400-e29b-41d4-a716-446655440000",
        "toolName": "AskUserQuestion",
        "toolInput": {
            "questions": [{
                "question": "Should the deployment target staging or production?",
                "options": [
                    { "label": "Staging" },
                    { "label": "Production" },
                ],
            }],
        },
    })
    .to_string()
    .into_bytes();
    let classified = classify(&body, "anthropic", |_| Some(0));
    assert_eq!(classified.decision, Decision::MarkInput);
    let request = classified
        .detail
        .request
        .expect("a question with answers carries its choices");
    assert_eq!(
        request.choices,
        vec!["Staging".to_string(), "Production".to_string()]
    );
}

/// Issue #1966 — an open question (no enumerated answers) and a
/// permission decision both carry NO request schema. Absence is the signal
/// that keeps a client from inventing yes/no semantics.
#[test]
fn open_question_and_permission_request_carry_no_choices() {
    let open = serde_json::json!({
        "hookEventName": "PreToolUse",
        "sessionId": "550e8400-e29b-41d4-a716-446655440000",
        "toolName": "AskUserQuestion",
        "toolInput": { "questions": [{ "question": "Which branch should I use?" }] },
    })
    .to_string()
    .into_bytes();
    assert!(classify(&open, "anthropic", |_| Some(0))
        .detail
        .request
        .is_none());

    let permission = serde_json::json!({
        "hookEventName": "PermissionRequest",
        "sessionId": "550e8400-e29b-41d4-a716-446655440000",
        "toolName": "Edit",
        "toolInput": { "file_path": "src/lib/auth.ts" },
    })
    .to_string()
    .into_bytes();
    let classified = classify(&permission, "anthropic", |_| Some(0));
    assert_eq!(classified.decision, Decision::MarkInput);
    assert!(classified.detail.request.is_none());
}

/// Issue #1966 — choices repeated across the questions of one request are
/// offered once, and a blank label is not an answer.
#[test]
fn question_choices_are_deduplicated_and_validated() {
    use crate::agent::session_lifecycle::InputRequest;
    assert_eq!(
        InputRequest::from_choices(vec!["Yes", "  ", "Yes", "No "]).map(|request| request.choices),
        Some(vec!["Yes".to_string(), "No".to_string()])
    );
    assert!(InputRequest::from_choices(Vec::<String>::new()).is_none());
}

/// Unstructured prose mentioning "question" must NOT become a
/// QuestionRequested (issue #1364 review — no free-text guessing);
/// it falls through the normal classification.
#[test]
fn prose_mentioning_question_is_not_question_requested() {
    let body = serde_json::json!({
        "hookEventName": "notification",
        "sessionId": "550e8400-e29b-41d4-a716-446655440000",
        "notificationType": "idle_prompt",
        "message": "I answered your question about the database",
    })
    .to_string()
    .into_bytes();
    let classified = classify(&body, "anthropic", |_| Some(0));
    assert_ne!(
        classified.detail.kind,
        Some(crate::agent::session_lifecycle::LifecycleKind::QuestionRequested)
    );
}

/// "Permission was already granted for Bash" must not read as a
/// permission request — the heuristic anchors to the documented
/// "needs your permission" verb envelope (issue #1364 review).
#[test]
fn permission_already_granted_prose_is_not_permission_requested() {
    let body = serde_json::json!({
        "hookEventName": "Notification",
        "transcript_path": "/tmp/session.jsonl",
        "message": "Permission was already granted for Bash",
    })
    .to_string()
    .into_bytes();
    let classified = classify(&body, "anthropic", |_| Some(0));
    assert_eq!(classified.decision, Decision::Ready);
    assert_ne!(
        classified.detail.kind,
        Some(crate::agent::session_lifecycle::LifecycleKind::PermissionRequested)
    );
}

/// The classifier records the provider event name and a high-confidence
/// health for a structured permission payload — the lifecycle derives
/// `PermissionRequested` from the semantic turn (pinned in
/// `session_lifecycle` tests).
#[test]
fn permission_payload_preserves_provider_event_and_health() {
    let body = serde_json::json!({
        "hook_event_name": "PermissionRequest",
        "transcript_path": "/tmp/session.jsonl",
        "tool_name": "Bash",
    })
    .to_string()
    .into_bytes();
    let classified = classify(&body, "anthropic", |_| Some(2));
    assert_eq!(classified.decision, Decision::MarkInput);
    assert_eq!(
        classified.detail.provider_event.as_deref(),
        Some("PermissionRequest")
    );
    assert_eq!(
        classified.detail.signal_health,
        crate::agent::session_lifecycle::SignalHealth::Ok
    );
}

// -------------------------------------------------------------------
// Runtime hook token scoping — issue #1366.
//
// The hook file at `~/.grok/hooks/buildmesh-attention.json` is the
// *global* always-trusted Grok hooks dir, so every Grok session on
// the box (including a non-Buildmesh invocation in a shell that
// happens to inherit `BUILDMESH_PORT`) loads our hooks. The
// `?token=$BUILDMESH_HOOK_TOKEN` query param gives the route a
// per-runtime secret so non-Buildmesh sessions can't deliver to a
// Buildmesh node. The real tests below exercise the module-scope
// OnceLock storage (round-trip) and the comparator's three
// outcomes (match / wrong / empty). The actual HTTP loopback
// round-trip is covered by `http::tests::attention_webhook_*`.
// -------------------------------------------------------------------

#[test]
fn extract_query_value_returns_value_when_key_matches() {
    assert_eq!(extract_query_value("token=abc123", "token"), Some("abc123"));
    assert_eq!(
        extract_query_value("foo=bar&token=abc123", "token"),
        Some("abc123")
    );
    assert_eq!(
        extract_query_value("token=abc123&foo=bar", "token"),
        Some("abc123")
    );
}

#[test]
fn extract_query_value_returns_none_when_key_absent() {
    assert_eq!(extract_query_value("foo=bar", "token"), None);
    assert_eq!(extract_query_value("", "token"), None);
    // No '=' so split_once fails — treated as absent, not errored.
    assert_eq!(extract_query_value("token", "token"), None);
}

/// Pin the module-scope OnceLock storage (issue #1366 review, point
/// 1.1). A previous revision kept `static TOKEN` inside two functions
/// — two separate allocations, one store and one read never agreed,
/// so `runtime_hook_token()` always returned `None`. The fix is
/// module-level storage; this test exercises the actual accessor
/// against `mint_runtime_hook_token()` round-trip.
#[test]
fn runtime_hook_token_round_trips_through_module_level_once_lock() {
    // Test setup: the runtime token is minted lazily by the Grok
    // adapter's `provision_attention_hooks` (production path),
    // not at every spawn. In a test process no Grok agent ever
    // spawns, so the OnceLock stays None until we mint here.
    // `mint_runtime_hook_token` is idempotent: it pins the same
    // value across all subsequent calls in this process, so calling
    // it from multiple tests is safe — every test that needs the
    // token will read the same value.
    let minted = crate::agent::mint_runtime_hook_token();
    let read_back = crate::agent::runtime_hook_token();
    assert_eq!(
        read_back.as_deref(),
        Some(minted.as_str()),
        "runtime_hook_token must read from the same OnceLock that \
     mint_runtime_hook_token writes to"
    );
    // Format sanity: 32 lowercase hex chars (16 random bytes).
    assert_eq!(minted.len(), 32);
    assert!(minted.chars().all(|c| c.is_ascii_hexdigit()));
}

/// The route gate at `handle_post:438-455` collapses to:
/// Cases are: no token minted means permissive; a matching query token
/// proceeds; any missing, wrong, or empty token is rejected. This pins the
/// comparator logic the route uses so a future refactor can't silently let
/// an unrelated harness through. The end-to-end POST loopback round-trip
/// is exercised by `http::tests::attention_webhook_*` for the loopback peer
/// path.
#[test]
fn runtime_token_validator_three_cases() {
    // `mint_runtime_hook_token` is idempotent across the process.
    // Explicitly mint here so this test passes both in isolation
    // (e.g. when selected by name) and under the default cargo-test
    // schedule. The round-trip test in this module reads the same
    // pinned value back to assert structural integrity; production
    // code only ever mints once per Buildmesh runtime lifetime.
    let minted = crate::agent::mint_runtime_hook_token();

    // Case A: matching token → accept.
    let query_with_match = format!("token={minted}");
    assert_eq!(
        extract_query_value(&query_with_match, "token"),
        Some(minted.as_str()),
        "matching token must round-trip via extract_query_value"
    );

    // Case B: wrong token → reject.
    let wrong_token = "z".repeat(32);
    let query_wrong = format!("token={wrong_token}");
    let presented =
        extract_query_value(&query_wrong, "token").expect("wrong-token query is non-empty");
    assert_ne!(
        presented, minted,
        "wrong token must not match the minted one"
    );

    // Case C: empty token (`$VAR` expansion on an unset env produces an
    // empty value, the typical non-Buildmesh-shell case) → reject.
    let query_empty = "token=";
    let presented_empty = extract_query_value(query_empty, "token")
        .expect("trailing '=' still parses as a key=value pair");
    assert_eq!(presented_empty, "");
    assert_ne!(presented_empty, minted);
}

/// Round-2 review fix 4 — the per-provider token gate.
///
/// Calls the production `verify_attention_token` helper directly
/// so a refactor that flips the comparator semantics or that
/// drops the per-provider discrimination fails here. The truth
/// table (minted `Some("grok_token")`, query varies):
///
///   provider="claude", no query  → accept (sibling bypass)
///   provider="claude", any token → accept (sibling bypass)
///   provider="grok",   no query  → reject (no token)
///   provider="grok",   wrong     → reject
///   provider="grok",   match     → accept
///   provider="grok",   minted=None, any → reject (defence in depth)
#[test]
fn verify_attention_token_truth_table() {
    let minted = Some("grok_token");

    // Sibling harnesses — bypass entirely even when a token is
    // minted. Their hook URLs never carry `?token=`, but more
    // importantly the per-provider lookup classifies them as
    // non-Grok so the comparator never runs.
    assert!(verify_attention_token("claude", None, minted));
    assert!(verify_attention_token("claude", Some("anything"), minted));
    assert!(verify_attention_token("codex", None, minted));
    assert!(verify_attention_token("agy", Some("token=z"), minted));
    // Empty provider string ("default anthropic" sentinel) is
    // also non-Grok — still bypass.
    assert!(verify_attention_token("", None, minted));

    // Grok callbacks — token required.
    assert!(!verify_attention_token("grok", None, minted));
    assert!(!verify_attention_token("grok", Some("token="), minted));
    assert!(!verify_attention_token("grok", Some("token=wrong"), minted));
    assert!(verify_attention_token(
        "grok",
        Some("token=grok_token"),
        minted
    ));
    // No minted token yet (no Grok spawn in this runtime) AND a
    // Grok callback arrives — refuse. This is the defensive 403
    // that catches a Buildmesh instance whose own Grok spawn
    // never ran but the file system somehow has a hook caller.
    assert!(!verify_attention_token(
        "grok",
        Some("token=grok_token"),
        None
    ));
    assert!(!verify_attention_token("grok", None, None));
}

// -------------------------------------------------------------------
// Cursor Agent (issue #1368) — snake_case `conversation_id` +
// `hook_event_name: "stop"` envelope. The route parser accepts
// Cursor's documented casing via stacked `#[serde(alias)]` on
// `session_id` (Claude's `session_id`, AGY's `conversationId`,
// Cursor's `conversation_id`).
// -------------------------------------------------------------------

/// Cursor documents `conversation_id` (snake_case) as the canonical
/// session id field. The route must extract the UUID from this
/// key just like it does from Claude's `session_id` / AGY's
/// `conversationId` / Grok's `sessionId`. Pinned via the alias
/// on `HookPayload::session_id`.
#[test]
fn hook_session_id_reads_cursor_conversation_id_snake_case() {
    let body = serde_json::json!({
        "conversation_id": "C1234567-89AB-CDEF-0123-456789ABCDEF",
        "hook_event_name": "stop",
        "transcript_path": "/tmp/session.jsonl",
    })
    .to_string();
    assert_eq!(
        hook_session_id(body.as_bytes(), "cursor").as_deref(),
        Some("c1234567-89ab-cdef-0123-456789abcdef")
    );
}

/// Cursor's `Stop` is a clean turn completion → Ready (issue
/// #1364). The route's transcript-scan path runs the same way it
/// does for Claude's `Stop` — Cursor ships a transcript path the
/// route converts to host form via `to_host_path` and scans for
/// pending tasks.
#[test]
fn cursor_stop_event_with_no_pending_tasks_is_ready() {
    let body = serde_json::json!({
        "conversation_id": "550e8400-e29b-41d4-a716-446655440000",
        "hook_event_name": "stop",
        "transcript_path": "/tmp/session.jsonl",
    })
    .to_string();
    assert_eq!(
        classify_decision(body.as_bytes(), "cursor", |_| Some(0)),
        Decision::Ready
    );
}

/// Cursor's `Stop` with launched-but-unfinished background tasks
/// suppresses (the false-yield pattern from issue #878). Same
/// transcript-scan gate as Claude's `Stop`.
#[test]
fn cursor_stop_event_with_pending_tasks_suppresses() {
    let body = serde_json::json!({
        "conversation_id": "550e8400-e29b-41d4-a716-446655440000",
        "hook_event_name": "stop",
        "transcript_path": "/tmp/session.jsonl",
    })
    .to_string();
    assert_eq!(
        classify_decision(body.as_bytes(), "cursor", |_| Some(2)),
        Decision::SuppressPendingBackground
    );
}

/// Cursor's `Stop` with no transcript path is Ready (clean turn
/// completion). Mirrors the Claude/Codex/Grok shape — without a
/// transcript the route can't prove pending work, so the turn is
/// treated as completed and the node lands in `Ready`, never in
/// `AwaitingInput` (issue #1364).
#[test]
fn cursor_stop_event_without_transcript_path_is_ready() {
    let body = serde_json::json!({
        "conversation_id": "550e8400-e29b-41d4-a716-446655440000",
        "hook_event_name": "stop",
    })
    .to_string();
    assert_eq!(
        classify_decision(body.as_bytes(), "cursor", |_| Some(3)),
        Decision::Ready
    );
}

/// Provider envelope preservation: the classified detail captures
/// `provider_event = "stop"` and `provider_session_id = "<UUID>"`
/// (lowercased) so the lifecycle event surfaces Cursor's own
/// classification alongside the shared lifecycle kind.
#[test]
fn cursor_stop_envelope_preserves_provider_event_and_session_id() {
    let body = serde_json::json!({
        "conversation_id": "550E8400-E29B-41D4-A716-446655440000",
        "hook_event_name": "stop",
        "transcript_path": "/tmp/session.jsonl",
    })
    .to_string();
    let classified = classify(body.as_bytes(), "cursor", |_| Some(0));
    assert_eq!(classified.decision, Decision::Ready);
    assert_eq!(classified.detail.provider_event.as_deref(), Some("stop"));
    assert_eq!(
        classified.detail.provider_session_id.as_deref(),
        Some("550e8400-e29b-41d4-a716-446655440000")
    );
}

/// A full Cursor fixture: hook stdin JSON with all documented
/// fields. Mirrors the AGY `agy_full_release_fixture_parses_all_fields`
/// precedent. Verifies the payload shape parses end-to-end and
/// classifies as Ready.
#[test]
fn cursor_full_release_fixture_parses_all_fields() {
    let json_body = serde_json::json!({
        "conversation_id": "550e8400-e29b-41d4-a716-446655440000",
        "hook_event_name": "stop",
        "transcript_path": "/Users/dev/project/.cursor/session.jsonl",
        "cwd": "/Users/dev/project",
    });
    let body = json_body.to_string().into_bytes();

    let parsed = normalizers::parse(&json_body, "cursor").expect("must parse full Cursor fixture");
    assert_eq!(
        parsed.session_id.as_deref(),
        Some("550e8400-e29b-41d4-a716-446655440000")
    );
    assert_eq!(parsed.hook_event_name.as_deref(), Some("stop"));
    assert_eq!(
        parsed.transcript_path.as_deref(),
        Some("/Users/dev/project/.cursor/session.jsonl")
    );

    assert_eq!(
        hook_session_id(&body, "cursor").as_deref(),
        Some("550e8400-e29b-41d4-a716-446655440000")
    );
    assert_eq!(
        classify_decision(&body, "cursor", |_| Some(0)),
        Decision::Ready
    );
}

// —— Issue #1775: Cline file-hook normalisation ——————————————————————

/// `TaskComplete` → `agent_end` is a clean turn completion (`afterRun`
/// only fires for a completed run), so the node lands in `Ready`.
#[test]
fn cline_task_complete_is_a_clean_turn_completion() {
    let body = serde_json::json!({
        "hookName": "agent_end",
        "taskId": "session_1790003303940_9ouga",
        "iteration": 1,
        "turn": { "status": "completed", "outputText": "done" }
    })
    .to_string()
    .into_bytes();
    let classified = classify(&body, "cline", |_| Some(0));
    assert_eq!(classified.decision, Decision::Ready);
    assert_eq!(
        classified.detail.provider_event.as_deref(),
        Some("agent_end")
    );
}

/// Cline's `session_shutdown` is the **abort** dispatch, not an exit: it
/// fires on a user interrupt while the session is still live. It must stay
/// lifecycle-neutral so the node is never written `Idle` (the #1853
/// blocking finding — a clean exit is still observed via PTY EOF).
#[test]
fn cline_session_shutdown_is_lifecycle_neutral() {
    let body = serde_json::json!({
        "hookName": "session_shutdown",
        "taskId": "session_1790003303940_9ouga",
        "reason": "user-cancel"
    })
    .to_string()
    .into_bytes();
    let classified = classify(&body, "cline", |_| Some(0));
    assert_eq!(classified.decision, Decision::Ignore);
    assert_eq!(
        classified.detail.signal_health,
        crate::agent::session_lifecycle::SignalHealth::Ok,
        "a claimed-but-neutral Cline event must not degrade the health"
    );
}

/// A Cline event Buildmesh does not provision is lifecycle-neutral — it
/// must not fall through to the generic "unknown event → degraded attention
/// mark" arm (which would set `Degraded` and flip the node to
/// `AwaitingInput`).
#[test]
fn cline_unprovisioned_event_is_lifecycle_neutral() {
    let body = serde_json::json!({
        "hookName": "tool_call",
        "taskId": "session_1790003303940_9ouga"
    })
    .to_string()
    .into_bytes();
    let classified = classify(&body, "cline", |_| Some(0));
    assert_eq!(classified.decision, Decision::Ignore);
    assert_eq!(
        classified.detail.signal_health,
        crate::agent::session_lifecycle::SignalHealth::Ok,
        "a claimed-but-neutral Cline event must not degrade the health"
    );
}

/// Cline's `hookName` / `taskId` field names parse through the shared
/// envelope aliases.
#[test]
fn cline_hook_name_and_task_id_aliases_parse() {
    let payload = HookPayload::parse(br#"{"hookName":"agent_end","taskId":"1789757012702_7of3e"}"#)
        .expect("must parse");
    assert_eq!(payload.hook_event_name.as_deref(), Some("agent_end"));
    assert_eq!(payload.session_id.as_deref(), Some("1789757012702_7of3e"));
}

fn classify(
    body: &[u8],
    provider: &str,
    count_pending: impl FnOnce(&Path) -> Option<usize>,
) -> Classified {
    let value: Option<serde_json::Value> = serde_json::from_slice(body).ok();
    normalizers::normalize(value.as_ref(), provider, count_pending).classified
}
