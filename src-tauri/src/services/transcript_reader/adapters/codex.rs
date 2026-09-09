//! Codex transcript adapter (issue #885, #887).
//!
//! Codex writes `rollout-<timestamp>-<session-id>.jsonl` files under
//! `~/.codex/sessions/YYYY/MM/DD/`. Lines are `{"type": <envelope>,
//! "payload": {...}}` envelopes — `parse_codex_turns` extracts the
//! `message`, `function_call`, and `function_call_output` payload kinds.
//!
//! Step 3 of #1661 will fold the reader's `find_codex_rollout` (rollout
//! tree walk) into `locate` here, eliminating the duplicated tree walk in
//! `services::codex_session`.

use std::path::PathBuf;

use crate::services::transcript_reader::adapter::{LocateCtx, TranscriptAdapter};
use crate::services::transcript_reader::types::Parsed;
use crate::services::transcript_reader::codex_concat_text;

use super::super::{find_codex_rollout, parse_codex_turns};

/// Drop-in [`TranscriptAdapter`] for Codex.
pub(crate) struct CodexAdapter;

impl TranscriptAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        // Codex keys sessions globally by id; `node_path` is ignored.
        let _ = ctx.node_path;
        find_codex_rollout(ctx.session_id)
    }

    fn parse(&self, lines: Box<dyn Iterator<Item = String> + '_>, keep: usize) -> Parsed {
        parse_codex_turns(lines, keep)
    }

    fn line_has_assistant_text(&self, line: &str) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        matches!(
            value.get("type").and_then(|kind| kind.as_str()),
            Some("response_item") | Some("event_msg")
        ) && value
            .get("payload")
            .is_some_and(|payload| {
                payload.get("type").and_then(|kind| kind.as_str()) == Some("message")
                    && payload.get("role").and_then(|role| role.as_str()) == Some("assistant")
                    && !payload
                        .get("content")
                        .map(codex_concat_text)
                        .unwrap_or_default()
                        .trim()
                        .is_empty()
            })
    }
}