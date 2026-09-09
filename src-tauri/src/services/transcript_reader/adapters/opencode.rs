//! OpenCode transcript adapter (issue #1296).
//!
//! OpenCode stores all session messages in a single local SQLite database
//! (`~/.local/share/opencode/opencode.db`), not in per-session JSONL files.
//! The reader bypasses the file-based `locate`/`parse` chain entirely:
//! `locate` returns `None`, `parse` is never reached. The reader's
//! `read_tail` / `read_last_assistant_message` / `read_assistant_report`
//! short-circuit on `OpenCodeAdapter` and call
//! `read_opencode_tail` / `read_opencode_digest` / `opencode_assistant_report`
//! directly.
//!
//! Step 7 of #1661 will fold the reader's `opencode_resolve` (which calls
//! `opencode_session::is_opencode_session_id` and `opencode_db_path`) into
//! a dedicated method on this adapter, inverting the current
//! `transcript -> opencode_session` dependency.

use std::path::PathBuf;

use crate::services::transcript_reader::adapter::{LocateCtx, TranscriptAdapter};
use crate::services::transcript_reader::types::Parsed;

/// Drop-in [`TranscriptAdapter`] for OpenCode.
///
/// `locate` returns `None` and `parse` is unreachable because the reader
/// short-circuits on `id() == "opencode"` to call the SQLite reader
/// directly (see `read_tail` / `read_last_assistant_message` in the
/// parent module).
pub(crate) struct OpenCodeAdapter;

impl TranscriptAdapter for OpenCodeAdapter {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn locate(&self, _ctx: LocateCtx<'_>) -> Option<PathBuf> {
        // OpenCode has no per-session transcript file (issue #1296); the
        // reader routes through `read_opencode_*` instead. Returning `None`
        // is the documented marker for "this adapter is database-backed".
        None
    }

    fn parse(&self, _lines: Box<dyn Iterator<Item = String> + '_>, _keep: usize) -> Parsed {
        // Unreachable: the reader short-circuits before this is called.
        // The empty `Parsed` mirrors the wildcard arm in the legacy
        // `parse_transcript` dispatch — degrades as `Empty` if reached.
        Parsed {
            turns: Vec::new(),
            last_assistant_message: None,
            saw_malformed: false,
        }
    }

    fn line_has_assistant_text(&self, _line: &str) -> bool {
        // OpenCode's per-line JSON is the SQLite `message.data` blob, not a
        // file-based JSONL line — the digest window uses
        // `read_opencode_digest` instead, so this is unreachable in practice.
        false
    }
}