//! Claude Code transcript adapter — the default transcript format for any
//! harness id that doesn't have its own registered adapter (mirrors the
//! legacy `TranscriptFormat::for_harness` default arm). Cursor reuses the
//! same message shape but a workspace-scoped path, so Cursor's adapter
//! shares Claude Code's parser.
//!
//! Issue #1661 step 1: this is the **first** harness behind the seam, so
//! the seam is real from day one. Future harnesses drop in by adding an
//! `adapters/<name>.rs` file and a catalog entry — not by editing the
//! reader's parallel `match` tables.

use std::path::PathBuf;

use crate::services::transcript_reader::adapter::{LocateCtx, TranscriptAdapter};
use crate::services::transcript_reader::types::Parsed;

use super::super::{concat_text_blocks, parse_turns, transcript_path};

/// Drop-in [`TranscriptAdapter`] for Claude Code (also the default for any
/// unknown harness id).
pub(crate) struct ClaudeCodeAdapter;

impl TranscriptAdapter for ClaudeCodeAdapter {
    fn id(&self) -> &'static str {
        "claude_code"
    }

    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        Some(transcript_path(ctx.session_id, ctx.node_path))
    }

    fn parse(&self, lines: Box<dyn Iterator<Item = String> + '_>, keep: usize) -> Parsed {
        parse_turns(lines, keep)
    }

    fn line_has_assistant_text(&self, line: &str) -> bool {
        // Same predicate the reader's `line_has_assistant_text` matches on
        // the Claude Code / Cursor arms (issue #341). Cursor reuses this
        // exact shape, which is why Cursor's adapter can delegate here once
        // step 5 lands.
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        value.get("type").and_then(|kind| kind.as_str()) == Some("assistant")
            && value
                .get("message")
                .and_then(|message| message.get("role"))
                .and_then(|role| role.as_str())
                == Some("assistant")
            && !concat_text_blocks(
                value.get("message").and_then(|message| message.get("content")),
            )
            .trim()
            .is_empty()
    }
}