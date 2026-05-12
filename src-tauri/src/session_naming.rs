//! Unified session naming module.
//!
//! Simplified single-map state machine:
//! - `SESSION_BUFFERS` is the only state — buffer exists = hasn't been renamed yet
//! - `on_spawn()` generates an initial random name
//! - `on_output(node_id, data)` buffers PTY output
//! - `on_turn(node_id, app)` triggers LLM rename when buffer is sufficient
//! - `cleanup(node_id)` removes buffer entry
//!
//! The buffer presence IS the renamed state. No separate guard map needed.

use rand::seq::IndexedRandom;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

// ---------------------------------------------------------------------------
// Random name generation (word lists + combinatorics)
// ---------------------------------------------------------------------------

static ADJECTIVES: &[&str] = &[
    "amber", "bold", "brave", "bright", "calm", "clean", "clear", "cool",
    "crisp", "dark", "deep", "dry", "eager", "early", "easy", "fair",
    "fast", "fine", "firm", "flat", "fond", "free", "fresh", "full",
    "glad", "gold", "good", "grand", "great", "green", "happy", "hard",
    "high", "holy", "hot", "huge", "keen", "kind", "lame", "last",
    "late", "lazy", "lean", "light", "live", "lone", "long", "loud",
    "lucky", "mad", "main", "mild", "neat", "new", "nice", "noble",
    "odd", "old", "open", "pale", "plain", "proud", "pure", "quick",
    "quiet", "rare", "raw", "real", "red", "rich", "ripe", "rough",
    "round", "safe", "sharp", "shy", "slim", "slow", "small", "smart",
    "soft", "solid", "sour", "spare", "steep", "still", "strong", "sure",
    "sweet", "tall", "tame", "thick", "thin", "tight", "tiny", "tough",
    "true", "vast", "warm", "weak", "wide", "wild", "wise", "young",
    "zany",
];

static NOUNS: &[&str] = &[
    "arch", "badge", "barn", "beam", "bell", "bird", "blade", "bloom",
    "boat", "bolt", "bone", "book", "bow", "box", "breeze", "brick",
    "brook", "brush", "cairn", "cape", "cave", "chain", "charm", "chest",
    "cliff", "clock", "cloud", "coast", "coin", "coral", "crane", "creek",
    "cross", "crown", "dawn", "deer", "dome", "dove", "drum", "dune",
    "elm", "ember", "fern", "field", "flame", "flint", "fog", "forge",
    "fork", "fox", "frost", "gate", "gem", "glen", "grove", "hawk",
    "hedge", "heron", "hill", "horn", "isle", "jade", "jay", "knot",
    "lake", "lamp", "lane", "lark", "leaf", "ledge", "marsh", "maze",
    "mist", "moon", "moss", "nest", "oak", "oar", "orbit", "otter",
    "owl", "palm", "path", "peak", "pine", "plume", "pond", "quill",
    "rain", "ranch", "reef", "ridge", "river", "robin", "rock", "rose",
    "sage", "seed", "shade", "shell", "shore", "slate", "slope", "spark",
    "spire", "star", "stone", "storm", "sun", "surf", "swan", "thorn",
    "tide", "tower", "trail", "tree", "vale", "vine", "wave", "well",
    "wind", "wing", "wolf", "wood", "wren",
];

// ---------------------------------------------------------------------------
// Single map — buffer exists = hasn't been renamed yet
// ---------------------------------------------------------------------------

static SESSION_BUFFERS: once_cell::sync::Lazy<Arc<Mutex<HashMap<i64, String>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

static SLUG_REGEX: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| regex::Regex::new(r"^[a-z][a-z0-9-]{2,50}$").unwrap());

static ANSI_ESCAPE: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07|\x1b[()][A-B012]").unwrap()
    });

const MAX_BUFFER_CHARS: usize = 4000;
const SUMMARIZE_BUFFER_CHARS: usize = 3000;

// ---------------------------------------------------------------------------
// Public lifecycle API
// ---------------------------------------------------------------------------

/// Generate an initial random name for a newly spawned agent node.
/// Returns a three-word hyphenated slug (e.g. "bold-keen-brook").
pub fn on_spawn() -> String {
    let mut rng = rand::rng();
    let adj1 = ADJECTIVES.choose(&mut rng).unwrap();
    let adj2 = ADJECTIVES.choose(&mut rng).unwrap();
    let noun = NOUNS.choose(&mut rng).unwrap();
    format!("{}-{}-{}", adj1, adj2, noun)
}

/// Buffer PTY output for a node. Accumulates in SESSION_BUFFERS until rename.
pub fn on_output(node_id: i64, data: &str) {
    let mut buffers = SESSION_BUFFERS.lock().unwrap();
    let buf = buffers.entry(node_id).or_default();
    buf.push_str(data);
    if buf.len() > MAX_BUFFER_CHARS {
        let mut drain_to = buf.len() - MAX_BUFFER_CHARS;
        while !buf.is_char_boundary(drain_to) {
            drain_to += 1;
        }
        buf.drain(..drain_to);
    }
}

/// Record a completed turn for a node. Triggers async LLM rename if buffer is sufficient.
pub fn on_turn(node_id: i64, app: AppHandle) {
    // Check DB: if node already has a non-default name, skip entirely.
    // This handles nodes renamed in a previous app run (cross-process detection).
    if let Ok(node) = crate::db::get_agent_node_by_id(node_id) {
        if !is_default_name(&node.name) {
            SESSION_BUFFERS.lock().unwrap().remove(&node_id);
            return;
        }
    }

    // Check buffer existence and sufficiency BEFORE removing (avoid unnecessary I/O).
    let buffer_len = {
        let buffers = SESSION_BUFFERS.lock().unwrap();
        buffers.get(&node_id).map(|b| b.len()).unwrap_or(0)
    };

    if buffer_len == 0 {
        return; // No buffer = already renamed or no output received yet
    }

    if buffer_len < SUMMARIZE_BUFFER_CHARS / 2 {
        return; // Not enough content for meaningful rename — keep buffering
    }

    // Capture and remove buffer synchronously — the async task owns the content.
    let buffer_snapshot = {
        let mut buffers = SESSION_BUFFERS.lock().unwrap();
        buffers.remove(&node_id)
    };

    let Some(buffer) = buffer_snapshot else {
        return;
    };

    tracing::info!("session_naming: triggering rename for node {} ({} chars)", node_id, buffer.len());

    tauri::async_runtime::spawn(async move {
        match summarize_and_rename(node_id, &buffer).await {
            Ok(slug) => {
                let _ = app.emit(
                    "session-renamed",
                    serde_json::json!({
                        "session_id": node_id,
                        "name": slug
                    }),
                );
                tracing::info!("Node {} renamed to '{}'", node_id, slug);
            }
            Err(e) => {
                tracing::warn!("Node {} rename failed: {}", node_id, e);
            }
        }
    });
}

/// Check if a name matches the random default pattern (adj-adj-noun).
pub fn is_default_name(name: &str) -> bool {
    let parts: Vec<&str> = name.split('-').collect();
    if parts.len() != 3 {
        return false;
    }
    ADJECTIVES.contains(&parts[0])
        && ADJECTIVES.contains(&parts[1])
        && NOUNS.contains(&parts[2])
}

/// Clear the buffer for a node. Called on kill so the node can resume fresh.
pub fn reset_buffers(node_id: i64) {
    SESSION_BUFFERS.lock().unwrap().remove(&node_id);
}

/// Clear the buffer for a node. Called on delete/archive.
pub fn cleanup(node_id: i64) {
    SESSION_BUFFERS.lock().unwrap().remove(&node_id);
}

// ---------------------------------------------------------------------------
// Diagnostic helpers
// ---------------------------------------------------------------------------

pub fn buffers_size_bytes() -> usize {
    let buffers = SESSION_BUFFERS.lock().unwrap();
    buffers.values().map(|b| b.len()).sum()
}

// ---------------------------------------------------------------------------
// Internal: LLM-based summarization
// ---------------------------------------------------------------------------

async fn summarize_and_rename(node_id: i64, buffer: &str) -> Result<String, String> {
    let clean_buffer = ANSI_ESCAPE.replace_all(buffer, "").to_string();

    let prompt = "You must output EXACTLY one line containing only 3-5 lowercase hyphenated words (e.g. fix-auth-token-refresh). No explanations, no punctuation, no quotes. Output ONLY the slug string.";

    tracing::info!("session_naming: running summarize command for node {} ({} chars)", node_id, clean_buffer.len());

    let mut cmd = {
        #[cfg(target_os = "windows")]
        {
            let mut c = tokio::process::Command::new("C:\\Windows\\System32\\cmd.exe");
            c.args(["/c", "cwrap", "--minimax", "-p", &prompt]);
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x08000000);
            c
        }
        #[cfg(not(target_os = "windows"))]
        {
            let mut c = tokio::process::Command::new("cwrap");
            c.args(["--minimax", "-p", &prompt]);
            c
        }
    };

    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn CLI: {}", e))?;

    let output = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin
                .write_all(clean_buffer.as_bytes())
                .await
                .map_err(|e| format!("failed to write buffer to CLI: {}", e))?;
        }
        child
            .wait_with_output()
            .await
            .map_err(|e| format!("failed to run CLI: {}", e))
    })
    .await
    .map_err(|_| "CLI timed out after 15s".to_string())??;

    if !output.status.success() {
        return Err(format!(
            "CLI exited with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let slug = slug_with_retry(&raw)?;
    crate::db::update_agent_node_name(node_id, &slug).map_err(|e| e.to_string())?;
    Ok(slug)
}

fn slug_with_retry(raw: &str) -> Result<String, String> {
    // Extract just the first hyphenated slug-like line
    let candidate = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .find(|l| l.contains('-'))
        .map(|l| l.trim().trim_matches('"').trim_matches('`').to_lowercase())
        .unwrap_or_else(|| raw.trim().trim_matches('"').trim_matches('`').to_lowercase());

    let word_count = candidate.split('-').count();
    if word_count >= 3 && word_count <= 5 && SLUG_REGEX.is_match(&candidate) {
        return Ok(candidate);
    }

    // Fallback: extract longest run of hyphenated words
    let fallback = candidate.split_whitespace()
        .filter(|w| w.contains('-'))
        .max_by_key(|w| w.split('-').count())
        .unwrap_or(&candidate)
        .to_string();

    let fallback_count = fallback.split('-').count();
    if fallback_count >= 3 && fallback_count <= 5 && SLUG_REGEX.is_match(&fallback) {
        return Ok(fallback);
    }

    Err(format!(
        "slug has {} words (expected 3-5): '{}'",
        fallback_count.max(word_count),
        candidate
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_three_word_hyphenated_name() {
        let name = on_spawn();
        let parts: Vec<&str> = name.split('-').collect();
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|p| !p.is_empty()));
        assert!(name.chars().all(|c| c.is_ascii_lowercase() || c == '-'));
    }

    #[test]
    fn is_default_name_positive() {
        assert!(is_default_name("bold-keen-brook"));
        assert!(is_default_name("calm-deep-oak"));
    }

    #[test]
    fn is_default_name_negative() {
        assert!(!is_default_name("fix-auth-token-refresh"));
        assert!(!is_default_name("too-short"));
        assert!(!is_default_name("not-a-valid-one-at-all"));
    }

    #[test]
    fn slug_with_retry_accepts_valid() {
        assert_eq!(
            slug_with_retry("fix-auth-token-refresh").unwrap(),
            "fix-auth-token-refresh"
        );
        assert_eq!(
            slug_with_retry("  rendering-terminal-bug\n").unwrap(),
            "rendering-terminal-bug"
        );
    }

    #[test]
    fn slug_with_retry_rejects_too_short() {
        assert!(slug_with_retry("fix-it").is_err());
    }

    #[test]
    fn slug_with_retry_rejects_too_long() {
        assert!(slug_with_retry("one-two-three-four-five-six").is_err());
    }

    #[test]
    fn buffer_caps_at_max() {
        let big = "x".repeat(5000);
        on_output(999, &big);
        let buffers = SESSION_BUFFERS.lock().unwrap();
        assert!(buffers.get(&999).unwrap().len() <= MAX_BUFFER_CHARS);
    }

    #[test]
    fn slug_with_retry_extracts_hyphenated_from_prose() {
        let result = slug_with_retry(
            "Based on the session, a good name would be: improve-auth-flow"
        );
        assert!(result.is_ok());
    }

    #[test]
    fn stale_buffer_cleared_on_cleanup() {
        let stale_text = "old context from a previous archived session";
        on_output(5, stale_text);
        {
            let buffers = SESSION_BUFFERS.lock().unwrap();
            assert!(buffers.get(&5).is_some(), "precondition: buffer exists");
        }

        cleanup(5);

        let buffers = SESSION_BUFFERS.lock().unwrap();
        assert!(
            buffers.get(&5).is_none(),
            "SESSION_BUFFERS[5] should be cleared by cleanup"
        );
    }

    #[test]
    fn cleanup_only_clears_target_session() {
        on_output(5, "session 5 output");
        on_output(99, "session 99 output");

        cleanup(5);

        {
            let buffers = SESSION_BUFFERS.lock().unwrap();
            assert!(buffers.get(&5).is_none());
            assert!(buffers.get(&99).is_some(), "session 99 buffer should survive");
        }
    }

    #[test]
    fn cleanup_is_idempotent() {
        on_output(8, "some output");

        cleanup(8);
        cleanup(8); // second call must not panic

        let buffers = SESSION_BUFFERS.lock().unwrap();
        assert!(buffers.get(&8).is_none());
    }

    #[test]
    fn buffer_truncation_splits_multibyte_utf8_correctly() {
        let node_id = 77;
        let base = "x".repeat(4000);
        let with_kanji = format!("{}{}", base, "日本");

        on_output(node_id, &with_kanji);

        let buffers = SESSION_BUFFERS.lock().unwrap();
        let buf = buffers.get(&node_id).unwrap();

        assert!(buf.len() <= MAX_BUFFER_CHARS);
        assert!(std::str::from_utf8(buf.as_bytes()).is_ok());
    }

    #[test]
    fn on_output_always_buffers_for_default_name_nodes() {
        // on_output should buffer unconditionally — no RENAMED_SESSIONS guard
        on_output(42, "first output");
        on_output(42, "second output");

        let buffers = SESSION_BUFFERS.lock().unwrap();
        assert_eq!(
            buffers.get(&42).unwrap().as_str(),
            "first outputsecond output",
            "on_output should accumulate without gate"
        );
    }

    #[test]
    fn reset_buffers_removes_only_target() {
        on_output(5, "session 5 output");
        on_output(99, "session 99 output");

        reset_buffers(5);

        let buffers = SESSION_BUFFERS.lock().unwrap();
        assert!(buffers.get(&5).is_none());
        assert!(buffers.get(&99).is_some());
    }
}
