use super::common::Decision;
use super::normalizers;
use super::ordering::{apply_hook_after_turn_fence, lifecycle_decision};
use super::Applied;
use crate::agent::hook_state::HookState;
use crate::agent::session_lifecycle::{testing::RecordingSink, LifecycleKind, SignalHealth};
use crate::models::SessionStatus;
use serde_json::{json, Value};

struct NodeFixture {
    state: HookState,
    sink: RecordingSink,
    turns: usize,
}

impl NodeFixture {
    fn running() -> Self {
        Self {
            state: HookState::default(),
            sink: RecordingSink::with_status(SessionStatus::Running),
            turns: 0,
        }
    }

    fn apply(&mut self, node_id: i64, provider: &str, value: Value) {
        let normalized = normalizers::normalize(Some(&value), provider, |_| Some(0));
        let permission_pending = normalized.classified.decision == Decision::CodexToolResult
            && self.state.has_permission_requests();
        let sink = &self.sink;
        let turns = &mut self.turns;
        apply_hook_after_turn_fence(
            node_id,
            &mut self.state,
            normalized.payload.as_ref(),
            &normalized.classified,
            None,
            |state, _, accept| {
                let decision = lifecycle_decision(
                    normalized.classified.decision,
                    state,
                    permission_pending,
                    accept,
                );
                let Some(kind) = decision.lifecycle_kind(&normalized.classified.detail) else {
                    return Ok(Applied::Applied);
                };
                crate::node_turn::publish_hook_with_sink(
                    sink,
                    node_id,
                    kind,
                    &normalized.classified.detail,
                    || *turns += 1,
                );
                Ok(Applied::Applied)
            },
        )
        .unwrap();
    }
}

#[test]
fn malformed_codex_callback_cannot_stall_claude_completion() {
    let mut codex = NodeFixture::running();
    let mut claude = NodeFixture::running();
    claude.apply(
        -187901,
        "anthropic",
        json!({"hook_event_name":"UserPromptSubmit", "turn_id":"claude-turn"}),
    );
    claude.apply(-187901, "anthropic", json!({"hook_event_name":"PermissionRequest", "turn_id":"claude-turn", "request_id":"approval-1"}));
    assert_eq!(claude.sink.status(), Some(SessionStatus::AwaitingInput));
    codex.apply(
        -187902,
        "codex",
        json!({"hook_event_name":"PermissionRequest", "tool_name":17}),
    );
    assert_eq!(codex.sink.status(), Some(SessionStatus::AwaitingInput));
    assert_eq!(
        codex.sink.lifecycle_changed()[0].kind,
        LifecycleKind::SignalUnavailable
    );
    assert!(
        !codex.state.has_questions(),
        "a malformed permission must not open a correlation fence"
    );
    assert!(claude.state.has_permission_requests());
    claude.apply(-187901, "anthropic", json!({"hook_event_name":"PermissionResult", "turn_id":"claude-turn", "request_id":"approval-1"}));
    claude.apply(
        -187901,
        "anthropic",
        json!({"hook_event_name":"Stop", "turn_id":"claude-turn"}),
    );
    assert_eq!(claude.sink.status(), Some(SessionStatus::Ready));
    assert_eq!(
        claude.sink.lifecycle_changed().last().unwrap().kind,
        LifecycleKind::TurnCompleted
    );
    assert_eq!(
        claude.turns, 2,
        "permission and completion each fan out after a lifecycle commit"
    );
    codex.apply(-187902, "codex", json!({"hook_event_name":"Stop"}));
    assert_eq!(
        codex.sink.status(),
        Some(SessionStatus::Ready),
        "the malformed callback does not wedge its own next valid turn either"
    );
    crate::attention_autoclear::disarm(-187901);
    crate::attention_autoclear::disarm(-187902);
}

#[test]
fn unknown_harness_never_borrows_a_mapping_or_reads_a_transcript() {
    let payload = json!({"hook_event_name":"Stop", "fullyIdle":true,
        "session_id":"550e8400-e29b-41d4-a716-446655440000", "transcript_path":"untrusted.jsonl"});
    for provider in [
        "",
        "future-harness",
        "terminal",
        "freebuff",
        "dsh",
        "muse",
        "commandcode",
    ] {
        let result = normalizers::normalize(Some(&payload), provider, |_| {
            panic!("unknown strategy cannot reconcile")
        });
        assert_eq!(
            result.classified.detail.kind,
            Some(LifecycleKind::SignalUnavailable),
            "{provider}"
        );
        assert_eq!(
            result.classified.detail.signal_health,
            SignalHealth::Degraded
        );
        assert!(result.payload.is_none());
        assert!(result.session_id.is_none());
        assert!(result.native_hook.is_none());
    }
}

#[test]
fn malformed_foreign_fields_do_not_invalidate_codex_payloads() {
    let raw = json!({"hook_event_name":"Stop", "fullyIdle":"broken", "executionNum":[], "workspacePaths":false});
    let codex = normalizers::normalize(Some(&raw), "codex", |_| panic!("no transcript supplied"));
    assert_eq!(codex.classified.decision, Decision::Ready);
    assert_eq!(
        codex.classified.detail.kind,
        Some(LifecycleKind::TurnCompleted)
    );
    assert_eq!(codex.classified.detail.signal_health, SignalHealth::Ok);
    let agy = normalizers::normalize(Some(&raw), "agy", |_| {
        panic!("invalid payload cannot reconcile")
    });
    assert_eq!(
        agy.classified.detail.kind,
        Some(LifecycleKind::SignalUnavailable)
    );
    assert!(agy.native_hook.is_none());
}

#[test]
fn foreign_permission_event_cannot_open_a_codex_wait() {
    let value = json!({"hook_event_name":"permission.asked", "request_id":"opencode-request"});
    let result = normalizers::normalize(Some(&value), "codex", |_| Some(0));
    assert_eq!(
        result.classified.detail.kind,
        Some(LifecycleKind::SignalUnavailable)
    );
    assert!(
        result.payload.is_none(),
        "unsupported hook cannot reach the correlation tracker"
    );
    let supported = normalizers::normalize(Some(&value), "opencode", |_| Some(0));
    assert_eq!(
        supported.classified.detail.kind,
        Some(LifecycleKind::PermissionRequested)
    );
    assert!(supported.payload.is_some());
}

#[test]
fn unreadable_transcript_keeps_a_validated_stop_in_the_turn_fence() {
    let raw =
        json!({"hook_event_name":"Stop", "turn_id":"current", "transcript_path":"missing.jsonl"});
    let result = normalizers::normalize(Some(&raw), "anthropic", |_| None);
    assert_eq!(
        result.classified.detail.kind,
        Some(LifecycleKind::SignalUnavailable)
    );
    assert_eq!(
        result.classified.detail.signal_health,
        SignalHealth::Degraded
    );
    assert!(
        result.payload.is_some(),
        "reconciliation failure does not invalidate an explicit Stop"
    );
    let mut state = HookState::default();
    assert!(state.accepts(Some("current"), true));
    let applied = apply_hook_after_turn_fence(
        -187903,
        &mut state,
        result.payload.as_ref(),
        &result.classified,
        None,
        |state, _, _| {
            assert!(
                !state.is_turn_active(),
                "Stop still ends the known foreground turn"
            );
            Ok(Applied::Applied)
        },
    )
    .unwrap();
    assert!(matches!(applied, Applied::Applied));
}

#[test]
fn canonical_fields_win_over_conflicting_aliases_in_production_normalization() {
    let raw = json!({"hook_event_name":"Stop", "hookEventName":17,
        "session_id":"550e8400-e29b-41d4-a716-446655440000", "sessionId":false});
    let original = raw.clone();
    let result = normalizers::normalize(Some(&raw), "grok", |_| Some(0));
    assert_eq!(result.classified.decision, Decision::Ready);
    assert_eq!(
        result.session_id.as_deref(),
        Some("550e8400-e29b-41d4-a716-446655440000")
    );
    assert_eq!(raw, original);
}

#[test]
fn circuit_receipts_follow_the_trusted_nodes_own_harness_strategy() {
    // The route trusts the node's stored provider, never the payload. Proxied
    // accounts execute their harness; a harness with no declared Circuit hook
    // yields lifecycle attention but never a Circuit receipt (issue #2128).
    let stop = json!({"hook_event_name":"Stop", "session_id":"session", "turn_id":"t1",
        "last_assistant_message":"done"});
    let receipt_provider = |stored: &str| {
        let provider = normalizers::provider_for(stored);
        normalizers::normalize(Some(&stop), &provider, |_| Some(0))
            .native_hook
            .map(|hook| hook.provider.unwrap_or_default())
    };
    assert_eq!(receipt_provider("anthropic").as_deref(), Some("anthropic"));
    assert_eq!(
        receipt_provider("claude:minimax").as_deref(),
        Some("anthropic")
    );
    assert_eq!(receipt_provider("codex").as_deref(), Some("codex"));
    assert_eq!(
        receipt_provider("codex:openrouter").as_deref(),
        Some("codex")
    );
    for stored in [
        "opencode",
        "opencode:account",
        "grok",
        "mcode",
        "cline",
        "kimi",
        "cursor",
        "future-harness:account",
        "",
    ] {
        assert_eq!(receipt_provider(stored), None, "{stored:?}");
    }
}

#[test]
fn malformed_antigravity_stop_cannot_reach_another_harnesss_circuit_state() {
    // A Stop without a valid conversation id is malformed for Antigravity and
    // produces no receipt; the same bytes on a Claude node follow Claude's own
    // contract and record Claude as the parser, so no Antigravity yield or
    // ownership vocabulary can attach to it.
    let malformed =
        json!({"hookEventName":"Stop", "conversationId":"not-a-uuid", "fullyIdle":false});
    let agy = normalizers::normalize(Some(&malformed), "agy", |_| Some(0));
    assert!(agy.native_hook.is_none());
    let claude = normalizers::normalize(Some(&malformed), "anthropic", |_| Some(0));
    if let Some(hook) = claude.native_hook {
        assert_eq!(hook.provider.as_deref(), Some("anthropic"));
        assert!(!hook.background_busy);
    }
}
