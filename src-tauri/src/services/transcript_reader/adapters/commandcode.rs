//! Command Code transcript adapter (issues #1407, #1500).
//!
//! Command Code writes structured per-session JSONL under
//! `~/.commandcode/projects/<encoded-cwd>/<session-id>.jsonl` — one
//! `{type: "message", message: {role, content}}` event per line.
//!
//! Step 3 of #1661 will fold the reader's `commandcode_sessions_dir` path
//! composition into `locate` here. The capture poller in
//! `services::commandcode_session` already delegates to
//! `commandcode_sessions_dir`, so the seam-inversion is one import rewrite.

use std::path::PathBuf;

use crate::services::transcript_reader::adapter::{LocateCtx, TranscriptAdapter};
use crate::services::transcript_reader::types::Parsed;

use super::super::{concat_text_blocks, find_commandcode_transcript, parse_commandcode_turns};

/// Drop-in [`TranscriptAdapter`] for Command Code.
pub(crate) struct CommandCodeAdapter;

impl TranscriptAdapter for CommandCodeAdapter {
    fn id(&self) -> &'static str {
        "commandcode"
    }

    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf> {
        find_commandcode_transcript(ctx.session_id, ctx.node_path)
    }

    fn parse(&self, lines: Box<dyn Iterator<Item = String> + '_>, keep: usize) -> Parsed {
        parse_commandcode_turns(lines, keep)
    }

    fn line_has_assistant_text(&self, line: &str) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        value.get("type").and_then(|kind| kind.as_str()) == Some("message")
            && value.get("message").is_some_and(|message| {
                message.get("role").and_then(|role| role.as_str()) == Some("assistant")
                    && !concat_text_blocks(message.get("content"))
                        .trim()
                        .is_empty()
            })
    }
}