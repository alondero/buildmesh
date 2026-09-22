//! Cline hook adapter (issue #1775).
//!
//! Cline's CLI (verified against 3.0.62) discovers *file hooks* — executable
//! files named exactly after an event — from four fixed directories
//! (`~/Documents/Cline/Hooks`, `~/.cline/hooks`, `<ws>/.clinerules/hooks`,
//! `<ws>/.cline/hooks`). Each file is run with a JSON payload on stdin whose
//! `hookName` is the serialized event name (e.g. `agent_end`), not the file
//! name (`TaskComplete`).
//!
//! Buildmesh provisions exactly one of them
//! ([`crate::agent::provider::adapters::cline`]): `TaskComplete` → `agent_end`,
//! a clean turn completion. Cline's file-hook layer has no clean-exit dispatch
//! (`SessionShutdown`/`session_shutdown` is reachable only from the abort
//! branch of `afterRun`, so it fires on a user interrupt — never on teardown —
//! and a still-live session must not be reported as exited), no
//! permission/question primitive under the default auto-approve launch, and no
//! failure signal Buildmesh consumes. Every other recognised Cline event is
//! therefore classified lifecycle-neutral.
//!
//! Cline has no Buildmesh transcript reader yet (issue #1776), so `locate`
//! returns `None` and the file-based `parse` path is unreachable — this
//! adapter exists for its `classify_hook` seam. It is registered in the
//! catalog so `classify_hook` reaches it; `adapter_id_for_format` never yields
//! `"cline"`, so the reader never dispatches here.

use crate::agent::session_lifecycle::LifecycleKind;
use crate::services::transcript_reader::adapter::{
    HookClassification, HookDecision, LocateCtx, TranscriptAdapter,
};
use crate::services::transcript_reader::types::Parsed;
use std::path::PathBuf;

pub(crate) struct ClineAdapter;

/// The Cline file-hook event names this adapter recognises. Payloads carrying
/// one of these are claimed so the attention route's generic "unknown event →
/// degraded attention mark" arm can never fire for a Cline body; anything
/// outside this set is left unclaimed for another adapter.
///
/// `pre_compact` is deliberately absent: `HOOK_CONFIG_FILE_EVENT_MAP` maps the
/// `PreCompact` file to `undefined`, so the file-hook layer never serialises
/// that event.
const CLINE_HOOK_EVENTS: &[&str] = &[
    "agent_start",
    "agent_resume",
    "agent_end",
    "agent_error",
    "agent_abort",
    "tool_call",
    "tool_result",
    "prompt_submit",
    "session_shutdown",
];

impl TranscriptAdapter for ClineAdapter {
    fn id(&self) -> &'static str {
        "cline"
    }

    fn locate(&self, _ctx: LocateCtx<'_>) -> Option<PathBuf> {
        // Cline's canonical history is `<home>/data/sessions/<id>/*.json`; the
        // reader lands with issue #1776. Until then there is no transcript to
        // resolve, so the digest degrades to a spine-only read.
        None
    }

    fn parse(&self, _lines: Box<dyn Iterator<Item = String> + '_>, _keep: usize) -> Parsed {
        Parsed {
            turns: Vec::new(),
            last_assistant_message: None,
            saw_malformed: false,
        }
    }

    fn line_has_assistant_text(&self, _line: &str) -> bool {
        false
    }

    /// Normalise a Cline file-hook payload from its `hookName`.
    ///
    /// - `agent_end` — a completed turn (`afterRun` only calls `runTurnEnd`
    ///   when `result.status === "completed"`) → [`HookDecision::Ready`].
    /// - every other recognised event is lifecycle-neutral
    ///   ([`HookDecision::Ignore`]): Buildmesh provisions no hook for it, and
    ///   classifying it as anything else would mislabel the lifecycle. In
    ///   particular `session_shutdown` is the **abort** dispatch — it fires
    ///   when the user interrupts a *live* session, so it is explicitly not a
    ///   session-exit signal (claiming it here also stops a stale hook file
    ///   from a previous build falling through to the route's degraded arm).
    fn classify_hook(&self, body: &[u8], provider: &str) -> Option<HookClassification> {
        if provider != "cline" {
            return None;
        }
        let payload: serde_json::Value = serde_json::from_slice(body).ok()?;
        let event = payload
            .get("hookName")
            .or_else(|| payload.get("hook_name"))
            .and_then(|value| value.as_str())
            .map(str::to_ascii_lowercase)?;
        if !CLINE_HOOK_EVENTS.contains(&event.as_str()) {
            return None;
        }
        match event.as_str() {
            "agent_end" => Some(HookClassification {
                decision: HookDecision::Ready,
                kind: Some(LifecycleKind::TurnCompleted),
            }),
            _ => Some(HookClassification {
                decision: HookDecision::Ignore,
                kind: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_end_maps_to_a_clean_turn_completion() {
        let body = br#"{"hookName":"agent_end","taskId":"session_1790003303940_9ouga","turn":{"status":"completed","outputText":"done"}}"#;
        let classified = ClineAdapter.classify_hook(body, "cline").expect("claimed");
        assert_eq!(classified.decision, HookDecision::Ready);
        assert_eq!(classified.kind, Some(LifecycleKind::TurnCompleted));
    }

    /// `session_shutdown` is the abort dispatch, not an exit: it fires when the
    /// user interrupts a still-live session. It must stay lifecycle-neutral —
    /// mapping it to `SessionExited` would write `Idle` on a live node (the
    /// blocked #1853 finding).
    #[test]
    fn session_shutdown_is_lifecycle_neutral_not_an_exit() {
        let body = br#"{"hookName":"session_shutdown","taskId":"session_1790003303940_9ouga","reason":"user-cancel"}"#;
        let classified = ClineAdapter.classify_hook(body, "cline").expect("claimed");
        assert_eq!(classified.decision, HookDecision::Ignore);
        assert_eq!(classified.kind, None);
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
            let classified = ClineAdapter
                .classify_hook(body.as_bytes(), "cline")
                .unwrap_or_else(|| panic!("{event} must be claimed"));
            assert_eq!(
                classified.decision,
                HookDecision::Ignore,
                "{event} must stay lifecycle-neutral"
            );
            assert_eq!(classified.kind, None);
        }
    }

    /// `PreCompact` maps to `undefined` in Cline's file-hook table, so the
    /// event is never serialised and must not be claimed.
    #[test]
    fn pre_compact_is_not_a_claimable_event() {
        assert!(ClineAdapter
            .classify_hook(br#"{"hookName":"pre_compact"}"#, "cline")
            .is_none());
    }

    #[test]
    fn unknown_hook_name_is_unclaimed() {
        assert!(ClineAdapter
            .classify_hook(br#"{"hookName":"mystery"}"#, "cline")
            .is_none());
        assert!(ClineAdapter
            .classify_hook(br#"{"notAHook":true}"#, "cline")
            .is_none());
        assert!(ClineAdapter.classify_hook(b"not json", "cline").is_none());
    }

    /// Provider gate: a body claiming a Cline event but arriving for a
    /// different harness must not be classified here.
    #[test]
    fn other_providers_are_not_claimed() {
        let body = br#"{"hookName":"agent_end"}"#;
        assert!(ClineAdapter.classify_hook(body, "codex").is_none());
        assert!(ClineAdapter.classify_hook(body, "").is_none());
    }
}
