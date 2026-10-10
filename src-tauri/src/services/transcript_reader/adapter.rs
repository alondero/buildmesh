//! Transcript-reader seam and the existing provider-hook registry.
//!
//! Each reader owns location, parsing, tail/digest reads and assistant reports.
//! JSONL readers share streaming defaults; document and SQLite readers override
//! the relevant methods. Unknown transcript harnesses never use a default reader.
//! The Claude default below supplies token verification for legacy callbacks.

use std::path::{Path, PathBuf};

use super::readers::{
    AgyAdapter, ClaudeCodeAdapter, ClineAdapter, CodexAdapter, CommandCodeAdapter, CursorAdapter,
    GrokAdapter, McodeAdapter, MuseAdapter, OpenCodeAdapter,
};
use super::types::{AssistantReport, Parsed, UnavailableReason};

/// Inputs for [`TranscriptAdapter::locate`]: a session id and the node's
/// working directory. The harness-specific locator decides what (if anything)
/// to do with each. `session_id` is the node's `cli_session_id`; `node_path`
/// is the agent's cwd (used by Claude Code's project directory encoding,
/// Cursor's workspace slug, etc.; ignored by Codex, which keys sessions
/// globally by id).
pub struct LocateCtx<'a> {
    pub session_id: &'a str,
    pub node_path: &'a str,
}

/// One first-class transcript format behind the seam.
///
/// Implementors are drop-in `&'static` references registered in this module's
/// static adapter table — the registry returns them by `id()` so the reader
/// and attention modules have one dispatch site per concern.
pub(crate) trait TranscriptReader: Send + Sync {
    /// Harness id this adapter handles (`"claude_code"`, `"codex"`, …).
    /// Overlaps the ids `TranscriptFormat::for_harness` resolves (which
    /// returns `None` for unwired harnesses since issue #1817); the
    /// enum remains the public compatibility surface.
    fn id(&self) -> &'static str;

    /// Resolve the on-disk transcript path for a session. `None` means "no
    /// transcript exists" (only the Codex walk can conclude that before an
    /// `exists()` check). A database reader locates its shared store.
    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf>;

    /// Parse JSONL lines into the shared [`Parsed`] contract: bounded turn
    /// window, whole-stream last-assistant tracking, malformed flag.
    /// File-based harnesses implement this directly; OpenCode's adapter
    /// overrides the store read methods instead.
    ///
    /// `Box<dyn Iterator>` (not `impl Iterator`) so the trait stays
    /// object-safe — the catalog dispatches `&'static dyn TranscriptAdapter`.
    /// The Box allocation is per-parse-call; the parsers themselves are
    /// streaming.
    /// `max_text` caps display previews; circuit reports retain full text
    /// from their bounded input window so a trailing verdict is not lost.
    fn parse(
        &self,
        lines: Box<dyn Iterator<Item = String> + '_>,
        keep: usize,
        max_text: usize,
    ) -> Parsed;

    /// Cheap per-line check: does this JSONL line carry assistant text?
    /// Used by the digest reader to find the latest assistant message in a
    /// 256 KiB window without running a full parser on every line (issue
    /// #341). OpenCode returns `false` because its per-line JSON doesn't
    /// match the file-based shape.
    fn line_has_assistant_text(&self, line: &str) -> bool;

    /// Read recent turns from the resolved store. File readers stream JSONL;
    /// database readers receive the session id alongside their shared store.
    fn read_tail(
        &self,
        path: &Path,
        _session_id: &str,
        keep: usize,
    ) -> Result<Parsed, UnavailableReason> {
        super::file::read_tail(self, path, keep)
    }

    /// Read a bounded digest window, retaining enough turns to identify availability.
    /// The dispatcher alone shapes the available/unavailable envelope.
    fn last_assistant_message(
        &self,
        path: &Path,
        _session_id: &str,
    ) -> Result<Parsed, UnavailableReason> {
        super::file::last_assistant_message(self, path)
    }

    /// Read the current assistant report from this reader's own store.
    fn assistant_report(&self, path: &Path, _session_id: &str) -> Option<AssistantReport> {
        super::file::assistant_report(self, path)
    }

    /// Explicit completion of the latest turn, when the harness records it.
    /// Prose and terminal silence are not lifecycle evidence.
    fn completed_turn(&self, _lines: &str) -> Option<super::NativeTurnCompletion> {
        None
    }

    /// A finished background task this harness recorded but cannot deliver to an
    /// idle session on its own (issue #2105).
    ///
    /// `lines` is this session's own transcript, `spawn_path` the CLI's cwd
    /// (which decides which data dir holds its runtime state), and `now_ms` the
    /// caller's clock, so a harness that judges quiescence can be driven
    /// deterministically from a test.
    ///
    /// The default is `None`: a harness that delegates background work to the
    /// session and never announces its completion has no such gap, and one that
    /// does announce it is not stalled. Each reader owns both the record shape
    /// that names a task and the on-disk layout that reports its end, so the
    /// worker never guesses a harness's private formats.
    fn stalled_background_task(
        &self,
        _lines: &str,
        _spawn_path: &str,
        _now_ms: i64,
    ) -> Option<String> {
        None
    }

    /// Verify the attention-route token gate (issue #1366 round-2 +
    /// round-3). The default accepts every callback; Grok's adapter
    /// implements the strict minted-token check.
    fn verify_attention_token(&self, _query_string: Option<&str>, _minted: Option<&str>) -> bool {
        true
    }
}

// Existing hook consumers keep the original trait name.
pub(crate) use TranscriptReader as TranscriptAdapter;

static CLAUDE_CODE_ADAPTER: ClaudeCodeAdapter = ClaudeCodeAdapter;
static AGY_ADAPTER: AgyAdapter = AgyAdapter;
static CLINE_ADAPTER: ClineAdapter = ClineAdapter;
static CODEX_ADAPTER: CodexAdapter = CodexAdapter;
static CURSOR_ADAPTER: CursorAdapter = CursorAdapter;
static COMMANDCODE_ADAPTER: CommandCodeAdapter = CommandCodeAdapter;
static GROK_ADAPTER: GrokAdapter = GrokAdapter;
static MCODE_ADAPTER: McodeAdapter = McodeAdapter;
static MUSE_ADAPTER: MuseAdapter = MuseAdapter;
static OPENCODE_ADAPTER: OpenCodeAdapter = OpenCodeAdapter;

static ADAPTERS: [&'static dyn TranscriptAdapter; 10] = [
    // The default adapter supplies token verification for compatible hooks.
    // This is NOT the
    // transcript-format resolver: `TranscriptFormat::for_harness` returns
    // `None` for unwired harness ids since issue #1817. Listed first so a
    // future "explicit claude-code harness id" maps there directly.
    &CLAUDE_CODE_ADAPTER,
    // Every other reader is keyed by its harness id. Unknown ids return None.
    &AGY_ADAPTER,
    &CLINE_ADAPTER,
    &MCODE_ADAPTER,
    &CODEX_ADAPTER,
    &CURSOR_ADAPTER,
    &COMMANDCODE_ADAPTER,
    &GROK_ADAPTER,
    &MUSE_ADAPTER,
    &OPENCODE_ADAPTER,
];

/// Seam entry point: `catalog.dispatch(id)` proves the seam — not the old
/// module — is the dispatch and test surface. Returns `None` for unknown
/// harness ids. Transcript reads never fall back to another harness.
pub(crate) fn dispatch(harness_id: &str) -> Option<&'static dyn TranscriptAdapter> {
    ADAPTERS.iter().copied().find(|a| a.id() == harness_id)
}

/// Every registered reader. Used by contract tests that must hold for all of
/// them, so a new harness cannot quietly skip a reader-wide rule.
#[cfg_attr(not(test), expect(dead_code, reason = "Contract-test surface"))]
pub(crate) fn registered() -> &'static [&'static dyn TranscriptAdapter] {
    &ADAPTERS
}

/// Default adapter (Claude Code). Returned for any harness id without an
/// explicit hook registration. Transcript dispatch never uses this fallback.
pub(crate) fn default_adapter() -> &'static dyn TranscriptAdapter {
    &CLAUDE_CODE_ADAPTER
}
