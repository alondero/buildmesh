//! Shared Claude-Code path + content primitives (issue #1661 step 5).
//!
//! Both `transcript_reader` (the Claude Code adapter's parser + locator)
//! and `agent_node_discovery` (the resumable-session scanner) reason over
//! Claude Code's on-disk JSONL shape: the `projects/<encoded cwd>/`
//! directory layout, the `tool_use` content-block family, the synthetic-
//! injection wrappers, and the per-block text-extraction rules. Hoisting
//! these primitives out of the reader's god-module (and away from a
//! per-adapter location) gives both consumers one source of truth for
//! the Claude Code wire — a future rename or field-add breaks in one
//! place rather than drifting between two copies.
//!
//! **`truncate` deliberately does NOT live here** — it's a
//! format-agnostic byte-bounded string utility that lives in
//! `transcript_reader::types` per issue #340 history of duplicated-fn
//! drift between the reader and discovery.

/// Encode a filesystem path the same way Claude Code does for its
/// `~/.claude/projects/<encoded>` directory names: replace every
/// non-alphanumeric character with `-`. On Windows this collapses the
/// drive colon and `\` separators (and `.` in `.claude`); on Unix it
/// covers `/`.
///
/// So `X:\src\buildmesh\.claude\worktrees\foo` round-trips to
/// `X--src-buildmesh--claude-worktrees-foo`.
pub fn encode_path(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// True when raw message text is a synthetic Claude Code injection rather
/// than genuine user input (e.g. the `local-command-caveat` wrapper).
/// Such lines are not real turns and must be skipped.
pub fn is_synthetic_message(text: &str) -> bool {
    text.trim_start().starts_with("<local-command-caveat>")
}

/// Pull the text out of a message `content` field, which Claude Code writes
/// either as a bare string (user prompts) or as an array of typed blocks
/// (assistant output, tool results). Only `text` blocks contribute;
/// `thinking`, `tool_use`, `tool_result`, `image`, etc. are not text.
/// Multiple text blocks are joined with newlines.
pub fn concat_text_blocks(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Pull the text of only the **first** `text` block out of a message
/// `content` field (or the whole string for a bare-string content). Unlike
/// [`concat_text_blocks`] this never joins multiple blocks:
/// `agent_node_discovery` wants a single-line session *title* from the
/// opening prompt, and joining all blocks with `\n` (which its
/// `strip_tags` doesn't collapse) would corrupt the title for a multi-
/// text-block user message (issue #335). For the common single-block
/// message the two functions are identical.
pub fn first_text_block(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .find_map(|b| b.get("text").and_then(|t| t.as_str()))
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}