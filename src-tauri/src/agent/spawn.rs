//! Agent spawning — the `spawn_agent_inner` function and its helpers.
//!
//! Provider-specific recipe lives behind `Provider::adapter()` (see `agent/provider`).
//! OS-specific wrapping lives in `spawn_environment`.
//! PTY-specific helpers (open_pty_pair, spawn_child) live in `process.rs`.

use crate::agent::process::{AgentProcess, PROCESS_REGISTRY};
use crate::agent::provider::{Platform, CLAUDE_BACKEND_ENV_VARS};
use crate::agent::spawn_diag;
use crate::agent::spawn_environment;
use crate::db;
use crate::env;
use crate::git::worktree::provision::{
    fork_remote_alias, locked_fetch_pr_head, provision_for_spawn, read_origin_ref_sha,
    ProvisionOutcome, SpawnContext, SpawnSource,
};
// The bare fetch helpers (`fetch_single_ref`, `fetch_fork_head`) are
// re-exported under `#[cfg(test)]` below — production code goes through
// `locked_fetch_pr_head` (issue #698), which wraps the per-Mesh
// `with_mesh_sync_lock` around the bare helpers so callers can't forget to
// serialize concurrent PR-spawn fetches.
use crate::models::{AgentNode, EnvType, Provider, SessionStatus};
use base64::Engine;
use portable_pty::{native_pty_system, CommandBuilder, PtyPair, PtySize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Emitter;

/// Default `worktree_mode` when the mesh config leaves it unset. Pinned by
/// the unit test in this module (`default_worktree_mode_is_branched`).
///
/// This was previously paired with a TS sentinel at `src/lib/worktreeMode.ts`,
/// deleted in #411 once the TS side lost its only consumer (a self-referential
/// test). If a future UI re-exposes a worktree-mode selector, re-introduce
/// the TS constant alongside it and re-couple by doc comment + paired test
/// (see [[feedback_cross-language-default-coupling]]). See
/// `docs/knowledge-primer.md` (Worktree Support) for the branched-vs-detached
/// rationale.
pub const DEFAULT_WORKTREE_MODE: &str = "branched";

/// Threshold for the PTY reader thread's early-exit heuristic (issue #654).
/// If the reader thread exits within this window the agent is flagged
/// `Error` — typically because `--resume <uuid>` failed against an expired
/// session. The orchestrator's delayed `Spawning → Running` promotion sleeps
/// just past this same window (see `spawn_agent_inner` step 14b) so the two
/// sites MUST stay in sync; bumping this constant without re-checking the
/// promotion delay recreates the ghost-Running race.
/// Shared by the reader thread's early-exit heuristic and the
/// orchestrator's delayed Spawning→Running promotion sleep (#654). The two
/// MUST stay in lock-step — drifting them recreates the race in either
/// direction.
pub const EARLY_EXIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);

/// Resolve the `base_ref` string that `git::sync::fetch_origin` will use for
/// the spawn-time auto-sync. The chain (each tier only runs if the previous
/// one yields nothing useful):
///
/// 1. The mesh's `base_ref` column from the `meshes` DB row — explicit
///    user intent wins, even on a repo whose default branch disagrees.
///    **The COALESCE default `'origin/main'` is treated as "no config"**:
///    a fresh mesh whose `base_ref` column was never explicitly set reads
///    as `'origin/main'` from the DB (see `db::MESH_COLUMNS`), and a
///    user who never touched the field is functionally identical to a
///    user who has no config. Detecting both via the same path is what
///    closes the master-trunk regression. **There is no `mesh.toml`
///    file**: the value lives on the `meshes` SQLite row (and is
///    mirrored to `.claude/settings.json` at the mesh root for Claude
///    Code, see `commands::mesh_properties`).
/// 2. The repo's actual default branch read from
///    `refs/remotes/origin/HEAD` (populated by `git clone` / `git fetch`)
///    — closes the master-trunk regression where a repo whose default
///    branch is `master` was always fetched as `origin/main`.
/// 3. The literal `"origin/main"` as a last resort. Used only for a
///    non-repo / unconfigured path so the spawn path never blocks.
///
/// Extracted from `spawn_agent_inner` so the regression test in
/// `mod tests` can call it directly without standing up the full async /
/// PTY / DB machinery — the call site is a single expression.
///
/// `pub(crate)` so the background mesh sync (`services::pool_worker`)
/// resolves its fetch target through the exact same 3-tier chain the spawn
/// uses — a worker that fetched a literal `origin/main` on a repo whose
/// default branch is `master` would fail every pass and never satisfy the
/// spawn-time freshness TTL.
pub(crate) fn resolve_base_ref_for_spawn(mesh_path: &str, config_base_ref: Option<&str>) -> String {
    const COALESCE_DEFAULT: &str = "origin/main";
    let user_set = config_base_ref.filter(|b| b.trim() != COALESCE_DEFAULT);
    if let Some(b) = user_set {
        let trimmed = b.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    // No explicit config (or the COALESCE sentinel) — read the repo's
    // actual default branch from `refs/remotes/origin/HEAD` (populated by
    // `git clone` / `git fetch`). `get_default_branch` falls back to
    // "main" if the repo can't be opened or the symbolic ref is missing,
    // so a non-repo / unconfigured mesh path still resolves to
    // "origin/main" — preserving pre-fix behaviour and never blocking the
    // spawn.
    //
    // Called from the synchronous spawn path — use the sync core directly
    // (issue #762). The blocking pool offload that the async wrapper
    // provides is irrelevant for this single repo-open + symbolic-ref read;
    // the small wall-clock cost is well within the spawn budget and the
    // outer spawn task already runs on a blocking-pool thread.
    let branch = crate::commands::git::get_default_branch_blocking(mesh_path.to_string())
        .unwrap_or_else(|_| "main".to_string());
    format!("origin/{}", branch)
}

/// Per-spawn timing log. Records elapsed milliseconds at each
/// `checkpoint(name)` call and at the end via `total()`. Output goes to
/// `buildmesh.log` via the existing `tracing` setup — no extra plumbing.
///
/// Born of the spawn-latency investigation (5-10s lag between clicking
/// "Spawn" and visible UI feedback). The checkpoints proved the bottleneck
/// was NOT the hypothesised `git::sync::fetch_origin` (network) but
/// `worktree_create` — 97% of which was libgit2's checkout. That checkout
/// now shells out to `git worktree add` (~20× faster; ADR 0007 amendment),
/// so a fresh node is usable in ~2s instead of ~14s. The timer is kept as a
/// cheap spawn-latency regression guard; its only consumer is the `tracing`
/// log file.
struct SpawnTimer {
    start: std::time::Instant,
    session_id: i64,
}

impl SpawnTimer {
    fn new(session_id: i64) -> Self {
        Self {
            start: std::time::Instant::now(),
            session_id,
        }
    }

    fn checkpoint(&self, name: &str) {
        tracing::info!(
            "spawn_timing: session={} checkpoint={} elapsed={}ms",
            self.session_id,
            name,
            self.start.elapsed().as_millis()
        );
    }

    fn total(&self) {
        tracing::info!(
            "spawn_timing: session={} TOTAL elapsed={}ms",
            self.session_id,
            self.start.elapsed().as_millis()
        );
    }

    /// Original start instant — exposed `pub(crate)` so `register_agent`
    /// can clone it onto `AgentProcess.spawn_start`, giving the
    /// `first_user_input` log line the same reference as every other
    /// `spawn_timing:` checkpoint.
    pub(crate) fn start(&self) -> std::time::Instant {
        self.start
    }
}

/// Options for spawning or resuming an agent process.
pub struct SpawnOptions {
    pub session_id: i64,
    pub provider: Provider,
    pub resume: Option<String>,
    pub rows: u16,
    pub cols: u16,
    pub prefill: Option<String>,
    /// Pre-fetched node to avoid a redundant DB read when the caller already has it.
    pub node: Option<AgentNode>,
}

/// Open a PTY pair using the native PTY system.
pub fn open_pty_pair(rows: u16, cols: u16) -> Result<PtyPair, String> {
    let pty_system = native_pty_system();
    pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("failed to open PTY: {}", e))
}

/// Session ID mode: either assign a new ID or resume an existing one.
pub enum SessionIdMode {
    Assign(String),
    Resume(String),
    None,
}

/// Whether the PTY reader thread should attempt to capture a session ID
/// from live PTY output (issue #651).
///
/// Two independent code paths target the same `agent_nodes.cli_session_id`
/// column: the orchestrator's pre-write in `spawn_agent_inner` step 4 (Assign
/// mode) and the reader thread's `session_capture::try_extract_session_id`
/// match. They are unsynchronised, so a last-writer-wins race leaves the DB
/// holding either the orchestrator's UUID or a regex match — and on
/// auto-resume `claude --resume <wrong-uuid>` → "Conversation not found".
///
/// This predicate is the single source of truth for which path is allowed to
/// write for a given spawn:
///
/// * `Assign(_)` — orchestrator is authoritative; the reader MUST NOT
///   capture (the orchestrator just wrote the UUID that the agent was
///   launched with via `--session-id <uuid>`).
/// * `Resume(_)` — the resume arg is authoritative; the DB column already
///   holds the same ID from a prior spawn. A reader capture would race
///   `claude --resume <id>` with a possibly-different UUID.
/// * `None` — orchestrator did not pre-write (Codex / Agy self-assign
///   internally). Capture is allowed only if the provider's adapter
///   declares `self_assigns_session_id() = true`; otherwise any UUID
///   match would be spurious noise from a non-resumable shell.
fn reader_should_capture_session_id(
    session_id_mode: &SessionIdMode,
    adapter_self_assigns: bool,
) -> bool {
    adapter_self_assigns && matches!(session_id_mode, SessionIdMode::None)
}

/// Collapse `\r\n` and bare `\r` to `\n` in prefill text.
///
/// GitHub issue/PR bodies come back from the REST API with CRLF line endings.
/// A bare carriage return reaching an agent's TUI input (notably when the
/// agent is launched through `cmd.exe` or PowerShell on Windows) is
/// interpreted as Enter, submitting the prompt after the first line — so an
/// issue-seeded agent only ever sees its first line. macOS and Linux
/// (`claude` spawned directly) tolerate CRLF, which is why this only bit
/// Windows.
fn normalize_prefill_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Build the spawn command by composing the provider's recipe with the runtime environment.
///
/// `backend_env` is the per-profile backend selection resolved by the caller
/// (`preferences::resolve_provider_env(&node.provider)`): the `ANTHROPIC_*`
/// variables a custom Claude-compatible profile (MiniMax/Kimi/DeepSeek) needs to
/// target its endpoint. Empty for the built-in Anthropic subscription and for
/// the native-binary providers. Passed in (rather than resolved here) so this
/// function stays a pure composition of its inputs — no disk / preferences-cache
/// access — and the env injection can be unit-tested with an explicit list.
#[allow(clippy::too_many_arguments)]
pub fn build_spawn_command(
    resolved: &env::ResolvedPath,
    provider_enum: Provider,
    backend_env: &[(String, String)],
    session_id_mode: &SessionIdMode,
    session_id: i64,
    model_override: Option<&str>,
    effort_override: Option<&str>,
    prefill: Option<&str>,
    sandbox: bool,
) -> CommandBuilder {
    let adapter = provider_enum.adapter();
    let platform = if resolved.env_type == EnvType::Wsl {
        Platform::Linux
    } else {
        Platform::current()
    };

    // The base recipe before session-id / override / prefill args are layered on.
    let base_recipe = || adapter.spawn_recipe(platform, resolved.env_type);

    let mut recipe = match session_id_mode {
        SessionIdMode::Resume(id) => {
            if let Some(resume_recipe) = adapter.spawn_recipe_for_resume(platform, id) {
                tracing::info!("spawn_agent: using resume recipe for session {}", id);
                resume_recipe
            } else {
                let mut r = base_recipe();
                let args = adapter.resume_args(id);
                if !args.is_empty() {
                    tracing::info!("spawn_agent: resuming session {}", id);
                    r.base_args.extend(args);
                }
                r
            }
        }
        SessionIdMode::Assign(id) => {
            let mut r = base_recipe();
            let args = adapter.session_assign_args(id);
            if !args.is_empty() {
                tracing::info!("spawn_agent: assigning session-id {}", id);
                r.base_args.extend(args);
            }
            r
        }
        SessionIdMode::None => base_recipe(),
    };

    if adapter.supports_model_override() {
        if let Some(model) = model_override.filter(|s| !s.is_empty()) {
            recipe.base_args.extend(adapter.model_args(model));
        }
        if let Some(effort) = effort_override.filter(|s| !s.is_empty()) {
            recipe.base_args.extend(adapter.effort_args(effort));
        }
    }

    if adapter.supports_prefill() {
        if let Some(text) = prefill.filter(|s| !s.is_empty()) {
            let normalized = normalize_prefill_newlines(text);
            recipe.base_args.extend(adapter.prefill_args(&normalized));
        }
    }

    let mut cmd =
        spawn_environment::wrap(recipe, resolved.env_type, &resolved.spawn_path, session_id, sandbox);

    // Reset the claude backend env vars cwrap would have `unset` before
    // `exec claude`, so a value inherited from buildmesh's own environment can't
    // leak into the agent. For the built-in Anthropic subscription this clean
    // slate is the whole job (no overrides follow); a custom Claude-compatible
    // profile resets then sets its own `backend_env` below. On the WSL path this
    // only clears the wsl.exe launcher's env — harmless, since only WSLENV-listed
    // vars cross the boundary anyway.
    if adapter.resets_backend_env() {
        for k in CLAUDE_BACKEND_ENV_VARS {
            cmd.env_remove(k);
        }
    }

    // Inject the per-profile backend-selecting env (custom Claude-compatible
    // base URL, API token, model) resolved by the caller from the node's paired
    // provider account. Empty for the built-in Anthropic subscription and the
    // native-binary providers.
    if !backend_env.is_empty() {
        for (k, v) in backend_env {
            cmd.env(k, v);
        }
        if resolved.env_type == EnvType::Wsl {
            // Append the key names to WSLENV so they propagate into WSL
            let mut wslenv = std::env::var("WSLENV").unwrap_or_default();
            for (k, _) in backend_env {
                let suffix = "/u";
                let entry = format!("{}{}", k, suffix);
                let already_has = wslenv.split(':').any(|part| {
                    part.split('/').next() == Some(k.as_str())
                });
                if !already_has {
                    if wslenv.is_empty() {
                        wslenv = entry;
                    } else {
                        wslenv = format!("{}:{}", wslenv, entry);
                    }
                }
            }
            if !wslenv.is_empty() {
                cmd.env("WSLENV", wslenv);
            }
        }
    }
    cmd
}

/// Spawn the child process.
pub fn spawn_child(
    pair: &PtyPair,
    cmd: CommandBuilder,
) -> Result<Box<dyn portable_pty::Child + Send + Sync>, String> {
    pair.slave
        .spawn_command(cmd)
        .map_err(|e| format!("failed to spawn agent: {}", e))
}

/// Spawn `cmd` inside the Windows agent sandbox. Returns the same
/// `Child`/`MasterPty` trait objects as the normal path. Windows-only; the
/// non-Windows stub exists only so `spawn_agent_inner` compiles cross-platform
/// (the `sandbox_enabled` seam never selects this branch off Windows).
///
/// Uses the **restricted-token** primitive (ADR-0014), not the AppContainer:
/// the AppContainer's object-namespace isolation hung `claude.exe` at libuv's
/// named-pipe creation (#528) and blocked loopback (#533). The §4 spike proved
/// the restricted token fixes both. It is launched **permissive**
/// (`include_user_sid = true`) — read-confinement is *not* delivered here (a
/// same-user token can't deny home reads while MSYS `bash` runs; see the spike's
/// `tradeoff` test and ADR-0014 §Spike result), so home grants are unnecessary
/// (`grant_home = false`). Deny-by-default reads are a tracked follow-up
/// (separate-user principal / WSL).
#[cfg(target_os = "windows")]
fn sandbox_spawn(
    cmd: &CommandBuilder,
    session_id: i64,
    host_path: &str,
    rows: u16,
    cols: u16,
) -> Result<(Box<dyn portable_pty::Child + Send + Sync>, Box<dyn portable_pty::MasterPty + Send>), String> {
    crate::sandbox::spawn::spawn_sandboxed_restricted(cmd, session_id, host_path, rows, cols, false, true)
}

#[cfg(not(target_os = "windows"))]
fn sandbox_spawn(
    _cmd: &CommandBuilder,
    _session_id: i64,
    _host_path: &str,
    _rows: u16,
    _cols: u16,
) -> Result<(Box<dyn portable_pty::Child + Send + Sync>, Box<dyn portable_pty::MasterPty + Send>), String> {
    Err("process sandbox is only supported on Windows".to_string())
}

/// Ensures the attention hooks exist in `{project}/.claude/settings.local.json`.
///
/// Writes a catch-all `Notification` hook (fires on permission prompts, idle
/// prompts, MCP elicitations — every type that means "the user is needed") plus
/// a `Stop` hook (fires the instant a turn ends). Both POST to the local
/// attention endpoint. Idempotent: re-runs no-op once the config matches, and
/// migrate an older `idle_prompt`-only config on the next spawn.
pub fn inject_attention_hook(project_path: &std::path::Path) {
    let claude_dir = project_path.join(".claude");
    if let Err(e) = std::fs::create_dir_all(&claude_dir) {
        tracing::warn!("inject_attention_hook: failed to create .claude dir: {}", e);
        return;
    }

    let settings_path = claude_dir.join("settings.local.json");
    let mut settings: serde_json::Value = match std::fs::read_to_string(&settings_path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({})),
        Err(_) => serde_json::json!({}),
    };

    // Resolve the port from $BUILDMESH_PORT at hook-run time (set per-agent in
    // spawn_environment) rather than baking a literal. This keeps the hook
    // correct across the 1992→1994 fallback and routes a dev-profile agent's
    // attention to the dev instance (2992), not the stable hub.
    let hook_command =
        "curl -sf -X POST http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID || true"
            .to_string();

    // We register two hooks so the user is told the *instant* their input is
    // needed, not just when the agent goes idle:
    //   - Notification with an empty (catch-all) matcher fires on every
    //     notification type — crucially `permission_prompt` (the agent is asking
    //     to run a tool / answer a question) as well as `idle_prompt`. Matching
    //     only `idle_prompt` (the old behaviour) missed every permission prompt,
    //     so the user was never alerted when an agent paused to ask something.
    //   - Stop fires the moment the agent finishes a turn, so "agent is waiting
    //     for you" lands immediately instead of after Claude Code's idle timer.
    // Both POST to the same attention endpoint; `mark_attention` is idempotent.
    let notification_hook = serde_json::json!({
        "type": "command",
        "command": hook_command
    });
    let expected_hooks = serde_json::json!({
        "Notification": [{
            "matcher": "",
            "hooks": [notification_hook.clone()]
        }],
        "Stop": [{
            "hooks": [notification_hook]
        }]
    });

    if settings.get("hooks") == Some(&expected_hooks) {
        return;
    }

    settings["hooks"] = expected_hooks;

    match serde_json::to_string_pretty(&settings) {
        Ok(content) => {
            if let Err(e) = std::fs::write(&settings_path, content) {
                tracing::warn!("inject_attention_hook: failed to write: {}", e);
            } else {
                tracing::info!("inject_attention_hook: wrote hook at {:?}", settings_path);
            }
        }
        Err(e) => tracing::warn!("inject_attention_hook: serialize failed: {}", e),
    }
}

/// Check if an agent is already running for this session.
pub fn is_agent_already_running(session_id: &i64) -> bool {
    if let Some(agent) = PROCESS_REGISTRY.get(session_id) {
        if agent.reader_alive.load(Ordering::SeqCst) {
            tracing::info!(
                "spawn_agent: session {} is already running, skipping spawn",
                session_id
            );
            return true;
        }
    }
    false
}

/// Register the agent process in the registry.
///
/// `spawn_start` is the original `SpawnTimer.start` clone — used by
/// `record_first_input_if_first` (via `AgentProcess.spawn_start`) to
/// timestamp the `first_user_input` log line against the same reference
/// as every other `spawn_timing:` checkpoint.
fn register_agent(
    session_id: i64,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn std::io::Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    reader_alive: Arc<AtomicBool>,
    job: Option<crate::process_util::JobHandle>,
    spawn_start: std::time::Instant,
    mesh_id: i64,
) {
    PROCESS_REGISTRY.insert(
        session_id,
        AgentProcess {
            child: Arc::new(Mutex::new(child)),
            writer: Arc::new(Mutex::new(writer)),
            // Wrap the master in `Some` so `kill_session` can `take()` it
            // out to drop the pseudoconsole (issue #300).
            master: Arc::new(Mutex::new(Some(master))),
            reader_alive,
            job,
            // The handle is set after the reader thread is spawned, via
            // `AgentProcess::set_reader_handle`. We insert first so a
            // concurrent `is_agent_already_running` sees the entry; the
            // window between insert and setter is benign (see process.rs).
            reader_handle: Mutex::new(None),
            spawn_start,
            // First-write gate: starts false, flipped true exactly once
            // by `record_first_input_if_first` on the first successful
            // `write_bytes` call for this session. Plain `AtomicBool` —
            // the field lives inside `Arc<AgentProcess>` already, so no
            // inner Arc is needed (the reader thread doesn't share this
            // flag).
            first_user_input_logged: AtomicBool::new(false),
            // Issue #634: stored at registration so `write_bytes` and the
            // PTY read loop can record per-mesh activity without a DB
            // lookup on every chunk. `mesh_id` was already resolved at
            // `spawn_agent_inner:797` via `db::get_mesh_by_path(&node.path)`
            // — the value is in scope here.
            mesh_id,
        },
    );
}

fn encode_pty_chunk(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// Core PTY read loop: read 8 KiB chunks until EOF or error, handing raw bytes
/// to `on_chunk`. Returns when the PTY closes.
///
/// Extracted so the production reader thread and the real-PTY integration test
/// exercise the exact same read path (see `src-tauri/tests/pty_spawn.rs`).
pub fn pump_pty_output(
    mut reader: Box<dyn std::io::Read + Send>,
    mut on_chunk: impl FnMut(&[u8]),
) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                on_chunk(&buf[..n]);
            }
            Err(e) => {
                tracing::error!("PTY read error: {}", e);
                break;
            }
        }
    }
}

/// Buffer a PTY chunk for session auto-naming — every chunk for LLM
/// providers, never for a plain terminal. A terminal's rename buffer is
/// never consumed: the rename LLM only fires from `on_turn`, which only
/// the Claude stop hook calls. Ungated, each Terminal node would retain
/// up to `MAX_BUFFER_CHARS` and contend the global NAMING mutex on every
/// chunk for the node's whole lifetime (issue #296).
///
/// Extracted from `start_reader`'s pump callback so the gate is
/// unit-testable without standing up an AppHandle / PTY (same seam
/// pattern as `resolve_base_ref_for_spawn`).
pub(crate) fn maybe_buffer_for_naming(is_plain_terminal: bool, session_id: i64, text: &str) {
    if !is_plain_terminal {
        crate::session_naming::on_output(session_id, text);
    }
}

/// Start the PTY reader thread. Returns the `JoinHandle` so the caller
/// can store it on `AgentProcess` and let `kill_session` join with a
/// bounded timeout (issue #300).
///
/// Two time references are passed in, with distinct semantics — keep
/// them separate:
///
/// * `spawned_at` — process-creation time (`Instant::now()` right after
///   `spawn_child` returns). Used by the 3-second early-exit heuristic
///   to detect a likely-failed `--resume`. **Must NOT be unified with
///   `spawn_start`**: a slow 14s spawn pipeline followed by an agent
///   dying 1s after process creation must still trigger `resume-failed`,
///   and the original "3s after process creation" semantic preserves
///   that detection.
/// * `spawn_start` — the original `SpawnTimer.start` from the top of
///   `spawn_agent_inner`. Used by the `first_pty_output` checkpoint log
///   so it lines up with every other `spawn_timing:` line (all
///   measured against the same "user clicked Spawn" instant).
fn start_reader(
    app: tauri::AppHandle,
    session_id: i64,
    needs_session_capture: bool,
    reader: Box<dyn std::io::Read + Send>,
    spawned_at: std::time::Instant,
    reader_alive: Arc<AtomicBool>,
    is_plain_terminal: bool,
    spawn_start: std::time::Instant,
    mesh_id: i64,
) -> std::thread::JoinHandle<()> {
    let app_clone = app;
    let reader_alive_clone = reader_alive;
    let session_captured = AtomicBool::new(!needs_session_capture);

    std::thread::spawn(move || {
        // The SpawnTimer in spawn_agent_inner stops at process *creation*
        // (`after_pty_spawn`), so the shell → agent-CLI boot tail is invisible
        // to it. Log the gap from spawn to the first byte of PTY output here —
        // that first byte is the earliest signal the agent process is actually
        // alive and producing a UI. Same `spawn_timing:` prefix so it sits
        // alongside the other checkpoints. Measured against `spawn_start` (not
        // `spawned_at`) so this elapsed time is comparable to every other
        // checkpoint in the log.
        let mut first_chunk = true;
        pump_pty_output(reader, |data| {
            if first_chunk {
                first_chunk = false;
                tracing::info!(
                    "spawn_timing: session={} checkpoint=first_pty_output elapsed={}ms (spawn start → first output; agent CLI boot tail)",
                    session_id,
                    spawn_start.elapsed().as_millis()
                );
            }
            // Mark THIS MESH as active so the background warm-pool worker
            // holds off its idle refills for this mesh's pool while an agent
            // is actively producing output (issue #613 AC2; issue #634 scopes
            // the activity per-mesh so a chatty agent on mesh A doesn't
            // starve mesh B's pool). `mesh_id` is captured from the spawn
            // context at thread start — the closure outlives the agent's
            // registry entry, so reading it from `PROCESS_REGISTRY` inside
            // the closure would race with `kill_session`'s `remove`.
            crate::services::pool_worker::note_activity_for_mesh(mesh_id);

            let text = String::from_utf8_lossy(data);
            maybe_buffer_for_naming(is_plain_terminal, session_id, &text);
            // Autopilot state evaluator tail (issue #483) — one in-memory
            // set lookup for non-piloted nodes.
            crate::autopilot::evaluator::on_output(session_id, &text);

            if !session_captured.load(Ordering::Relaxed) {
                if let Some(uuid) = crate::session_capture::try_extract_session_id(&text) {
                    let _ = db::update_cli_session_id(session_id, uuid);
                    session_captured.store(true, Ordering::Relaxed);
                    // [DEBUG-concurrent-spawn] Reader-thread capture. The
                    // `pty_output` source means we matched the regex from
                    // the live PTY stream (vs. the orchestrator's
                    // pre-assigned UUID in Assign mode). Two reads from
                    // this stream while the orchestrator is still
                    // in-flight would surface as two reader_event lines
                    // with overlapping timestamps; a capture AFTER
                    // `phase=exit` in the orchestrator's stream is the
                    // "stale UUID on auto-resume" failure mode.
                    crate::agent::spawn_diag::reader_event(session_id, "pty_output", uuid);
                    tracing::info!("session_capture: captured session ID {} for node {}", uuid, session_id);
                }
            }

            let _ = app_clone.emit(
                "agent-output",
                serde_json::json!({
                    "session_id": session_id,
                    "data": encode_pty_chunk(data)
                }),
            );

            // Forward to any connected mobile WebSocket clients
            crate::http_server::send_pty_output(session_id, data.to_vec());
        });
        tracing::debug!("PTY reader loop ended for session {}, reader exiting", session_id);
        reader_alive_clone.store(false, Ordering::SeqCst);

        if is_plain_terminal {
            // A plain terminal's shell exiting — whether via `exit`, the
            // user closing the window, or the process being killed — is
            // a normal Idle state, never an Error. Skip the LLM-specific
            // 3-second "resume-failed" early-exit warning and event: a
            // shell is not a --resume, so a fast exit isn't a resume
            // signal; emitting one would confuse the frontend.
            let _ = db::update_agent_node_status(session_id, SessionStatus::Idle);
        } else {
            // Detect early exit (likely a failed --resume). Uses
            // `spawned_at` (process-creation time), NOT `spawn_start`,
            // because the heuristic answers "did the process die
            // almost immediately after it was created?" — a slow
            // spawn pipeline followed by a 1s-later death should
            // still trigger `resume-failed`. Switching the reference
            // to `spawn_start` here would add the entire pipeline
            // duration (often 2-14s) to the threshold and miss
            // legitimate early-exit detections on slow spawns.
            let elapsed = spawned_at.elapsed();
            if elapsed < EARLY_EXIT_WINDOW {
                tracing::warn!(
                    "Node {} reader exited after {:?} — likely resume failure",
                    session_id,
                    elapsed
                );
                // Symmetric to the orchestrator's Spawning write (#654):
                // never resurrect `Archived`, and don't double-write `Error`
                // (which would bump `status_changed_at` needlessly).
                let _ = db::update_agent_node_status_unless_in(
                    session_id,
                    SessionStatus::Error,
                    &[SessionStatus::Error, SessionStatus::Archived],
                );
                let _ = app_clone.emit(
                    "resume-failed",
                    serde_json::json!({
                        "node_id": session_id,
                        "error": "Agent exited immediately after spawn — session may have expired"
                    }),
                );
            } else {
                let _ = db::update_agent_node_status(session_id, SessionStatus::Idle);
            }
        }

        tracing::debug!("PTY reader thread exited for session {}", session_id);
    })
}

// ---------------------------------------------------------------------------
// Public Tauri command interface
// ---------------------------------------------------------------------------

/// Inner implementation shared by spawn_agent command and auto_resume_sessions.
pub async fn spawn_agent_inner(
    app: &tauri::AppHandle,
    opts: SpawnOptions,
) -> Result<(), String> {
    let SpawnOptions { session_id, provider, resume, rows, cols, prefill, node: preloaded_node } = opts;

    tracing::info!(
        "spawn_agent_inner: session_id={}, provider={:?}, resume={:?}, size={}x{}",
        session_id,
        provider,
        resume,
        cols,
        rows
    );

    let timer = SpawnTimer::new(session_id);
    // [DEBUG-concurrent-spawn] RAII counter — guards `IN_FLIGHT` across the
    // entire function body (Ok/Err/?-returns all decrement via Drop). One
    // log line on enter, one on exit, with a delta in between. The label
    // discriminates manual vs Issue vs PR spawns so the dev log can group
    // a single burst.
    let diag_label: &'static str = match (&preloaded_node, resume.as_deref()) {
        (_, Some(_)) => "resume",
        (Some(n), _) if n.source_pr.is_some() => "pr-spawn",
        (Some(n), _) if n.source_issue.is_some() => "issue-spawn",
        _ => "manual-spawn",
    };
    let diag = spawn_diag::InFlightGuard::enter(session_id, diag_label);

    // 1. Check if already running
    if is_agent_already_running(&session_id) {
        diag.checkpoint("already_running_short_circuit");
        return Ok(());
    }
    diag.checkpoint("after_already_running_check");

    // 2. Kill any stale process for this session
    tracing::debug!("spawn_agent_inner: killing stale processes for session {}", session_id);
    crate::commands::agent::kill_agent(session_id).await.ok();
    diag.checkpoint("after_kill_stale");

    // 3. Get node and resolve paths (skip DB read if caller provided the node)
    let node = match preloaded_node {
        Some(n) => n,
        None => db::get_agent_node_by_id(session_id).map_err(|e| {
            let err = format!("spawn_agent: failed to get agent node {}: {}", session_id, e);
            tracing::error!("{}", err);
            err
        })?,
    };
    tracing::info!("spawn_agent_inner: node path={}, env={:?}", node.path, node.env);
    timer.checkpoint("after_node_db_read");
    diag.checkpoint("after_node_db_read");

    let adapter = provider.adapter();

    // 4. Determine session ID mode
    let session_id_mode = if adapter.supports_resume() {
        match resume {
            Some(ref id) if !id.is_empty() => SessionIdMode::Resume(id.clone()),
            _ => {
                if adapter.self_assigns_session_id() {
                    SessionIdMode::None
                } else {
                    let cli_uuid = uuid::Uuid::new_v4().to_string();
                    db::update_cli_session_id(session_id, &cli_uuid).map_err(|e| e.to_string())?;
                    tracing::info!("spawn_agent_inner: assigned cli_session_id={}", cli_uuid);
                    diag.db_write("cli_session_id", &cli_uuid);
                    SessionIdMode::Assign(cli_uuid)
                }
            }
        }
    } else {
        SessionIdMode::None
    };

    // 5. Read mesh row for use_worktree / model / effort / worktree_mode
    let row = env::mesh_row(&std::path::PathBuf::from(&node.path));
    let use_worktree = row.as_ref().map(|r| r.use_worktree).unwrap_or(true);
    let model_override = row.as_ref().and_then(|r| r.model.as_deref());
    let effort_override = row.as_ref().and_then(|r| r.effort.as_deref());
    // OS-level sandbox toggle (macOS Seatbelt #497, Windows AppContainer #498).
    // Off by default; the per-OS spawn policy is decided in `spawn_environment::wrap`
    // and `crate::sandbox::spawn::spawn_sandboxed`.
    let sandbox = row.as_ref().map(|r| r.sandbox).unwrap_or(false);
    let worktree_mode = row
        .as_ref()
        .and_then(|r| r.worktree_mode.as_deref())
        .unwrap_or(DEFAULT_WORKTREE_MODE);
    // Autopilot enforcement (issue #482, PRD #480): auto-spawned nodes must
    // always work on a real branch (and in a worktree) — the wrap-up sequence
    // pushes a branch and opens a PR, which a detached-HEAD worktree or a
    // shared mesh root cannot do. The ledger row is written before stage-2
    // starts, so this read is ordered correctly. The node row itself already
    // carries `use_worktree = true` (spawn override in `services::autopilot`).
    let is_autopilot = db::get_autopilot_run(session_id)
        .ok()
        .flatten()
        .is_some();
    let use_worktree = use_worktree || is_autopilot;
    let worktree_mode = if is_autopilot { "branched" } else { worktree_mode };
    let base_ref = resolve_base_ref_for_spawn(
        &node.path,
        row.as_ref().and_then(|r| r.base_ref.as_deref()),
    );

    timer.checkpoint("after_mesh_row_read");
    diag.checkpoint("after_mesh_row_read");

    // 6. Compute spawn path. The pool claim (issue #609/#612) decides whether
    //    the spawn adopts a pre-warmed worktree (Manual: pool slug IS the
    //    node name; Issue/PR: `git worktree move` the pool dir onto the
    //    `gh{N}-`/`pr{N}-` target) or falls through to a cold create. A
    //    claim failure is non-fatal — the spawn falls back to cold; it
    //    only fails on an actual worktree-create error.
    let mesh_id = db::get_mesh_by_path(&node.path).map(|m| m.id).unwrap_or(-1);
    // `is_rename_spawn` selects between the two warm-pool adoption modes
    // downstream: Manual adopts the pool's slug as the node name (issue #609);
    // Issue/PR keep their own `gh{N}-`/`pr{N}-` name and move the pool dir
    // to match (issue #612). Consumed by the post-spawn name adoption
    // (further below) and by the SpawnContext built at phase 7.
    let is_rename_spawn = node.source_issue.is_some() || node.source_pr.is_some();
    let mut warm_claimed: Option<crate::services::warm_pool::ClaimedWarmEntry> = None;
    // Issue #653: a successful `try_claim` that the use-site recheck later
    // dropped still drained the pool by one row — `warm_claimed` is None
    // (the spawn fell back to cold), but the mesh's pool inventory is one
    // short. Track "we claimed at least once this spawn" so the post-spawn
    // refill still fires (otherwise the pool stays at target-1 until the
    // next reconcile). Distinct from `warm_claimed` because `warm_claimed`
    // tracks "we adopted the warm entry as this node's worktree" — that's
    // what `forget_after_spawn` and the manual name adoption gate on.
    let mut pool_was_drained_by_this_spawn = false;
    if use_worktree {
        // The path the node resolves to WITHOUT a pool claim. If it's already
        // on disk this spawn is a resume / handover / re-spawn reusing an
        // existing worktree — never claim a pool entry for it (that would
        // re-point the node at a different directory and abandon its work).
        let existing = env::resolve_agent_path(&node.path, node.worktree_name.as_deref());
        let existing_present = std::path::Path::new(&existing.host_path).exists();
        if mesh_id > 0
            && crate::services::warm_pool::should_claim_for_spawn(existing_present)
        {
            match crate::services::warm_pool::try_claim(&app, mesh_id) {
                Ok(Some(entry)) => {
                    tracing::info!(
                        "spawn_agent_inner: claimed warm pool entry id={} path={} slug={} base_sha={}",
                        entry.id,
                        entry.path,
                        entry.preassigned_name,
                        entry.base_sha.as_deref().unwrap_or("none"),
                    );
                    spawn_diag::warm_claim_event(mesh_id, session_id, &format!("ok:id={}:path={}", entry.id, entry.path));
                    // Issue #653 use-site guard: `try_claim` just checked
                    // the directory exists, but the spawn then waits
                    // seconds inside `fetch_origin` + git worktree move;
                    // another thread can delete the directory in that gap.
                    // Re-check immediately before committing to the warm
                    // path. On false, `recheck_after_claim` already dropped
                    // the row + tombstone; we just leave `warm_claimed`
                    // None so the existing `spawn_worktree_name` fallback
                    // resolves to the throwaway slug and the cold-create
                    // block runs naturally for both spawn modes (Issue/PR
                    // and manual).
                    if crate::services::warm_pool::recheck_after_claim(entry.id, &entry.path) {
                        warm_claimed = Some(entry);
                        pool_was_drained_by_this_spawn = true;
                    } else {
                        spawn_diag::warm_claim_event(mesh_id, session_id, "none:dir_vanished");
                        // Note: `recheck_after_claim` already logs the
                        // reason (claimed row N's directory disappeared...).
                        // Don't duplicate that WARN here — the spawn-diag
                        // event is enough structured signal; logging twice
                        // is just operator noise for one race event.
                        // warm_claimed stays None — do NOT adopt. The row
                        // was already dropped by recheck_after_claim, but
                        // the pool inventory is still down by one; the
                        // post-spawn refill below must run regardless of
                        // the local `did_claim_warm` flag (which checks
                        // `warm_claimed.is_some()`, not the DB).
                        pool_was_drained_by_this_spawn = true;
                    }
                }
                Ok(None) => {
                    spawn_diag::warm_claim_event(mesh_id, session_id, "none:pool_empty_or_already_claimed");
                    tracing::info!(
                        "spawn_agent_inner: warm pool empty for mesh {}; cold spawn",
                        mesh_id
                    );
                }
                Err(e) => {
                    spawn_diag::warm_claim_event(mesh_id, session_id, &format!("err:{}", e));
                    tracing::warn!(
                        "spawn_agent_inner: warm pool claim failed (non-fatal, falling back to cold): {}",
                        e
                    );
                }
            }
        }
    }

    // The effective spawn_worktree_name + path.
    //
    //  * Manual warm claim (`!is_rename_spawn`): adopt the pool's preassigned
    //    slug as the node's `worktree_name`, so the rest of the pipeline
    //    resolves straight onto the already-on-disk pool directory (#609).
    //  * Issue/PR warm claim (`is_rename_spawn`): keep the node's own
    //    `gh{N}-`/`pr{N}-` `worktree_name`. It resolves to a path that does
    //    NOT exist yet, so we enter the cold-create block below — where the
    //    PR-head fetch runs — and there `git worktree move` the pool directory
    //    onto this target instead of a cold `git worktree add` (#612).
    //  * No claim: fall back to whatever the node row carries (resumes, or a
    //    cold issue/PR spawn).
    //
    // Owned (`Option<String>`, not `Option<&str>`) on purpose: the Issue/PR
    // path mutates `warm_claimed` (take / re-assign) inside the worktree block
    // below, so `spawn_worktree_name` must not hold a borrow into it. The slugs
    // are short, so the clone is negligible.
    let spawn_worktree_name: Option<String> = if let Some(ref entry) = warm_claimed {
        if is_rename_spawn {
            node.worktree_name.clone()
        } else {
            Some(entry.preassigned_name.clone())
        }
    } else if use_worktree {
        node.worktree_name.clone()
    } else {
        tracing::info!("spawn_agent_inner: use_worktree=false, using repo root directly");
        None
    };

    let resolved = env::resolve_agent_path(&node.path, spawn_worktree_name.as_deref());
    tracing::info!(
        "spawn_agent_inner: resolved spawn_path={}, host_path={}, env={:?}",
        resolved.spawn_path,
        resolved.host_path,
        resolved.env_type
    );

    // For a Manual warm claim, the pool's preassigned slug IS the node's
    // `worktree_name` once the spawn completes — the post-spawn DB write
    // (below, before `register_agent`) persists that, but `provision_for_spawn`
    // needs the right branch name in the Spawn Context NOW so the manual
    // `Upgraded` branch's `git checkout -B <branch>` targets the pool's slug
    // rather than the node's stage-1 throwaway. Mutate `node.worktree_name`
    // in place here; `node.clone()` carries the value into the Spawn Context.
    let mut node = node;
    if let (false, Some(ref entry)) = (is_rename_spawn, &warm_claimed) {
        node.worktree_name = Some(entry.preassigned_name.clone());
    }

    // Set true when the spawn-time fetch advances the mesh's base ref, so the
    // single post-spawn pool-maintenance task at the end runs the ref-freshness
    // pass (issue #613 AC3). Carried to the end rather than firing its own
    // thread here so refresh + refill share ONE fill-lock acquisition and can
    // never lose a lock race to each other (issue #613 review).
    let mut ref_advanced_for_pool = false;

    // Auto-sync (issue #213) + PR-head-fetch (#420/#443) + worktree_base_ref
    // resolution only run when the host path doesn't exist yet — for resume /
    // handover / re-spawn the existing worktree's tree IS the agent's starting
    // point and re-syncing would churn refs unnecessarily. Root Nodes
    // (`use_worktree = false`) skip both auto-sync and the PR-head-fetch by
    // virtue of `spawn_worktree_name` being None.
    let host_path_exists = std::path::Path::new(&resolved.host_path).exists();
    let worktree_base_ref = if spawn_worktree_name.is_some() {
        if !host_path_exists {
            // Auto-sync the parent **Mesh** before we cut a new worktree
            // (issue #213). The sync is best-effort: a network failure or
            // a non-fast-forwardable history is surfaced as a `mesh-sync-
            // warning` Tauri event so the frontend can show a non-fatal
            // toast, but spawn always proceeds from the local HEAD.
            // Skips (dirty parent, no remote, already up to date) are
            // silent — the user doesn't need to know about them.
            //
            // The remote is derived from the mesh's `base_ref` (issue
            // #276), so a Mesh with `base_ref = "upstream/main"` syncs
            // against `upstream` rather than hardcoded `origin`. We move
            // `base_ref` into the closure because `spawn_blocking` needs
            // a `'static` closure.
            // Freshness skip (ADR 0020): the background mesh sync in
            // `services::pool_worker` (and any recent spawn / manual Sync)
            // stamps `services::fetch_freshness` on every successful fetch.
            // When the mesh was synced within `SPAWN_FETCH_TTL`, this whole
            // network round-trip is redundant — the remote-tracking ref the
            // worktree is cut from is already current to within minutes —
            // so we skip it and the spawn goes straight to provisioning.
            // `ref_advanced_for_pool` stays false: whichever path recorded
            // the fresh fetch already ran the warm-pool freshness pass.
            // The manual Sync button remains the "I need the latest RIGHT
            // NOW" override — it fetches unconditionally.
            if crate::services::fetch_freshness::spawn_can_skip_fetch(&node.path) {
                tracing::info!(
                    "spawn_agent_inner: skipping auto-sync for session {} — mesh {} was synced {}s ago (< TTL)",
                    session_id,
                    node.path,
                    crate::services::fetch_freshness::time_since_success(&node.path).as_secs()
                );
                timer.checkpoint("fetch_origin_skipped_fresh");
                diag.git_event("fetch_origin", "skip:fresh");
            } else {
                let root = node.path.clone();
                let base_ref_owned = base_ref.to_string();
                timer.checkpoint("before_fetch_origin");
                diag.git_event("fetch_origin", "start");
                // Issue #652 — per-Mesh serialization. Without this lock, N
                // concurrent spawns against the same Mesh race on
                // .git/FETCH_HEAD, .git/index.lock, and refs/heads/<branch>.lock:
                // one git fetch wins, the others fail with "another git process"
                // and the spawn lands on a stale ref. The lock is *blocking*
                // (not try_lock-or-skip), so caller #2 waits for caller #1 to
                // populate the refs and then reuses them (its natural outcome
                // is UpToDate, which is correct).
                //
                // Issue #709 — the wrap is consolidated into
                // `git::sync::locked_fetch_origin` so the lock-acquisition
                // shape is identical to the manual `git_sync`'s
                // `locked_do_sync`, the PR-spawn's `locked_fetch_pr_head`,
                // and the prune's `locked_prune_remote_tracking`. The
                // `tokio::task::spawn_blocking` + `with_mesh_sync_lock`
                // pair used to live inline here.
                let sync_result =
                    crate::git::sync::locked_fetch_origin(root, base_ref_owned).await;
                let fetch_outcome: &'static str = match &sync_result {
                    Ok(crate::git::sync::FetchOutcome::Synced { .. }) => "ok:synced",
                    Ok(crate::git::sync::FetchOutcome::UpToDate) => "ok:up_to_date",
                    Ok(crate::git::sync::FetchOutcome::FetchedButDirty { .. }) => {
                        "ok:fetched_dirty"
                    }
                    Ok(crate::git::sync::FetchOutcome::SkippedNoRemote) => "skip:no_remote",
                    Ok(crate::git::sync::FetchOutcome::FetchedButDiverged { .. }) => "ok:diverged",
                    Err(_) => "err",
                };
                diag.git_event("fetch_origin", fetch_outcome);
                timer.checkpoint("after_fetch_origin");
                // Ref-freshness (issue #613 AC3): if the fetch actually pulled new
                // commits, the mesh's base ref has moved, so any OTHER warm pool
                // entries for this mesh are now parked on a stale SHA and must be
                // `git reset --hard`ed onto the new commit. Only `Synced` /
                // `FetchedButDiverged` advance the ref — `UpToDate` / skipped means
                // nothing moved. We record the fact here and let the single
                // post-spawn maintenance task (at the end of this fn) run the
                // freshness pass, so refresh and refill share one fill-lock
                // acquisition instead of racing on two threads (issue #613 review).
                ref_advanced_for_pool = sync_result
                    .as_ref()
                    .map(|o| o.advanced_ref())
                    .unwrap_or(false);
                emit_sync_outcome_event(app, session_id, &node.path, sync_result);
            } // end freshness-gated fetch_origin block

            // Worktree adoption for PR-spawned nodes (issue #420, extended
            // by #443 for fork PRs). When the node carries a `source_pr`,
            // the head ref stored in `node.branch` is the PR's actual source
            // branch (e.g. `feat/420-pr-spawn`), and the worktree needs to
            // be cut from `<remote>/<head_ref>` so the agent lands on the
            // same commits the PR is built from. Two cases:
            //
            //  - Same-repo PRs (`head_repo_owner` is `None`): the head
            //    lives on `origin` — we call `locked_fetch_pr_head` and
            //    use `origin/<head_ref>` (the #420 path).
            //  - Fork PRs (`head_repo_owner` is `Some`): the head lives on
            //    the fork's clone URL — `locked_fetch_pr_head` calls
            //    `fetch_fork_head`, which registers the fork as a remote
            //    (`fork-<login>`) and fetches from there (issue #443,
            //    follow-up to #36). The worktree base_ref becomes
            //    `fork-<login>/<head_ref>`.
            //
            // The fetch is best-effort: a network failure or stale local ref
            // falls back to the mesh's `base_ref` (the ADR 0001 offline
            // pattern), and the user sees the agent spawn on the wrong
            // commits rather than a hard error — strictly worse than a clean
            // spawn on the right commits, but a strict-error spawn is
            // brittle to the very first offline session.
            //
            // Even so, the fallback MUST surface to the user: the spawn
            // otherwise reports success, the dock closes, and the agent
            // silently lands on the wrong commits. We piggy-back on the
            // existing `mesh-sync-warning` event (the same non-fatal channel
            // the auto-sync path uses) with a `pr_head_unfetchable` or
            // `pr_fork_unfetchable` outcome — the App.tsx listener already
            // renders a toast for that event, so no frontend change is
            // required.
            if node.source_pr.is_some() {
                let head_ref_owned = node.branch.clone();
                let root = node.path.clone();
                let fork_owner_owned = node.head_repo_owner.clone();
                let fork_url_owned = node.head_repo_clone_url.clone();
                timer.checkpoint("before_fetch_pr_head");
                let fetch_ok = tokio::task::spawn_blocking(move || {
                    // Issue #698 — per-Mesh serialization for the PR-spawn
                    // fetch. The match lives inside `locked_fetch_pr_head`
                    // so both branches share one lock acquisition keyed on
                    // `&root` (the mesh's DB-stored path, same key
                    // `fetch_origin` uses two steps above). Without the
                    // lock, two concurrent PR-spawns (or a PR-spawn racing
                    // the manual `git_sync` from #680) collide on
                    // `.git/FETCH_HEAD` / `refs/remotes/<remote>/<ref>.lock`
                    // and the losing spawn silently falls back to `base_ref`.
                    // The fork branch additionally writes `git remote add/
                    // set-url` config that the next caller must observe,
                    // so the lock covers both remote registration and
                    // fetch in one critical section.
                    locked_fetch_pr_head(
                        &root,
                        &head_ref_owned,
                        fork_owner_owned.as_deref(),
                        fork_url_owned.as_deref(),
                    )
                })
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        "spawn_agent_inner: fetch task panicked: {}",
                        e
                    );
                    false
                });
                timer.checkpoint("after_fetch_pr_head");
                if fetch_ok {
                    // Pick the right remote-name prefix for the base_ref
                    // string the worktree will be cut from. Same-repo PRs
                    // use `origin/<head_ref>` (matches the mesh's default
                    // `base_ref`); fork PRs use `fork-<login>/<head_ref>`.
                    // The mesh's `base_ref` is overwritten to use the fork
                    // remote so a future head-branch push is picked up by
                    // the same fetch the auto-sync path runs.
                    let remote_name = match node.head_repo_owner.as_deref() {
                        Some(owner) => fork_remote_alias(owner),
                        None => "origin".to_string(),
                    };
                    let remote_ref = format!("{}/{}", remote_name, node.branch);

                    // Issue #444 — exact-pinning: after a successful fetch,
                    // compare the local SHA at the remote ref we just
                    // populated to the `source_pr_pinned_sha` we stored at
                    // spawn time. On mismatch (PR was force-pushed / rebased
                    // between click-time and spawn-time) emit a non-fatal
                    // `pr_sha_drift` warning via the same `mesh-sync-warning`
                    // channel the offline-fallback path uses. The worktree
                    // proceeds on the new tip — strict-fail would block
                    // legitimate rebase-and-merge workflows for one stale
                    // click. The drift check is a no-op for v15-and-earlier
                    // PR-spawned rows where `source_pr_pinned_sha` is None
                    // (the column was added in v16) and for any empty
                    // GitHub response: read_origin_ref_sha returns None
                    // for a missing ref, and a None expected/actual pair
                    // is treated as "no SHA to compare" and skipped.
                    let root_for_sha = node.path.clone();
                    let head_ref_for_sha = remote_ref.clone();
                    let expected_sha = node.source_pr_pinned_sha.clone();
                    let actual_sha = tokio::task::spawn_blocking(move || {
                        read_origin_ref_sha(&root_for_sha, &head_ref_for_sha)
                    })
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(
                            "spawn_agent_inner: read_origin_ref_sha task panicked: {}",
                            e
                        );
                        None
                    });
                    if let (Some(expected), Some(actual)) = (expected_sha.as_deref(), actual_sha.as_deref()) {
                        if expected != actual {
                            let pr_number = node.source_pr.unwrap_or(-1);
                            let head_ref = node.branch.clone();
                            let message = format!(
                                "PR #{} was force-pushed or rebased after you clicked Spawn                                  (expected {}, now {} on {}). Spawning on the new tip —                                  re-spawn to pin to a fresh SHA.",
                                pr_number, expected, actual, remote_ref,
                            );
                            tracing::warn!(
                                "spawn_agent_inner: {} (node {})",
                                message,
                                session_id,
                            );
                            let _ = app.emit(
                                "mesh-sync-warning",
                                serde_json::json!({
                                    "node_id": session_id,
                                    "mesh_path": node.path,
                                    "outcome": "pr_sha_drift",
                                    "pr_number": pr_number,
                                    "head_ref": head_ref,
                                    "expected_sha": expected,
                                    "actual_sha": actual,
                                    "message": message,
                                }),
                            );
                        }
                    }
                    remote_ref
                } else {
                    let pr_number = node.source_pr.unwrap_or(-1);
                    let head_ref = node.branch.clone();
                    // Distinguish the two failure modes in the toast: a
                    // fork fetch failure is more likely to be permanent
                    // (the user renamed or deleted the fork) than a same-
                    // repo failure (usually transient network).
                    let is_fork = node.head_repo_owner.is_some();
                    let outcome = if is_fork {
                        "pr_fork_unfetchable"
                    } else {
                        "pr_head_unfetchable"
                    };
                    let source_label = if is_fork {
                        let alias = node
                            .head_repo_owner
                            .as_deref()
                            .map(fork_remote_alias)
                            .unwrap_or_else(|| "fork".to_string());
                        format!("the fork remote '{}'", alias)
                    } else {
                        "origin".to_string()
                    };
                    let message = format!(
                        "Could not fetch PR #{} head ref '{}' from {};                          spawning from the mesh's base ref '{}' instead.                          The agent may land on stale commits — re-spawn                          when the network is back to retry.",
                        pr_number, head_ref, source_label, base_ref,
                    );
                    tracing::warn!(
                        "spawn_agent_inner: {} (node {})",
                        message,
                        session_id,
                    );
                    let mut payload = serde_json::json!({
                        "node_id": session_id,
                        "mesh_path": node.path,
                        "outcome": outcome,
                        "pr_number": pr_number,
                        "head_ref": head_ref,
                        "fallback_base_ref": base_ref,
                        "message": message,
                    });
                    if let (Some(owner), Some(url)) = (
                        node.head_repo_owner.as_deref(),
                        node.head_repo_clone_url.as_deref(),
                    ) {
                        payload["head_repo_owner"] = serde_json::Value::String(owner.to_string());
                        payload["head_repo_clone_url"] = serde_json::Value::String(url.to_string());
                    }
                    let _ = app.emit("mesh-sync-warning", payload);
                    base_ref.to_string()
                }
            } else {
                base_ref.to_string()
            }
        } else {
            // Path already exists (resume / handover / re-spawn). No
            // auto-sync, no PR-head-fetch — the existing worktree's tree IS
            // the spawn point.
            base_ref.to_string()
        }
    } else {
        // Root Node (`use_worktree = false`) — no worktree, no base_ref
        // resolution needed; `provision_for_spawn` short-circuits to `Reused`.
        base_ref.to_string()
    };

    // 7. Provision the Worktree Node via `provision_for_spawn` (issue #677).
    //    The 4-way decision (Reused / Adopted / Upgraded / Created) lives
    //    inside `git::worktree::provision` now; this orchestrator just
    //    builds the Spawn Context, awaits the call, and matches the
    //    outcome to drive the post-spawn bookkeeping.
    //
    //    CRITICAL CORRECTNESS:
    //    * `ctx.base_ref` is `worktree_base_ref` (post-fetch for PR/Issue,
    //      the mesh base otherwise). Setting this AFTER the PR-head-fetch
    //      block — not the original `base_ref` — is what makes every PR
    //      spawn land on the freshly fetched PR head rather than going
    //      cold. For Resume / Root Node it's `base_ref` (no fetch ran).
    //    * `warm_claimed.take()` moves the claim into the context; the
    //      post-spawn housekeeping rebuilds it from the outcome's `entry`
    //      field on `Adopted` / `Upgraded`.
    //    * `is_rename_spawn` is preserved unchanged — the post-spawn name
    //      adoption (further below) still reads it.
    //
    //    `warm_claimed.take()` moves the claim into the context — but we
    //    ALSO hold a copy in `warm_entry_for_cleanup` so the error branch
    //    below can find it for the warm-pool cleanup. Without this second
    //    handle, the `Err` branch's cleanup is dead code (the move above
    //    already emptied `warm_claimed`) and the post-spawn `forget_after_spawn`
    //    never runs on a provision failure. Cloning the entry is cheap (4
    //    short strings) and only happens once per spawn — the alternative
    //    of having `provision_for_spawn` return the still-owned entry on
    //    `Err` would expand the API surface for the rare failure case.
    let warm_entry_for_cleanup = warm_claimed.take();
    let provision_ctx = SpawnContext {
        node: node.clone(),
        source: SpawnSource::from_node(&node),
        base_ref: worktree_base_ref.clone(),
        resolved_base_sha: String::new(),
        worktree_mode: worktree_mode.to_string(),
        use_worktree,
        sandbox,
        warm_entry: warm_entry_for_cleanup.clone(),
        spawn_path: resolved.spawn_path.clone(),
        host_path: resolved.host_path.clone(),
    };
    timer.checkpoint("before_provision");
    diag.git_event("provision_for_spawn", "start");
    let provision_result = tokio::task::spawn_blocking(move || provision_for_spawn(provision_ctx))
        .await
        .unwrap_or_else(|e| Err(format!("provision_for_spawn task panicked: {}", e)));
    timer.checkpoint("after_provision");
    let provision_outcome = match provision_result {
        Ok(o) => o,
        Err(e) => {
            // Provision failed. Two cases:
            //
            // (a) We had a warm claim (`warm_entry_for_cleanup` is Some) — the
            //     warm-path helpers inside `provision_for_spawn` left the pool
            //     directory in either the pool-path or target-path state (the
            //     `git worktree move` may have succeeded and the
            //     `git checkout -b` failed, or vice versa). Remove BOTH paths
            //     (idempotent on whichever was untouched) and forget the row,
            //     then FALL THROUGH to a cold create so the spawn still
            //     succeeds on the cold path — preserves pre-refactor behavior
            //     where warm-adopt failures were a graceful degradation rather
            //     than a fatal error (issue #612).
            // (b) No warm claim — `provision_for_spawn` failed on the cold
            //     create itself; surface the error.
            diag.git_event("provision_for_spawn", &format!("err:{}", e));
            tracing::warn!(
                "spawn_agent_inner: provision_for_spawn failed ({}); warm_entry_present={}",
                e,
                warm_entry_for_cleanup.is_some()
            );
            if let Some(entry) = warm_entry_for_cleanup.as_ref() {
                let pool_path = entry.path.clone();
                let target_path = resolved.host_path.clone();
                let row_id = entry.id;
                let _ = tokio::task::spawn_blocking(move || {
                    let _ = crate::git::worktree::remove_one_worktree(&target_path);
                    let _ = crate::git::worktree::remove_one_worktree(&pool_path);
                })
                .await;
                crate::services::warm_pool::forget_after_spawn(row_id);
                // Retry as cold create. The SpawnContext is rebuilt with
                // `warm_entry: None` so `provision_for_spawn` takes the
                // `Created` branch on the no-warm-claim / path-doesn't-exist
                // path. `pool_was_drained_by_this_spawn` is still true from
                // the original `try_claim` so the post-spawn refill below
                // restores the pool inventory after this spawn completes.
                timer.checkpoint("before_cold_fallback");
                let cold_ctx = SpawnContext {
                    node: node.clone(),
                    source: SpawnSource::from_node(&node),
                    base_ref: worktree_base_ref.clone(),
                    resolved_base_sha: String::new(),
                    worktree_mode: worktree_mode.to_string(),
                    use_worktree,
                    sandbox,
                    warm_entry: None,
                    spawn_path: resolved.spawn_path.clone(),
                    host_path: resolved.host_path.clone(),
                };
                let cold_result =
                    tokio::task::spawn_blocking(move || provision_for_spawn(cold_ctx))
                        .await
                        .unwrap_or_else(|e| {
                            Err(format!("cold-create fallback task panicked: {}", e))
                        });
                timer.checkpoint("after_cold_fallback");
                match cold_result {
                    Ok(o) => {
                        diag.git_event("provision_for_spawn", "ok_cold_fallback");
                        o
                    }
                    Err(cold_e) => {
                        diag.git_event(
                            "provision_for_spawn",
                            &format!("err_cold_fallback:{}", cold_e),
                        );
                        let msg = format!(
                            "spawn_agent_inner: provision_for_spawn failed AND cold fallback failed: \
                             warm={}, cold={}",
                            e, cold_e
                        );
                        tracing::error!("{}", msg);
                        return Err(msg);
                    }
                }
            } else {
                let msg = format!("spawn_agent_inner: provision_for_spawn: {}", e);
                tracing::error!("{}", msg);
                return Err(msg);
            }
        }
    };
    let wt_outcome: String = match &provision_outcome {
        ProvisionOutcome::Reused { .. } => "reused".to_string(),
        ProvisionOutcome::Adopted { .. } => "adopted".to_string(),
        ProvisionOutcome::Upgraded { .. } => "upgraded".to_string(),
        ProvisionOutcome::Created { .. } => "created".to_string(),
    };
    diag.git_event("provision_for_spawn", &wt_outcome);

    // Rebuild `warm_claimed` from the outcome so the post-spawn bookkeeping
    // (`forget_after_spawn` + name adoption) sees the entry on
    // Adopted/Upgraded and None on Reused/Created. The Manual `Upgraded`
    // variant carries the entry whose `preassigned_name` overwrites the
    // node's stage-1 throwaway slug (the DB write below persists this).
    if let ProvisionOutcome::Adopted { entry, .. } | ProvisionOutcome::Upgraded { entry, .. } =
        &provision_outcome
    {
        warm_claimed = Some(entry.clone());
    }

    // Fix WSL/Windows path mismatches in the worktree's .git file — without
    // this, agent commands run inside the worktree see a broken gitlink on
    // Windows-side shells. Best-effort: a failure is logged, never fatal.
    if let Err(e) = crate::git::worktree::sanitize_git_worktree(&resolved.host_path, resolved.env_type) {
        tracing::warn!("spawn_agent_inner: failed to sanitize worktree .git file: {}", e);
    }

    // 8-9. Build the command, then spawn it — either normally (portable-pty)
    //       or, when the mesh opts in on Windows, inside an AppContainer sandbox
    //       (issue #498). The sandbox path owns its ConPTY spawn but returns the
    //       same `Child`/`MasterPty` trait objects, so everything downstream
    //       (Job Object containment, reader thread, resize, kill) is identical.
    // The node's stored `provider` is the harness-profile id; resolve its paired
    // model-provider account into the `ANTHROPIC_*` backend env (issue #538). A
    // built-in/absent account yields an empty list → vanilla claude on the
    // Anthropic subscription.
    let backend_env = crate::preferences::resolve_provider_env(&node.provider);
    // Custom-endpoint tiers preflight — refuses to spawn if a Claude-compatible
    // third-party (OpenRouter, Generic) is configured without a primary model,
    // which would otherwise silently 400 in the spawned `claude` (the binary
    // sends its hardcoded `claude-*` default; OpenRouter wants `provider/model`).
    crate::preferences::preflight_resolve_provider_env(&node.provider)
        .map_err(|e| format!("spawn preflight failed: {}", e))?;
    let cmd = build_spawn_command(
        &resolved,
        provider,
        &backend_env,
        &session_id_mode,
        session_id,
        model_override,
        effort_override,
        prefill.as_deref(),
        sandbox,
    );

    let emit_provider_error = |e: &String| {
        let _ = app.emit(
            "provider-error",
            serde_json::json!({
                "session_id": session_id,
                "provider": provider,
                "message": e
            }),
        );
    };

    let (child, master): (
        Box<dyn portable_pty::Child + Send + Sync>,
        Box<dyn portable_pty::MasterPty + Send>,
    ) = if crate::sandbox::sandbox_enabled(sandbox) {
        tracing::info!("spawn_agent_inner: spawning session {} inside AppContainer sandbox", session_id);
        sandbox_spawn(&cmd, session_id, &resolved.host_path, rows, cols)
            .inspect_err(|e| emit_provider_error(e))?
    } else {
        let pair = open_pty_pair(rows, cols)?;
        let child = spawn_child(&pair, cmd).inspect_err(|e| emit_provider_error(e))?;
        (child, pair.master)
    };

    tracing::info!("spawn_agent_inner: process spawned successfully");
    timer.checkpoint("after_pty_spawn");

    // Contain the whole process tree in a Job Object straight away, before the
    // shell launches the agent CLI — so any process the agent later detaches
    // (e.g. a dev server it backgrounds) is still killed on close, even when its
    // parent has exited and `taskkill /T` could no longer reach it.
    let job = child.process_id().and_then(crate::process_util::JobHandle::contain);
    if job.is_none() {
        tracing::warn!(
            "spawn_agent_inner: could not contain session {} in a Job Object; \
             close will fall back to taskkill (detached children may survive)",
            session_id
        );
    }

    // 10. Setup IO
    let reader = master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = master.take_writer().map_err(|e| e.to_string())?;
    let reader_alive = Arc::new(AtomicBool::new(true));

    // 11. Inject attention hook
    crate::agent::workspace_trust::ensure_trusted(&resolved);
    timer.checkpoint("after_workspace_trust");
    if adapter.requires_attention_hook() {
        inject_attention_hook(std::path::Path::new(&resolved.host_path));
    }
    timer.checkpoint("after_inject_hook");

    // 12. Register BEFORE starting the reader thread. The pre-#300 order
    //     (register-then-start) is the one that closes the TOCTOU window
    //     in `is_agent_already_running`: a concurrent spawn for the
    //     same session_id sees the entry and bails. The `reader_handle`
    //     is stashed via a setter after the thread is spawned — the
    //     tiny window between insert and setter is benign (kill_session
    //     arriving then sees `reader_handle = None` and skips the join,
    //     matching the natural-exit test path).
    tracing::info!("spawn_agent_inner: storing agent process for session {}", session_id);
    // Persist the adopted pool slug as the node's `worktree_name` and `name`
    // BEFORE registering the agent (issue #609). The PTY reader thread and
    // any concurrent `agent-spawned` listener fire right after registration,
    // so doing the UPDATE post-register would let them briefly observe the
    // throwaway stage-1 slug instead of the adopted pool slug.
    //
    // Manual claims only: an Issue/PR claim (`is_rename_spawn`) kept its own
    // `gh{N}-`/`pr{N}-` `worktree_name` — the pool directory was moved to
    // match it — so there is nothing to overwrite (#612).
    if let (false, Some(ref entry)) = (is_rename_spawn, &warm_claimed) {
        if let Err(e) = db::set_agent_node_worktree_name(session_id, &entry.preassigned_name) {
            tracing::warn!(
                "spawn_agent_inner: failed to set worktree_name={} on node {}: {}",
                entry.preassigned_name,
                session_id,
                e
            );
        } else {
            diag.db_write("worktree_name", &entry.preassigned_name);
        }
    }
    diag.checkpoint("before_register_agent");
    register_agent(session_id, child, writer, master, reader_alive.clone(), job, timer.start(), mesh_id);
    tracing::info!("spawn_agent_inner: stored agent process");
    diag.checkpoint("after_register_agent");

    // 13. Start reader thread
    let spawned_at = std::time::Instant::now();
    // `spawn_start` is the original SpawnTimer reference, used by the
    // reader-thread `first_pty_output` checkpoint log for timeline
    // alignment with every other `spawn_timing:` line. Distinct from
    // `spawned_at` (process-creation time) which the early-exit
    // heuristic needs — see `start_reader` doc comment.
    let spawn_start = timer.start();
    tracing::debug!("spawn_agent_inner: starting reader thread for session {}", session_id);
    crate::http_server::ensure_pty_channel(session_id);
    // Issue #651: derive the reader-capture gate from `session_id_mode`
    // (the orchestrator's authoritative decision) rather than from
    // `adapter.self_assigns_session_id() && node.cli_session_id.is_none()`
    // (a derived condition that could drift if a future adapter violates the
    // "Assign => !self_assigns" invariant). The two writes — orchestrator
    // pre-write at step 4 and reader capture at `start_reader` — are
    // unsynchronised; only one path must own the column for any given spawn.
    let needs_session_capture = reader_should_capture_session_id(
        &session_id_mode,
        adapter.self_assigns_session_id(),
    );
    let reader_handle = start_reader(
        app.clone(),
        session_id,
        needs_session_capture,
        reader,
        spawned_at,
        reader_alive,
        adapter.is_plain_terminal(),
        spawn_start,
        mesh_id,
    );

    // 13b. Start natural-exit watcher (issue #287). On Windows ConPTY
    //      10.0.28120 the master read pipe no longer EOFs on child
    //      exit, so the reader thread stays blocked in `read()` until
    //      the pseudoconsole itself is closed. This poller drops the
    //      master within ~500ms of the child exiting, EOFing the
    //      reader, which then sets `reader_alive = false` and flips
    //      the node status to `Idle`. The watcher uses `try_wait` +
    //      `try_lock` on the child so it never blocks kill_session
    //      (which also locks that mutex).
    if let Some(entry) = PROCESS_REGISTRY.get(&session_id) {
        crate::agent::process::watch_child_exit(entry.child.clone(), entry.master.clone());
    }

    // 14. Stash the JoinHandle on the registered entry. `kill_session`
    //     reads it under a Mutex so the concurrent kill_session path
    //     is race-free (see `process.rs::kill_session`).
    if let Some(entry) = PROCESS_REGISTRY.get(&session_id) {
        entry.set_reader_handle(reader_handle);
    }

    tracing::info!("spawn_agent_inner: reader thread spawned, updating node status");
    // Issue #654 — close the post-spawn status + early-exit race. The
    // `NOT IN (Error, Archived)` guard is the symmetric race fix: prevents
    // the orchestrator from resurrecting a reader-written Error back to
    // Spawning (which would let the delayed promotion later write Running
    // onto a dead node — same ghost-Running bug, other direction).
    diag.db_write("status", "Spawning");
    db::update_agent_node_status_unless_in(
        session_id,
        SessionStatus::Spawning,
        &[SessionStatus::Error, SessionStatus::Archived],
    )
    .map_err(|e| e.to_string())?;
    std::thread::spawn(move || {
        // Promote to Running iff the reader hasn't already written Error.
        // Both delay and reader check must share `EARLY_EXIT_WINDOW`.
        std::thread::sleep(EARLY_EXIT_WINDOW);
        match db::update_agent_node_status_if(
            session_id,
            SessionStatus::Running,
            SessionStatus::Spawning,
        ) {
            Ok(true) => {
                crate::agent::spawn_diag::promotion_event(session_id, "running");
            }
            Ok(false) => {
                crate::agent::spawn_diag::promotion_event(session_id, "noop");
            }
            Err(e) => {
                // Log under `phase=promotion` so a single grep reconstructs
                // the four-way race outcome; the old free-form warn swallowed
                // this signal.
                crate::agent::spawn_diag::promotion_event(session_id, "err");
                tracing::warn!(
                    "spawn_agent_inner: conditional Running promotion failed for session {}: {}",
                    session_id,
                    e
                );
            }
        }
    });

    // Warm-pool post-claim housekeeping (issue #609). After the spawn
    // succeeds we (a) drop the bookkeeping row — the directory now lives
    // on as the node's worktree so the row's purpose is done — and (b)
    // fire a background refill so the pool is back at target before the
    // next spawn. Both are best-effort: a failed delete leaves an orphan
    // `claimed` row that the next startup reconcile prunes (its `path`
    // exists, but the `claimed` status means no future claim will pick
    // it up — it just sits there as harmless bookkeeping until a future
    // issue adds GC for it); a failed refill is logged and retried on the
    // next reconcile pass.
    //
    // Issue #653: `warm_claimed` is None when the use-site recheck fired
    // (the spawn fell back to cold), but the pool inventory is still
    // drained — `pool_was_drained_by_this_spawn` captures that case for
    // the post-spawn refill trigger. `warm_claimed.is_some()` would
    // miss the recheck-fired case and leave the pool at target-1.
    if let Some(entry) = warm_claimed.take() {
        crate::services::warm_pool::forget_after_spawn(entry.id);
        // Full name adoption — MANUAL spawns only (issue #609, PRD #608 §3
        // "Manual Spawns"). The node takes the warm entry's slug as BOTH its
        // `worktree_name` (so path resolution lands on the pre-warmed
        // directory) AND its display `name` (so the on-disk directory and the
        // node name match with zero rename). At stage-1 the node was created
        // under a throwaway slug; here we overwrite both with the adopted slug.
        // The slug is a plain `on_spawn` adj-adj-noun, so it's still a
        // `is_default_name` match — the auto-LLM renamer can override it later
        // exactly as it would the throwaway slug. The `worktree_name` UPDATE
        // was already done above (before `register_agent`) so the reader thread
        // sees the adopted value; only `name` needs an event here.
        //
        // Issue/PR claims (`is_rename_spawn`) keep their own `gh{N}-`/`pr{N}-`
        // name — the pool DIRECTORY was renamed to match the node, not the
        // reverse — so they skip the name adoption entirely (#612).
        if !is_rename_spawn {
            if let Err(e) = db::update_agent_node_name(session_id, &entry.preassigned_name) {
                tracing::warn!(
                    "spawn_agent_inner: failed to adopt name={} on node {}: {}",
                    entry.preassigned_name,
                    session_id,
                    e
                );
            } else {
                // Re-label the frontend's optimistic stage-1 row (created under
                // the throwaway slug) via the same `node-renamed` channel the
                // manual-rename command uses (agentNodeStore listens for it).
                let _ = app.emit(
                    "node-renamed",
                    serde_json::json!({
                        "node_id": session_id,
                        "name": entry.preassigned_name,
                    }),
                );
            }
        }
    }

    // Single post-spawn pool-maintenance task (issue #613). Runs on its own
    // thread (it shells out to `git` and re-locks the DB) so it never delays
    // the spawn caller, and does BOTH jobs under ONE fill-lock acquisition:
    //   * ref-freshness — `git reset --hard` stale warm entries onto the new
    //     base SHA, when the spawn-time fetch advanced the ref;
    //   * refill — top the pool back up to target after this spawn claimed an
    //     entry.
    // Combining them means refresh and refill can never lose a fill-lock race
    // to each other (the previous split into two threads dropped whichever
    // lost, with no in-session retry — issue #613 review). Fired whenever
    // either job has work to do.
    if mesh_id > 0 && (ref_advanced_for_pool || pool_was_drained_by_this_spawn) {
        let mesh_id_for_pool = mesh_id;
        let do_refresh = ref_advanced_for_pool;
        // Issue #653: `pool_was_drained_by_this_spawn` covers the
        // recheck-fired case where `did_claim_warm` (gated on
        // `warm_claimed.is_some()`) is false but the pool still needs a
        // refill. Use the drained flag for the refill trigger, keep
        // `did_claim_warm` for the name-adoption bookkeeping above (those
        // are distinct concerns: refill restores inventory; name adoption
        // only makes sense when the spawn actually adopted the entry).
        let do_refill = pool_was_drained_by_this_spawn;
        let app_for_pool = app.clone();
        std::thread::spawn(move || {
            crate::services::warm_pool::post_spawn_maintenance(
                mesh_id_for_pool,
                do_refresh,
                do_refill,
                &app_for_pool,
            );
        });
    }

    // Emit the post-spawn reconcile trigger (issue #332). Async-spawn paths
    // (auto-resume on startup, fresh auto-spawn, handover, etc.) race the
    // frontend's attach-fit: term.onResize fires `resize_agent(real cols)`
    // before the agent process exists, so the IPC returns "Agent not
    // running" and is silently swallowed. The PTY was created at the
    // caller-supplied `rows`/`cols` (80x24 for auto_resume_sessions), and
    // because term.cols is already the fitted value no further onResize
    // fires — the PTY stays at the spawn-time size and the agent wraps
    // its first lines of output inside a wider pane. By emitting here
    // (after the agent is registered AND the DB status flips to
    // `Spawning` — the transient state between process launch and the
    // conditional `Spawning → Running` promotion 3s later; issue #654),
    // we give the frontend a definitive "agent is up, push the real
    // size now" signal that closes the race uniformly for all three
    // paths. Frontend consumer: TerminalRegistry listens and calls
    // syncPtySize, which is self-guarding (no-op on detached/missing
    // terminals) and swallows the "Agent not running" rejection.
    let _ = app.emit(
        "agent-spawned",
        serde_json::json!({
            "session_id": session_id,
            "rows": rows,
            "cols": cols,
        }),
    );

    tracing::info!("spawn_agent_inner: complete");
    timer.total();
    Ok(())
}

/// Map an `crate::git::sync::fetch_origin` outcome to either a silent `tracing` log
/// or a `mesh-sync-warning` Tauri event. The frontend's `App.tsx`
/// listens for the event and shows a non-fatal warning toast.
///
/// Per issue #213:
/// - `FetchedButDirty`, `SkippedNoRemote`, `UpToDate`, `Synced` are silent.
/// - `FetchedButDiverged`, `FetchFailed`, `RepoUnusable` emit a
///   warning so the user knows the spawn fell back to local HEAD.
///
/// Spawn proceeds either way; the event is purely informational.
fn emit_sync_outcome_event(
    app: &tauri::AppHandle,
    session_id: i64,
    mesh_path: &str,
    outcome: Result<crate::git::sync::FetchOutcome, crate::git::sync::FetchError>,
) {
    let (event_name, payload) = match outcome {
        Ok(crate::git::sync::FetchOutcome::FetchedButDirty { new_commits }) => {
            // Silent, like Synced/UpToDate: the fetch reached the remote and
            // advanced the tracking refs the worktree is cut from — the new
            // node IS fresh. Only the parent checkout's fast-forward was
            // skipped, and the user already knows their own tree is dirty.
            tracing::info!(
                "spawn_agent_inner: auto-sync fetched {} commit(s) but skipped the pull \
                 (parent dirty) for session {}",
                new_commits,
                session_id
            );
            return;
        }
        Ok(crate::git::sync::FetchOutcome::SkippedNoRemote) => {
            tracing::info!(
                "spawn_agent_inner: auto-sync skipped (no origin) for session {}",
                session_id
            );
            return;
        }
        Ok(crate::git::sync::FetchOutcome::UpToDate) => {
            tracing::info!(
                "spawn_agent_inner: auto-sync up-to-date for session {}",
                session_id
            );
            return;
        }
        Ok(crate::git::sync::FetchOutcome::Synced { new_commits }) => {
            tracing::info!(
                "spawn_agent_inner: auto-sync pulled {} commit(s) for session {}",
                new_commits,
                session_id
            );
            return;
        }
        Ok(crate::git::sync::FetchOutcome::FetchedButDiverged { new_commits, reason }) => {
            // Diverged is informational, not an error — the fetch
            // succeeded, the new commits are visible locally, we just
            // can't auto-apply them without a real merge. The user
            // should know so they can decide whether to `git pull`
            // themselves or rebase.
            let message = format!(
                "Fetched {} new commit(s) from origin, but local history has diverged ({}). Spawning from local HEAD — pull manually to sync.",
                new_commits, reason
            );
            tracing::warn!("spawn_agent_inner: {}", message);
            (
                "mesh-sync-warning",
                serde_json::json!({
                    "node_id": session_id,
                    "mesh_path": mesh_path,
                    "outcome": "diverged",
                    "new_commits": new_commits,
                    "message": message,
                }),
            )
        }
        Err(crate::git::sync::FetchError::RepoUnusable(reason)) => {
            let message = format!(
                "Couldn't auto-sync the mesh — repository is unusable: {}. Spawning from local HEAD instead.",
                reason
            );
            tracing::warn!("spawn_agent_inner: {}", message);
            (
                "mesh-sync-warning",
                serde_json::json!({
                    "node_id": session_id,
                    "mesh_path": mesh_path,
                    "outcome": "repo_unusable",
                    "message": message,
                }),
            )
        }
        Err(crate::git::sync::FetchError::FetchFailed(reason)) => {
            // The most common case: network down. We don't try to
            // distinguish "no network" from "auth failure" — both look
            // the same to `git fetch`. The user knows whether they
            // have connectivity; we just tell them we couldn't sync.
            let message = if reason.is_empty() {
                "Couldn't auto-sync the mesh (fetch failed). Spawning from local HEAD instead.".to_string()
            } else {
                format!(
                    "Couldn't auto-sync the mesh ({}). Spawning from local HEAD instead.",
                    reason
                )
            };
            tracing::warn!("spawn_agent_inner: {}", message);
            (
                "mesh-sync-warning",
                serde_json::json!({
                    "node_id": session_id,
                    "mesh_path": mesh_path,
                    "outcome": "fetch_failed",
                    "message": message,
                }),
            )
        }
    };
    let _ = app.emit(event_name, payload);
}

// The worktree-provision helpers — `fetch_single_ref`, `locked_fetch_pr_head`,
// `fork_remote_alias`, `fetch_fork_head`, `read_origin_ref_sha`,
// `upgrade_warm_to_mode`, `adopt_warm_worktree_by_move`,
// `checkout_worktree_to_base`, `run_git_checkout` — live in
// `crate::git::worktree::provision` (ADR 0007 consolidation, issue #677, plus
// #698's `locked_fetch_pr_head` wrapper). The spawn path reaches them through
// the module-level `use` at the top of this file; the call sites inside
// `spawn_agent_inner` use them transparently.

#[cfg(test)]
mod tests {
    use super::*;
    // The eight worktree-provision helpers were moved to
    // `crate::git::worktree::provision` in PR #676 / issue #677, and #698
    // added `locked_fetch_pr_head` on top. The tests here exercise them by
    // name, so re-import at the test-module scope.
    use crate::git::worktree::provision::{
        adopt_warm_worktree_by_move, fetch_fork_head, fetch_single_ref, fork_remote_alias,
        locked_fetch_pr_head, read_origin_ref_sha, upgrade_warm_to_mode,
    };
    use tempfile::TempDir;

    /// Pin the spawn-time fallback. Sole pin of `DEFAULT_WORKTREE_MODE`
    /// after #411 deleted the TS-side sentinel (it had no real consumer).
    #[test]
    fn default_worktree_mode_is_branched() {
        assert_eq!(DEFAULT_WORKTREE_MODE, "branched");
    }

    // -----------------------------------------------------------------------
    // Warm-pool manual claim — .worktreeinclude re-application (issue #639
    // gap 1). The cold `create_git_worktree` and the Issue/PR `adopt…by_move`
    // both call `apply_worktree_include` so an adopted worktree is byte-for-
    // byte equivalent to a cold spawn. The manual warm-claim fast path
    // (upgrade_warm_to_mode) MUST do the same — otherwise a user who edits a
    // `.worktreeinclude`-referenced file (typical: `.env`, build cache) between
    // prewarm time and spawn time lands on a stale copy.
    // -----------------------------------------------------------------------

    #[test]
    fn upgrade_warm_to_mode_reapplies_worktreeinclude_after_checkout() {
        use std::fs;
        let (_td, root, pool) = setup_warm_pool_with_include();

        // User edits the source file BETWEEN prewarm and manual spawn —
        // exactly the window the missing apply_worktree_include used to leak.
        fs::write(root.join("secrets.env"), "v1=NEW\n").unwrap();

        // The manual warm claim's mode upgrade — must re-copy `.worktreeinclude`
        // sources so the agent's worktree matches the live repo state, not the
        // stale prewarm snapshot.
        upgrade_warm_to_mode(root.to_str().unwrap(), pool.to_str().unwrap(), "bold-amber-fox", "branched")
            .expect("upgrade_warm_to_mode must succeed");

        // The worktree's `.worktreeinclude`-referenced file must now reflect
        // the live repo content (NEW), not the prewarm-time snapshot (old).
        assert_eq!(
            fs::read_to_string(pool.join("secrets.env")).unwrap(),
            "v1=NEW\n",
            "manual warm claim must re-apply .worktreeinclude so the agent sees the live source"
        );
    }

    /// No `.worktreeinclude` at the repo root → the upgrade is still a no-op
    /// rather than an error. Prevents a regression where adding the include
    /// re-application broke a repo that never used the feature.
    #[test]
    fn upgrade_warm_to_mode_is_noop_when_no_worktreeinclude() {
        use crate::env::test_helpers::init_repo_with_commit;
        use std::fs;
        // Skip the .worktreeinclude side of the helper — bare repo + pool.
        let td = TempDir::new().unwrap();
        let root = td.path();
        let _ = init_repo_with_commit(root, &[("f.txt", "tracked\n")]);
        let pool = root.join(".claude").join("worktrees").join("warm-amber-fox");
        crate::git::worktree::create_git_worktree(
            root.to_str().unwrap(),
            pool.to_str().unwrap(),
            "warm-amber-fox",
            "detached",
            "HEAD",
        )
        .unwrap();
        let _ = td; // keep alive for the duration of the test

        upgrade_warm_to_mode(root.to_str().unwrap(), pool.to_str().unwrap(), "bold-amber-fox", "branched")
            .expect("must succeed when no .worktreeinclude exists");
        // No spurious `.worktreeinclude` was created in the worktree.
        assert!(
            !pool.join(".worktreeinclude").exists(),
            "absent manifest must not be materialised by the upgrade"
        );
        // The tracked file round-trips.
        assert_eq!(fs::read_to_string(pool.join("f.txt")).unwrap(), "tracked\n");
    }

    /// Detached mode must also re-apply `.worktreeinclude` (issue #639 gap 1,
    /// review finding). The original `upgrade_warm_to_mode` returned early on
    /// `mode == "detached"` and skipped the include copy — a regression that
    /// re-instated that early-return would pass `…_reapplies…_after_checkout`
    /// (branched) but leave a detached-mode spawn on the stale prewarm
    /// snapshot, defeating the gap-1 fix for half the meshes.
    #[test]
    fn upgrade_warm_to_mode_reapplies_worktreeinclude_in_detached_mode() {
        use std::fs;
        let (_td, root, pool) = setup_warm_pool_with_include();

        // User edits the source — same window as the branched-mode test.
        fs::write(root.join("secrets.env"), "v1=NEW\n").unwrap();

        // Upgrade in DETACHED mode. The branch name is unused (no checkout),
        // but we pass the preassigned slug for consistency with the call site.
        upgrade_warm_to_mode(
            root.to_str().unwrap(),
            pool.to_str().unwrap(),
            "warm-amber-fox",
            "detached",
        )
        .expect("upgrade_warm_to_mode must succeed in detached mode");

        assert_eq!(
            fs::read_to_string(pool.join("secrets.env")).unwrap(),
            "v1=NEW\n",
            "manual warm claim in detached mode must also re-apply .worktreeinclude"
        );
        // And the worktree stayed detached — no branch was created.
        let wt = git2::Repository::open(&pool).unwrap();
        assert!(
            wt.head_detached().unwrap_or(false),
            "detached mode must leave the worktree detached"
        );
    }

    /// Shared setup for the two `upgrade_warm_to_mode` `.worktreeinclude`
    /// re-application tests (#642.5). The third test
    /// (`…_is_noop_when_no_worktreeinclude`) deliberately inlines its own
    /// setup because the no-manifest case is the whole point of that test
    /// — running it through the helper would materialise `secrets.env` and
    /// `.worktreeinclude` in the worktree, defeating the no-op assertion.
    ///
    /// The helper stands up: a tempdir holding a real git repo with
    /// `secrets.env` + `.worktreeinclude` (both tracked), AND a pool-shaped
    /// DETACHED worktree under `.claude/worktrees/warm-amber-fox` that has
    /// already had the include copied at prewarm time (so the tests assert
    /// the upgrade re-applies, not the original copy). Both the branched and
    /// the detached call-site tests cut the pool as detached (the pool's
    /// on-disk shape) — the difference between them is the
    /// `upgrade_warm_to_mode` mode argument, not the helper's setup.
    ///
    /// Returns `(tempdir, repo_root_path, pool_path)`. The tempdir is held
    /// to keep the underlying directory alive for the duration of the test
    /// — dropping it would delete the repo and break subsequent asserts.
    fn setup_warm_pool_with_include() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
        use crate::env::test_helpers::{commit_file, init_repo_with_commit};
        use std::fs;

        let td = TempDir::new().unwrap();
        let root = td.path().to_path_buf();

        init_repo_with_commit(&root, &[("f.txt", "tracked\n")]);
        fs::write(root.join("secrets.env"), "v1=old\n").unwrap();
        fs::write(root.join(".worktreeinclude"), "secrets.env\n").unwrap();
        // Commit the manifest so `.worktreeinclude` is reachable for `git
        // worktree add`; the pool helper copies files relative to the repo
        // root regardless of whether the manifest itself is tracked, but
        // committing keeps the test setup close to a realistic repo.
        let repo = git2::Repository::open(&root).unwrap();
        commit_file(&repo, &root, ".worktreeinclude", "secrets.env\n");

        let pool = root.join(".claude").join("worktrees").join("warm-amber-fox");
        crate::git::worktree::create_git_worktree(
            root.to_str().unwrap(),
            pool.to_str().unwrap(),
            "warm-amber-fox",
            "detached",
            "HEAD",
        )
        .expect("prewarm-shape worktree must be creatable for this helper");
        assert_eq!(
            fs::read_to_string(pool.join("secrets.env")).unwrap(),
            "v1=old\n",
            "prewarm-time copy must reflect the original source"
        );
        (td, root, pool)
    }

    // -----------------------------------------------------------------------
    // Warm-pool Issue/PR adoption (issue #612): move a detached pool worktree
    // onto the node's target name and check it out to the resolved base SHA on
    // its own branch. These pin the code-review fixes for two confirmed bugs:
    // resolving `base_ref` → SHA (offline resilience), and using `-b` (NOT
    // `-B`) so a re-spawn can never force-reset a branch carrying prior work.
    // -----------------------------------------------------------------------

    #[test]
    fn adopt_warm_worktree_moves_and_branches_at_base_sha() {
        use crate::env::test_helpers::init_repo_with_commit;
        let td = TempDir::new().unwrap();
        let root = td.path();
        let repo = init_repo_with_commit(root, &[("f.txt", "a\n")]);
        let head = repo.head().unwrap().peel_to_commit().unwrap().id().to_string();

        // The pool's on-disk shape: a DETACHED worktree under a plain slug.
        let pool = root.join(".claude").join("worktrees").join("warm-amber-fox");
        crate::git::worktree::create_git_worktree(
            root.to_str().unwrap(),
            pool.to_str().unwrap(),
            "warm-amber-fox",
            "detached",
            "HEAD",
        )
        .unwrap();

        let target = root.join(".claude").join("worktrees").join("gh123-fix");
        adopt_warm_worktree_by_move(
            root.to_str().unwrap(),
            pool.to_str().unwrap(),
            target.to_str().unwrap(),
            "gh123-fix",
            "branched",
            "HEAD",
        )
        .expect("adoption must succeed");

        assert!(!pool.exists(), "pool directory must be gone after the move");
        assert!(target.exists(), "target directory must exist after the move");
        let wt = git2::Repository::open(&target).unwrap();
        assert_eq!(
            wt.head().unwrap().shorthand().unwrap(),
            "gh123-fix",
            "the adopted worktree must be on the node's own branch"
        );
        assert_eq!(
            wt.head().unwrap().peel_to_commit().unwrap().id().to_string(),
            head,
            "the branch must sit at the resolved base SHA"
        );
    }

    #[test]
    fn adopt_warm_worktree_refuses_to_clobber_an_existing_branch() {
        use crate::env::test_helpers::init_repo_with_commit;
        let td = TempDir::new().unwrap();
        let root = td.path();
        let repo = init_repo_with_commit(root, &[("f.txt", "a\n")]);
        // A pre-existing deterministic branch standing in for a prior spawn's
        // work. Force-resetting it (the old `-B` bug) would orphan its commits.
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("gh7-x", &head_commit, false).unwrap();

        let pool = root.join(".claude").join("worktrees").join("warm-amber-fox");
        crate::git::worktree::create_git_worktree(
            root.to_str().unwrap(),
            pool.to_str().unwrap(),
            "warm-amber-fox",
            "detached",
            "HEAD",
        )
        .unwrap();

        let target = root.join(".claude").join("worktrees").join("gh7-x");
        let err = adopt_warm_worktree_by_move(
            root.to_str().unwrap(),
            pool.to_str().unwrap(),
            target.to_str().unwrap(),
            "gh7-x",
            "branched",
            "HEAD",
        )
        .expect_err("adoption must refuse to overwrite an existing branch");
        assert!(
            err.contains("git checkout"),
            "the failure must come from the refusing `-b` checkout, got: {}",
            err
        );
    }

    // -----------------------------------------------------------------------
    // base_ref resolution (master-trunk regression)
    //
    // Pre-fix, the spawn path hardcoded `"origin/main"` as the default
    // `base_ref` when the `meshes.base_ref` DB column was `'origin/main'`
    // (its COALESCE default) — meaning a master-trunk repo always hit
    // `mesh-sync-warning` on every spawn (`fatal: couldn't find remote
    // ref main`). These tests pin the resolution chain:
    //
    //   1. meshes.base_ref (BUT NOT the COALESCE default — that's
    //      treated as "no config" so the detection chain runs)
    //   2. refs/remotes/origin/HEAD read from the local repo
    //   3. "origin/main" last resort
    //
    // The COALESCE-sentinel treatment is critical: the DB column is
    // NOT NULL with default `'origin/main'`, so `Mesh.base_ref` is
    // ALWAYS a non-empty `String` and `MeshRow.base_ref` is ALWAYS
    // `Some(_)` — a naive `if let Some(b) = config_base_ref { return b }`
    // would make the detection chain dead code in production. The
    // `resolve_base_ref_treats_coalesce_sentinel_as_unset` test pins the
    // production call path (`Some("origin/main")`).
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_base_ref_uses_config_value_when_set() {
        // The config wins even on a non-repo / non-master path — explicit
        // user intent overrides any auto-detection. Empty / whitespace
        // config falls through to the detection chain (regression guard
        // for an empty-string value slipping through the COALESCE).
        let tmp = TempDir::new().unwrap();
        assert_eq!(
            resolve_base_ref_for_spawn(tmp.path().to_str().unwrap(), Some("origin/develop")),
            "origin/develop"
        );
        // Empty / whitespace strings are treated as "no config" so the
        // detection chain runs — mirrors the COALESCE-to-default contract
        // in the DB layer.
        assert_eq!(
            resolve_base_ref_for_spawn(tmp.path().to_str().unwrap(), Some("")),
            "origin/main",
            "empty config base_ref must fall through to detection, not propagate"
        );
        assert_eq!(
            resolve_base_ref_for_spawn(tmp.path().to_str().unwrap(), Some("   ")),
            "origin/main",
            "whitespace-only config base_ref must fall through to detection"
        );
    }

    #[test]
    fn resolve_base_ref_falls_back_to_origin_main_for_non_repo() {
        // Non-repo path with no config — must not panic. Last-resort
        // behaviour preserved: `get_default_branch` returns "main" on a
        // failed `Repository::open`, and we prefix it with "origin/".
        // The spawn path itself short-circuits to `RepoUnusable` so the
        // auto-sync result is non-blocking.
        let tmp = TempDir::new().unwrap();
        let resolved = resolve_base_ref_for_spawn(tmp.path().to_str().unwrap(), None);
        assert_eq!(resolved, "origin/main");
    }

    #[test]
    fn resolve_base_ref_detects_master_via_origin_head() {
        // Headline regression test: a master-trunk repo with no
        // `base_ref` in mesh config must produce "origin/master", not
        // the legacy "origin/main". Pre-fix, this always returned
        // "origin/main" and the spawn emitted a `mesh-sync-warning` on
        // every node.
        use crate::env::test_helpers::TestDir;
        use git2;

        let td = TestDir::new("base_ref_master");
        let parent = td.path();
        // Create a working repo on whatever default branch git picks.
        // The local branch name doesn't matter — what matters is that
        // `refs/remotes/origin/HEAD` points at `refs/remotes/origin/master`.
        crate::env::test_helpers::init_repo_with_commit(
            parent,
            &[("README.md", "v1\n")],
        );

        let repo = git2::Repository::open(parent).unwrap();
        let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
        // Build the symbolic ref that `get_default_branch` reads.
        repo.reference(
            "refs/remotes/origin/master",
            oid,
            true,
            "test setup",
        )
        .unwrap();
        repo.reference_symbolic(
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/master",
            true,
            "test setup",
        )
        .unwrap();

        // Sanity: precondition for the test to be meaningful.
        let head_ref = repo
            .find_reference("refs/remotes/origin/HEAD")
            .unwrap()
            .symbolic_target()
            .unwrap()
            .to_string();
        assert_eq!(
            head_ref, "refs/remotes/origin/master",
            "precondition: origin/HEAD must point at refs/remotes/origin/master"
        );

        let resolved = resolve_base_ref_for_spawn(parent.to_str().unwrap(), None);
        assert_eq!(
            resolved, "origin/master",
            "master-trunk repo with no base_ref in config must yield origin/master, \
             not the legacy hardcoded origin/main (this is the master-trunk regression)"
        );
    }

    #[test]
    fn resolve_base_ref_detects_main_via_origin_head() {
        // Sanity pin: the existing main-trunk behaviour (a repo whose
        // origin/HEAD points at `main`) must still resolve to
        // "origin/main" after the fix. Guards against the master fix
        // accidentally regressing the main case.
        use crate::env::test_helpers::TestDir;
        use git2;

        let td = TestDir::new("base_ref_main");
        let parent = td.path();
        crate::env::test_helpers::init_repo_with_commit(
            parent,
            &[("README.md", "v1\n")],
        );

        let repo = git2::Repository::open(parent).unwrap();
        let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
        repo.reference(
            "refs/remotes/origin/main",
            oid,
            true,
            "test setup",
        )
        .unwrap();
        repo.reference_symbolic(
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
            true,
            "test setup",
        )
        .unwrap();

        let resolved = resolve_base_ref_for_spawn(parent.to_str().unwrap(), None);
        assert_eq!(
            resolved, "origin/main",
            "main-trunk repo must still resolve to origin/main (no regression)"
        );
    }

    #[test]
    fn resolve_base_ref_treats_coalesce_sentinel_as_unset() {
        // The production call path: `meshes.base_ref` is a NOT NULL
        // column with a COALESCE default of `'origin/main'` (see
        // `db::MESH_COLUMNS`). A fresh mesh whose base_ref was never
        // explicitly set reads as `Some("origin/main")` from the DB →
        // `MeshRow.base_ref = Some("origin/main")` →
        // `config.as_ref().and_then(|c| c.base_ref.as_deref())` returns
        // `Some("origin/main")`. The helper MUST treat this sentinel as
        // "no config" and fall through to the detection chain, otherwise
        // a master-trunk repo's spawn still hits `mesh-sync-warning`.
        // The earlier `_detects_master_via_origin_head` test passes
        // `None` (which never reaches production); THIS test pins the
        // actual production contract.
        use crate::env::test_helpers::TestDir;
        use git2;

        let td = TestDir::new("base_ref_coalesce_master");
        let parent = td.path();
        crate::env::test_helpers::init_repo_with_commit(
            parent,
            &[("README.md", "v1\n")],
        );

        let repo = git2::Repository::open(parent).unwrap();
        let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
        repo.reference(
            "refs/remotes/origin/master",
            oid,
            true,
            "test setup",
        )
        .unwrap();
        repo.reference_symbolic(
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/master",
            true,
            "test setup",
        )
        .unwrap();

        // Production-shaped input: COALESCE default from the DB.
        let resolved = resolve_base_ref_for_spawn(
            parent.to_str().unwrap(),
            Some("origin/main"),
        );
        assert_eq!(
            resolved, "origin/master",
            "the COALESCE default 'origin/main' from a fresh mesh's DB row \
             must be treated as 'no config' — fall through to origin/HEAD \
             detection. A master-trunk repo with an unconfigured mesh \
             produces origin/master, not origin/main. This is the actual \
             production contract; the test passing None never reaches \
             production."
        );
    }

    #[test]
    fn resolve_base_ref_keeps_explicit_user_value_for_main_trunk() {
        // A user who LEGITIMATELY sets `base_ref = "origin/main"` (via
        // the 'Fresh' UI option) on a main-trunk repo must still get
        // "origin/main" back. The COALESCE-sentinel treatment must
        // apply to the *fresh* / *unconfigured* case, not penalize a
        // user who explicitly chose the same value. For a main-trunk
        // repo the auto-detect would return the same value, so this
        // test is mostly a documentation pin.
        use crate::env::test_helpers::TestDir;
        use git2;

        let td = TestDir::new("base_ref_explicit_main");
        let parent = td.path();
        crate::env::test_helpers::init_repo_with_commit(
            parent,
            &[("README.md", "v1\n")],
        );

        let repo = git2::Repository::open(parent).unwrap();
        let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
        repo.reference(
            "refs/remotes/origin/main",
            oid,
            true,
            "test setup",
        )
        .unwrap();
        repo.reference_symbolic(
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
            true,
            "test setup",
        )
        .unwrap();

        let resolved = resolve_base_ref_for_spawn(
            parent.to_str().unwrap(),
            Some("origin/main"),
        );
        assert_eq!(
            resolved, "origin/main",
            "explicit user-set 'origin/main' on a main-trunk repo must resolve \
             to 'origin/main' (same as auto-detect — no behaviour change)"
        );
    }

    // -----------------------------------------------------------------------
    // SHA-drift detection (issue #444)
    //
    // `read_origin_ref_sha` returns the local SHA at `origin/<head_ref>` so
    // the spawn path can compare it to the user-pinned `source_pr_pinned_sha`
    // and emit a `pr_sha_drift` warning on mismatch. The unit test creates
    // the local ref directly via git2 (no real remote / fetch roundtrip) so
    // the test is hermetic and fast.
    // -----------------------------------------------------------------------

    #[test]
    fn read_origin_ref_sha_returns_local_sha_when_ref_exists() {
        let tmp = TempDir::new().unwrap();
        let repo = git2::Repository::init(tmp.path()).unwrap();
        // Create a real commit on a known branch — we need a tree OID the
        // commit can point at. `Repository::init` leaves the index empty
        // but write_tree() on an empty index still produces a valid tree.
        let tree_oid = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = git2::Signature::now("test", "test@example.com").unwrap();
        let commit_oid = repo
            .commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        // Manually create the remote-tracking ref the function reads. In
        // production this is what `git fetch origin -- <head_ref>` writes;
        // here we shortcut the network roundtrip to keep the test hermetic.
        let ref_name = "refs/remotes/origin/feat-x";
        repo.reference(ref_name, commit_oid, true, "test").unwrap();

        let sha = read_origin_ref_sha(tmp.path().to_str().unwrap(), "origin/feat-x");
        assert_eq!(
            sha.as_deref(),
            Some(commit_oid.to_string().as_str()),
            "read_origin_ref_sha must return the full 40-char SHA the ref points to"
        );
    }

    #[test]
    fn read_origin_ref_sha_returns_none_for_missing_ref() {
        let tmp = TempDir::new().unwrap();
        git2::Repository::init(tmp.path()).unwrap();
        // No refs/remotes/origin/* exists; the function must return None
        // (the spawn path treats this as "skip drift check" rather than
        // failing — same fail-open semantics as `pr_head_unfetchable`).
        let sha = read_origin_ref_sha(tmp.path().to_str().unwrap(), "origin/nope");
        assert!(sha.is_none(), "missing ref must return None, not error");
    }

    #[test]
    fn read_origin_ref_sha_returns_none_for_non_git_directory() {
        // A path that isn't a git repo at all — `git rev-parse` exits non-zero,
        // the helper must swallow that and return None rather than panicking.
        let tmp = TempDir::new().unwrap();
        let sha = read_origin_ref_sha(tmp.path().to_str().unwrap(), "origin/main");
        assert!(sha.is_none(), "non-repo path must return None, not error");
    }

    fn read_injected_settings(project: &std::path::Path) -> serde_json::Value {
        let path = project.join(".claude").join("settings.local.json");
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("settings.local.json not written: {}", e));
        serde_json::from_str(&content).expect("settings.local.json is not valid JSON")
    }

    /// The Notification hook must fire on EVERY notification type, not just
    /// `idle_prompt`. An empty matcher is Claude Code's "match all" — without it
    /// the hook ignores `permission_prompt` notifications, so the user is never
    /// alerted when an agent asks to run a tool or otherwise needs a decision.
    /// Regression guard for the "only alerted after the agent finishes" gap.
    #[test]
    fn attention_hook_notification_matcher_is_catch_all() {
        let temp = TempDir::new().unwrap();
        inject_attention_hook(temp.path());

        let settings = read_injected_settings(temp.path());
        let notification = &settings["hooks"]["Notification"][0];
        assert_eq!(
            notification["matcher"], "",
            "Notification matcher must be empty (catch-all) so permission_prompt \
             notifications alert the user, not just idle_prompt"
        );
        let command = notification["hooks"][0]["command"]
            .as_str()
            .expect("notification hook command should be a string");
        assert!(
            command.contains("/api/attention/"),
            "notification hook should POST to the attention endpoint, got: {command}"
        );
    }

    /// A `Stop` hook fires the instant the agent finishes a turn, so the user is
    /// alerted immediately rather than waiting for the `idle_prompt` idle timer.
    #[test]
    fn attention_hook_includes_stop_event() {
        let temp = TempDir::new().unwrap();
        inject_attention_hook(temp.path());

        let settings = read_injected_settings(temp.path());
        let command = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .expect("Stop hook command should be present so turn-end alerts fire immediately");
        assert!(
            command.contains("/api/attention/"),
            "Stop hook should POST to the attention endpoint, got: {command}"
        );
    }

    /// Injection is idempotent: a second call over an already-correct file must
    /// not rewrite it (the early-return guard) and must leave it parseable.
    #[test]
    fn attention_hook_injection_is_idempotent() {
        let temp = TempDir::new().unwrap();
        inject_attention_hook(temp.path());
        let first = read_injected_settings(temp.path());
        inject_attention_hook(temp.path());
        let second = read_injected_settings(temp.path());
        assert_eq!(first, second, "second injection should be a no-op");
    }

    /// Injection must preserve unrelated keys already present in the user's
    /// settings.local.json (e.g. `permissions`) — it only owns `hooks`.
    #[test]
    fn attention_hook_preserves_other_settings() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("settings.local.json"),
            r#"{"permissions":{"allow":["Bash(ls:*)"]}}"#,
        )
        .unwrap();

        inject_attention_hook(temp.path());

        let settings = read_injected_settings(temp.path());
        assert_eq!(
            settings["permissions"]["allow"][0], "Bash(ls:*)",
            "pre-existing permissions must survive hook injection"
        );
        assert_eq!(settings["hooks"]["Notification"][0]["matcher"], "");
    }

    // ----- fork alias + fetch_fork_head (issue #443) ---------------------

    /// `fork-<login>` is the human-readable alias used in `git remote -v` and
    /// the worktree `base_ref` string. The `fork-` prefix keeps our entries
    /// easy to spot in the remote list and trivial to clean up if we ever
    /// need to. Pin the format so a future refactor that swaps the prefix
    /// surfaces as a test failure rather than a silent rename in user
    /// worktrees.
    #[test]
    fn fork_remote_alias_uses_fork_prefix() {
        assert_eq!(fork_remote_alias("alice"), "fork-alice");
        assert_eq!(fork_remote_alias("alondero"), "fork-alondero");
    }

    /// Build a bare "fork" repo (a real local clone target so the test
    /// doesn't need a network round-trip) and a regular repo that will
    /// register the fork as a remote. The fork has a single commit on
    /// `main` plus a `feat/443-fork` branch so the fetch can target a
    /// non-default ref. Returns `(local, fork_bare_dir, fork_path)` —
    /// the caller holds the dirs for the duration of the test.
    fn init_fork_fixture() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
        // Source: a regular repo with a feature branch we can fetch.
        let src = TempDir::new().unwrap();
        let src_path = src.path().to_path_buf();
        let src_repo = git2::Repository::init(&src_path).unwrap();
        let sig = git2::Signature::now("test", "test@example.com").unwrap();
        std::fs::write(src_path.join("README.md"), "fork-source\n").unwrap();
        let mut index = src_repo.index().unwrap();
        index.add_path(std::path::Path::new("README.md")).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = src_repo.find_tree(tree_oid).unwrap();
        let main_commit = src_repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        // Branch off a feature branch.
        let feat_commit = src_repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "feat: fork-only commit",
            &tree,
            &[&src_repo.find_commit(main_commit).unwrap()],
        )
        .unwrap();
        let _ = tree;
        // `main_commit` is a `git2::Oid` (Copy) — no need to `drop` it; the
        // explicit `drop()` was a no-op flagged by clippy.
        let feat_commit = src_repo.find_commit(feat_commit).unwrap();
        src_repo
            .branch("feat/443-fork", &feat_commit, true)
            .unwrap();
        // Bare clone target (so the fork has no working tree, like a real
        // remote on GitHub — `git fetch` reads its objects directly).
        // Use a unique, path-safe name — avoid `{:?}` on the source path
        // (it produces `C:\...` with backslashes and quotes that don't
        // round-trip as a directory name on Windows).
        let bare_dir = std::env::temp_dir().join(format!(
            "buildmesh_fork_bare_{}_{}",
            std::process::id(),
            NEXT_FORK_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        ));
        let _ = std::fs::remove_dir_all(&bare_dir);
        let clone = git2::Repository::init_bare(&bare_dir).unwrap();
        let mut remote = clone.remote("origin", src_path.to_str().unwrap()).unwrap();
        remote
            .fetch(&["refs/heads/*:refs/heads/*"], None, None)
            .unwrap();
        // Local: a fresh repo with no remotes — this is what
        // `fetch_fork_head` will register the fork on.
        let local = TempDir::new().unwrap();
        git2::Repository::init(local.path()).unwrap();
        (local, bare_dir, src_path)
    }

    /// Atomic counter for unique bare-repo paths (one per test run).
    static NEXT_FORK_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// First-time registration: the fork is added as `fork-alice` and the
    /// head ref is materialised. `fetch_fork_head` returns `true` and
    /// the resulting `git ls-remote` shows the ref under the alias.
    /// This is the end-to-end "fork spawn" path that issue #443 opens up.
    #[test]
    fn fetch_fork_head_registers_remote_and_fetches_ref() {
        let (local, bare_dir, _src) = init_fork_fixture();
        let bare_dir_str = bare_dir.to_str().unwrap().to_string();

        let ok = fetch_fork_head(
            local.path().to_str().unwrap(),
            "alice",
            &bare_dir_str,
            "feat/443-fork",
        );
        assert!(ok, "fetch_fork_head must succeed on a real bare repo");

        // Verify the alias + URL are registered.
        let local_repo = git2::Repository::open(local.path()).unwrap();
        let remote = local_repo
            .find_remote("fork-alice")
            .expect("fork-alice remote must be registered");
        let url = remote.url().expect("remote URL must be set");
        assert_eq!(url, bare_dir_str, "remote URL must match the fork's clone URL");

        // Verify the ref was fetched — it should be visible as
        // `fork-alice/feat/443-fork`.
        let reference = local_repo
            .find_reference("refs/remotes/fork-alice/feat/443-fork")
            .expect("fetched ref must be present under fork-alice/");
        assert!(reference.target().is_some(), "ref target must be a real OID");
    }

    /// Idempotent: a second call on a repo that already has the remote
    /// registered AND the right URL is a no-op. The user can spawn a
    /// second agent on the same fork PR (e.g. after closing the first)
    /// without `git remote add` failing. The function still returns
    /// `true` because the fetch succeeds.
    #[test]
    fn fetch_fork_head_is_idempotent_on_repeat_call() {
        let (local, bare_dir, _src) = init_fork_fixture();
        let bare_dir_str = bare_dir.to_str().unwrap().to_string();

        let first = fetch_fork_head(
            local.path().to_str().unwrap(),
            "alice",
            &bare_dir_str,
            "feat/443-fork",
        );
        assert!(first, "first call must succeed");

        // Second call with the SAME URL — must not error (the `remote add`
        // path is the failure-prone one without the existence check; the
        // `get-url` probe should return the right URL and skip the add).
        let second = fetch_fork_head(
            local.path().to_str().unwrap(),
            "alice",
            &bare_dir_str,
            "feat/443-fork",
        );
        assert!(second, "second call must still succeed (idempotent)");

        // Remote is still there, single entry.
        let local_repo = git2::Repository::open(local.path()).unwrap();
        let remote = local_repo
            .find_remote("fork-alice")
            .expect("fork-alice remote must still be registered after repeat call");
        assert_eq!(remote.url().unwrap(), bare_dir_str);
    }

    /// URL drift: if the fork's clone URL changes between spawns (the
    /// user renamed the repo, or — more likely — the first call stored a
    /// stale URL), the second call should update the existing remote's
    /// URL via `git remote set-url` rather than fail or keep the stale
    /// URL. Pin this so a future refactor that skips the set-url branch
    /// surfaces as a test failure (the second call would silently fetch
    /// the wrong ref).
    #[test]
    fn fetch_fork_head_updates_url_on_drift() {
        let (local, bare_dir, _src) = init_fork_fixture();
        let stale_url = bare_dir.to_str().unwrap().to_string();
        // Reuse the SAME bare dir (so the second call still finds a real
        // repo) but pretend the URL "drifted" by passing a different
        // string that ALSO resolves to the same on-disk repo. We achieve
        // that with a file:// URL on Windows (path with backslashes
        // round-trip cleanly through git remote add).
        let drifted_url = format!("file://{}", stale_url.replace('\\', "/"));

        // First call: register the stale URL.
        let first = fetch_fork_head(
            local.path().to_str().unwrap(),
            "alice",
            &stale_url,
            "feat/443-fork",
        );
        assert!(first, "first call must succeed");

        // Second call: same alias, drifted URL — the function should run
        // `git remote set-url` and re-fetch.
        let second = fetch_fork_head(
            local.path().to_str().unwrap(),
            "alice",
            &drifted_url,
            "feat/443-fork",
        );
        assert!(second, "second call must still succeed after URL drift");

        // The stored URL must be the drifted one, not the original.
        let local_repo = git2::Repository::open(local.path()).unwrap();
        let remote = local_repo
            .find_remote("fork-alice")
            .expect("remote must still be registered");
        let stored = remote.url().unwrap();
        // git normalises file:// URLs slightly on Windows — assert it's
        // the drifted one rather than the original.
        assert_ne!(
            stored, stale_url,
            "URL must have been updated, not left at the stale value"
        );
    }

    /// Failure path: a non-existent clone URL must return `false` rather
    /// than panic. The caller (`spawn_agent_inner`) falls back to the
    /// mesh's `base_ref` and emits a `mesh-sync-warning` toast with
    /// `outcome: "pr_fork_unfetchable"`. Without the failure-as-false
    /// contract, a typo'd clone URL would either spawn on the wrong
    /// commits silently or surface as a hard error every offline session.
    #[test]
    fn fetch_fork_head_returns_false_on_bad_clone_url() {
        let (local, _bare_dir, _src) = init_fork_fixture();
        let bad_url = "/nonexistent/path/to/fork/that/does/not/exist".to_string();

        let ok = fetch_fork_head(
            local.path().to_str().unwrap(),
            "alice",
            &bad_url,
            "feat/443-fork",
        );
        assert!(!ok, "fetch_fork_head must return false on a bad clone URL");
    }

    // ----- fetch_single_ref (issue #420) ---------------------------------
    //
    // Same-repo PR spawn (#420) — the worktree adoption path calls
    // `fetch_single_ref` to materialise `origin/<head_ref>` so the worktree
    // can be cut from it. As of issue #446 the function is a thin wrapper
    // over `git::sync::do_fetch_only` (the fetch-only half of `do_sync` —
    // open + dirty-check + has-remote + `git fetch`, NO `git pull` tail);
    // the `-`-adversarial-ref hardening is preserved at the wrapper
    // boundary because `do_fetch_only` passes the branch as a plain argv
    // entry without a `--` separator (it doesn't know about the spawn
    // context).
    //
    // These tests pin the cases the issue calls out:
    //   1. success — ref exists on origin
    //   2. ref-not-found — ref missing on origin (caller falls back to base_ref)
    //   3. non-git path — caller passed a directory that isn't a repo
    //   4. adversarial ref — `-`-prefixed input is rejected by the wrapper
    //      before `do_fetch_only` sees it (the hardening migrated from the
    //      shell-out's `--` separator to an upfront string check, since
    //      `do_fetch_only` doesn't pass a `--` separator to `git fetch`)
    //   5. dirty-skip (issue #446 acceptance #2) — a parent repo with
    //      uncommitted changes must return `false` (mirrors
    //      `fetch_origin_skips_dirty_parent` in `git/fetch_origin_tests.rs`)
    //
    // The fixture mirrors `init_fork_fixture` but for the same-repo path:
    // a bare repo holds a single branch, the local repo has `origin`
    // pointed at the bare, and the test calls `fetch_single_ref` against
    // the local repo's path.

    /// Build a "remote + local" pair: the bare repo has a single commit on
    /// `main` plus a `feat/420-pr-spawn` branch; the local repo has `origin`
    /// pointed at the bare. Returns `(local, bare_path)` — the local TempDir
    /// owns its on-disk path; `bare_path` is a plain PathBuf that lives
    /// inside `std::env::temp_dir()` and is reused across calls (it gets
    /// re-populated with the same content each time, so the SHA is stable
    /// per-test-process).
    fn init_same_repo_fixture() -> (TempDir, std::path::PathBuf) {
        // Source: a working repo with a feature branch we can fetch.
        // We reuse the same on-disk source across tests in a single
        // process — `init_same_repo_fixture` is only called from the
        // same-repo tests below, and the contents are deterministic.
        static SRC_DIR: std::sync::OnceLock<std::path::PathBuf> =
            std::sync::OnceLock::new();
        let src_path = SRC_DIR
            .get_or_init(|| {
                let src = TempDir::new().unwrap();
                let src_path = src.path().to_path_buf();
                let src_repo = git2::Repository::init(&src_path).unwrap();
                let sig = git2::Signature::now("test", "test@example.com").unwrap();
                std::fs::write(src_path.join("README.md"), "init\n").unwrap();
                let mut index = src_repo.index().unwrap();
                index.add_path(std::path::Path::new("README.md")).unwrap();
                index.write().unwrap();
                let tree_oid = index.write_tree().unwrap();
                let tree = src_repo.find_tree(tree_oid).unwrap();
                let main_commit = src_repo
                    .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                    .unwrap();
                let main_commit_obj = src_repo.find_commit(main_commit).unwrap();
                src_repo
                    .branch("feat/420-pr-spawn", &main_commit_obj, true)
                    .unwrap();
                // Leak the TempDir guard — we want src_path to stay alive
                // for the whole process, and the bare-fetch step below
                // re-reads from the on-disk path on every test.
                std::mem::forget(src);
                src_path
            })
            .clone();

        // Bare remote — same pattern as `init_fork_fixture`. A unique
        // name per process so parallel `cargo test` invocations don't
        // collide on the bare dir.
        let bare_dir = std::env::temp_dir().join(format!(
            "buildmesh_same_repo_bare_{}_{}",
            std::process::id(),
            NEXT_FORK_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        ));
        let _ = std::fs::remove_dir_all(&bare_dir);
        let clone = git2::Repository::init_bare(&bare_dir).unwrap();
        let mut remote = clone
            .remote("origin", src_path.to_str().unwrap())
            .unwrap();
        remote
            .fetch(&["refs/heads/*:refs/heads/*"], None, None)
            .unwrap();

        // Local repo with `origin` pointed at the bare. `fetch_single_ref`
        // will use this `origin` remote to materialise the ref.
        let local = TempDir::new().unwrap();
        let local_repo = git2::Repository::init(local.path()).unwrap();
        local_repo
            .remote("origin", bare_dir.to_str().unwrap())
            .unwrap();
        (local, bare_dir)
    }

    /// Success path: a ref that exists on `origin` is fetched into
    /// `refs/remotes/origin/<head_ref>` and the function returns `true`.
    /// This is the happy path the spawn-time worktree adoption relies on.
    #[test]
    fn fetch_single_ref_returns_true_when_ref_exists() {
        let (local, _bare) = init_same_repo_fixture();
        let ok = fetch_single_ref(local.path().to_str().unwrap(), "feat/420-pr-spawn");
        assert!(
            ok,
            "fetch_single_ref must return true when the ref exists on origin"
        );
        // Verify the ref actually got materialised — a true return with no
        // visible ref would mean a silent no-op, which is a worse failure
        // mode than a hard error.
        let local_repo = git2::Repository::open(local.path()).unwrap();
        let reference = local_repo
            .find_reference("refs/remotes/origin/feat/420-pr-spawn")
            .expect("origin/feat/420-pr-spawn must be materialised after success");
        assert!(
            reference.target().is_some(),
            "fetched ref must point at a real OID, not be unborn"
        );
    }

    /// Ref-not-found path: a ref that does NOT exist on `origin` causes
    /// `git fetch` to exit non-zero. The function returns `false` (not
    /// an error) so the spawn path can fall back to the mesh's
    /// `base_ref` — this is the ADR 0001 offline pattern, surface as
    /// `pr_head_unfetchable` rather than failing the spawn.
    #[test]
    fn fetch_single_ref_returns_false_when_ref_missing() {
        let (local, _bare) = init_same_repo_fixture();
        let ok = fetch_single_ref(local.path().to_str().unwrap(), "does-not-exist");
        assert!(
            !ok,
            "fetch_single_ref must return false when the ref is missing on origin \
             (caller falls back to base_ref per the offline-fallback contract)"
        );
    }

    /// Non-git path: a directory that isn't a git repo at all. `git fetch`
    /// errors immediately; the function swallows that and returns `false`.
    /// This is the "user has a partial / broken clone" edge case — the
    /// spawn must not panic.
    #[test]
    fn fetch_single_ref_returns_false_for_non_git_directory() {
        let tmp = TempDir::new().unwrap();
        let ok = fetch_single_ref(tmp.path().to_str().unwrap(), "feat/420-pr-spawn");
        assert!(
            !ok,
            "fetch_single_ref must return false (not panic) for a non-git path"
        );
    }

    /// Adversarial-ref pin (issue #420 hardening): a ref starting with `-`
    /// (e.g. `--upload-pack=evil`) is rejected by `git` itself because of
    /// the `--` separator before `head_ref`. Without the separator, `git`
    /// would parse `--upload-pack=evil` as a flag and use it for the
    /// fetch — a vector for arbitrary command execution on a malicious
    /// server (CVE-2017-1000117 / CVE-2018-17456 class). The hardening
    /// lives in `fetch_single_ref`; this test pins the contract so a
    /// future refactor that drops the `--` separator fails the test
    /// rather than silently re-introducing the vulnerability.
    ///
    /// We pass a ref that, WITHOUT the separator, `git` would parse as a
    /// flag (`--upload-pack=evil`) — `git fetch` will then error out on
    /// "fatal: bad config name", proving the separator did its job. With
    /// the separator, the value reaches the ref-spec parser as a
    /// literal ref name (which still doesn't exist on origin, so the
    /// call returns `false` either way — the contract is "the function
    /// returns false rather than letting `--upload-pack` reach git").
    #[test]
    fn fetch_single_ref_rejects_adversarial_dash_ref() {
        let (local, _bare) = init_same_repo_fixture();
        let ok = fetch_single_ref(local.path().to_str().unwrap(), "--upload-pack=evil");
        assert!(
            !ok,
            "fetch_single_ref must return false for a ref starting with '-' \
             (the wrapper rejects it before do_sync sees it)"
        );
    }

    /// Dirty-parent pin (issue #446 acceptance #2, inverted 2026-07-17): a
    /// parent repo with uncommitted changes must STILL fetch the PR head.
    /// A `git fetch` never touches the working tree — the pre-2026-07-17
    /// dirty-skip meant a mesh whose root checkout stayed dirty silently
    /// fell back to `base_ref` on every PR spawn, cutting the worktree
    /// from the wrong commits. Pin the new contract so a future refactor
    /// that re-introduces a pre-fetch dirty gate fails this test.
    ///
    /// `is_dirty` includes untracked files, so writing one to the freshly-
    /// init'd local repo is enough to dirty it — no need to seed a tracked
    /// file first.
    #[test]
    fn fetch_single_ref_fetches_despite_dirty_parent() {
        let (local, _bare) = init_same_repo_fixture();
        // Precondition: the fixture's local repo must start clean, then we
        // make it dirty with an untracked file.
        assert!(
            !crate::env::test_helpers::repo_is_dirty(local.path()),
            "precondition: freshly-init'd local repo must start clean"
        );
        std::fs::write(local.path().join("dirty-marker.txt"), "uncommitted\n").unwrap();
        assert!(
            crate::env::test_helpers::repo_is_dirty(local.path()),
            "precondition: writing an untracked file must dirty the repo"
        );

        let ok = fetch_single_ref(local.path().to_str().unwrap(), "feat/420-pr-spawn");
        assert!(
            ok,
            "fetch_single_ref must fetch on a dirty parent — a fetch never \
             touches the working tree, and skipping cut PR worktrees from \
             stale refs"
        );
        // The head ref must be materialised so the worktree can be cut
        // from it — the whole point of the fetch.
        let repo = git2::Repository::open(local.path()).unwrap();
        assert!(
            repo.find_reference("refs/remotes/origin/feat/420-pr-spawn")
                .is_ok(),
            "the fetch must materialise refs/remotes/origin/<head_ref>"
        );
        // And the dirty marker must be untouched.
        assert_eq!(
            std::fs::read_to_string(local.path().join("dirty-marker.txt")).unwrap(),
            "uncommitted\n"
        );
    }

    // -----------------------------------------------------------------------
    // locked_fetch_pr_head — per-Mesh sync_lock wrap (issue #698)
    //
    // `locked_fetch_pr_head` must run inside `services::sync_lock::with_mesh_
    // sync_lock` so two concurrent PR-spawns (or a PR-spawn racing the manual
    // `git_sync` from #680 / the spawn-time `fetch_origin` from #652) can't
    // collide on `.git/FETCH_HEAD` / `.git/refs/remotes/<remote>/<ref>.lock`.
    // Without the wrap the losing fetch fails with "another git process" and
    // the spawn silently lands on `base_ref` (the wrong commits).
    //
    // We test the wrap with a wall-clock bound (mirroring the #680
    // `git_sync_serializes_via_per_mesh_sync_lock_gh680` shape in
    // `commands/git_tests.rs`). The `with_mesh_sync_lock` unit tests in
    // `services::sync_lock` prove the primitive itself serialises; this test
    // proves THIS specific call site uses the SAME key the spawn path uses,
    // which is the bug class #698 closes.
    //
    // Holder enters the per-mesh lock and announces entry via an AtomicUsize
    // flag before sleeping. Main thread spin-waits on the flag (deterministic
    // — no `thread::sleep` race), then times `locked_fetch_pr_head`. With the
    // wrap, `locked_fetch_pr_head` blocks ~450 ms waiting for the holder;
    // without, it runs concurrently with the holder and finishes in tens of ms.
    // -----------------------------------------------------------------------

    /// Regression test for issue #698 — `locked_fetch_pr_head` must acquire
    /// the per-Mesh `with_mesh_sync_lock` keyed on the spawn's `node.path`,
    /// matching what `spawn_agent_inner` calls `fetch_origin` with two steps
    /// earlier. Without this wrap, concurrent PR-spawns on the same Mesh
    /// (and a PR-spawn racing the manual `git_sync` button) race on
    /// `.git/FETCH_HEAD` / `refs/remotes/<remote>/<ref>.lock` and the loser
    /// silently falls back to `base_ref`.
    ///
    /// Strategy: holder thread enters `with_mesh_sync_lock(&path_key, ...)`
    /// and announces via an AtomicUsize flag, then sleeps. Main thread
    /// spin-waits on the flag (deterministic — no `thread::sleep` race), then
    /// times `locked_fetch_pr_head`. With the wrap, `locked_fetch_pr_head`
    /// blocks waiting for the holder; without, it returns immediately while
    /// the holder is still inside its critical section.
    ///
    /// Why wall-clock (not `fetch_add`): the per-Mesh lock is correctly
    /// implemented (issue #652 + `services::sync_lock` unit tests prove it),
    /// so it *prevents* simultaneous critical-section entries — `max_concurrent
    /// == 1` even on a working lock. The only signal that `locked_fetch_pr_head`
    /// shares the same key is that it waits for the holder to release the lock.
    ///
    /// The test uses the same-repo branch (passes `None, None` for fork
    /// fields). The fork branch shares the same wrapper so the regression
    /// coverage is sufficient with one call site — a #698 regression that
    /// branched out of the wrapper entirely would fail this test and the
    /// #443 fork tests would still pass on the unwrapped helper, surfacing
    /// the gap.
    #[test]
    fn locked_fetch_pr_head_serializes_via_per_mesh_sync_lock_gh698() {
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        use std::time::{Duration, Instant};

        let (local, _bare) = init_same_repo_fixture();
        let path_key = local.path().to_string_lossy().into_owned();

        // Holder enters the per-mesh lock and announces entry via
        // `entered_flag` before sleeping. Spinning on the flag avoids the
        // `thread::sleep` race — CI jitter can't make `locked_fetch_pr_head`
        // sneak in first.
        let entered_flag = std::sync::Arc::new(AtomicUsize::new(0));
        let holder_path = path_key.clone();
        let entered_holder = std::sync::Arc::clone(&entered_flag);
        let holder = std::thread::spawn(move || {
            crate::services::sync_lock::with_mesh_sync_lock(&holder_path, || {
                entered_holder.store(1, AOrdering::SeqCst);
                std::thread::sleep(Duration::from_millis(500));
            });
        });

        // Spin-wait (bounded) for the holder to actually be inside the
        // critical section. Cap at 2 s so a hung holder surfaces as a
        // test panic, not a forever-wait.
        let deadline = Instant::now() + Duration::from_secs(2);
        while entered_flag.load(AOrdering::SeqCst) == 0 {
            assert!(
                Instant::now() < deadline,
                "holder thread never entered the per-mesh lock"
            );
            std::thread::sleep(Duration::from_millis(1));
        }

        let start = Instant::now();
        let _ = locked_fetch_pr_head(&path_key, "feat/420-pr-spawn", None, None);
        let elapsed = start.elapsed();

        holder.join().unwrap();

        // With wrap: elapsed >= ~450 ms (`locked_fetch_pr_head` waited for
        // the holder). Without wrap: elapsed = tens of ms (the fetch ran
        // concurrently with the holder's sleep). Bound is 400 ms — leaves
        // 100 ms of slack for setup overhead and CI jitter on a busy box.
        assert!(
            elapsed >= Duration::from_millis(400),
            "locked_fetch_pr_head did not block on the per-mesh lock \
             (elapsed = {:?}); issue #698 wrap is missing — concurrent PR-spawn \
             and spawn-time fetch_origin (or manual git_sync from #680) would \
             race on .git/FETCH_HEAD and refs/remotes/<remote>/<ref>.lock",
            elapsed,
        );
    }

    /// Companion to `locked_fetch_pr_head_serializes_via_per_mesh_sync_lock_gh698`
    /// — exercises the FORK branch (`Some/Some` → `fetch_fork_head`) of the
    /// wrapper. The same-repo test alone leaves a CI blind spot: a #698
    /// regression that bypassed the wrapper for fork PRs (e.g. an inlined
    /// `fetch_fork_head` call in `spawn_agent_inner` to skip the remote-
    /// config lock acquisition) would still pass the same-repo test and
    /// every existing #443 fork unit test (those hit the bare helper
    /// directly, no lock). This test closes the gap by hitting the fork
    /// arm of the wrapper with the same wall-clock shape; its `git remote
    /// add` then `git fetch` sequence MUST hold the lock for the holder's
    /// 500 ms sleep.
    #[test]
    fn locked_fetch_pr_head_serializes_fork_branch_via_per_mesh_sync_lock_gh698() {
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        use std::time::{Duration, Instant};

        let (local, bare_dir, _src) = init_fork_fixture();
        let bare_dir_str = bare_dir.to_str().unwrap().to_string();
        let path_key = local.path().to_string_lossy().into_owned();

        // Holder enters the per-mesh lock (same key as the wrapper) and
        // announces via `entered_flag` before sleeping.
        let entered_flag = std::sync::Arc::new(AtomicUsize::new(0));
        let holder_path = path_key.clone();
        let entered_holder = std::sync::Arc::clone(&entered_flag);
        let holder = std::thread::spawn(move || {
            crate::services::sync_lock::with_mesh_sync_lock(&holder_path, || {
                entered_holder.store(1, AOrdering::SeqCst);
                std::thread::sleep(Duration::from_millis(500));
            });
        });

        let deadline = Instant::now() + Duration::from_secs(2);
        while entered_flag.load(AOrdering::SeqCst) == 0 {
            assert!(
                Instant::now() < deadline,
                "holder thread never entered the per-mesh lock"
            );
            std::thread::sleep(Duration::from_millis(1));
        }

        let start = Instant::now();
        let _ = locked_fetch_pr_head(
            &path_key,
            "feat/443-fork",
            Some("alice"),
            Some(&bare_dir_str),
        );
        let elapsed = start.elapsed();

        holder.join().unwrap();

        assert!(
            elapsed >= Duration::from_millis(400),
            "locked_fetch_pr_head (fork branch) did not block on the per-mesh \
             lock (elapsed = {:?}); issue #698 wrap is missing for the fork path \
             — concurrent fork-PR spawns would race on .git/FETCH_HEAD, \
             refs/remotes/fork-<login>/<ref>.lock, AND the git remote add/config \
             files that fetch_fork_head writes before its fetch",
            elapsed,
        );
    }

    // -----------------------------------------------------------------------
    // Reader-thread session-id capture gate (issue #651)
    //
    // The orchestrator's pre-write at spawn_agent_inner (Assign mode) and the
    // PTY reader thread's capture-from-output path both target the same
    // `agent_nodes.cli_session_id` column. They are unsynchronised, so a
    // last-writer-wins race left the row holding a UUID the agent never
    // claimed — and auto-resume later invoked `claude --resume <wrong-uuid>`
    // → "Conversation not found". The fix pins the gate to a single function
    // of `session_id_mode` (the source of truth) so the two writers can never
    // both target the same column. Each test pins one row of the truth table;
    // the regression test is the `Assign(_)` row.
    // -----------------------------------------------------------------------

    /// Regression for issue #651. Even if a future adapter returns
    /// `self_assigns_session_id() = true`, the reader thread MUST NOT capture
    /// when the orchestrator is in Assign mode — the orchestrator already
    /// wrote a UUID at `spawn_agent_inner` step 4, and the reader would
    /// overwrite it with whatever UUID matched the regex on PTY output
    /// (possibly a different log line, possibly never echoed back).
    #[test]
    fn reader_should_not_capture_in_assign_mode_even_if_provider_self_assigns() {
        assert!(
            !reader_should_capture_session_id(
                &SessionIdMode::Assign("orchestrator-uuid".into()),
                true,
            ),
            "Assign mode is authoritative — reader MUST NOT overwrite the \
             orchestrator's pre-written UUID with a regex match from PTY output \
             (issue #651: 'a UUID the agent never claimed')"
        );
    }

    /// Resume already has the authoritative ID stored in `cli_session_id`
    /// (or, for fresh `--resume` calls, the resume arg passed to the CLI).
    /// Capture would race the in-flight `claude --resume <id>` with a
    /// possibly-different UUID from the regex, so the reader must stay quiet.
    #[test]
    fn reader_should_not_capture_in_resume_mode() {
        assert!(
            !reader_should_capture_session_id(
                &SessionIdMode::Resume("resume-uuid".into()),
                true,
            ),
            "Resume mode carries the authoritative ID; reader MUST NOT capture"
        );
    }

    /// `None` mode is the only mode where reader capture is allowed — and only
    /// for providers that self-assign session IDs (Codex, Agy). Anthropic,
    /// OpenCode, and Terminal accept `--session-id` (or don't track sessions
    /// at all), so the regex match would be spurious noise.
    #[test]
    fn reader_should_capture_when_provider_self_assigns_and_mode_is_none() {
        assert!(
            reader_should_capture_session_id(&SessionIdMode::None, true),
            "Codex / Agy fresh spawns rely on the reader capturing the UUID \
             from PTY output (orchestrator has no pre-write in None mode)"
        );
    }

    /// Self-assigning capability is necessary but not sufficient — if the
    /// provider accepts `--session-id` (Anthropic / OpenCode), the regex
    /// match is not the source of truth even when the orchestrator didn't
    /// pre-write (e.g. a Terminal-style provider that doesn't accept
    /// `--session-id`).
    #[test]
    fn reader_should_not_capture_when_provider_does_not_self_assign() {
        assert!(
            !reader_should_capture_session_id(&SessionIdMode::None, false),
            "reader MUST NOT capture when provider does not self-assign; \
             any UUID match would overwrite the existing cli_session_id"
        );
    }
}
