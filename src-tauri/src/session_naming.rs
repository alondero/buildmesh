//! Unified session naming module.
//!
//! State maps:
//! - `SESSION_BUFFERS` — PTY output accumulator; entry exists = hasn't been renamed yet
//! - `RENAMING_IN_PROGRESS` — guards against duplicate concurrent LLM calls
//! - `RENAME_ATTEMPTS` — per-node failure counter; caps retries at MAX_RENAME_ATTEMPTS
//!
//! Lifecycle:
//! - `on_spawn()` generates an initial random name
//! - `on_output(node_id, data)` buffers PTY output
//! - `on_turn(node_id, app)` triggers LLM rename when buffer is sufficient
//! - `cleanup(node_id)` removes all state for a node

use rand::seq::IndexedRandom;
use std::collections::{HashMap, HashSet};
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

/// Set of node IDs that have a rename task currently in-flight.
/// Prevents duplicate concurrent LLM calls for the same node.
static RENAMING_IN_PROGRESS: once_cell::sync::Lazy<Arc<Mutex<HashSet<i64>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashSet::new())));

/// Per-node rename attempt counter. After MAX_RENAME_ATTEMPTS, stop retrying.
static RENAME_ATTEMPTS: once_cell::sync::Lazy<Arc<Mutex<HashMap<i64, u8>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

const MAX_RENAME_ATTEMPTS: u8 = 3;

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

    // Skip if max attempts exhausted for this node (check before buffer work)
    {
        let attempts = RENAME_ATTEMPTS.lock().unwrap();
        if attempts.get(&node_id).copied().unwrap_or(0) >= MAX_RENAME_ATTEMPTS {
            tracing::debug!("on_turn({}): max rename attempts reached, giving up", node_id);
            clear_node_state(node_id);
            return;
        }
    }

    // Check buffer existence and sufficiency BEFORE removing (avoid unnecessary I/O).
    let buffer_len = {
        let buffers = SESSION_BUFFERS.lock().unwrap();
        buffers.get(&node_id).map(|b| b.len()).unwrap_or(0)
    };

    if buffer_len == 0 {
        return;
    }

    if buffer_len < SUMMARIZE_BUFFER_CHARS / 2 {
        return;
    }

    let buffer_content = {
        let buffers = SESSION_BUFFERS.lock().unwrap();
        buffers.get(&node_id).cloned()
    };

    let Some(buffer) = buffer_content else {
        return;
    };

    // Skip if a rename is already in-flight for this node
    {
        let mut in_progress = RENAMING_IN_PROGRESS.lock().unwrap();
        if in_progress.contains(&node_id) {
            tracing::debug!("on_turn({}): rename already in progress, skipping", node_id);
            return;
        }
        in_progress.insert(node_id);
    }

    tracing::info!("session_naming: triggering rename for node {} ({} chars)", node_id, buffer.len());

    let node_id_for_task = node_id;
    let app_for_task = app.clone();
    tauri::async_runtime::spawn(async move {
        match summarize_and_rename(node_id_for_task, &buffer).await {
            Ok(slug) => {
                clear_node_state(node_id_for_task);
                let _ = app_for_task.emit(
                    "session-renamed",
                    serde_json::json!({
                        "session_id": node_id_for_task,
                        "name": slug
                    }),
                );
                tracing::info!("Node {} renamed to '{}'", node_id_for_task, slug);
            }
            Err(e) => {
                let attempt = {
                    let mut attempts = RENAME_ATTEMPTS.lock().unwrap();
                    let count = attempts.entry(node_id_for_task).or_insert(0);
                    *count += 1;
                    *count
                };
                if attempt >= MAX_RENAME_ATTEMPTS {
                    tracing::warn!(
                        "Node {} giving up on rename after {} attempts (last error: {})",
                        node_id_for_task, attempt, e
                    );
                    SESSION_BUFFERS.lock().unwrap().remove(&node_id_for_task);
                } else {
                    tracing::warn!(
                        "Node {} rename attempt {}/{} failed (buffer preserved for retry): {}",
                        node_id_for_task, attempt, MAX_RENAME_ATTEMPTS, e
                    );
                }
            }
        }
        RENAMING_IN_PROGRESS.lock().unwrap().remove(&node_id_for_task);
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
    clear_node_state(node_id);
}

/// Clear all state for a node. Called on delete/archive.
pub fn cleanup(node_id: i64) {
    clear_node_state(node_id);
}

fn clear_node_state(node_id: i64) {
    SESSION_BUFFERS.lock().unwrap().remove(&node_id);
    RENAME_ATTEMPTS.lock().unwrap().remove(&node_id);
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

    let output = tokio::time::timeout(std::time::Duration::from_secs(30), async {
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
    .map_err(|_| "CLI timed out after 30s".to_string())??;

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

fn is_conversational_response(s: &str) -> bool {
    if s.contains('?') {
        return true;
    }
    let lower = s.to_lowercase();
    let conversational_prefixes = [
        "it looks like",
        "i'm not sure",
        "what can i",
        "how can i",
        "that looks like",
        "i don't",
        "this looks like",
        "it seems like",
        "i can help",
        "let me help",
    ];
    conversational_prefixes.iter().any(|p| lower.starts_with(p))
}

fn slug_with_retry(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_matches('"').trim_matches('`');
    if trimmed.split_whitespace().nth(5).is_some() && is_conversational_response(trimmed) {
        let preview = trimmed.char_indices().nth(80).map_or(trimmed, |(i, _)| &trimmed[..i]);
        return Err(format!("naming LLM returned conversational response: '{}'", preview));
    }

    // Extract just the first hyphenated slug-like line
    let candidate = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .find(|l| l.contains('-'))
        .map(|l| l.trim().trim_matches('"').trim_matches('`').to_lowercase())
        .unwrap_or_else(|| raw.trim().trim_matches('"').trim_matches('`').to_lowercase());

    // Normalise space-separated words to hyphens when no hyphens present
    let candidate = if !candidate.contains('-') {
        candidate.split_whitespace().collect::<Vec<_>>().join("-")
    } else {
        candidate
    };

    let token_count = candidate.split('-').count();
    if token_count >= 3 && token_count <= 5 && SLUG_REGEX.is_match(&candidate) {
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
        "slug has {} dash-separated tokens (expected 3-5): '{}'",
        fallback_count.max(token_count),
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
        let err = slug_with_retry("fix-it").unwrap_err();
        assert!(err.contains("dash-separated tokens"));
    }

    #[test]
    fn slug_with_retry_rejects_too_long() {
        let err = slug_with_retry("one-two-three-four-five-six").unwrap_err();
        assert!(err.contains("dash-separated tokens"));
    }

    #[test]
    fn slug_with_retry_normalises_space_separated_words() {
        assert_eq!(
            slug_with_retry("not skip audit kirby tests").unwrap(),
            "not-skip-audit-kirby-tests"
        );
        assert_eq!(
            slug_with_retry("fix auth token flow").unwrap(),
            "fix-auth-token-flow"
        );
    }

    #[test]
    fn slug_with_retry_detects_conversational_response() {
        let cases = [
            "it looks like there might be a terminal issue with repeated commands. how can i help you?",
            "i'm not sure what you're trying to do with that terminal output",
            "what can i help you with in the buildmesh project?",
            "that looks like terminal output from a different project. what task can i assist you with?",
        ];
        for case in &cases {
            let err = slug_with_retry(case).unwrap_err();
            assert!(
                err.contains("conversational response"),
                "expected conversational detection for: '{}', got: {}",
                case,
                err
            );
        }
    }

    #[test]
    fn slug_with_retry_does_not_flag_short_non_slug_as_conversational() {
        // Short responses without conversational markers should get the token count error
        let err = slug_with_retry("pond").unwrap_err();
        assert!(err.contains("dash-separated tokens"));
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
