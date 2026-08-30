//! Canonical `.gitignore` block used by the AI-context portability commit
//! (issue #1401).
//!
//! Why a string constant in a module of its own, rather than living inside
//! `commands::ai_context` or being `include_str!`'d from somewhere else:
//!
//! - **The project's own `.gitignore`** holds session-local patterns
//!   (`.wayfinder/`, `docs/superpowers/`, `*.exe`/`*.pdb` build artefacts,
//!   the local `coverage/` line, etc.). `include_str!`'ing from it would
//!   leak those into a PR opened on someone else's repository.
//! - **The harness-adapter derive approach** (each `provider::adapters::*`
//!   declares its own ignore paths) is cleaner long-term but couples
//!   portability to every adapter's `SpawnRecipe`. Tracked as a future
//!   refactor; for now, the literal block from issue #1401 wins on
//!   simplicity.
//! - **Top-of-module constant in `commands/ai_context.rs`** would dump
//!   ~50 lines of unrelated string data into the Tauri command handler
//!   the reader is trying to understand. A dedicated module makes the
//!   template discoverable and replaceable.

/// Header line that doubles as the idempotency marker. If a `.gitignore`
/// already contains this line, the portability commit is a no-op for
/// `.gitignore` — the existing blob OID is reused via the `TreeBuilder`,
/// no duplicate blob is written.
pub const HEADER: &str = "# Agent Harnesses (runtime, local settings, and ephemeral files)";

/// Full canonical block written into the target `.gitignore`. The contents
/// are the issue #1401 spec block, verbatim.
pub const BLOCK: &str = "\
# Agent Harnesses (runtime, local settings, and ephemeral files)\n\
.codex/\n\
CODEX.local.md\n\
codex.local.md\n\
.agents/hooks.json\n\
.agents/settings.local.json\n\
.agents/tasks/\n\
.agents/memory/\n\
.agents/worktrees/\n\
.agents/sessions/\n\
.agents/tmp/\n\
.agents/*.local.*\n\
AGENTS.local.md\n\
.antigravity/\n\
.antigravitycli/\n\
.gemini/\n\
.opencode/\n\
.open-code/\n\
OPENCODE.local.md\n\
.grok/\n\
GROK.local.md\n\
.mcode/\n\
.dsh/\n\
.kimi/\n\
.cursor/cache/\n\
.cursor/debug/\n\
.cursor/index/\n\
.cursor/tasks/\n\
.cursor/transcripts/\n\
.cursor/worktrees/\n\
.cursor-tutor/\n\
CURSOR.local.md\n\
.aider*\n\
.cline/\n\
.roo/\n\
.roomodes.local\n\
.roomodes.local.json\n\
.goose/\n\
.goosehints.local\n\
.windsurf/\n\
.codeium/\n";

/// True when `gitignore_content` already contains the canonical agent
/// harness ignore block. Used to keep the portability commit idempotent.
///
/// Uses `from_utf8_lossy` so a legacy Windows-1252 / Latin-1 `.gitignore`
/// (uncommon but legal on Windows) does not silently trigger
/// duplicate-block appending — at worst the comment header is mangled
/// for the comparison and we re-append. That's preferable to dropping
/// non-UTF-8 bytes wholesale, and the duplicate is still safe (same
/// bytes again).
pub fn has_block(gitignore_content: &[u8]) -> bool {
    let haystack = String::from_utf8_lossy(gitignore_content);
    haystack.contains(HEADER)
}
