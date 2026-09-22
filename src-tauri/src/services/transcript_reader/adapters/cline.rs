//! Cline hook adapter (issue #1775).
//!
//! Cline's CLI (verified against 3.0.62) discovers *file hooks* — executable
//! files named exactly after an event — from four fixed directories
//! (`~/Documents/Cline/Hooks`, `~/.cline/hooks`, `<ws>/.clinerules/hooks`,
//! `<ws>/.cline/hooks`). Each file is run with a JSON payload on stdin whose
//! `hookName` is the serialized event name (e.g. `agent_end`), not the file
//! name (`TaskComplete`).
//!
//! Buildmesh provisions only the two events it can honestly normalise
//! ([`crate::agent::provider::adapters::cline`]): `TaskComplete` →
//! `agent_end` (a clean turn completion) and `SessionShutdown` →
//! `session_shutdown` (the session process going away). Cline has no
//! permission/prompt primitive under its default auto-approve launch, so
//! nothing else is claimed.
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
const CLINE_HOOK_EVENTS: &[&str] = &[
    "agent_start",
    "agent_resume",
    "agent_end",
    "agent_error",
    "agent_abort",
    "tool_call",
    "tool_result",
    "prompt_submit",
    "pre_compact",
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
    /// - `agent_end` — a completed turn (`afterRun` fires only when
    ///   `status === "completed"`) → [`HookDecision::Ready`].
    /// - `session_shutdown` → [`HookDecision::SessionExited`].
    /// - every other recognised Cline event is lifecycle-neutral
    ///   ([`HookDecision::Ignore`]) because Buildmesh provisions no hook for it;
    ///   claiming it keeps the route's degraded-fallback from firing.
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
            "session_shutdown" => Some(HookClassification {
                decision: HookDecision::SessionExited,
                kind: Some(LifecycleKind::SessionExited),
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

    #[test]
    fn session_shutdown_maps_to_session_exited() {
        let body = br#"{"hookName":"session_shutdown","taskId":"session_1790003303940_9ouga","reason":"user-exit"}"#;
        let classified = ClineAdapter.classify_hook(body, "cline").expect("claimed");
        assert_eq!(classified.decision, HookDecision::SessionExited);
        assert_eq!(classified.kind, Some(LifecycleKind::SessionExited));
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
            "pre_compact",
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
