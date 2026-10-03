//! cline hook validation and lifecycle mapping.
use super::super::common::*;
use crate::agent::session_lifecycle::HookSignalDetail;
use serde_json::Value;
use std::path::Path;

pub(super) fn parse(value: &Value) -> Option<HookPayload> {
    HookPayload::parse_for(value, COMPATIBLE_FIELDS)
}

pub(super) fn classify(
    payload: &HookPayload,
    detail: HookSignalDetail,
    _count_pending: &mut dyn FnMut(&Path) -> Option<usize>,
) -> Classified {
    match payload.event().as_str() {
        "agentend" => Classified::ready(detail),
        "agentstart" | "agentresume" | "agenterror" | "agentabort" | "toolcall" | "toolresult"
        | "promptsubmit" | "sessionshutdown" | "pretooluse" => ignore(detail),
        _ => unavailable(detail),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn replay() {
        super::super::replay(
            "cline",
            include_str!("../../../../../tests/fixtures/attention/cline.json"),
        );
    }

    use super::*;
    use crate::agent::session_lifecycle::LifecycleKind;
    fn classify_hook(body: &[u8], provider: &str) -> Option<Classified> {
        let value: Value = serde_json::from_slice(body).ok()?;
        let normalized = super::super::normalize(Some(&value), provider, |_| Some(0));
        (normalized.classified.detail.kind != Some(LifecycleKind::SignalUnavailable))
            .then_some(normalized.classified)
    }
    #[test]
    fn agent_end_maps_to_a_clean_turn_completion() {
        let body = br#"{"hookName":"agent_end","taskId":"session_1790003303940_9ouga","turn":{"status":"completed","outputText":"done"}}"#;
        let classified = classify_hook(body, "cline").expect("claimed");
        assert_eq!(classified.decision, Decision::Ready);
        assert_eq!(classified.detail.kind, Some(LifecycleKind::TurnCompleted));
    }

    /// `session_shutdown` is the abort dispatch, not an exit: it fires when the
    /// user interrupts a still-live session. It must stay lifecycle-neutral —
    /// mapping it to `SessionExited` would write `Idle` on a live node (the
    /// blocked #1853 finding).
    #[test]
    fn session_shutdown_is_lifecycle_neutral_not_an_exit() {
        let body = br#"{"hookName":"session_shutdown","taskId":"session_1790003303940_9ouga","reason":"user-cancel"}"#;
        let classified = classify_hook(body, "cline").expect("claimed");
        assert_eq!(classified.decision, Decision::Ignore);
        assert_eq!(classified.detail.kind, None);
    }

    /// A Cline event Buildmesh does not provision must be claimed as
    /// lifecycle-neutral: falling through would let the route's generic
    /// "unknown event" arm mark the node for attention with degraded health.
    #[test]
    fn unprovisioned_cline_events_are_lifecycle_neutral() {
        for event in [
            "tool_call",
            "tool_result",
            "prompt_submit",
            "agent_start",
            "agent_resume",
            "agent_error",
            "agent_abort",
        ] {
            let body = format!(r#"{{"hookName":"{event}","taskId":"session_1_abcde"}}"#);
            let classified = classify_hook(body.as_bytes(), "cline")
                .unwrap_or_else(|| panic!("{event} must be claimed"));
            assert_eq!(
                classified.decision,
                Decision::Ignore,
                "{event} must stay lifecycle-neutral"
            );
            assert_eq!(classified.detail.kind, None);
        }
    }

    /// `PreCompact` maps to `undefined` in Cline's file-hook table, so the
    /// event is never serialised and must not be claimed.
    #[test]
    fn pre_compact_is_not_a_claimable_event() {
        assert!(classify_hook(br#"{"hookName":"pre_compact"}"#, "cline").is_none());
    }

    #[test]
    fn unknown_hook_name_is_unclaimed() {
        assert!(classify_hook(br#"{"hookName":"mystery"}"#, "cline").is_none());
        assert!(classify_hook(br#"{"notAHook":true}"#, "cline").is_none());
        assert!(classify_hook(b"not json", "cline").is_none());
    }

    /// Provider gate: a body claiming a Cline event but arriving for a
    /// different harness must not be classified here.
    #[test]
    fn other_providers_are_not_claimed() {
        let body = br#"{"hookName":"agent_end"}"#;
        assert!(classify_hook(body, "codex").is_none());
        assert!(classify_hook(body, "").is_none());
    }
}
