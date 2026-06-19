//! Unified session naming module.
//!
//! State: one [`NodeNamingState`] per node behind a single map (`NAMING`). It
//! folds together what used to be four parallel statics — a PTY-output buffer,
//! a "buffering ready" gate, a "rename in progress" flag, and a failed-attempt
//! counter. Keeping a node's whole naming state in one entry under one lock
//! means it can be read at once, and removes the cross-map self-deadlock hazard
//! that four non-reentrant mutexes invited (a lock can't deadlock against
//! siblings that no longer exist). The fields:
//! - `buffer` — PTY output accumulator; an entry exists = the node hasn't been
//!   renamed yet.
//! - `buffering_ready` — the gate. PTY output is only accumulated after the
//!   *first* Node Turn, which discards the chrome Claude Code prints on
//!   startup (the "Bypass Permissions" warning, plugin/skill listings, banner
//!   repaints) so the renaming LLM never sees that text. Without this gate the
//!   LLM kept producing names like `bypass-permissions-worktree`.
//! - `renaming` — guards against duplicate concurrent LLM calls.
//! - `attempts` — per-node failure counter; caps retries at MAX_RENAME_ATTEMPTS.
//!
//! The LLM rename never holds the `NAMING` lock across its `.await`: the buffer
//! is snapshotted under the lock and released before the network call.
//!
//! Lifecycle:
//! - `on_spawn()` generates an initial random name
//! - `on_output(node_id, data)` buffers PTY output once the node's gate is open
//! - `on_turn(node_id, app)` flips the buffering gate (first call) and triggers
//!   LLM rename when the buffer is sufficient (subsequent calls)
//! - `cleanup(node_id)` removes all state for a node
//!
//! Diagnostic: set `BUILDMESH_DUMP_NAME_BUFFER=1` to dump the raw + cleaned
//! buffer to `%TEMP%/bm-name-buffer-<node_id>-<ts>.txt` whenever a rename runs.

use rand::seq::IndexedRandom;
use std::collections::HashMap;
use std::sync::Mutex;
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
// Per-node naming state — one entry per node, one lock
// ---------------------------------------------------------------------------

/// Everything the renamer tracks for a single node. An entry's mere existence
/// (with `buffering_ready`) marks a node as still rename-eligible; `cleanup`
/// removes the entry to free it.
#[derive(Default)]
struct NodeNamingState {
    /// PTY output accumulated for the renamer. Only appended once the gate is
    /// open, and capped at `MAX_BUFFER_CHARS`.
    buffer: String,
    /// The gate: PTY output is buffered only after the first Node Turn, so
    /// Claude Code's startup chrome is discarded before it can reach the LLM.
    buffering_ready: bool,
    /// A rename task is in flight for this node — guards against duplicate LLM
    /// calls.
    renaming: bool,
    /// Failed-rename counter; once it reaches `MAX_RENAME_ATTEMPTS` the node is
    /// stuck in a sticky lockout (buffer + gate dropped, counter kept).
    attempts: u8,
}

static NAMING: once_cell::sync::Lazy<Mutex<HashMap<i64, NodeNamingState>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));

/// Lock the naming map, recovering from poisoning rather than cascading a panic
/// across every other node (mirrors `services::agent_node::DRAIN_LOCK`). A
/// poisoned map only means some thread panicked mid-update; the per-node state
/// is still structurally valid to continue from.
fn naming() -> std::sync::MutexGuard<'static, HashMap<i64, NodeNamingState>> {
    NAMING.lock().unwrap_or_else(|e| e.into_inner())
}

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

/// Build the initial node name for a node spawned from a GitHub issue.
///
/// Composes `gh{issue_number}-{slug_from_title}` so the user can spot the
/// origin in the mesh list from the moment the spawn modal closes (e.g.
/// issue #123 "fix this feature" → `gh123-fix-this-feature`). The prefix is
/// the same on both spawn paths (`spawn_issue_agent` for the mobile HTTP
/// route and `create_issue_node` for the desktop two-stage flow).
///
/// Behaviour:
/// - Re-uses `slugify_issue_title` for the title → slug step, so all the
///   rules there (lowercase, hyphenation, 50-char cap, fallback to a
///   random default when the title is unslugifyable) carry over.
/// - Prepends `gh{n}-`. If the combined string is still under 50 chars the
///   result is final; otherwise it's truncated to 50 with a trailing-hyphen
///   re-trim (mirroring `slugify_issue_title`'s truncation step).
/// - Validates the final result against `SLUG_REGEX`. The only realistic
///   failure mode is a truncation that lands mid-token and yields something
///   that no longer matches (e.g. trailing punctuation in the digit run);
///   in that case we fall back to a plain random name so the caller always
///   gets a valid slug (an empty string would break worktree creation).
pub fn issue_node_name(issue_number: i64, title: &str) -> String {
    prefixed_node_name("gh", issue_number, title)
}

/// Build the initial node name for a node spawned from a GitHub pull request
/// (issue #420). `pr{N}-` prefix distinguishes PR-spawned nodes from
/// issue-spawned `gh{N}-` ones in the sidebar.
pub fn pr_node_name(pr_number: i64, title: &str) -> String {
    prefixed_node_name("pr", pr_number, title)
}

/// Shared core for `issue_node_name` / `pr_node_name` — both flows just
/// pick a prefix and the rules below apply uniformly. Centralising the
/// 50-char cap, trailing-hyphen re-trim, and `SLUG_REGEX` / `on_spawn()`
/// fallback in one place means a future tweak (e.g. bumping the cap) lands
/// for every spawn source at once.
///
/// Wire format: `{prefix}{number}-{slug}` (no dash between prefix and
/// number — `gh123-fix-this-feature`, not `gh-123-fix-this-feature`).
/// This matches what existing users have on disk from previous builds
/// and what the existing issue-spawn tests pin.
fn prefixed_node_name(prefix: &str, number: i64, title: &str) -> String {
    let slug = slugify_issue_title(title);
    let mut full = format!("{}{}-{}", prefix, number, slug);

    if full.len() > 50 {
        full.truncate(50);
        while full.ends_with('-') {
            full.pop();
        }
    }

    if SLUG_REGEX.is_match(&full) {
        full
    } else {
        on_spawn()
    }
}

/// Derive a node name from a GitHub issue title (issue #111).
///
/// Pipeline: lowercase → spaces/underscores become hyphens → strip
/// non-alphanumeric characters (keeping hyphens) → trim leading and trailing
/// hyphens → cap at 50 chars (with another trailing-hyphen trim in case
/// truncation lands on one). If the result is empty, starts with a digit, or
/// otherwise fails `SLUG_REGEX`, fall back to a random default name from
/// `on_spawn()` so the caller always gets a valid name — an empty string
/// would break the worktree directory creation downstream.
///
/// Pure function — exposed for `spawn_issue_agent` / `create_issue_node` to
/// give issue-spawned nodes a meaningful initial identifier, and unit-tested
/// here so the rules stay pinned.
///
/// Note on rename interaction: the result is a non-default slug, so the LLM
/// rename path's `is_default_name` guard will see it as already-named and
/// skip the LLM rename for issue-spawned nodes. The user can still manually
/// rename via `rename_session`; making the LLM fire on issue-derived names
/// is tracked as a follow-up (would need a separate "rename_locked" column
/// to distinguish user/prior-LLM renames from issue-derived slugs).
pub fn slugify_issue_title(title: &str) -> String {
    // 1. lowercase, 2. spaces + underscores → hyphens
    let mut s: String = title
        .to_lowercase()
        .replace([' ', '_'], "-");

    // 3. strip non-alphanumeric, keeping hyphens
    s.retain(|c| c.is_ascii_alphanumeric() || c == '-');

    // 4. trim leading and trailing hyphens
    s = s.trim_matches('-').to_string();

    // 5. cap at 50 chars (re-trim trailing hyphens if truncation split a word)
    if s.len() > 50 {
        s.truncate(50);
        while s.ends_with('-') {
            s.pop();
        }
    }

    // 6. validate against SLUG_REGEX; fall back to a random default if invalid
    if SLUG_REGEX.is_match(&s) {
        s
    } else {
        on_spawn()
    }
}

/// Buffer PTY output for a node. Accumulates in the node's `buffer` once its
/// gate (`buffering_ready`) is open (which only happens after the first
/// `on_turn`, so startup chrome is dropped).
pub fn on_output(node_id: i64, data: &str) {
    let mut map = naming();
    // No entry, or the gate hasn't opened yet → drop the output (it's startup
    // chrome). The entry is created when the gate opens in `should_trigger_rename`.
    let Some(st) = map.get_mut(&node_id) else {
        return;
    };
    if !st.buffering_ready {
        return;
    }
    st.buffer.push_str(data);
    if st.buffer.len() > MAX_BUFFER_CHARS {
        let mut drain_to = st.buffer.len() - MAX_BUFFER_CHARS;
        while !st.buffer.is_char_boundary(drain_to) {
            drain_to += 1;
        }
        st.buffer.drain(..drain_to);
    }
}

/// Testable core: checks whether rename should trigger, returns the buffer if so.
///
/// Side effect: on the very first call for a default-named node, this opens the
/// node's gate (`buffering_ready`) so subsequent PTY output begins accumulating.
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

    // IMPORTANT: `clear_node_state` above re-locks `NAMING`, and std::sync::Mutex
    // is not reentrant — so it must run BEFORE we take the guard here. From this
    // point on it's a single lock held for the whole decision.
    let mut map = naming();
    let st = map.entry(node_id).or_default();

    if st.attempts >= MAX_RENAME_ATTEMPTS {
        tracing::debug!("on_turn({}): max rename attempts reached, giving up", node_id);
        // Drop buffer + gate but DELIBERATELY keep the attempt counter so
        // future on_turn calls keep tripping this branch (sticky lockout).
        // Removing the entry would wipe the counter and let the cycle restart.
        st.buffer.clear();
        st.buffering_ready = false;
        return None;
    }

    // First Node Turn for this node — open the buffering gate so future
    // PTY output is captured, then return without renaming. This drops all the
    // startup chrome (banner, permission warnings, plugin/skill listings).
    if !st.buffering_ready {
        st.buffering_ready = true;
        tracing::info!(
            "session_naming({}): opened buffering gate (post-startup); rename will run from next turn",
            node_id
        );
        return None;
    }

    if st.buffer.len() < SUMMARIZE_BUFFER_CHARS / 2 {
        return None;
    }

    if st.renaming {
        tracing::debug!("on_turn({}): rename already in progress, skipping", node_id);
        return None;
    }
    st.renaming = true;

    Some(st.buffer.clone())
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
                    // Removing the entry also drops the `renaming` flag.
                    clear_node_state(node_id);
                    return;
                }
                if let Err(e) =
                    DbSessionNamingRepository.update_agent_node_name(node_id, &slug)
                {
                    tracing::warn!("Node {} rename write failed: {}", node_id, e);
                }
                clear_node_state(node_id);
                let _ = app_for_task.emit(
                    "node-renamed",
                    serde_json::json!({
                        "node_id": node_id,
                        "name": slug
                    }),
                );
                tracing::info!("Node {} renamed to '{}'", node_id, slug);
            }
            Err(e) => {
                // `record_failed_attempt` bumps the counter and, on reaching the
                // cap, tears down buffer + gate while keeping the counter set so
                // the next on_turn short-circuits (sticky lockout).
                let attempt = record_failed_attempt(node_id);
                if attempt >= MAX_RENAME_ATTEMPTS {
                    tracing::warn!(
                        "Node {} giving up on rename after {} attempts (last error: {})",
                        node_id, attempt, e
                    );
                } else {
                    tracing::warn!(
                        "Node {} rename attempt {}/{} failed (buffer preserved for retry): {}",
                        node_id, attempt, MAX_RENAME_ATTEMPTS, e
                    );
                }
            }
        }
        set_renaming(node_id, false);
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
    naming().remove(&node_id);
}

/// Clear the in-flight `renaming` flag for a node, if it still has an entry.
/// A no-op when the entry was already removed (e.g. by `clear_node_state` in
/// the same epilogue) — we never recreate state just to unset the flag.
fn set_renaming(node_id: i64, value: bool) {
    if let Some(st) = naming().get_mut(&node_id) {
        st.renaming = value;
    }
}

/// Record a failed rename attempt, returning the new count. On reaching the cap
/// it drops the buffer + gate (sticky lockout) but keeps the counter so future
/// turns short-circuit in `should_trigger_rename`.
fn record_failed_attempt(node_id: i64) -> u8 {
    let mut map = naming();
    let st = map.entry(node_id).or_default();
    st.attempts += 1;
    let attempt = st.attempts;
    if attempt >= MAX_RENAME_ATTEMPTS {
        st.buffer.clear();
        st.buffering_ready = false;
    }
    attempt
}

// ---------------------------------------------------------------------------
// Diagnostic helpers
// ---------------------------------------------------------------------------

pub fn buffers_size_bytes() -> usize {
    naming().values().map(|st| st.buffer.len()).sum()
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
        naming().entry(node_id).or_default().buffering_ready = true;
    }

    /// The node's accumulated buffer, or `None` if it has no naming state.
    fn buffer_of(node_id: i64) -> Option<String> {
        naming().get(&node_id).map(|st| st.buffer.clone())
    }

    /// Whether the buffering gate is open for the node.
    fn gate_open(node_id: i64) -> bool {
        naming().get(&node_id).map(|st| st.buffering_ready).unwrap_or(false)
    }

    /// Whether the node has any naming state at all.
    fn has_state(node_id: i64) -> bool {
        naming().contains_key(&node_id)
    }

    fn set_renaming_in_progress(node_id: i64) {
        naming().entry(node_id).or_default().renaming = true;
    }

    fn clear_renaming_in_progress(node_id: i64) {
        if let Some(st) = naming().get_mut(&node_id) {
            st.renaming = false;
        }
    }

    fn set_attempts(node_id: i64, n: u8) {
        naming().entry(node_id).or_default().attempts = n;
    }

    #[test]
    fn generates_three_word_hyphenated_name() {
        let name = on_spawn();
        let parts: Vec<&str> = name.split('-').collect();
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|p| !p.is_empty()));
        assert!(name.chars().all(|c| c.is_ascii_lowercase() || c == '-'));
    }

    // --- slugify_issue_title ---

    /// Happy path: a multi-word issue title lowercases, hyphenates, and stays
    /// under the 50-char limit. The result must round-trip as a valid
    /// `SLUG_REGEX` match — that's the contract spawn_issue_agent relies on.
    #[test]
    fn slugify_issue_title_happy_path() {
        assert_eq!(
            slugify_issue_title("Fix terminal flash on resize"),
            "fix-terminal-flash-on-resize"
        );
    }

    /// Underscores in the title become hyphens (the spec explicitly calls
    /// underscores out alongside spaces).
    #[test]
    fn slugify_issue_title_underscores_become_hyphens() {
        assert_eq!(slugify_issue_title("fix_oauth_callback"), "fix-oauth-callback");
        assert_eq!(
            slugify_issue_title("with_mixed AND_separators"),
            "with-mixed-and-separators"
        );
    }

    /// Punctuation that isn't part of a word gets stripped (colons, apostrophes,
    /// exclamation marks). The remaining alphanumeric runs join with hyphens.
    /// The spec is "strip" not "replace with hyphen" — so punctuation between
    /// two word-runs collapses them (e.g. `Foo::Bar` → `foobar`). The result
    /// still matches `SLUG_REGEX` and the user can manually rename if they
    /// want a different boundary.
    #[test]
    fn slugify_issue_title_strips_non_alphanumeric() {
        assert_eq!(
            slugify_issue_title("Bug: something's broken!"),
            "bug-somethings-broken"
        );
        // `::` between words is stripped; the two words collapse into one run.
        assert_eq!(
            slugify_issue_title("Refactor session_naming::on_turn"),
            "refactor-session-namingon-turn"
        );
    }

    /// Leading and trailing whitespace and hyphens are trimmed.
    #[test]
    fn slugify_issue_title_trims_leading_and_trailing_hyphens() {
        assert_eq!(slugify_issue_title("   hello world   "), "hello-world");
        assert_eq!(slugify_issue_title("---hello---"), "hello");
        assert_eq!(slugify_issue_title("  --fix bug--  "), "fix-bug");
    }

    /// Titles over 50 chars get truncated. The truncation must not leave a
    /// dangling trailing hyphen (it would otherwise be one char over the cap
    /// when stripped).
    #[test]
    fn slugify_issue_title_truncates_to_50_chars() {
        let long = "a-very-long-issue-title-that-exceeds-fifty-characters-yes-indeed";
        let result = slugify_issue_title(long);
        assert!(result.len() <= 50, "got len={}: {:?}", result.len(), result);
        assert!(!result.ends_with('-'), "trailing hyphen: {:?}", result);
        assert!(SLUG_REGEX.is_match(&result), "regex mismatch: {:?}", result);
    }

    /// When the input produces an invalid slug (empty, all-punctuation, starts
    /// with a digit, too short), the function MUST fall back to a default
    /// adj-adj-noun name. The caller relies on always getting back a valid
    /// node name — an empty string would break worktree creation.
    #[test]
    fn slugify_issue_title_falls_back_to_default_for_empty_input() {
        let result = slugify_issue_title("");
        let parts: Vec<&str> = result.split('-').collect();
        assert_eq!(parts.len(), 3, "fallback must be adj-adj-noun");
        assert!(SLUG_REGEX.is_match(&result), "fallback must match SLUG_REGEX: {:?}", result);
    }

    #[test]
    fn slugify_issue_title_falls_back_for_only_punctuation() {
        let result = slugify_issue_title("!!!---???");
        let parts: Vec<&str> = result.split('-').collect();
        assert_eq!(parts.len(), 3, "fallback must be adj-adj-noun, got: {:?}", result);
    }

    #[test]
    fn slugify_issue_title_falls_back_when_starts_with_digit() {
        // SLUG_REGEX requires the first char to be a letter, so "123-foo-bar"
        // cannot be a valid slug. Must fall back to a default name.
        let result = slugify_issue_title("123-something-here");
        let parts: Vec<&str> = result.split('-').collect();
        assert_eq!(parts.len(), 3, "digit-prefixed must fall back, got: {:?}", result);
    }

    /// Contract: the result is ALWAYS a valid SLUG_REGEX match. This is the
    /// invariant `services::agent_node::create` relies on when it stores the
    /// name as the worktree directory.
    #[test]
    fn slugify_issue_title_always_produces_valid_slug() {
        let cases = [
            "Fix OAuth callback",
            "Add dark mode to settings",
            "fix terminal flash on resize",
            "  ",
            "",
            "!!!",
            "123-abc",
            "a",
            "ab",
            "---",
            "中文 issue",   // non-ASCII filter-stripped; falls back
        ];
        for case in &cases {
            let result = slugify_issue_title(case);
            assert!(
                SLUG_REGEX.is_match(&result),
                "slugify({:?}) = {:?} does not match SLUG_REGEX",
                case,
                result
            );
        }
    }

    #[test]
    fn is_default_name_positive() {
        assert!(is_default_name("bold-keen-brook"));
        assert!(is_default_name("calm-deep-oak"));
    }

    // --- issue_node_name (gh{N}- prefix) ---

    /// Happy path: the issue number is prepended as `gh{N}-` and the title
    /// slugifies in the same way `slugify_issue_title` already documents.
    /// This is the user-facing change — `fix-this-feature` becomes
    /// `gh123-fix-this-feature`.
    #[test]
    fn issue_node_name_prefixes_with_gh_number() {
        assert_eq!(
            issue_node_name(123, "fix this feature"),
            "gh123-fix-this-feature"
        );
    }

    /// Underscores in the title become hyphens (inherited from
    /// `slugify_issue_title`).
    #[test]
    fn issue_node_name_hyphenates_underscored_title() {
        assert_eq!(
            issue_node_name(7, "fix_oauth_callback"),
            "gh7-fix-oauth-callback"
        );
    }

    /// Punctuation in the title is stripped (inherited behaviour). The
    /// `gh` prefix is preserved even when the title collapses to nothing
    /// because the result still has the `gh{N}-` prefix attached to the
    /// random fallback name.
    #[test]
    fn issue_node_name_preserves_prefix_when_title_has_punctuation() {
        let result = issue_node_name(42, "Bug: something's broken!");
        assert!(
            result.starts_with("gh42-"),
            "prefix must survive punctuation stripping: {:?}",
            result
        );
        assert!(
            SLUG_REGEX.is_match(&result),
            "must still match SLUG_REGEX: {:?}",
            result
        );
    }

    /// When the title is empty / unslugifyable, `slugify_issue_title` falls
    /// back to a random adj-adj-noun. The prefix must still be applied so
    /// the user can tell which issue the node came from.
    #[test]
    fn issue_node_name_keeps_prefix_when_title_is_empty() {
        let result = issue_node_name(5, "");
        assert!(
            result.starts_with("gh5-"),
            "empty title must still produce the gh-prefix: {:?}",
            result
        );
        assert!(
            SLUG_REGEX.is_match(&result),
            "must still match SLUG_REGEX: {:?}",
            result
        );
    }

    #[test]
    fn issue_node_name_keeps_prefix_when_title_is_punctuation_only() {
        let result = issue_node_name(99, "!!!---???");
        assert!(
            result.starts_with("gh99-"),
            "punctuation-only title must still produce the gh-prefix: {:?}",
            result
        );
        assert!(
            SLUG_REGEX.is_match(&result),
            "must still match SLUG_REGEX: {:?}",
            result
        );
    }

    /// Total length is capped at 50 chars (matching `SLUG_REGEX`'s upper
    /// bound). The truncation must not leave a trailing hyphen (a 51-char
    /// name after stripping a hyphen would round-trip to 50, but a 50-char
    /// name with a trailing hyphen is still rejected by `SLUG_REGEX`).
    #[test]
    fn issue_node_name_caps_total_length_at_50() {
        let long_title = "a-very-long-issue-title-that-exceeds-fifty-characters-yes-indeed";
        let result = issue_node_name(123, long_title);
        assert!(
            result.len() <= 50,
            "name must be <= 50 chars: len={} {:?}",
            result.len(),
            result
        );
        assert!(
            !result.ends_with('-'),
            "trailing hyphen would fail SLUG_REGEX: {:?}",
            result
        );
        assert!(
            SLUG_REGEX.is_match(&result),
            "must match SLUG_REGEX: {:?}",
            result
        );
    }

    /// Contract: the result is ALWAYS a valid SLUG_REGEX match. This is
    /// the invariant `services::agent_node::create` relies on when it
    /// stores the name as the worktree directory.
    #[test]
    fn issue_node_name_always_produces_valid_slug() {
        let cases = [
            ("Fix OAuth callback", 1i64),
            ("Add dark mode to settings", 4242),
            ("fix terminal flash on resize", 7),
            ("  ", 99),
            ("", 12),
            ("!!!", 3),
            ("123-abc", 5),
            ("---", 8),
            ("中文 issue", 11),
        ];
        for (title, n) in &cases {
            let result = issue_node_name(*n, title);
            assert!(
                SLUG_REGEX.is_match(&result),
                "issue_node_name({:?}, {:?}) = {:?} does not match SLUG_REGEX",
                n,
                title,
                result
            );
        }
    }

    /// Larger issue numbers must still produce a valid slug. Issue numbers
    /// can be quite large on long-running repos; the cap is the 50-char
    /// total, not the digit count.
    #[test]
    fn issue_node_name_handles_large_issue_numbers() {
        let result = issue_node_name(987654, "fix something");
        assert!(
            result.starts_with("gh987654-"),
            "large issue number must still appear in full: {:?}",
            result
        );
        assert!(
            SLUG_REGEX.is_match(&result),
            "must match SLUG_REGEX: {:?}",
            result
        );
    }

    // --- pr_node_name (issue #420, pr{N}- prefix) ---

    /// PR-spawned nodes use the `pr{N}-` prefix (vs the issue-spawn
    /// `gh{N}-` prefix) so the user can distinguish the two spawn sources
    /// at a glance in the sidebar. Otherwise the slugification rules are
    /// identical to `issue_node_name`'s.
    #[test]
    fn pr_node_name_prefixes_with_pr_number() {
        assert_eq!(pr_node_name(420, "spawn on PR"), "pr420-spawn-on-pr");
    }

    /// Empty title still produces a valid slug, keeping the `pr` prefix
    /// (matches the issue-spawn behaviour for "keep the prefix" — the
    /// user can always tell which PR the node came from).
    #[test]
    fn pr_node_name_keeps_prefix_when_title_is_empty() {
        let result = pr_node_name(5, "");
        assert!(
            result.starts_with("pr5-") || result.starts_with("pr5"),
            "empty title must keep the pr prefix: {:?}",
            result
        );
        assert!(
            SLUG_REGEX.is_match(&result) || result == on_spawn(),
            "must match SLUG_REGEX or fall back to a random default: {:?}",
            result
        );
    }

    /// Symmetry with `issue_node_name` — for the same input the PR variant
    /// only differs in the prefix (`pr` vs `gh`). Pin so a future refactor
    /// that drifts one without the other surfaces as a test failure.
    #[test]
    fn pr_node_name_differs_from_issue_name_only_by_prefix() {
        let title = "Add dark mode to settings";
        let issue = issue_node_name(42, title);
        let pr = pr_node_name(42, title);
        // Strip the prefix from each and the slug must match.
        let issue_slug = issue.strip_prefix("gh42-").unwrap();
        let pr_slug = pr.strip_prefix("pr42-").unwrap();
        assert_eq!(issue_slug, pr_slug, "pr and issue slugs must match for the same title");
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
        assert!(buffer_of(999).unwrap().len() <= MAX_BUFFER_CHARS);
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
        assert!(buffer_of(5).is_some(), "precondition: buffer exists");

        cleanup(5);

        assert!(
            buffer_of(5).is_none(),
            "node 5's buffer should be cleared by cleanup"
        );
    }

    #[test]
    fn cleanup_only_clears_target_session() {
        open_gate(5);
        open_gate(99);
        on_output(5, "session 5 output");
        on_output(99, "session 99 output");

        cleanup(5);

        assert!(buffer_of(5).is_none());
        assert!(buffer_of(99).is_some(), "session 99 buffer should survive");
        cleanup(99);
    }

    #[test]
    fn cleanup_is_idempotent() {
        open_gate(8);
        on_output(8, "some output");

        cleanup(8);
        cleanup(8); // second call must not panic

        assert!(buffer_of(8).is_none());
    }

    #[test]
    fn buffer_truncation_splits_multibyte_utf8_correctly() {
        let node_id = 77;
        open_gate(node_id);
        let base = "x".repeat(4000);
        let with_kanji = format!("{}{}", base, "日本");

        on_output(node_id, &with_kanji);

        let buf = buffer_of(node_id).unwrap();

        assert!(buf.len() <= MAX_BUFFER_CHARS);
        assert!(std::str::from_utf8(buf.as_bytes()).is_ok());
    }

    #[test]
    fn on_output_drops_data_until_gate_opens() {
        // Distinct id to avoid cross-test contamination with shared statics
        let node_id = 4242;
        cleanup(node_id);

        // Before any Node Turn, all output is discarded — this is the
        // bypass-permissions-warning fix: chrome printed at startup never
        // reaches the renaming buffer.
        on_output(node_id, "chrome chrome chrome");
        on_output(node_id, "Bypass Permissions warning text...");

        assert!(
            buffer_of(node_id).is_none(),
            "buffer must stay empty before gate opens"
        );

        // Now the gate flips (simulating first idle_prompt webhook)
        open_gate(node_id);

        on_output(node_id, "real user content");
        assert_eq!(
            buffer_of(node_id).as_deref(),
            Some("real user content"),
            "post-gate output must accumulate, no chrome contamination"
        );
        cleanup(node_id);
    }

    #[test]
    fn on_output_accumulates_after_gate_opens() {
        open_gate(42);
        on_output(42, "first output");
        on_output(42, "second output");

        assert_eq!(
            buffer_of(42).as_deref(),
            Some("first outputsecond output"),
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

        assert!(buffer_of(5).is_none());
        assert!(buffer_of(99).is_some());
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
        assert!(
            buffer_of(node_id).is_none(),
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
                source_pr: None,
                head_repo_owner: None,
                head_repo_clone_url: None,
                source_pr_pinned_sha: None,
                position: 0,
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

        assert!(buffer_of(node_id).is_none(), "buffer should be cleared");
        assert!(!gate_open(node_id), "gate should be closed for renamed node");
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
        assert!(gate_open(node_id), "gate must be open after first call");
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

        clear_renaming_in_progress(node_id);
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
        set_renaming_in_progress(node_id);

        let repo = MockRepo::with_name("bold-keen-brook");
        let result = should_trigger_rename(&repo, node_id);
        assert!(result.is_none(), "should skip when rename in progress");

        clear_renaming_in_progress(node_id);
        cleanup(node_id);
    }

    #[test]
    fn should_trigger_rename_skips_when_max_attempts_reached() {
        let node_id = 70006;
        cleanup(node_id);
        open_gate(node_id);
        on_output(node_id, &"x".repeat(2000));
        set_attempts(node_id, MAX_RENAME_ATTEMPTS);

        let repo = MockRepo::with_name("bold-keen-brook");
        let result = should_trigger_rename(&repo, node_id);
        assert!(result.is_none(), "should skip when max attempts reached");

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

        clear_renaming_in_progress(node_id);
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
    fn cleanup_removes_node_from_rename_eligible_state() {
        // The manual-rename command relies on `cleanup` to free the buffer,
        // gate, and attempt counter for a node that no longer needs a rename.
        // With the consolidated state these all live in one entry, so cleanup
        // removing the entry frees them together. Seed a fully-populated entry
        // and assert cleanup drains it.
        let node_id = 81000i64;
        cleanup(node_id); // start from a clean slate

        {
            let mut map = naming();
            let st = map.entry(node_id).or_default();
            st.buffer = "buffered output".to_string();
            st.buffering_ready = true;
            st.attempts = 2;
        }

        // Sanity: the entry exists with its fields set.
        assert!(has_state(node_id));
        assert!(gate_open(node_id));
        assert_eq!(buffer_of(node_id).as_deref(), Some("buffered output"));

        cleanup(node_id);

        // Post-cleanup: the node's entire naming state is gone.
        assert!(!has_state(node_id), "cleanup must remove the node's entry");
    }
}
