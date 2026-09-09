//! Antigravity (`agy`) transcript adapter (issue #1283, #1367).
//!
//! Per-conversation JSONL under `~/.gemini/antigravity-cli/brain/<conv>/
//! .system_generated/logs/transcript.jsonl` (token-efficient form), with
//! `transcript_full.jsonl` as the untruncated fallback. One JSON object per
//! turn — flat shape, no `message.id` coalescing.
//!
//! Step 2 of #1661 will move `agy_locator_in` + `parse_agy_turns` into
//! this file and remove the `pub(crate)` re-exports in `super::super`.

use std::path::PathBuf;

use crate::services::transcript_reader::adapter::{LocateCtx, TranscriptAdapter};
use crate::services::transcript_reader::types::Parsed;

use super::super::{find_agy_transcript, parse_agy_turns};

/// Drop-in [`TranscriptAdapter`] for Antigravity.
pub(crate) struct AgyAdapter;

impl TranscriptAdapter for AgyAdapter {
    fn id(&self) -> &'static str {
        "agy"
    }

    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        find_agy_transcript(ctx.session_id)
    }

    fn parse(&self, lines: Box<dyn Iterator<Item = String> + '_>, keep: usize) -> Parsed {
        parse_agy_turns(lines, keep)
    }

    fn line_has_assistant_text(&self, line: &str) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        value.get("source").and_then(|source| source.as_str()) == Some("MODEL")
            && value
                .get("content")
                .and_then(|content| content.as_str())
                .is_some_and(|text| !text.trim().is_empty())
    }
}