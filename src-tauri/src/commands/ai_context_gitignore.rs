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

/// Marker for the agent harness block. Older blocks still need the
/// provider-neutral worktree pattern before portability is complete.
pub const HEADER: &str = "# Agent Harnesses (runtime, local settings, and ephemeral files)";

/// Incremental addition for repositories with an older harness block.
const WORKTREE_BLOCK: &str = "# Buildmesh worktrees\n.worktrees/\n";

/// Canonical block written into the target `.gitignore`.
pub const BLOCK: &str = "\
# Agent Harnesses (runtime, local settings, and ephemeral files)\n\
.worktrees/\n\
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
.cursor/hooks.json\n\
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

/// Return only the missing block so older portability PRs can be upgraded
/// without duplicating their harness patterns. Inspect lossily; the caller
/// preserves the original bytes when appending.
pub fn missing_block(gitignore_content: &[u8]) -> Option<&'static str> {
    let haystack = String::from_utf8_lossy(gitignore_content);
    if !haystack.lines().any(|line| line.trim() == HEADER) {
        Some(BLOCK)
    } else if !haystack
        .lines()
        .any(|line| matches!(line.trim(), ".worktrees/" | "/.worktrees/"))
    {
        Some(WORKTREE_BLOCK)
    } else {
        None
    }
}

/// True when the harness block and the default worktree pattern are present.
pub fn has_block(gitignore_content: &[u8]) -> bool {
    missing_block(gitignore_content).is_none()
}
