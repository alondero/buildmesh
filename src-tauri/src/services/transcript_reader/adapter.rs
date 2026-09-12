//! The `TranscriptAdapter` seam (issue #1661).
//!
//! After the split the reader module keeps envelope types ([`super::types`])
//! and one dispatch site per concern. One trait sits between the catalog and
//! per-harness adapters:
//!
//! ```text
//! reader module (envelope + truncation/scrub only, deep: small interface)
//!              │ seam: TranscriptAdapter { locate, parse, line_has_assistant_text }
//!    ┌─────────┼──────────┬──────────────┬─── …nth adapter
//! claude_code agy        codex     cursor  commandcode  grok  opencode
//! adapter     adapter    adapter   adapter  adapter     adapter adapter
//! ```
//!
//! Catalog dispatch asks the seam — never per-format lore. Adding harness N
//! means adding one `adapters/<name>.rs` file and one catalog entry (drop-in
//! adapter to delete), not editing four parallel `match` tables.

use std::path::PathBuf;

use super::adapters::{
    AgyAdapter, ClaudeCodeAdapter, CodexAdapter, CommandCodeAdapter, CursorAdapter, GrokAdapter,
    MuseAdapter, OpenCodeAdapter,
};
use super::types::Parsed;

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
pub(crate) trait TranscriptAdapter: Send + Sync {
    /// Harness id this adapter handles (`"claude_code"`, `"codex"`, …).
    /// Matches the keys returned by `TranscriptFormat::for_harness` today; the
    /// enum is replaced by a registry lookup in step 1.
    fn id(&self) -> &'static str;

    /// Resolve the on-disk transcript path for a session. `None` means "no
    /// transcript exists" (only the Codex walk can conclude that before an
    /// `exists()` check). OpenCode returns `None` because its data lives in a
    /// shared SQLite DB rather than per-session files (issue #1296).
    fn locate(&self, ctx: LocateCtx<'_>) -> Option<PathBuf>;

    /// Parse JSONL lines into the shared [`Parsed`] contract: bounded turn
    /// window, whole-stream last-assistant tracking, malformed flag.
    /// File-based harnesses implement this directly; OpenCode's adapter
    /// implements a different read entry point and the reader's OpenCode
    /// path bypasses this method.
    ///
    /// `Box<dyn Iterator>` (not `impl Iterator`) so the trait stays
    /// object-safe — the catalog dispatches `&'static dyn TranscriptAdapter`.
    /// The Box allocation is per-parse-call; the parsers themselves are
    /// streaming.
    fn parse(&self, lines: Box<dyn Iterator<Item = String> + '_>, keep: usize) -> Parsed;

    /// Cheap per-line check: does this JSONL line carry assistant text?
    /// Used by the digest reader to find the latest assistant message in a
    /// 256 KiB window without running a full parser on every line (issue
    /// #341). OpenCode returns `false` because its per-line JSON doesn't
    /// match the file-based shape.
    fn line_has_assistant_text(&self, line: &str) -> bool;

    /// Per-harness hook classification. The attention route calls this
    /// first; `Some(classified)` short-circuits to that decision, `None`
    /// falls through to the shared post-processing gates (transcript
    /// scan, AGY's `fullyIdle == false` shape gate). Most adapters return
    /// `None` for every payload; OpenCode (session.idle / session.created),
    /// Grok (notification_type), and Claude Code (the "needs your
    /// permission" prose substring) carry their own logic here.
    ///
    /// `provider` is the harness id from the hook payload (often empty
    /// for legacy Claude Code hooks). Adapters whose classifier keys
    /// on body content alone (Grok, Claude Code) ignore it; OpenCode's
    /// `session.idle` / `session.created` event names are
    /// OpenCode-specific, so OpenCodeAdapter gates on `provider` to
    /// avoid false-positives if a sibling harness ever borrowed the
    /// same event names.
    fn classify_hook(
        &self,
        _body: &[u8],
        _provider: &str,
    ) -> Option<HookClassification> {
        None
    }

    /// Verify the attention-route token gate (issue #1366 round-2 +
    /// round-3). The default accepts every callback; Grok's adapter
    /// implements the strict minted-token check.
    fn verify_attention_token(
        &self,
        _query_string: Option<&str>,
        _minted: Option<&str>,
    ) -> bool {
        true
    }
}

/// What an adapter's [`TranscriptAdapter::classify_hook`] returns. `Some(_)`
/// short-circuits the attention route's shared post-processing; `None`
/// falls through to the transcript-scan fallback and the AGY `fullyIdle`
/// shape gate.
#[derive(Debug, Clone)]
pub(crate) struct HookClassification {
    pub decision: HookDecision,
    pub kind: Option<crate::agent::session_lifecycle::LifecycleKind>,
}

/// Decision an adapter's hook classifier returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookDecision {
    /// User input needed — land the node in `AwaitingInput`.
    MarkInput,
    /// Turn finished cleanly — land in `Ready` (never the autopilot-only
    /// `Completed`).
    Ready,
    /// Capture-only event (e.g. OpenCode's `session.created` carrying
    /// the freshly minted `ses_…` id) — publish the Node Turn
    /// without attention marking.
    Ignore,
}

static CLAUDE_CODE_ADAPTER: ClaudeCodeAdapter = ClaudeCodeAdapter;
static AGY_ADAPTER: AgyAdapter = AgyAdapter;
static CODEX_ADAPTER: CodexAdapter = CodexAdapter;
static CURSOR_ADAPTER: CursorAdapter = CursorAdapter;
static COMMANDCODE_ADAPTER: CommandCodeAdapter = CommandCodeAdapter;
static GROK_ADAPTER: GrokAdapter = GrokAdapter;
static MUSE_ADAPTER: MuseAdapter = MuseAdapter;
static OPENCODE_ADAPTER: OpenCodeAdapter = OpenCodeAdapter;

static ADAPTERS: [&'static dyn TranscriptAdapter; 8] = [
    // Claude Code is the default format for any harness id that doesn't have
    // a registered adapter (mirrors `TranscriptFormat::for_harness`'s default
    // arm). Listed first so a future "explicit claude-code harness id" maps
    // there directly.
    &CLAUDE_CODE_ADAPTER,
    // Every other adapter is keyed by its harness id; an unknown harness id
    // falls back to Claude Code.
    &AGY_ADAPTER,
    &CODEX_ADAPTER,
    &CURSOR_ADAPTER,
    &COMMANDCODE_ADAPTER,
    &GROK_ADAPTER,
    &MUSE_ADAPTER,
    &OPENCODE_ADAPTER,
];

/// Seam entry point: `catalog.dispatch(id)` proves the seam — not the old
/// module — is the dispatch and test surface. Returns `None` for unknown
/// harness ids (the caller is expected to fall back to Claude Code, the
/// default adapter).
pub(crate) fn dispatch(harness_id: &str) -> Option<&'static dyn TranscriptAdapter> {
    ADAPTERS.iter().copied().find(|a| a.id() == harness_id)
}

/// Iterate every registered adapter's [`TranscriptAdapter::classify_hook`]
/// in registration order, returning the first non-`None` decision. The
/// attention route calls this once per hook POST; most adapters return
/// `None` (default impl) for every payload, so the cost is one
/// function call per harness id. Used because the route cannot rely on
/// the `provider` field alone (the legacy hooks send empty strings) and
/// per-harness classifiers inspect the body itself (OpenCode's
/// `session.idle`, Grok's `notificationType`, Claude Code's
/// "needs your permission" prose substring).
pub(crate) fn classify_hook(body: &[u8], provider: &str) -> Option<HookClassification> {
    ADAPTERS
        .iter()
        .copied()
        .find_map(|adapter| adapter.classify_hook(body, provider))
}

/// Default adapter (Claude Code). Returned for any harness id without an
/// explicit registration, matching the legacy `TranscriptFormat::for_harness`
/// default arm.
pub(crate) fn default_adapter() -> &'static dyn TranscriptAdapter {
    &CLAUDE_CODE_ADAPTER
}