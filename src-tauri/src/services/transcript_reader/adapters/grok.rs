//! Grok Code transcript adapter (issue #1281).
//!
//! Grok writes per-session directories under
//! `~/.grok/sessions/<urlencoded-cwd>/<session-id>/` carrying
//! `chat_history.jsonl` (the per-message conversation log) and
//! `updates.jsonl` (event-level telemetry). The reader uses
//! `chat_history.jsonl`. Per-message JSON: `{"role", "content"}` where
//! `content` may be a string or an array of typed blocks.

use std::path::PathBuf;

use crate::services::transcript_reader::adapter::{LocateCtx, TranscriptAdapter};
use crate::services::transcript_reader::types::Parsed;

use super::super::{find_grok_transcript, parse_grok_turns};

/// Drop-in [`TranscriptAdapter`] for Grok Code.
pub(crate) struct GrokAdapter;

impl TranscriptAdapter for GrokAdapter {
    fn id(&self) -> &'static str {
        "grok"
    }

    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        find_grok_transcript(ctx.session_id, ctx.node_path)
    }

    fn parse(&self, lines: Box<dyn Iterator<Item = String> + '_>, keep: usize) -> Parsed {
        parse_grok_turns(lines, keep)
    }

    fn line_has_assistant_text(&self, line: &str) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        value.get("role").and_then(|role| role.as_str()) == Some("assistant")
            && value.get("content").is_some_and(|content| match content {
                serde_json::Value::String(text) => !text.trim().is_empty(),
                serde_json::Value::Array(blocks) => blocks
                    .iter()
                    .filter(|block| {
                        block.get("type").and_then(|kind| kind.as_str()) == Some("text")
                    })
                    .filter_map(|block| block.get("text").and_then(|text| text.as_str()))
                    .any(|text| !text.trim().is_empty()),
                _ => false,
            })
    }
}