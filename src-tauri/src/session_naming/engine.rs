use super::repository::{DbSessionNamingRepository, SessionNamingRepository};
use super::slug::slug_with_retry;
use super::{is_default_name, NamingBackendFailedPayload, NodeRenamedPayload};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

// ---------------------------------------------------------------------------
// Per-node naming state — one entry per node, one lock
// ---------------------------------------------------------------------------

/// Everything the renamer tracks for a single node. An entry's mere existence
/// (with `buffering_ready`) marks a node as still rename-eligible; `cleanup`
/// removes the entry to free it.
#[derive(Default)]
pub(super) struct NodeNamingState {
    /// PTY output accumulated for the renamer. Only appended once the gate is
    /// open, and capped at `MAX_BUFFER_CHARS`.
    pub(super) buffer: String,
    /// The gate: PTY output is buffered only after the first Node Turn, so
    /// Claude Code's startup chrome is discarded before it can reach the LLM.
    pub(super) buffering_ready: bool,
    /// A rename task is in flight for this node — guards against duplicate LLM
    /// calls.
    pub(super) renaming: bool,
    /// Failed-rename counter; once it reaches `MAX_RENAME_ATTEMPTS` the node is
    /// stuck in a sticky lockout (buffer + gate dropped, counter kept).
    pub(super) attempts: u8,
}

pub(super) static NAMING: once_cell::sync::Lazy<Mutex<HashMap<i64, NodeNamingState>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));

/// Lock the naming map, recovering from poisoning rather than cascading a panic
/// across every other node (mirrors `services::agent_node::DRAIN_LOCK`). A
/// poisoned map only means some thread panicked mid-update; the per-node state
/// is still structurally valid to continue from.
pub(super) fn naming() -> std::sync::MutexGuard<'static, HashMap<i64, NodeNamingState>> {
    NAMING.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) const MAX_RENAME_ATTEMPTS: u8 = 3;

// `pub(crate)` so Autopilot's state evaluator (`autopilot::evaluator`,
// issue #483) shares the exact same terminal-cleaning rule instead of
// growing a second, subtly-different ANSI regex.
pub(crate) static ANSI_ESCAPE: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07|\x1b[()][A-B012]").unwrap()
    });

pub(super) const CLAUDE_CODE_BANNER_CHARS: &[char] = &[
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
pub(super) fn strip_claude_code_banner(s: &str) -> String {
    s.lines()
        .filter(|line| !line.chars().any(|c| CLAUDE_CODE_BANNER_CHARS.contains(&c)))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) const MAX_BUFFER_CHARS: usize = 4000;
pub(super) const SUMMARIZE_BUFFER_CHARS: usize = 3000;

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

/// Testable core: checks whether rename should trigger, returns the buffer
/// if so.
///
/// Side effect: on the very first call for a default-named node, this opens the
/// node's gate (`buffering_ready`) so subsequent PTY output begins accumulating.
/// On that first call the buffer is by definition empty, so this returns None
/// and the actual rename fires on a later turn against clean post-startup content.
///
/// Returns a [`RenameTrigger`] (not just the buffer) so the caller can
/// see what was harvested. The actual LLM-call backend is resolved in
/// `on_turn_with` from `AppPreferences.naming_provider` (issue #824 v2),
/// so the trigger no longer carries the node's `provider` — sharing the
/// node read here keeps the per-turn cost to one `get_agent_node_by_id`
/// call.
pub(super) fn should_trigger_rename(
    repo: &dyn SessionNamingRepository,
    node_id: i64,
) -> Option<RenameTrigger> {
    // Single DB read, hoisted above the default-name check. An Err here
    // is non-fatal: default-name bypass falls through, the trigger is
    // skipped, and the next turn retries.
    let node = repo.get_agent_node_by_id(node_id).ok();
    if let Some(ref n) = node {
        if !is_default_name(&n.name) {
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
        tracing::debug!(
            "on_turn({}): max rename attempts reached, giving up",
            node_id
        );
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

    Some(RenameTrigger {
        buffer: st.buffer.clone(),
    })
}

/// Bundle returned by [`should_trigger_rename`] when a rename fires:
/// the harvested PTY buffer. The LLM-call backend is resolved in
/// `on_turn_with` from `AppPreferences.naming_provider` (issue #824 v2),
/// so the trigger no longer carries the node's `provider`.
pub(super) struct RenameTrigger {
    pub(super) buffer: String,
}

/// Record a completed turn for a node. Triggers async LLM rename if buffer is sufficient.
pub fn on_turn(node_id: i64, app: AppHandle) {
    on_turn_with(Arc::new(DbSessionNamingRepository), node_id, app);
}

pub(super) fn on_turn_with(repo: Arc<dyn SessionNamingRepository>, node_id: i64, app: AppHandle) {
    // User-config gate (issue #824 v2): auto-naming is opt-in via
    // Settings → Auto-naming. `None` (the default for users who never
    // visited that page) skips the rename entirely — the node keeps its
    // random `adjective-adjective-noun` slug. Distinct from the
    // previously-shipped `node.provider` lookup, which would route through
    // whatever model the spawned node is on (could be Opus with xhigh
    // effort). The point of this gate is to *opt in*, never to inherit.
    let Some(user_naming_provider) = crate::preferences::naming_provider() else {
        return;
    };

    let Some(trigger) = should_trigger_rename(&*repo, node_id) else {
        return;
    };
    let RenameTrigger { buffer } = trigger;

    tracing::info!(
        "session_naming: triggering rename for node {} ({} chars)",
        node_id,
        buffer.len()
    );

    // Resolve the LLM-call env once at trigger time so a node's configured
    // backend (or the built-in Anthropic default) is honoured by
    // `summarize_and_rename_with`. The provider comes from
    // `AppPreferences.naming_provider` — NOT `node.provider`. The
    // default is "disabled"; the user explicitly opts in.
    let backend_env = naming_backend_env(&user_naming_provider);

    let app_for_task = app.clone();
    // Clone the Arc so the spawned future owns its own handle; the
    // `Arc<dyn SessionNamingRepository>` shape satisfies the
    // `'static` bound `tauri::async_runtime::spawn` requires, and
    // also lets the unit tests pass a mock repo in (see
    // `commit_rename_preserves_state_on_db_write_failure`).
    let repo_for_task = repo.clone();
    tauri::async_runtime::spawn(async move {
        match summarize_and_rename_with(node_id, &buffer, backend_env).await {
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
                // Route the DB write through the injected `repo` so the
                // mock-repo tests can exercise this commit path. The
                // production caller (see `on_turn` above) passes the
                // real `DbSessionNamingRepository`.
                commit_rename(&*repo_for_task, node_id, &slug, |name| {
                    let _ = app_for_task.emit(
                        "node-renamed",
                        NodeRenamedPayload {
                            node_id,
                            name: name.to_string(),
                        },
                    );
                });
            }
            Err(e) => {
                // `record_failed_attempt` bumps the counter and, on reaching the
                // cap, tears down buffer + gate while keeping the counter set so
                // the next on_turn short-circuits (sticky lockout).
                let attempt = record_failed_attempt(node_id);
                if attempt >= MAX_RENAME_ATTEMPTS {
                    tracing::warn!(
                        "Node {} giving up on rename after {} attempts (last error: {})",
                        node_id,
                        attempt,
                        e
                    );
                    // Issue #824: surface the terminal rename failure to the
                    // frontend so the user sees a toast / Settings hint
                    // instead of a silent log. The frontend can map the
                    // event to its existing toast primitive.
                    let _ = app_for_task.emit(
                        "naming-backend-failed",
                        NamingBackendFailedPayload { node_id, reason: e },
                    );
                } else {
                    tracing::warn!(
                        "Node {} rename attempt {}/{} failed (buffer preserved for retry): {}",
                        node_id,
                        attempt,
                        MAX_RENAME_ATTEMPTS,
                        e
                    );
                }
            }
        }
        set_renaming(node_id, false);
    });
}

/// Race-guard helper: returns true if the node's name in the DB is not a
/// default slug (i.e. it was either LLM-renamed earlier or manually renamed
/// by the user). Called from the LLM commit path to make sure a user
/// rename that landed during the LLM call wins over the auto-rename. On a
/// DB read error we err on the side of "user has not renamed" so the LLM
/// rename is allowed to proceed (failing closed here would be a silent
/// regression of the auto-rename for nodes whose DB read transiently fails).
pub(crate) fn user_renamed_mid_flight(repo: &dyn SessionNamingRepository, node_id: i64) -> bool {
    repo.get_agent_node_by_id(node_id)
        .map(|n| !is_default_name(&n.name))
        .unwrap_or(false)
}

/// Clear the buffer for a node. Called on kill so the node can resume fresh.
pub fn reset_buffers(node_id: i64) {
    clear_node_state(node_id);
}

/// Commit an LLM-derived slug to the DB and notify the frontend.
///
/// Extracted from the `Ok(slug)` arm of `on_turn_with` so the
/// write-or-skip behaviour is unit-testable through `MockRepo` without
/// having to spawn `claude --print`. The emit callback is injected so
/// the test can observe whether a `node-renamed` event would have
/// been broadcast.
///
/// Failure path (issue #1223): when the DB write errors — e.g. transient
/// write-lock contention with the pool worker — we log a warning and
/// leave the node's naming state intact. The frontend MUST NOT see a
/// `node-renamed` event for a name SQLite never persisted: the prior
/// behaviour cleared state and emitted regardless, so the UI patched
/// its in-memory list to a name the DB never accepted AND the retry
/// buffer was wiped, so the next Node Turn couldn't naturally retry.
/// This mirrors the LLM-error arm above, which preserves state across
/// transient backend failures.
pub(super) fn commit_rename(
    repo: &dyn SessionNamingRepository,
    node_id: i64,
    slug: &str,
    emit_renamed: impl FnOnce(&str),
) {
    if let Err(e) = repo.update_agent_node_name(node_id, slug) {
        tracing::warn!("Node {} rename write failed: {}", node_id, e);
        // Skip clear_node_state and the emit — the rename was never
        // persisted, so the UI must not see it, and the retry state
        // must remain intact for the next Node Turn to re-attempt.
        return;
    }
    clear_node_state(node_id);
    emit_renamed(slug);
    tracing::info!("Node {} renamed to '{}'", node_id, slug);
}

/// Clear all state for a node. Called on delete/archive.
pub fn cleanup(node_id: i64) {
    clear_node_state(node_id);
}

pub(super) fn clear_node_state(node_id: i64) {
    naming().remove(&node_id);
}

/// Clear the in-flight `renaming` flag for a node, if it still has an entry.
/// A no-op when the entry was already removed (e.g. by `clear_node_state` in
/// the same epilogue) — we never recreate state just to unset the flag.
pub(super) fn set_renaming(node_id: i64, value: bool) {
    if let Some(st) = naming().get_mut(&node_id) {
        st.renaming = value;
    }
}

/// Record a failed rename attempt, returning the new count. On reaching the cap
/// it drops the buffer + gate (sticky lockout) but keeps the counter so future
/// turns short-circuit in `should_trigger_rename`.
pub(super) fn record_failed_attempt(node_id: i64) -> u8 {
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
pub(super) fn maybe_dump_rename_buffer(node_id: i64, raw: &str, cleaned: &str) {
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

/// Resolve the absolute path of the `claude` binary used by
/// `summarize_and_rename_with`.
///
/// Why this exists: the previous `Command::new("claude")` call relied on
/// the buildmesh process's inherited `PATH`, which on Windows is captured
/// at process start. If buildmesh was launched before Claude Code was
/// installed/added to the user PATH — or from a launch context that
/// doesn't merge `HKCU\Environment` (Start menu via MSI, sandboxed
/// launchers, services) — the rename silently fails with
/// `failed to spawn CLI: program not found` and the node trips the
/// sticky 3-attempt lockout (the toast: "Couldn't auto-name node …").
///
/// Resolution order, matching `where.exe`:
/// 1. `which::which("claude")` — walks the process `PATH` honouring
///    `PATHEXT`. Returns the absolute path of the first hit, mirroring
///    the same lookup the regular Claude Code spawn relies on.
/// 2. **Well-known install fallback** — probes the standard install
///    locations for Claude Code on Windows, derived from the *current*
///    `USERPROFILE` / `APPDATA` env vars (not the path captured at
///    process start, so a recently-installed binary is still found):
///    - `%USERPROFILE%\.local\bin\claude.exe` (official installer)
///    - `%APPDATA%\npm\claude.cmd` / `claude` / `claude.exe` (npm shim)
///    - `%APPDATA%\npm\claude-code.cmd` / `claude-code` (npm package
///      scoped to the `claude-code` command name)
/// 3. **Clear error** — when both miss, surface a message that points
///    the user at Settings → Auto-naming (which `App.tsx`'s toast
///    already references) instead of a generic OS ENOENT string.
///
/// Pure and side-effect-free: reads env via `std::env::var`, never
/// mutates it. `pub(crate)` so a future test in a sibling module can
/// exercise the well-known-fallback arm against a stubbed `USERPROFILE`
/// without going through a real spawn.
pub(crate) fn resolve_claude_binary() -> Result<std::path::PathBuf, String> {
    if let Ok(p) = which::which("claude") {
        return Ok(p);
    }
    if let Some(p) = resolve_from_windows_install_paths(
        std::env::var("USERPROFILE").ok().as_deref(),
        std::env::var("APPDATA").ok().as_deref(),
    ) {
        return Ok(p);
    }
    Err("claude binary not found on PATH or in well-known install \
         locations (checked USERPROFILE\\.local\\bin and APPDATA\\npm); \
         install Claude Code (https://docs.claude.com/claude-code) so \
         the auto-rename can spawn `claude --print`"
        .to_string())
}

/// Probe the standard Windows install locations for a `claude` binary.
/// No-op on platforms where `USERPROFILE` / `APPDATA` are unset.
pub(super) fn resolve_from_windows_install_paths(
    userprofile: Option<&str>,
    appdata: Option<&str>,
) -> Option<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(home) = userprofile {
        candidates.push(std::path::PathBuf::from(home).join(r".local\bin\claude.exe"));
    }
    if let Some(appdata) = appdata {
        let npm = std::path::PathBuf::from(appdata).join("npm");
        candidates.push(npm.join("claude.cmd"));
        candidates.push(npm.join("claude"));
        candidates.push(npm.join("claude.exe"));
        candidates.push(npm.join("claude-code.cmd"));
        candidates.push(npm.join("claude-code"));
    }

    candidates.into_iter().find(|p| p.is_file())
}

/// Pure routing decision. The single input is fed in via a closure so
/// tests can stub the disk-reading helper
/// (`preferences::resolve_provider_env`) without touching real
/// preferences. Production callers pass the real helper via
/// [`naming_backend_env`].
///
/// Routing (issue #824 v2 — after the user-review pivot to a dedicated
/// Settings config):
/// 1. **User-configured `naming_provider`** (a spawn-option id from
///    [`crate::preferences::naming_provider`]). Honours whatever the user
///    picked — a Provider Account, a built-in like `"anthropic"`, etc.
///    This is the *only* layer the user can opt into; the node's own
///    provider is intentionally **not** consulted (auto-rename runs
///    frequently on trivial content, see the `#824` review follow-up).
/// 2. **Empty / unset** — caller should not invoke `summarize_and_rename_with`
///    at all (auto-naming is off). Returning an empty Vec here is a safety
///    net for the "user set a value that didn't resolve to an env" path;
///    the higher-level `on_turn_with` short-circuits on `Option::None`
///    before the helper is consulted.
///
/// Legacy `~/.claude/providers.conf` MiniMax is no longer the implicit
/// fallback. A user who wants cheap MiniMax renames now picks
/// `"minimax"` (or the configured `claude:minimax` account id) in
/// Settings → Auto-naming explicitly, mirroring the same opt-in shape as
/// every other rename backend. The historic `minimax_backend_env()` is
/// kept for any future regression check or one-shot tooling, but is no
/// longer called from the rename path.
pub(crate) fn naming_backend_env_with<F>(
    provider: &str,
    resolve_provider_env: F,
) -> Vec<(String, String)>
where
    F: FnOnce(&str) -> Vec<(String, String)>,
{
    if provider.is_empty() {
        return Vec::new();
    }
    if provider == "anthropic" {
        // Built-in Anthropic with a pinned haiku tier. The exact model name
        // is whichever Anthropic ships Claude Code with by default — we
        // deliberately don't pin a date-suffixed model name here (those
        // burn out and need updating alongside Anthropic's release cycle;
        // Claude Code's own haiku resolver picks the current default).
        return vec![(
            "ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(),
            "claude-3-5-haiku-latest".to_string(),
        )];
    }
    resolve_provider_env(provider)
}

/// Production wrapper: resolve the naming-side-channel env for the user's
/// configured `naming_provider` (spawn-option id from
/// `AppPreferences.naming_provider`). See [`naming_backend_env_with`] for
/// the routing contract; this is the thin caller-friendly version that
/// resolves via [`crate::preferences::resolve_provider_env`].
pub(crate) fn naming_backend_env(provider: &str) -> Vec<(String, String)> {
    naming_backend_env_with(provider, |p| crate::preferences::resolve_provider_env(p))
}

pub(super) async fn summarize_and_rename_with(
    node_id: i64,
    buffer: &str,
    backend_env: Vec<(String, String)>,
) -> Result<String, String> {
    let ansi_stripped = ANSI_ESCAPE.replace_all(buffer, "").to_string();
    let clean_buffer = strip_claude_code_banner(&ansi_stripped);

    maybe_dump_rename_buffer(node_id, buffer, &clean_buffer);

    let prompt = "The text on stdin is a terminal log from an AI coding-assistant session. \
                  Generate a short slug that describes the task the user is working on in this session. \
                  Output EXACTLY one line: 3 to 5 lowercase words joined by hyphens, nothing else \
                  (no explanation, no punctuation, no quotes, no example labels).";

    tracing::info!(
        "session_naming: running summarize command for node {} ({} chars)",
        node_id,
        clean_buffer.len()
    );

    // Compose the full prompt (instructions + terminal log) so Claude Code's
    // `--print` mode can read it from stdin. `claude --print` reads the user
    // message from stdin when no positional arg is given — passing both an
    // arg AND piping stdin is implementation-defined across Claude Code
    // versions, so we use the stdin-only mode for reliability.
    let full_input = format!("{}\n\nTerminal log to summarize:\n{}", prompt, clean_buffer);

    // Resolve `claude` to an absolute path before spawning — a direct
    // `Command::new("claude")` would rely on the buildmesh process's
    // inherited `PATH` (Windows: captured at process start; stale if
    // Claude Code was installed after launch).
    let claude_path = resolve_claude_binary()?;
    tracing::info!(
        "session_naming: resolved claude binary to {}",
        claude_path.display()
    );
    let mut cmd: tokio::process::Command =
        crate::process_util::command_no_window(&claude_path).into();
    // Caller (on_turn_with, line 734) is `tauri::async_runtime::spawn` and this
    // future is wrapped in a 30s `tokio::time::timeout` (line below) — both
    // cancel the awaiter, not the child, so without this flag a claude leak
    // accumulates up to MAX_RENAME_ATTEMPTS times per node (gh688).
    cmd.kill_on_drop(true);
    cmd.args(["--print"]);

    // Clear any inherited claude backend env (cwrap `unset` parity) so a value
    // exported in buildmesh's own environment can't override the resolved
    // backend below — then inject the env chosen by `naming_backend_env`
    // (per-node provider account, legacy `MINIMAX_API_KEY`, or built-in
    // Anthropic subscription when `backend_env` is empty). `naming_backend_env`
    // replaces the unconditional `minimax_backend_env()` injection that
    // #824 documented: previously this site routed every node through MiniMax
    // regardless of the node's own provider, and silently failed for any
    // user without a `MINIMAX_API_KEY`.
    for k in crate::agent::provider::CLAUDE_BACKEND_ENV_VARS {
        cmd.env_remove(k);
    }
    for (k, v) in &backend_env {
        cmd.env(k, v);
    }

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
                .write_all(full_input.as_bytes())
                .await
                .map_err(|e| format!("failed to write prompt+buffer to CLI: {}", e))?;
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
