//! Unified session naming module.
//!
//! State maps:
//! - `BUFFERING_READY` — node IDs whose PTY output should be accumulated. We
//!   only flip a node into this set after the *first* idle/turn signal arrives,
//!   which discards the chrome that Claude Code prints on startup (the
//!   "Bypass Permissions" warning, plugin/skill listings, banner repaints) so
//!   the renaming LLM never sees that text. Without this gate the LLM kept
//!   producing names like `bypass-permissions-worktree` because the warning
//!   dominated the buffer on short first turns.
//! - `SESSION_BUFFERS` — PTY output accumulator; entry exists = hasn't been renamed yet
//! - `RENAMING_IN_PROGRESS` — guards against duplicate concurrent LLM calls
//! - `RENAME_ATTEMPTS` — per-node failure counter; caps retries at MAX_RENAME_ATTEMPTS
//!
//! Lifecycle:
//! - `on_spawn()` generates an initial random name
//! - `on_output(node_id, data)` buffers PTY output once the node is in `BUFFERING_READY`
//! - `on_turn(node_id, app)` flips the buffering gate (first call) and triggers
//!   LLM rename when the buffer is sufficient (subsequent calls)
//! - `cleanup(node_id)` removes all state for a node
//!
//! Diagnostic: set `BUILDMESH_DUMP_NAME_BUFFER=1` to dump the raw + cleaned
//! buffer to `%TEMP%/bm-name-buffer-<node_id>-<ts>.txt` whenever a rename runs.

use rand::seq::IndexedRandom;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

use crate::models::AgentNode;

// ---------------------------------------------------------------------------
// Repository trait — abstracts DB calls for testability
// ---------------------------------------------------------------------------

pub(crate) trait SessionNamingRepository: Send + Sync {
    fn get_agent_node_by_id(&self, id: i64) -> Result<AgentNode, String>;
    fn update_agent_node_name(&self, id: i64, name: &str) -> Result<(), String>;
}

pub(crate) struct DbSessionNamingRepository;

impl SessionNamingRepository for DbSessionNamingRepository {
    fn get_agent_node_by_id(&self, id: i64) -> Result<AgentNode, String> {
        crate::db::get_agent_node_by_id(id).map_err(|e| e.to_string())
    }
    fn update_agent_node_name(&self, id: i64, name: &str) -> Result<(), String> {
        crate::db::update_agent_node_name(id, name).map_err(|e| e.to_string())
    }
}

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

/// Nodes whose PTY output we are currently collecting for the renamer.
/// A node is added on its first `on_turn` and removed on cleanup/reset/successful
/// rename — anything emitted before the first turn signal is discarded so the
/// startup chrome (banner, permission warnings, plugin listing) never reaches
/// the LLM.
static BUFFERING_READY: once_cell::sync::Lazy<Arc<Mutex<HashSet<i64>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashSet::new())));

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

const CLAUDE_CODE_BANNER_CHARS: &[char] = &[
    '\u{2588}', // █
    '\u{258C}', // ▌
    '\u{2590}', // ▐
    '\u{2598}', // ▘
    '\u{259B}', // ▛
    '\u{259C}', // ▜
    '\u{259D}', // ▝
];

/// Drop Claude Code splash lines before the LLM sees the buffer — the
/// third banner line contains the cwd, which otherwise leaks into slugs
/// (e.g. `open-lucky-box-worktree`).
fn strip_claude_code_banner(s: &str) -> String {
    s.lines()
        .filter(|line| !line.chars().any(|c| CLAUDE_CODE_BANNER_CHARS.contains(&c)))
        .collect::<Vec<_>>()
        .join("\n")
}

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

/// Buffer PTY output for a node. Accumulates in SESSION_BUFFERS once the node
/// is in `BUFFERING_READY` (which only happens after the first `on_turn`,
/// so startup chrome is dropped).
pub fn on_output(node_id: i64, data: &str) {
    if !BUFFERING_READY.lock().unwrap().contains(&node_id) {
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

/// Testable core: checks whether rename should trigger, returns the buffer if so.
///
/// Side effect: on the very first call for a default-named node, this flips the
/// node into `BUFFERING_READY` so subsequent PTY output begins accumulating.
/// On that first call the buffer is by definition empty, so this returns None
/// and the actual rename fires on a later turn against clean post-startup content.
fn should_trigger_rename(
    repo: &dyn SessionNamingRepository,
    node_id: i64,
) -> Option<String> {
    if let Ok(node) = repo.get_agent_node_by_id(node_id) {
        if !is_default_name(&node.name) {
            // Node has a real name already; tear down everything so its PTY
            // output stops being buffered for no reason.
            clear_node_state(node_id);
            return None;
        }
    }

    // IMPORTANT: scope the lock guard to its own block and drop it BEFORE
    // calling `clear_node_state` — std::sync::Mutex is not reentrant, and
    // `clear_node_state` re-locks `RENAME_ATTEMPTS`. Holding the guard across
    // that call self-deadlocks and freezes every other thread that later
    // touches the same mutex.
    let max_attempts_reached = {
        let attempts = RENAME_ATTEMPTS.lock().unwrap();
        attempts.get(&node_id).copied().unwrap_or(0) >= MAX_RENAME_ATTEMPTS
    };
    if max_attempts_reached {
        tracing::debug!("on_turn({}): max rename attempts reached, giving up", node_id);
        // Drop buffer + gate but DELIBERATELY keep the attempt counter so
        // future on_turn calls keep tripping this branch (sticky lockout).
        // Calling clear_node_state here would wipe the counter and let the
        // rename cycle restart.
        SESSION_BUFFERS.lock().unwrap().remove(&node_id);
        BUFFERING_READY.lock().unwrap().remove(&node_id);
        return None;
    }

    // First idle/turn signal for this node — open the buffering gate so future
    // PTY output is captured, then return without renaming. This drops all the
    // startup chrome (banner, permission warnings, plugin/skill listings).
    let was_newly_opened = BUFFERING_READY.lock().unwrap().insert(node_id);
    if was_newly_opened {
        tracing::info!(
            "session_naming({}): opened buffering gate (post-startup); rename will run from next turn",
            node_id
        );
        return None;
    }

    let buffer = {
        let buffers = SESSION_BUFFERS.lock().unwrap();
        let b = buffers.get(&node_id)?;
        if b.len() < SUMMARIZE_BUFFER_CHARS / 2 {
            return None;
        }
        b.clone()
    };

    {
        let mut in_progress = RENAMING_IN_PROGRESS.lock().unwrap();
        if in_progress.contains(&node_id) {
            tracing::debug!("on_turn({}): rename already in progress, skipping", node_id);
            return None;
        }
        in_progress.insert(node_id);
    }

    Some(buffer)
}

/// Record a completed turn for a node. Triggers async LLM rename if buffer is sufficient.
pub fn on_turn(node_id: i64, app: AppHandle) {
    on_turn_with(&DbSessionNamingRepository, node_id, app);
}

fn on_turn_with(repo: &dyn SessionNamingRepository, node_id: i64, app: AppHandle) {
    let Some(buffer) = should_trigger_rename(repo, node_id) else {
        return;
    };

    tracing::info!("session_naming: triggering rename for node {} ({} chars)", node_id, buffer.len());

    let app_for_task = app.clone();
    tauri::async_runtime::spawn(async move {
        match summarize_and_rename_with(node_id, &buffer).await {
            Ok(slug) => {
                // User-rename race guard: the LLM call above can take 5-30s.
                // During that window the user may have invoked `rename_session`
                // and overwritten the node's name in the DB. Re-read and skip
                // both the write and the event emit if the user has taken
                // ownership — the user always wins.
                if user_renamed_mid_flight(&DbSessionNamingRepository, node_id) {
                    tracing::info!(
                        "Node {} was manually renamed during LLM call; skipping slug '{}'",
                        node_id,
                        slug
                    );
                    clear_node_state(node_id);
                    RENAMING_IN_PROGRESS.lock().unwrap().remove(&node_id);
                    return;
                }
                if let Err(e) =
                    DbSessionNamingRepository.update_agent_node_name(node_id, &slug)
                {
                    tracing::warn!("Node {} rename write failed: {}", node_id, e);
                }
                clear_node_state(node_id);
                let _ = app_for_task.emit(
                    "session-renamed",
                    serde_json::json!({
                        "session_id": node_id,
                        "name": slug
                    }),
                );
                tracing::info!("Node {} renamed to '{}'", node_id, slug);
            }
            Err(e) => {
                let attempt = {
                    let mut attempts = RENAME_ATTEMPTS.lock().unwrap();
                    let count = attempts.entry(node_id).or_insert(0);
                    *count += 1;
                    *count
                };
                if attempt >= MAX_RENAME_ATTEMPTS {
                    tracing::warn!(
                        "Node {} giving up on rename after {} attempts (last error: {})",
                        node_id, attempt, e
                    );
                    // Tear down buffer + gate so we stop collecting; the attempt
                    // counter is intentionally left set so the next on_turn
                    // sees max-attempts-reached and short-circuits (sticky lockout).
                    SESSION_BUFFERS.lock().unwrap().remove(&node_id);
                    BUFFERING_READY.lock().unwrap().remove(&node_id);
                } else {
                    tracing::warn!(
                        "Node {} rename attempt {}/{} failed (buffer preserved for retry): {}",
                        node_id, attempt, MAX_RENAME_ATTEMPTS, e
                    );
                }
            }
        }
        RENAMING_IN_PROGRESS.lock().unwrap().remove(&node_id);
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

/// Race-guard helper: returns true if the node's name in the DB is not a
/// default slug (i.e. it was either LLM-renamed earlier or manually renamed
/// by the user). Called from the LLM commit path to make sure a user
/// rename that landed during the LLM call wins over the auto-rename. On a
/// DB read error we err on the side of "user has not renamed" so the LLM
/// rename is allowed to proceed (failing closed here would be a silent
/// regression of the auto-rename for nodes whose DB read transiently fails).
pub(crate) fn user_renamed_mid_flight(
    repo: &dyn SessionNamingRepository,
    node_id: i64,
) -> bool {
    repo.get_agent_node_by_id(node_id)
        .map(|n| !is_default_name(&n.name))
        .unwrap_or(false)
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
    BUFFERING_READY.lock().unwrap().remove(&node_id);
}

// ---------------------------------------------------------------------------
// Diagnostic helpers
// ---------------------------------------------------------------------------

pub fn buffers_size_bytes() -> usize {
    let buffers = SESSION_BUFFERS.lock().unwrap();
    buffers.values().map(|b| b.len()).sum()
}

// ---------------------------------------------------------------------------
// Diagnostic dump (opt-in via env var)
// ---------------------------------------------------------------------------

/// Write the raw + cleaned rename buffer to a temp file when
/// `BUILDMESH_DUMP_NAME_BUFFER` is set. Used to capture real fixtures when
/// the renamer produces bad slugs. Failures are silently logged — diagnosis
/// shouldn't break the rename pipeline.
fn maybe_dump_rename_buffer(node_id: i64, raw: &str, cleaned: &str) {
    if std::env::var_os("BUILDMESH_DUMP_NAME_BUFFER").is_none() {
        return;
    }
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3f");
    let path = std::env::temp_dir().join(format!("bm-name-buffer-{}-{}.txt", node_id, ts));
    let body = format!(
        "=== RAW ({} chars) ===\n{}\n\n=== CLEANED ({} chars, sent to LLM) ===\n{}\n",
        raw.len(),
        raw,
        cleaned.len(),
        cleaned
    );
    match std::fs::write(&path, body) {
        Ok(_) => tracing::info!("session_naming: dumped rename buffer to {:?}", path),
        Err(e) => tracing::warn!("session_naming: failed to dump rename buffer: {}", e),
    }
}

// ---------------------------------------------------------------------------
// Internal: LLM-based summarization
// ---------------------------------------------------------------------------

async fn summarize_and_rename_with(
    node_id: i64,
    buffer: &str,
) -> Result<String, String> {
    let ansi_stripped = ANSI_ESCAPE.replace_all(buffer, "").to_string();
    let clean_buffer = strip_claude_code_banner(&ansi_stripped);

    maybe_dump_rename_buffer(node_id, buffer, &clean_buffer);

    let prompt = "The text on stdin is a terminal log from an AI coding-assistant session. \
                  Generate a short slug that describes the task the user is working on in this session. \
                  Output EXACTLY one line: 3 to 5 lowercase words joined by hyphens, nothing else \
                  (no explanation, no punctuation, no quotes, no example labels).";

    tracing::info!("session_naming: running summarize command for node {} ({} chars)", node_id, clean_buffer.len());

    let mut cmd = {
#[cfg(target_os = "windows")]
            {
                let mut c = tokio::process::Command::new("C:\\Windows\\System32\\cmd.exe");
                c.args(["/c", "cwrap", "--minimax", "-p", prompt]);
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
    if (3..=5).contains(&token_count) && SLUG_REGEX.is_match(&candidate) {
        return Ok(candidate);
    }

    // Fallback: extract longest run of hyphenated words
    let fallback = candidate.split_whitespace()
        .filter(|w| w.contains('-'))
        .max_by_key(|w| w.split('-').count())
        .unwrap_or(&candidate)
        .to_string();

    let fallback_count = fallback.split('-').count();
    if (3..=5).contains(&fallback_count) && SLUG_REGEX.is_match(&fallback) {
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

    /// Open the buffering gate for a node so `on_output` writes immediately.
    /// Real code opens the gate via `should_trigger_rename`; tests use this
    /// to simulate "the agent has already had at least one turn".
    fn open_gate(node_id: i64) {
        BUFFERING_READY.lock().unwrap().insert(node_id);
    }

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
        let err = slug_with_retry("pond").unwrap_err();
        assert!(err.contains("dash-separated tokens"));
    }

    #[test]
    fn buffer_caps_at_max() {
        open_gate(999);
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
        open_gate(5);
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
        open_gate(5);
        open_gate(99);
        on_output(5, "session 5 output");
        on_output(99, "session 99 output");

        cleanup(5);

        {
            let buffers = SESSION_BUFFERS.lock().unwrap();
            assert!(buffers.get(&5).is_none());
            assert!(buffers.get(&99).is_some(), "session 99 buffer should survive");
        }
        cleanup(99);
    }

    #[test]
    fn cleanup_is_idempotent() {
        open_gate(8);
        on_output(8, "some output");

        cleanup(8);
        cleanup(8); // second call must not panic

        let buffers = SESSION_BUFFERS.lock().unwrap();
        assert!(buffers.get(&8).is_none());
    }

    #[test]
    fn buffer_truncation_splits_multibyte_utf8_correctly() {
        let node_id = 77;
        open_gate(node_id);
        let base = "x".repeat(4000);
        let with_kanji = format!("{}{}", base, "日本");

        on_output(node_id, &with_kanji);

        let buffers = SESSION_BUFFERS.lock().unwrap();
        let buf = buffers.get(&node_id).unwrap();

        assert!(buf.len() <= MAX_BUFFER_CHARS);
        assert!(std::str::from_utf8(buf.as_bytes()).is_ok());
    }

    #[test]
    fn on_output_drops_data_until_gate_opens() {
        // Distinct id to avoid cross-test contamination with shared statics
        let node_id = 4242;
        cleanup(node_id);

        // Before any on_turn signal, all output is discarded — this is the
        // bypass-permissions-warning fix: chrome printed at startup never
        // reaches the renaming buffer.
        on_output(node_id, "chrome chrome chrome");
        on_output(node_id, "Bypass Permissions warning text...");

        {
            let buffers = SESSION_BUFFERS.lock().unwrap();
            assert!(
                buffers.get(&node_id).is_none(),
                "buffer must stay empty before gate opens"
            );
        }

        // Now the gate flips (simulating first idle_prompt webhook)
        open_gate(node_id);

        on_output(node_id, "real user content");
        let buffers = SESSION_BUFFERS.lock().unwrap();
        assert_eq!(
            buffers.get(&node_id).unwrap().as_str(),
            "real user content",
            "post-gate output must accumulate, no chrome contamination"
        );
        drop(buffers);
        cleanup(node_id);
    }

    #[test]
    fn on_output_accumulates_after_gate_opens() {
        open_gate(42);
        on_output(42, "first output");
        on_output(42, "second output");

        let buffers = SESSION_BUFFERS.lock().unwrap();
        assert_eq!(
            buffers.get(&42).unwrap().as_str(),
            "first outputsecond output",
            "on_output should accumulate once the gate is open"
        );
    }

    #[test]
    fn reset_buffers_removes_only_target() {
        open_gate(5);
        open_gate(99);
        on_output(5, "session 5 output");
        on_output(99, "session 99 output");

        reset_buffers(5);

        let buffers = SESSION_BUFFERS.lock().unwrap();
        assert!(buffers.get(&5).is_none());
        assert!(buffers.get(&99).is_some());
        drop(buffers);
        cleanup(99);
    }

    #[test]
    fn reset_buffers_closes_gate_so_next_session_starts_fresh() {
        // After a kill/resume cycle, the gate must close so the next agent's
        // startup chrome is dropped just like the first time.
        let node_id = 5151;
        open_gate(node_id);
        on_output(node_id, "turn-1 content");
        reset_buffers(node_id);

        // After reset, gate is closed — output is dropped until a new turn.
        on_output(node_id, "resumed-agent startup chrome");
        let buffers = SESSION_BUFFERS.lock().unwrap();
        assert!(
            buffers.get(&node_id).is_none(),
            "reset_buffers must also close the gate so resume-startup chrome is dropped"
        );
    }

    // --- SessionNamingRepository mock tests ---

    struct MockRepo {
        node_name: String,
        should_fail: bool,
        updates: std::sync::Mutex<Vec<(i64, String)>>,
    }

    impl MockRepo {
        fn with_name(name: &str) -> Self {
            Self {
                node_name: name.to_string(),
                should_fail: false,
                updates: std::sync::Mutex::new(vec![]),
            }
        }
    }

    impl SessionNamingRepository for MockRepo {
        fn get_agent_node_by_id(&self, id: i64) -> Result<AgentNode, String> {
            if self.should_fail {
                return Err("mock db error".into());
            }
            Ok(AgentNode {
                id,
                mesh_id: 1,
                name: self.node_name.clone(),
                path: "/tmp/test".to_string(),
                branch: "main".to_string(),
                env: crate::models::EnvType::Windows,
                provider: crate::models::Provider::Anthropic,
                status: crate::models::SessionStatus::Running,
                cli_session_id: None,
                worktree_name: None,
                use_worktree: false,
                source_issue: None,
                created_at: chrono::Utc::now(),
            })
        }
        fn update_agent_node_name(&self, id: i64, name: &str) -> Result<(), String> {
            self.updates.lock().unwrap().push((id, name.to_string()));
            Ok(())
        }
    }

    #[test]
    fn should_trigger_rename_skips_already_renamed_node() {
        let node_id = 70001;
        open_gate(node_id);
        on_output(node_id, &"x".repeat(2000));

        let repo = MockRepo::with_name("fix-auth-token-refresh");
        let result = should_trigger_rename(&repo, node_id);
        assert!(result.is_none(), "should skip node with custom name");

        let buffers = SESSION_BUFFERS.lock().unwrap();
        assert!(buffers.get(&node_id).is_none(), "buffer should be cleared");
        assert!(
            !BUFFERING_READY.lock().unwrap().contains(&node_id),
            "gate should be closed for renamed node"
        );
    }

    #[test]
    fn should_trigger_rename_opens_gate_on_first_call_for_default_named_node() {
        // First call (no gate yet) is the "discard startup chrome" turn.
        // It opens the gate and returns None even if the buffer would have
        // been large enough, because the buffer at this point would be chrome.
        let node_id = 70002;
        cleanup(node_id);

        let repo = MockRepo::with_name("bold-keen-brook");
        let result = should_trigger_rename(&repo, node_id);
        assert!(result.is_none(), "first call must defer rename to open the gate");
        assert!(
            BUFFERING_READY.lock().unwrap().contains(&node_id),
            "gate must be open after first call"
        );
        cleanup(node_id);
    }

    #[test]
    fn should_trigger_rename_returns_buffer_on_second_call() {
        let node_id = 70003;
        cleanup(node_id);

        let repo = MockRepo::with_name("bold-keen-brook");
        // First call opens the gate, returns None
        let first = should_trigger_rename(&repo, node_id);
        assert!(first.is_none());

        // Now real PTY output accumulates (gate is open)
        on_output(node_id, &"x".repeat(2000));

        // Second call sees the buffer and returns it for renaming
        let second = should_trigger_rename(&repo, node_id);
        assert!(second.is_some(), "second call should trigger rename");
        assert_eq!(second.unwrap().len(), 2000);

        RENAMING_IN_PROGRESS.lock().unwrap().remove(&node_id);
        cleanup(node_id);
    }

    #[test]
    fn should_trigger_rename_skips_insufficient_buffer() {
        let node_id = 70004;
        cleanup(node_id);
        open_gate(node_id); // pretend gate already opened by an earlier turn
        on_output(node_id, "short");

        let repo = MockRepo::with_name("bold-keen-brook");
        let result = should_trigger_rename(&repo, node_id);
        assert!(result.is_none(), "should skip when buffer too small");
        cleanup(node_id);
    }

    #[test]
    fn should_trigger_rename_skips_when_already_in_progress() {
        let node_id = 70005;
        cleanup(node_id);
        open_gate(node_id);
        on_output(node_id, &"x".repeat(2000));
        RENAMING_IN_PROGRESS.lock().unwrap().insert(node_id);

        let repo = MockRepo::with_name("bold-keen-brook");
        let result = should_trigger_rename(&repo, node_id);
        assert!(result.is_none(), "should skip when rename in progress");

        RENAMING_IN_PROGRESS.lock().unwrap().remove(&node_id);
        cleanup(node_id);
    }

    #[test]
    fn should_trigger_rename_skips_when_max_attempts_reached() {
        let node_id = 70006;
        cleanup(node_id);
        open_gate(node_id);
        on_output(node_id, &"x".repeat(2000));
        RENAME_ATTEMPTS.lock().unwrap().insert(node_id, MAX_RENAME_ATTEMPTS);

        let repo = MockRepo::with_name("bold-keen-brook");
        let result = should_trigger_rename(&repo, node_id);
        assert!(result.is_none(), "should skip when max attempts reached");

        RENAME_ATTEMPTS.lock().unwrap().remove(&node_id);
        cleanup(node_id);
    }

    #[test]
    fn end_to_end_bypass_permissions_chrome_excluded_from_rename_buffer() {
        // Regression for the bypass-permissions slug bug.
        //
        // Simulated lifecycle of a freshly-spawned agent:
        //   1. Claude Code prints its startup chrome (banner + Bypass
        //      Permissions warning + plugin listing).
        //   2. First idle_prompt arrives -> on_turn fires -> gate opens.
        //   3. User does one real turn; PTY emits the real content.
        //   4. Second idle_prompt arrives -> on_turn fires -> buffer is read.
        //
        // The buffer handed to the renaming LLM must contain only step-3 content.
        let node_id = 70999;
        cleanup(node_id);

        let chrome = "\
            ╭─ WARNING ────────────────────────────────────╮\n\
            │  Claude Code running in Bypass Permissions   │\n\
            │  mode. Only use in a sandboxed environment.  │\n\
            ╰──────────────────────────────────────────────╯\n\
            Plugins loaded: hookify, feature-dev, frontend-design\n\
            ";
        // Step 1: chrome arrives before any on_turn — must be dropped
        on_output(node_id, chrome);

        let repo = MockRepo::with_name("bold-keen-brook");
        // Step 2: first turn opens the gate
        assert!(should_trigger_rename(&repo, node_id).is_none());

        // Step 3: real user content accumulates
        let real = "User: refactor the authentication module to use JWT.\n\
                    Assistant: I'll start by reading auth/login.rs.\n";
        let bulk = real.repeat(40); // push past SUMMARIZE_BUFFER_CHARS / 2
        on_output(node_id, &bulk);

        // Step 4: second turn — buffer is harvested
        let harvested = should_trigger_rename(&repo, node_id)
            .expect("rename should trigger on second turn with real content");

        assert!(
            !harvested.contains("Bypass Permissions"),
            "chrome leaked into rename buffer:\n{}",
            harvested
        );
        assert!(
            !harvested.contains("Plugins loaded"),
            "plugin listing leaked into rename buffer:\n{}",
            harvested
        );
        assert!(
            !harvested.contains("hookify"),
            "skill name leaked from startup chrome:\n{}",
            harvested
        );
        assert!(
            harvested.contains("authentication module"),
            "real user content missing from rename buffer:\n{}",
            harvested
        );

        RENAMING_IN_PROGRESS.lock().unwrap().remove(&node_id);
        cleanup(node_id);
    }

    #[test]
    fn mock_repo_update_records_calls() {
        let repo = MockRepo::with_name("bold-keen-brook");
        repo.update_agent_node_name(42, "test-slug-name").unwrap();
        let calls = repo.updates.lock().unwrap();
        assert_eq!(calls[0], (42, "test-slug-name".to_string()));
    }

    #[test]
    fn strip_claude_code_banner_removes_logo_and_cwd_lines() {
        let input = "\u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259C}\u{258C}   Claude Code v2.1.145\n\
                     \u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{259B}\u{2598}  Opus 4.7 with xhigh effort \u{00B7} Claude Pro\n\
                     \u{0020}\u{0020}\u{2598}\u{2598} \u{259D}\u{259D}    X:\\src\\buildmesh\\.claude\\worktrees\\open-lucky-box\n\
                     \n\
                     User asked to refactor the authentication flow.\n\
                     Assistant: I'll start by reading the auth module.";

        let stripped = strip_claude_code_banner(input);

        assert!(!stripped.contains("Claude Code v"), "version line should be gone:\n{}", stripped);
        assert!(!stripped.contains("Opus 4.7"), "model line should be gone:\n{}", stripped);
        assert!(!stripped.contains("open-lucky-box"), "cwd/banner line should be gone:\n{}", stripped);
        assert!(stripped.contains("refactor the authentication flow"), "real content must survive:\n{}", stripped);
    }

    #[test]
    fn strip_claude_code_banner_is_noop_when_no_banner_present() {
        let input = "User asked to fix a bug.\nAssistant: Looking at the code now.";
        assert_eq!(strip_claude_code_banner(input), input);
    }

    // --- Manual rename race guard ---

    #[test]
    fn user_renamed_mid_flight_returns_true_for_user_chosen_name() {
        // "Fix OAuth callback" is not a default adj-adj-noun slug — the
        // guard must return true so the LLM slug is dropped.
        let repo = MockRepo::with_name("Fix OAuth callback");
        assert!(user_renamed_mid_flight(&repo, 1));
    }

    #[test]
    fn user_renamed_mid_flight_returns_true_for_prior_llm_rename() {
        // A node whose LLM rename already committed is also "renamed";
        // the guard skips re-renaming in that case too.
        let repo = MockRepo::with_name("fix-auth-token-refresh");
        assert!(user_renamed_mid_flight(&repo, 1));
    }

    #[test]
    fn user_renamed_mid_flight_returns_false_for_default_name() {
        // Default adj-adj-noun slugs (e.g. "bold-keen-brook") mean the node
        // has NOT been renamed, so the LLM rename is allowed to proceed.
        let repo = MockRepo::with_name("bold-keen-brook");
        assert!(!user_renamed_mid_flight(&repo, 1));
    }

    #[test]
    fn user_renamed_mid_flight_returns_false_on_db_error() {
        // A transient DB read failure must NOT silently disable the
        // auto-rename path; err on the side of "user has not renamed" so
        // the LLM slug still gets a chance to commit.
        let repo = MockRepo {
            node_name: String::new(),
            should_fail: true,
            updates: std::sync::Mutex::new(vec![]),
        };
        assert!(!user_renamed_mid_flight(&repo, 1));
    }

    // --- cleanup() frees the rename-eligible state ---

    #[test]
    fn cleanup_removes_node_from_rename_eligible_maps() {
        // The manual-rename command relies on `cleanup` to free the buffer,
        // gate, and attempt counter for a node that no longer needs a
        // rename. `RENAMING_IN_PROGRESS` is intentionally NOT touched here:
        // it lives in the spawned task's epilogue so a concurrent LLM
        // rename is never lost mid-flight. Seed the three eligible maps
        // and assert cleanup drains them.
        let node_id = 81000i64;
        cleanup(node_id); // start from a clean slate

        SESSION_BUFFERS.lock().unwrap().insert(node_id, "buffered output".to_string());
        BUFFERING_READY.lock().unwrap().insert(node_id);
        RENAME_ATTEMPTS.lock().unwrap().insert(node_id, 2);

        // Sanity: present in all three.
        assert!(SESSION_BUFFERS.lock().unwrap().contains_key(&node_id));
        assert!(BUFFERING_READY.lock().unwrap().contains(&node_id));
        assert_eq!(RENAME_ATTEMPTS.lock().unwrap().get(&node_id).copied(), Some(2));

        cleanup(node_id);

        // Post-cleanup: the three eligible maps are drained.
        assert!(!SESSION_BUFFERS.lock().unwrap().contains_key(&node_id));
        assert!(!BUFFERING_READY.lock().unwrap().contains(&node_id));
        assert!(!RENAME_ATTEMPTS.lock().unwrap().contains_key(&node_id));
    }
}
