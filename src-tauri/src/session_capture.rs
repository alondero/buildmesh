//! Captures provider-assigned session IDs from PTY output.
//!
//! Some providers (e.g. Codex) auto-assign session UUIDs rather than accepting
//! one via CLI flag. This module watches PTY output and stores the captured ID
//! in the database for future resume operations.
//!
//! Limitation: capture happens at PTY read time. If a node exited before its
//! provider's banner reached the PTY (or before this regex matched it), the
//! node's `cli_session_id` stays NULL; `decide_startup_resume` will refuse to
//! resume the node, leaving it Suspended. Such nodes must be resumed manually,
//! regenerated, or deleted — see issue #1191 for the original regression.

use once_cell::sync::Lazy;
use regex::Regex;

// Captures a UUID preceded by a provider-printed label like `session:`,
// `session id:`, `conversation:`, or `conversation id:`. `(?:\s+id)?` is
// OPTIONAL inside each branch so the same regex handles both the two-word
// Codex banner shape and the legacy single-word shape — for the latter,
// the `[:\s]+` after the alternation consumes the colon itself.
static LABELED_SESSION_ID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(?:session(?:\s+id)?|conversation(?:\s+id)?)[:\s]+([0-9a-f]{6,8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})")
        .unwrap()
});

/// Attempt to extract a session ID from PTY output. Returns the UUID if found.
pub fn try_extract_session_id(data: &str) -> Option<&str> {
    LABELED_SESSION_ID_RE
        .captures(data)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str())
}

#[cfg(test)]
mod tests {
    use super::try_extract_session_id;

    /// Codex's interactive TUI startup banner prints `session id: <UUID>`
    /// (two words, then `:`). The earlier regex only matched the single-word
    /// `session:` shape, so every Codex node's `cli_session_id` stayed NULL
    /// and `decide_startup_resume` was forced to leave them Suspended.
    #[test]
    fn captures_session_id_with_two_word_label() {
        assert_eq!(
            try_extract_session_id("session id: 01a024d2-7cd6-7ea2-b907-531b0d261be7"),
            Some("01a024d2-7cd6-7ea2-b907-531b0d261be7")
        );
    }

    /// The mirror form — `conversation id:` — was already supported; pin it
    /// explicitly so a future shrink of the alternation cannot silently
    /// regress it.
    #[test]
    fn captures_conversation_id_with_two_word_label() {
        assert_eq!(
            try_extract_session_id("conversation id: 01a024d2-7cd6-7ea2-b907-531b0d261be7"),
            Some("01a024d2-7cd6-7ea2-b907-531b0d261be7")
        );
    }

    /// Legacy `session: <UUID>` shape (single word, colon) was the original
    /// supported form. Keep it pinned so the two-word fix doesn't drop it.
    #[test]
    fn captures_session_with_single_word_label() {
        assert_eq!(
            try_extract_session_id("session: 01a024d2-7cd6-7ea2-b907-531b0d261be7"),
            Some("01a024d2-7cd6-7ea2-b907-531b0d261be7")
        );
    }

    /// The single-word `conversation: <UUID>` shape shares its match path
    /// with `session:` (the `[:\s]+` after the alternation consumes the
    /// colon). Pin it explicitly so a future shrink of the alternation
    /// that drops `(?:\s+id)?` from the `conversation` branch cannot
    /// silently regress this shape while leaving the two-word test green.
    #[test]
    fn captures_conversation_with_single_word_label() {
        assert_eq!(
            try_extract_session_id("conversation: 01a024d2-7cd6-7ea2-b907-531b0d261be7"),
            Some("01a024d2-7cd6-7ea2-b907-531b0d261be7")
        );
    }

    /// A bare UUID without a leading label must not match — capturing an
    /// unattributed UUID would race against the orchestrator's pre-write
    /// (`spawn.rs:1544-1547` `let cli_uuid = uuid::Uuid::new_v4()`).
    #[test]
    fn ignores_uuid_without_label() {
        assert!(try_extract_session_id("01a024d2-7cd6-7ea2-b907-531b0d261be7").is_none());
    }

    /// Real Codex banner shape — the label sits in a multi-line block with
    /// ANSI-free prose around it.
    #[test]
    fn extracts_from_real_codex_exec_banner_block() {
        let block = "OpenAI Codex v0.148.0\n--------\nsession id: 01a024d2-7cd6-7ea2-b907-531b0d261be7\n--------\n";
        assert_eq!(
            try_extract_session_id(block),
            Some("01a024d2-7cd6-7ea2-b907-531b0d261be7")
        );
    }
}
