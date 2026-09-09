//! Cursor CLI transcript adapter.
//!
//! Cursor writes per-session JSONL under
//! `~/.cursor/projects/<workspace-slug>/agent-transcripts/<session>/
//! <session>.jsonl` with the **same message shape** as Claude Code, but
//! the workspace-scoped path. Step 5 of #1661 will replace the duplicate
//! `parse_turns` call here with a delegation to `ClaudeCodeAdapter::parse`
//! (Cursor's wire is byte-identical to Claude Code's).

use std::path::PathBuf;

use crate::services::transcript_reader::adapter::{LocateCtx, TranscriptAdapter};
use crate::services::transcript_reader::types::Parsed;

use super::super::{cursor_transcript_path, parse_turns};

/// Drop-in [`TranscriptAdapter`] for Cursor.
pub(crate) struct CursorAdapter;

impl TranscriptAdapter for CursorAdapter {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        // Cursor's path layout always builds a path; the file-existence
        // check happens at the reader's call site (matches the legacy
        // `locate_transcript` Cursor arm).
        Some(cursor_transcript_path(ctx.session_id, ctx.node_path))
    }

    fn parse(&self, lines: Box<dyn Iterator<Item = String> + '_>, keep: usize) -> Parsed {
        // Cursor's JSONL shape matches Claude Code's — same `parse_turns`.
        parse_turns(lines, keep)
    }

    fn line_has_assistant_text(&self, line: &str) -> bool {
        // Same predicate Claude Code uses (Cursor's envelope matches).
        crate::services::transcript_reader::adapters::claude_code::ClaudeCodeAdapter
            .line_has_assistant_text(line)
    }
}