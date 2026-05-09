//! Unified session naming module.
//!
//! Lifecycle-oriented interface for agent node naming:
//! - `on_spawn()` — generates an initial random name
//! - `on_output(node_id, data)` — buffers output for future summarization
//! - `on_turn(node_id, app)` — increments turn counter, may trigger LLM rename
//! - `is_default_name(name)` — checks if a name is still the random default
//! - `cleanup(node_id)` — clears all state for a deleted/archived node
//!
//! Also exposes diagnostic helpers for crash snapshots.

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
// Global mutable state (required for PTY reader thread access)
// ---------------------------------------------------------------------------

static SESSION_BUFFERS: once_cell::sync::Lazy<Arc<Mutex<HashMap<i64, String>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

static TURN_COUNTERS: once_cell::sync::Lazy<Arc<Mutex<HashMap<i64, u32>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

static RENAMED_SESSIONS: once_cell::sync::Lazy<Arc<Mutex<HashSet<i64>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashSet::new())));

static SLUG_REGEX: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| regex::Regex::new(r"^[a-z][a-z0-9-]{2,50}$").unwrap());

const MAX_BUFFER_CHARS: usize = 4000;
const SUMMARIZE_BUFFER_CHARS: usize = 3000;
const RENAME_AT_TURN: u32 = 1;

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

/// Buffer PTY output for a node. Used to build context for LLM summarization.
/// No-op if the node has already been renamed.
pub fn on_output(node_id: i64, data: &str) {
    if RENAMED_SESSIONS.lock().unwrap().contains(&node_id) {
        return;
    }
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

/// Record a completed turn for a node. Increments the turn counter and, when
/// the threshold is reached, triggers an async LLM-based rename.
pub fn on_turn(node_id: i64, app: AppHandle) {
    if RENAMED_SESSIONS.lock().unwrap().contains(&node_id) {
        return;
    }

    // Check DB: if node already has a non-default name, it was renamed in a previous run
    if let Ok(node) = crate::db::get_agent_node_by_id(node_id) {
        if !is_default_name(&node.name) {
            RENAMED_SESSIONS.lock().unwrap().insert(node_id);
            return;
        }
    }

    let should_rename = {
        let mut counters = TURN_COUNTERS.lock().unwrap();
        let count = counters.entry(node_id).or_insert(0);
        *count += 1;
        tracing::info!("session_naming: on_turn node={} turn={}", node_id, *count);
        *count == RENAME_AT_TURN
    };

    if !should_rename {
        return;
    }

    tracing::info!("session_naming: triggering rename for node {}", node_id);

    RENAMED_SESSIONS.lock().unwrap().insert(node_id);

    let buffer_snapshot = {
        let mut buffers = SESSION_BUFFERS.lock().unwrap();
        buffers
            .remove(&node_id)
            .map(|b| {
                let target = b.len().min(SUMMARIZE_BUFFER_CHARS);
                let mut end = target;
                while end > 0 && !b.is_char_boundary(end) {
                    end -= 1;
                }
                b[..end].to_string()
            })
            .unwrap_or_default()
    };

    if buffer_snapshot.is_empty() {
        return;
    }

    tauri::async_runtime::spawn(async move {
        match summarize_and_rename(node_id, &buffer_snapshot).await {
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

/// Clear all naming state for a node (buffers, counters, renamed guard).
/// Call this when a node is deleted or archived.
pub fn cleanup(node_id: i64) {
    SESSION_BUFFERS.lock().unwrap().remove(&node_id);
    TURN_COUNTERS.lock().unwrap().remove(&node_id);
    RENAMED_SESSIONS.lock().unwrap().remove(&node_id);
}

// ---------------------------------------------------------------------------
// Diagnostic helpers (used by debug_crash_snapshot)
// ---------------------------------------------------------------------------

pub fn renamed_sessions_count() -> usize {
    RENAMED_SESSIONS.lock().unwrap().len()
}

pub fn buffers_size_bytes() -> usize {
    let buffers = SESSION_BUFFERS.lock().unwrap();
    buffers.values().map(|b| b.len()).sum()
}

pub fn turn_counter_count() -> usize {
    TURN_COUNTERS.lock().unwrap().len()
}

// ---------------------------------------------------------------------------
// Internal: LLM-based summarization
// ---------------------------------------------------------------------------

async fn summarize_and_rename(node_id: i64, _buffer: &str) -> Result<String, String> {
    let prompt = "You must output EXACTLY one line containing only 3-5 lowercase hyphenated words (e.g. fix-auth-token-refresh). No explanations, no punctuation, no quotes. Output ONLY the slug string.".to_string();

    tracing::info!("session_naming: running summarize command for node {}", node_id);

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

    let output = cmd
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| format!("failed to run CLI: {}", e))?;

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
        // A name composed of valid adj-adj-noun should return true
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
    fn stale_buffers_cleared_on_cleanup() {
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
    fn turn_counter_reset_on_cleanup() {
        {
            let mut counters = TURN_COUNTERS.lock().unwrap();
            counters.insert(5, 1);
        }
        {
            let mut buffers = SESSION_BUFFERS.lock().unwrap();
            buffers.insert(5, "fresh context".to_string());
        }

        cleanup(5);

        let counters = TURN_COUNTERS.lock().unwrap();
        assert!(
            counters.get(&5).is_none(),
            "TURN_COUNTERS[5] should be removed by cleanup"
        );
    }

    #[test]
    fn renamed_sessions_cleared_on_cleanup() {
        {
            let mut renamed = RENAMED_SESSIONS.lock().unwrap();
            renamed.insert(5);
        }
        {
            let mut buffers = SESSION_BUFFERS.lock().unwrap();
            buffers.insert(5, "fresh context for new node".to_string());
        }

        cleanup(5);

        let renamed = RENAMED_SESSIONS.lock().unwrap();
        assert!(
            !renamed.contains(&5),
            "RENAMED_SESSIONS[5] should be cleared by cleanup"
        );
    }

    #[test]
    fn cleanup_resets_all_state() {
        on_output(7, "some output text for session 7");
        {
            let mut counters = TURN_COUNTERS.lock().unwrap();
            counters.insert(7, 2);
        }
        {
            let mut renamed = RENAMED_SESSIONS.lock().unwrap();
            renamed.insert(7);
        }
        // verify preconditions
        {
            let buffers = SESSION_BUFFERS.lock().unwrap();
            assert!(buffers.get(&7).is_some());
        }
        {
            let counters = TURN_COUNTERS.lock().unwrap();
            assert_eq!(counters.get(&7), Some(&2));
        }
        {
            let renamed = RENAMED_SESSIONS.lock().unwrap();
            assert!(renamed.contains(&7));
        }

        cleanup(7);

        {
            let buffers = SESSION_BUFFERS.lock().unwrap();
            assert!(
                buffers.get(&7).is_none(),
                "SESSION_BUFFERS[7] should be removed by cleanup"
            );
        }
        {
            let counters = TURN_COUNTERS.lock().unwrap();
            assert!(
                counters.get(&7).is_none(),
                "TURN_COUNTERS[7] should be removed by cleanup"
            );
        }
        {
            let renamed = RENAMED_SESSIONS.lock().unwrap();
            assert!(
                !renamed.contains(&7),
                "RENAMED_SESSIONS[7] should be removed by cleanup"
            );
        }
    }

    #[test]
    fn cleanup_only_clears_target_session() {
        on_output(5, "session 5 output");
        on_output(99, "session 99 output");
        {
            let mut counters = TURN_COUNTERS.lock().unwrap();
            counters.insert(5, 1);
            counters.insert(99, 2);
        }
        {
            let mut renamed = RENAMED_SESSIONS.lock().unwrap();
            renamed.insert(5);
            renamed.insert(99);
        }

        cleanup(5);

        {
            let buffers = SESSION_BUFFERS.lock().unwrap();
            assert!(buffers.get(&5).is_none());
            assert!(buffers.get(&99).is_some(), "session 99 buffer should survive");
        }
        {
            let counters = TURN_COUNTERS.lock().unwrap();
            assert!(counters.get(&5).is_none());
            assert_eq!(counters.get(&99), Some(&2), "session 99 counter should survive");
        }
        {
            let renamed = RENAMED_SESSIONS.lock().unwrap();
            assert!(!renamed.contains(&5));
            assert!(renamed.contains(&99), "session 99 renamed guard should survive");
        }
    }

    #[test]
    fn cleanup_is_idempotent() {
        on_output(8, "some output");
        {
            let mut counters = TURN_COUNTERS.lock().unwrap();
            counters.insert(8, 1);
        }
        {
            let mut renamed = RENAMED_SESSIONS.lock().unwrap();
            renamed.insert(8);
        }

        cleanup(8);
        cleanup(8); // second call must not panic

        let buffers = SESSION_BUFFERS.lock().unwrap();
        let counters = TURN_COUNTERS.lock().unwrap();
        let renamed = RENAMED_SESSIONS.lock().unwrap();
        assert!(buffers.get(&8).is_none());
        assert!(counters.get(&8).is_none());
        assert!(!renamed.contains(&8));
    }

    #[test]
    fn on_output_drops_data_after_renamed_gate() {
        {
            let mut renamed = RENAMED_SESSIONS.lock().unwrap();
            renamed.insert(42);
        }

        on_output(42, "this should be dropped");
        on_output(42, "and this too");

        let buffers = SESSION_BUFFERS.lock().unwrap();
        assert!(
            buffers.get(&42).is_none(),
            "on_output must drop data for sessions in RENAMED_SESSIONS"
        );
    }

    #[test]
    fn turn_counter_accumulates_from_zero_to_rename() {
        let node_id = 55;

        {
            let mut counters = TURN_COUNTERS.lock().unwrap();
            counters.insert(node_id, 1);
        }
        on_output(node_id, "turn 2 output");

        {
            let mut counters = TURN_COUNTERS.lock().unwrap();
            let count = counters.entry(node_id).or_insert(0);
            *count += 1;
            assert_eq!(
                *count, 2,
                "second turn (count=2) should trigger rename"
            );
        }
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
}
