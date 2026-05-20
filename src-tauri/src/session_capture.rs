//! Captures provider-assigned session IDs from PTY output.
//!
//! Some providers (e.g. Codex) auto-assign session UUIDs rather than accepting
//! one via CLI flag. This module watches PTY output and stores the captured ID
//! in the database for future resume operations.

use once_cell::sync::Lazy;
use regex::Regex;

static CODEX_SESSION_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)session[:\s]+([0-9a-f]{6,8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})")
        .unwrap()
});

/// Attempt to extract a session ID from PTY output. Returns the UUID if found.
pub fn try_extract_session_id(data: &str) -> Option<&str> {
    CODEX_SESSION_RE
        .captures(data)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str())
}
