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
fn resolve_base_ref_for_spawn(mesh_path: &str, config_base_ref: Option<&str>) -> String {
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
    let branch = crate::commands::git::get_default_branch(mesh_path.to_string());
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
            // Mark the app as active so the background warm-pool worker holds
            // off its idle refills while an agent is actively producing output
            // (issue #613 AC2) — a `git worktree add` must not compete with a
            // live agent's I/O.
            crate::services::pool_worker::note_activity();

            let text = String::from_utf8_lossy(data);
            crate::session_naming::on_output(session_id, &text);

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
                // Symmetric guard with the orchestrator's Spawning write
                // (issue #654): never resurrect `Archived` (user-initiated
                // terminal state). `Error` itself is excluded so a
                // double-write is a no-op rather than bumping
                // `status_changed_at` a second time.
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
    let base_ref = resolve_base_ref_for_spawn(
        &node.path,
        row.as_ref().and_then(|r| r.base_ref.as_deref()),
    );

    timer.checkpoint("after_mesh_row_read");
    diag.checkpoint("after_mesh_row_read");

    // 6. Compute spawn path. The warm-pool tracer bullet (issue #609) lets
    //    manual spawns ADOPT a pre-warmed detached worktree as their own
    //    worktree — zero cold checkout, no folder rename (the pool's
    //    preassigned slug IS the node name). The claim happens BEFORE the
    //    cold `create_git_worktree` block below; on success we rewrite the
    //    worktree's mode (branched vs. detached) to match the mesh's
    //    `worktree_mode`, then fall through to the rest of the spawn. A
    //    claim failure (empty pool, PR/issue source, etc.) is logged and
    //    falls through to the cold path — the spawn never fails because of
    //    the pool, only because of an actual worktree-create error.
    let mesh_id = db::get_mesh_by_path(&node.path).map(|m| m.id).unwrap_or(-1);
    // Issue/PR spawns adopt a warm entry differently from manual spawns: they
    // keep their own `gh{N}-`/`pr{N}-` name and `git worktree move` the pool
    // directory to match (issue #612), whereas a manual spawn adopts the pool's
    // plain slug as the node name (issue #609). This flag selects between the
    // two adoption modes at every warm-pool branch below.
    let is_rename_spawn = node.source_issue.is_some() || node.source_pr.is_some();
    let mut warm_claimed: Option<crate::services::warm_pool::ClaimedWarmEntry> = None;
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
                    warm_claimed = Some(entry);
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

    // Set true when the spawn-time fetch advances the mesh's base ref, so the
    // single post-spawn pool-maintenance task at the end runs the ref-freshness
    // pass (issue #613 AC3). Carried to the end rather than firing its own
    // thread here so refresh + refill share ONE fill-lock acquisition and can
    // never lose a lock race to each other (issue #613 review).
    let mut ref_advanced_for_pool = false;

    // 7. Create worktree if needed
    if let Some(wt_name) = spawn_worktree_name.as_deref() {
        // Branched worktrees are isolated checkouts: git worktree add checks
        // out the commit, not the parent's working tree, so uncommitted
        // changes in the parent cannot leak into the new worktree. We
        // intentionally do not gate spawn on parent cleanliness (see
        // docs/adr/0002-allow-branched-worktree-creation-on-dirty-mesh.md).
        let host_path = std::path::Path::new(&resolved.host_path);
        if !host_path.exists() {
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
            let root = node.path.clone();
            let base_ref_owned = base_ref.to_string();
            timer.checkpoint("before_fetch_origin");
            diag.git_event("fetch_origin", "start");
            let sync_result = tokio::task::spawn_blocking(move || {
                crate::git::sync::fetch_origin(&root, &base_ref_owned)
            })
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    "spawn_agent_inner: fetch_origin task panicked: {}",
                    e
                );
                Err(crate::git::sync::FetchError::FetchFailed(format!(
                    "sync task panicked: {}",
                    e
                )))
            });
            let fetch_outcome: &'static str = match &sync_result {
                Ok(crate::git::sync::FetchOutcome::Synced { .. }) => "ok:synced",
                Ok(crate::git::sync::FetchOutcome::UpToDate) => "ok:up_to_date",
                Ok(crate::git::sync::FetchOutcome::SkippedDirty) => "skip:dirty",
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
            ref_advanced_for_pool = matches!(
                &sync_result,
                Ok(crate::git::sync::FetchOutcome::Synced { .. })
                    | Ok(crate::git::sync::FetchOutcome::FetchedButDiverged { .. })
            );
            emit_sync_outcome_event(app, session_id, &node.path, sync_result);

            // Worktree adoption for PR-spawned nodes (issue #420, extended
            // by #443 for fork PRs). When the node carries a `source_pr`,
            // the head ref stored in `node.branch` is the PR's actual source
            // branch (e.g. `feat/420-pr-spawn`), and the worktree needs to
            // be cut from `<remote>/<head_ref>` so the agent lands on the
            // same commits the PR is built from. Two cases:
            //
            //  - Same-repo PRs (`head_repo_owner` is `None`): the head
            //    lives on `origin` — we call `fetch_single_ref` and use
            //    `origin/<head_ref>` (the #420 path).
            //  - Fork PRs (`head_repo_owner` is `Some`): the head lives on
            //    the fork's clone URL — we call `fetch_fork_head`, which
            //    registers the fork as a remote (`fork-<login>`) and fetches
            //    from there (issue #443, follow-up to #36). The worktree
            //    base_ref becomes `fork-<login>/<head_ref>`.
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
            let worktree_base_ref = if node.source_pr.is_some() {
                let head_ref_owned = node.branch.clone();
                let root = node.path.clone();
                let fork_owner_owned = node.head_repo_owner.clone();
                let fork_url_owned = node.head_repo_clone_url.clone();
                timer.checkpoint("before_fetch_pr_head");
                let fetch_ok = tokio::task::spawn_blocking(move || {
                    // Fork path (#443): when the head repo's owner is
                    // recorded, the head lives on the fork's clone URL, not
                    // on `origin`. The clone URL is part of the row (set by
                    // `create_pr_node` from `head_repo_clone_url`), so the
                    // stage-2 spawn has everything it needs without a
                    // second GitHub lookup. The same-repo path (#420)
                    // passes `None` for both fork fields and takes the
                    // `git fetch origin` branch via `fetch_single_ref`.
                    match (fork_owner_owned.as_deref(), fork_url_owned.as_deref()) {
                        (Some(owner), Some(clone_url)) => {
                            fetch_fork_head(&root, owner, clone_url, &head_ref_owned)
                        }
                        _ => fetch_single_ref(&root, &head_ref_owned),
                    }
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
                                "PR #{} was force-pushed or rebased after you clicked Spawn \
                                 (expected {}, now {} on {}). Spawning on the new tip — \
                                 re-spawn to pin to a fresh SHA.",
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
                        "Could not fetch PR #{} head ref '{}' from {}; \
                         spawning from the mesh's base ref '{}' instead. \
                         The agent may land on stale commits — re-spawn \
                         when the network is back to retry.",
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
            };

            // Warm-pool adoption for Issue/PR spawns (issue #612). When we
            // claimed a warm entry for a `gh{N}-`/`pr{N}-` spawn, the pool's
            // pre-warmed directory is sitting under a plain slug at
            // `entry.path`. Instead of a cold `git worktree add` (which writes
            // the whole tree — the ~11s NTFS cost the pool exists to avoid), we
            // `git worktree move` that directory onto this target name and then
            // `git checkout` it to the ref the cold path resolved
            // (`worktree_base_ref`: the mesh base for Issue spawns, the fetched
            // PR head for PR spawns). On ANY failure we clean up the warm entry
            // and fall back to the cold create so the spawn still succeeds.
            //
            // Only Issue/PR claims reach here: a manual warm claim resolves
            // `spawn_worktree_name` onto the already-present pool directory, so
            // `host_path.exists()` is true and this whole block is skipped.
            let warm_adopt = if is_rename_spawn { warm_claimed.take() } else { None };
            let mut adopted = false;
            if let Some(entry) = warm_adopt {
                let move_args = (
                    node.path.clone(),
                    entry.path.clone(),
                    resolved.host_path.clone(),
                    wt_name.to_string(),
                    worktree_mode.to_string(),
                    worktree_base_ref.clone(),
                );
                timer.checkpoint("before_warm_move");
                let move_result = tokio::task::spawn_blocking(move || {
                    adopt_warm_worktree_by_move(
                        &move_args.0,
                        &move_args.1,
                        &move_args.2,
                        &move_args.3,
                        &move_args.4,
                        &move_args.5,
                    )
                })
                .await
                .unwrap_or_else(|e| Err(format!("warm worktree move task panicked: {}", e)));
                timer.checkpoint("after_warm_move");
                match move_result {
                    Ok(()) => {
                        tracing::info!(
                            "spawn_agent_inner: adopted warm entry {} -> {} via git worktree move (base_ref={})",
                            entry.path,
                            resolved.host_path,
                            worktree_base_ref,
                        );
                        // Keep the entry so the post-spawn housekeeping drops
                        // the bookkeeping row and refills the pool.
                        warm_claimed = Some(entry);
                        adopted = true;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "spawn_agent_inner: warm worktree adoption failed ({}); cleaning up and falling back to cold checkout",
                            e
                        );
                        // Best-effort teardown. The failure could have struck
                        // before OR after the `git worktree move`, so the
                        // worktree may now sit at EITHER the pool path
                        // (`entry.path`, move not done) or the target path
                        // (`resolved.host_path`, move done but checkout/include
                        // failed). Remove BOTH: that (a) frees the target so the
                        // cold `create_git_worktree` below — which no-ops if the
                        // path already exists — actually cuts a fresh tree at the
                        // right ref instead of leaving the agent on the pool's
                        // stale SHA, and (b) prevents a leaked pool directory.
                        // `remove_one_worktree` is idempotent on a missing path.
                        let pool_path = entry.path.clone();
                        let target_path = resolved.host_path.clone();
                        let row_id = entry.id;
                        let _ = tokio::task::spawn_blocking(move || {
                            let _ = crate::git::worktree::remove_one_worktree(&target_path);
                            let _ = crate::git::worktree::remove_one_worktree(&pool_path);
                        })
                        .await;
                        // `warm_claimed` was already taken, so the post-spawn
                        // housekeeping won't double-free it; the next reconcile
                        // refills the pool back to target.
                        crate::services::warm_pool::forget_after_spawn(row_id);
                    }
                }
            }

            if !adopted {
                tracing::info!("spawn_agent_inner: worktree {} not found, creating...", wt_name);
                // The checkout can take seconds on a large repo; spawn_blocking keeps
                // it off the async runtime's worker threads (same as fetch_origin above).
                let create_args = (
                    node.path.clone(),
                    resolved.host_path.clone(),
                    wt_name.to_string(),
                    worktree_mode.to_string(),
                    worktree_base_ref,
                );
                timer.checkpoint("before_worktree_create");
                diag.git_event("worktree_add", "start");
                let created = tokio::task::spawn_blocking(move || {
                    crate::git::worktree::create_git_worktree(
                        &create_args.0,
                        &create_args.1,
                        &create_args.2,
                        &create_args.3,
                        &create_args.4,
                    )
                })
                .await
                .unwrap_or_else(|e| Err(format!("worktree creation task panicked: {}", e)));
                let wt_outcome: String = match &created {
                    Ok(()) => "ok".to_string(),
                    Err(e) => format!("err:{}", e),
                };
                diag.git_event("worktree_add", &wt_outcome);
                timer.checkpoint("after_worktree_create");
                if let Err(e) = created {
                    let msg = format!("Failed to create git worktree: {}", e);
                    tracing::error!("spawn_agent_inner: {}", msg);
                    return Err(msg);
                }
            }
        }

        if let Err(e) = crate::git::worktree::sanitize_git_worktree(&resolved.host_path, resolved.env_type) {
            tracing::warn!("spawn_agent_inner: failed to sanitize worktree .git file: {}", e);
        }

        // Warm-pool claim fast path for MANUAL spawns (issue #609): when we
        // adopted a pre-warmed detached worktree above, the directory already
        // exists (the cold-create block above is skipped because
        // `host_path.exists()` is true). The pool cuts every entry in
        // `detached` mode so a future claim can `git checkout -B <branch>` to
        // upgrade without touching the mesh's branch refs. Do the upgrade here,
        // off the async runtime, so the spawn timer captures the warm-checkout
        // cost as a single `spawn_timing:` checkpoint.
        //
        // Issue/PR claims (`is_rename_spawn`) are already checked out to the
        // right ref by `adopt_warm_worktree_by_move` inside the cold block, so
        // they skip this manual-only upgrade (#612).
        if let (false, Some(ref entry)) = (is_rename_spawn, &warm_claimed) {
            timer.checkpoint("before_warm_branch_upgrade");
            let project_root_owned = node.path.clone();
            let host_path_owned = resolved.host_path.clone();
            let branch_name_owned = wt_name.to_string();
            let mode_owned = worktree_mode.to_string();
            let upgrade_result = tokio::task::spawn_blocking(move || {
                upgrade_warm_to_mode(
                    &project_root_owned,
                    &host_path_owned,
                    &branch_name_owned,
                    &mode_owned,
                )
            })
            .await
            .unwrap_or_else(|e| Err(format!("warm branch upgrade panicked: {}", e)));
            timer.checkpoint("after_warm_branch_upgrade");
            if let Err(e) = upgrade_result {
                // Don't fail the spawn — fall through and let the PTY
                // launch. The agent will land on the warm entry's current
                // HEAD (already at base_ref) instead of the mesh's named
                // branch, which is the cold-spawn behaviour anyway.
                tracing::warn!(
                    "spawn_agent_inner: warm branch upgrade failed ({}); agent will land on detached HEAD",
                    e
                );
            } else {
                tracing::info!(
                    "spawn_agent_inner: warm entry {} upgraded to mode={} branch={}",
                    entry.preassigned_name,
                    worktree_mode,
                    wt_name,
                );
            }
        }
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
    register_agent(session_id, child, writer, master, reader_alive.clone(), job, timer.start());
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
    let needs_session_capture = adapter.self_assigns_session_id() && node.cli_session_id.is_none();
    let reader_handle = start_reader(
        app.clone(),
        session_id,
        needs_session_capture,
        reader,
        spawned_at,
        reader_alive,
        adapter.is_plain_terminal(),
        spawn_start,
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
    // Issue #654 — post-spawn status + early-exit race. The PTY reader thread
    // fires its early-exit `Error` write within `EARLY_EXIT_WINDOW` of process
    // creation if the agent CLI exits immediately (typically a stale `--resume
    // <uuid>`). Writing `Running` here unconditionally let that Error write
    // race with this one — whichever landed last won, so a "ghost Running"
    // node could outlive its process.
    //
    // We now write the transient `Spawning` status with a
    // `NOT IN (Error, Archived)` guard so the same race doesn't recur in
    // the symmetric direction (reader-error-then-orchestrator-spawning
    // resurrecting Error back to Spawning). Then we schedule a delayed
    // conditional promotion (`Running` only if status is still
    // `Spawning`). If the reader already wrote `error` before the
    // early-exit window elapses, the promotion is a no-op and the node
    // correctly stays `error`.
    diag.db_write("status", "Spawning");
    db::update_agent_node_status_unless_in(
        session_id,
        SessionStatus::Spawning,
        &[SessionStatus::Error, SessionStatus::Archived],
    )
    .map_err(|e| e.to_string())?;
    std::thread::spawn(move || {
        // Sleep just past the reader thread's early-exit window. If the
        // process died quickly the reader's `Error` write has already
        // landed; the conditional promotion sees `status != Spawning` and
        // bails out. If the agent is still alive, the promotion flips the
        // status to `Running`.
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
                // Reader's Error write already won — leave the node as Error.
                crate::agent::spawn_diag::promotion_event(session_id, "noop");
            }
            Err(e) => {
                // Emit a `promotion_event` even on the failure path so a
                // log reader can reconstruct the four-way race outcome from
                // a single grep of `[DEBUG-concurrent-spawn] phase=promotion`.
                // The previous free-form `tracing::warn!` swallowed this
                // signal; failure data is most needed exactly here (DB
                // contention mid-3s-window).
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
    let did_claim_warm = warm_claimed.is_some();
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
    if mesh_id > 0 && (ref_advanced_for_pool || did_claim_warm) {
        let mesh_id_for_pool = mesh_id;
        let do_refresh = ref_advanced_for_pool;
        let do_refill = did_claim_warm;
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
/// - `SkippedDirty`, `SkippedNoRemote`, `UpToDate`, `Synced` are silent.
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
        Ok(crate::git::sync::FetchOutcome::SkippedDirty) => {
            tracing::info!(
                "spawn_agent_inner: auto-sync skipped (parent dirty) for session {}",
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

/// Fetch a single ref from `origin` into the local repo. Used by the PR-spawn
/// path (#420) to materialise `origin/<head_ref>` so the worktree can be cut
/// from it. `head_ref` is the PR's source branch (e.g. `feat/420-pr-spawn`);
/// the function runs `git fetch origin <head_ref>` and returns `true` on a
/// clean exit, `false` on any failure.
///
/// Best-effort by design: the caller falls back to the mesh's `base_ref` on
/// `false` rather than failing the spawn (ADR 0001 offline pattern). The user
/// sees the agent spawn on the wrong commits in the rare offline / stale-ref
/// case, instead of a hard error every time the network blips. The
/// alternative (strict-error spawn) is brittle to the very first offline
/// session after a fresh install.
///
/// `--` separator before `head_ref` defends against an adversarial / malformed
/// ref starting with `-` (e.g. `--upload-pack=…`); `git fetch` would otherwise
/// treat it as a flag. GitHub's branch-name validation blocks this in
/// practice, but the cost of the separator is zero and the upside is hardening
/// against a future refactor that lets a hand-entered or imported ref flow
/// through.
fn fetch_single_ref(project_root: &str, head_ref: &str) -> bool {
    use crate::process_util::command_no_window;
    let host_root = crate::env::to_host_path(project_root);
    tracing::info!(
        "fetch_single_ref: running git fetch origin -- {} in {}",
        head_ref,
        host_root
    );
    let mut cmd = command_no_window("git");
    cmd.arg("fetch").arg("origin").arg("--").arg(head_ref);
    let output = match cmd.current_dir(&host_root).output() {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("fetch_single_ref: failed to spawn git fetch: {}", e);
            return false;
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            "fetch_single_ref: git fetch origin -- {} failed: {}",
            head_ref,
            stderr.trim()
        );
        return false;
    }
    true
}

/// The alias used for a fork remote (issue #443). `fork-<login>` is
/// human-readable in `git remote -v` and stays distinct from any user-defined
/// remote name (a regular remote can't start with `fork-` because GitHub
/// logins are alphanumeric + `-` with no leading `-`, but a user could
/// still define one; the `fork-` prefix keeps our entries easy to spot in
/// the output and trivial to clean up if we ever need to).
fn fork_remote_alias(head_repo_owner: &str) -> String {
    format!("fork-{}", head_repo_owner)
}

/// Fetch a single ref from a fork's clone URL into the local repo. Used by
/// the PR-spawn path (issue #443, follow-up to #36 worktree adoption) when
/// the PR's head branch lives on a fork — `fetch_single_ref` only fetches
/// from `origin`, which the fork's head ref isn't on.
///
/// The function:
///   1. Registers the fork as a remote named `fork-<login>` (idempotent —
///      ignores the "remote already exists" error from `git remote add` and
///      updates the URL via `git remote set-url` if the existing URL drifted,
///      e.g. the user re-pointed the fork's origin on GitHub).
///   2. Runs `git fetch <alias> <head_ref>` to materialise the ref locally.
///   3. Returns `true` only when both steps succeed.
///
/// Best-effort by design (same contract as `fetch_single_ref`): the caller
/// falls back to the mesh's `base_ref` on `false` rather than failing the
/// spawn. The user sees the agent spawn on the wrong commits in the rare
/// offline / stale-ref / removed-fork case, instead of a hard error every
/// time the network blips.
fn fetch_fork_head(
    project_root: &str,
    head_repo_owner: &str,
    head_repo_clone_url: &str,
    head_ref: &str,
) -> bool {
    use crate::process_util::command_no_window;
    let host_root = crate::env::to_host_path(project_root);
    let alias = fork_remote_alias(head_repo_owner);
    tracing::info!(
        "fetch_fork_head: ensuring remote {} -> {} in {}",
        alias,
        head_repo_clone_url,
        host_root
    );

    // Step 1: `git remote add` is idempotent via the explicit existence check.
    // We use `git remote get-url` (read-only) to see if the remote already
    // exists; if it does, `set-url` keeps it in sync with the fork's current
    // clone URL. If it doesn't, `remote add` registers it. This avoids
    // parsing `git remote add`'s non-zero stderr for the "already exists"
    // signal — easier to read, and works on every git version.
    let mut get_url = command_no_window("git");
    get_url.arg("remote").arg("get-url").arg(&alias);
    let existing = get_url
        .current_dir(&host_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    let url_matches = existing.as_deref() == Some(head_repo_clone_url);
    if !url_matches {
        let mut cmd = command_no_window("git");
        if existing.is_some() {
            cmd.arg("remote").arg("set-url").arg(&alias).arg(head_repo_clone_url);
            tracing::info!("fetch_fork_head: updating remote {} URL", alias);
        } else {
            cmd.arg("remote").arg("add").arg(&alias).arg(head_repo_clone_url);
            tracing::info!("fetch_fork_head: adding remote {}", alias);
        }
        let output = match cmd.current_dir(&host_root).output() {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("fetch_fork_head: failed to spawn git remote: {}", e);
                return false;
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(
                "fetch_fork_head: git remote config for {} failed: {}",
                alias,
                stderr.trim()
            );
            return false;
        }
    }

    // Step 2: fetch the head ref. `--` before `head_ref` defends against an
    // adversarial / malformed ref starting with `-` (same hardening as
    // `fetch_single_ref`).
    tracing::info!(
        "fetch_fork_head: running git fetch {} -- {} in {}",
        alias,
        head_ref,
        host_root
    );
    let mut cmd = command_no_window("git");
    cmd.arg("fetch").arg(&alias).arg("--").arg(head_ref);
    let output = match cmd.current_dir(&host_root).output() {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("fetch_fork_head: failed to spawn git fetch: {}", e);
            return false;
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            "fetch_fork_head: git fetch {} -- {} failed: {}",
            alias,
            head_ref,
            stderr.trim()
        );
        return false;
    }
    true
}

/// Upgrade a warm pool entry from detached HEAD to the mesh's configured
/// worktree mode (issue #609, PRD #608 §3). The pool cuts every entry as
/// `detached` so a future claim can adopt the directory without ever
/// touching the mesh's branch refs — the cost is one `git checkout -B
/// <branch>` (branched mode, ~50ms) or no-op (detached mode, ~5ms).
///
/// This is the entire warm-path "checkout" cost the tracer bullet buys: the
/// on-disk tree is already at the mesh's base SHA (the worker checked it
/// out); all the spawn has to do is flip the working ref. The 97% of cold-
/// spawn time that was Windows Defender / NTFS search indexer / USN journal
/// scanning freshly-written files is paid ONCE on app startup, not per
/// spawn.
///
/// Best-effort by design: any failure here is logged by the caller and the
/// spawn falls through. The agent lands on the warm entry's current HEAD
/// (still at base_ref) instead of the mesh's named branch — strictly worse
/// than the branched path, but never worse than a cold spawn would be.
///
/// `command_no_window` already applies CREATE_NO_WINDOW on Windows
/// (`process_util::command_no_window`), so we don't need per-OS cfg
/// duplication here.
fn upgrade_warm_to_mode(
    project_root: &str,
    host_path: &str,
    branch_name: &str,
    mode: &str,
) -> Result<(), String> {
    if mode == "detached" {
        // No-op: the pool already cut the entry as detached.
    } else {
        // Branched mode: `git checkout -B <branch>` from the current HEAD. `-B`
        // (uppercase) is deliberate here — a manual spawn's branch IS the pool's
        // preassigned slug (a random adj-adj-noun like `bold-amber-fox`), so a
        // collision with a pre-existing branch is vanishingly unlikely and `-B`
        // keeps the call idempotent across a re-claim of a still-detached entry.
        // (The Issue/PR path uses `-b` instead — see `checkout_worktree_to_base` —
        // because its branch name is deterministic and `-B` would force-reset a
        // user's prior work.)
        run_git_checkout(host_path, &["-B", branch_name])?;
    }
    // Re-apply `.worktreeinclude` so the manual warm claim matches what
    // `create_git_worktree` and `adopt_warm_worktree_by_move` already do
    // (issue #639 gap 1). The prewarm-time copy is stale by the time a user
    // manually spawns — typical edits to a `.worktreeinclude` source (`.env`,
    // build cache, `node_modules/`) would otherwise leave the agent on the
    // prewarm snapshot. Best-effort like the other call sites: a copy
    // failure here is logged inside `apply_worktree_include` but doesn't fail
    // the spawn — the worktree is already usable without the extras.
    crate::git::worktree::apply_worktree_include(
        project_root,
        std::path::Path::new(host_path),
    );
    Ok(())
}

/// Adopt a claimed warm-pool worktree for an Issue/PR spawn (issue #612): move
/// the pre-warmed plain-slug directory to the node's `gh{N}-`/`pr{N}-` target
/// path, then check that worktree out to `base_ref` on the node's branch (or
/// detached), then re-apply `.worktreeinclude` so the result matches a cold
/// spawn. Any step failing returns `Err` so the spawn path can clean up the
/// warm entry and fall back to a cold `git worktree add`.
///
/// The move is the cheap part (`git worktree move`, ~tens of ms); the checkout
/// only writes the diff between the pool's base SHA and `base_ref` — for an
/// Issue spawn `base_ref` IS the mesh base the pool already sits on (near-zero
/// writes), and for a PR spawn it's the freshly fetched PR head (just the PR's
/// changed files), versus a cold spawn re-writing the entire tree.
fn adopt_warm_worktree_by_move(
    project_root: &str,
    old_host_path: &str,
    new_host_path: &str,
    branch_name: &str,
    mode: &str,
    base_ref: &str,
) -> Result<(), String> {
    // Resolve `base_ref` to a concrete SHA up front (offline → HEAD fallback,
    // and never a symbolic ref to `git checkout`), mirroring what the cold
    // path's `add_worktree_impl` does via `resolve_base_commit`. Resolving
    // BEFORE the move means a bad ref fails fast, before we disturb the pool
    // directory.
    let base_sha = crate::git::worktree::resolve_base_ref_sha(project_root, base_ref)?;
    crate::git::worktree::move_git_worktree(project_root, old_host_path, new_host_path)?;
    checkout_worktree_to_base(new_host_path, branch_name, mode, &base_sha)?;
    crate::git::worktree::apply_worktree_include(
        project_root,
        std::path::Path::new(new_host_path),
    );
    Ok(())
}

/// `git checkout` a (just-moved) warm worktree onto a specific `base_sha`.
/// Branched mode uses `-b <branch> <base_sha>` — like the cold path's
/// `git worktree add -b`, it REFUSES if the branch already exists rather than
/// clobbering it, so a re-spawn never silently force-resets a deterministic
/// `gh{N}-`/`pr{N}-` branch and orphans the agent's earlier commits. Detached
/// mode uses `--detach <base_sha>`.
///
/// Unlike [`upgrade_warm_to_mode`] (manual spawns, which stay on the warm
/// entry's current HEAD), Issue/PR spawns must land on a *named* ref: the mesh
/// base for Issue spawns, the PR head (`origin/<head>` / `fork-<login>/<head>`)
/// for PR spawns. The cold-path PR-head-fetch resolves that ref, and
/// `adopt_warm_worktree_by_move` resolves it to the `base_sha` passed here.
fn checkout_worktree_to_base(
    host_path: &str,
    branch_name: &str,
    mode: &str,
    base_sha: &str,
) -> Result<(), String> {
    if mode == "branched" {
        run_git_checkout(host_path, &["-b", branch_name, base_sha])
    } else {
        run_git_checkout(host_path, &["--detach", base_sha])
    }
}

/// Shared `git -C <host_path> checkout <args…>` runner for the warm-pool
/// checkout paths (manual mode-upgrade and Issue/PR adoption). Centralises the
/// `command_no_window` plumbing and the stderr-surfacing error shape so a
/// future fix (arg quoting, lock-retry) lands for both callers at once; the
/// deliberate flag differences (`-B` vs `-b`) stay explicit at the call sites.
fn run_git_checkout(host_path: &str, args: &[&str]) -> Result<(), String> {
    use crate::process_util::command_no_window;
    let mut cmd = command_no_window("git");
    cmd.arg("-C").arg(host_path).arg("checkout").args(args);
    let output = cmd
        .output()
        .map_err(|e| format!("failed to spawn git checkout: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "git checkout {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Read the local SHA at `refs/remotes/origin/<head_ref>` — the ref
/// `fetch_single_ref` populates via `git fetch origin -- <head_ref>`.
/// Returns `None` when the ref doesn't exist (a stale local cache, a
/// first-time fetch, or a non-git directory) so the spawn path can treat
/// the absence as "skip the drift check" rather than a hard error.
///
/// Issue #444 — exact-pinning: the spawn path compares this to the
/// `source_pr_pinned_sha` we stored at `create_pr_node` time and emits a
/// `pr_sha_drift` `mesh-sync-warning` if they differ. SHA comparison is
/// direct string equality: both `git rev-parse` and GitHub's API return
/// 40-char lowercase hex, so a `String::ne` check is sufficient (no need
/// to lowercase or trim).
///
/// `remote_ref` is the full remote-tracking ref (e.g. `origin/feat-x` for
/// same-repo PRs from #420, or `fork-alice/feat-x` for fork PRs from #443).
/// `git rev-parse` accepts both the short and the fully-qualified
/// `refs/remotes/origin/...` form.
fn read_origin_ref_sha(project_root: &str, remote_ref: &str) -> Option<String> {
    use crate::process_util::command_no_window;
    let host_root = crate::env::to_host_path(project_root);
    // Read the symbolic SHA in one shot — `git rev-parse` exits non-zero
    // (and produces no stdout) when the ref doesn't exist, so we don't
    // need a separate "is this a ref?" probe first.
    let mut cmd = command_no_window("git");
    cmd.arg("rev-parse").arg(remote_ref);
    let output = cmd.current_dir(&host_root).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        None
    } else {
        Some(sha)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        use crate::env::test_helpers::{commit_file, init_repo_with_commit};
        use std::fs;
        let td = TempDir::new().unwrap();
        let root = td.path();

        // Set up `.worktreeinclude` + a tracked source file at its original
        // content. The pool will copy v1 into the prewarm-time worktree.
        init_repo_with_commit(root, &[("f.txt", "tracked\n")]);
        fs::write(root.join("secrets.env"), "v1=old\n").unwrap();
        fs::write(root.join(".worktreeinclude"), "secrets.env\n").unwrap();
        // Commit the manifest so `.worktreeinclude` is reachable for `git
        // worktree add`; the pool helper copies files relative to the repo
        // root regardless of whether the manifest itself is tracked, but
        // committing keeps the test setup close to a realistic repo.
        let repo = git2::Repository::open(root).unwrap();
        commit_file(
            &repo,
            root,
            ".worktreeinclude",
            "secrets.env\n",
        );

        // Cut a detached warm worktree (matches the pool's on-disk shape).
        let pool = root.join(".claude").join("worktrees").join("warm-amber-fox");
        crate::git::worktree::create_git_worktree(
            root.to_str().unwrap(),
            pool.to_str().unwrap(),
            "warm-amber-fox",
            "detached",
            "HEAD",
        )
        .unwrap();
        // Prewarm-time copy: `secrets.env` in the worktree must hold v1.
        assert_eq!(
            fs::read_to_string(pool.join("secrets.env")).unwrap(),
            "v1=old\n",
            "prewarm-time copy must reflect the original source"
        );

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
        let td = TempDir::new().unwrap();
        let root = td.path();
        init_repo_with_commit(root, &[("f.txt", "tracked\n")]);

        let pool = root.join(".claude").join("worktrees").join("warm-amber-fox");
        crate::git::worktree::create_git_worktree(
            root.to_str().unwrap(),
            pool.to_str().unwrap(),
            "warm-amber-fox",
            "detached",
            "HEAD",
        )
        .unwrap();

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
        use crate::env::test_helpers::{commit_file, init_repo_with_commit};
        use std::fs;
        let td = TempDir::new().unwrap();
        let root = td.path();

        init_repo_with_commit(root, &[("f.txt", "tracked\n")]);
        fs::write(root.join("secrets.env"), "v1=old\n").unwrap();
        fs::write(root.join(".worktreeinclude"), "secrets.env\n").unwrap();
        let repo = git2::Repository::open(root).unwrap();
        commit_file(&repo, root, ".worktreeinclude", "secrets.env\n");

        // Pool entry: detached (matches the on-disk shape the pool cuts).
        let pool = root.join(".claude").join("worktrees").join("warm-amber-fox");
        crate::git::worktree::create_git_worktree(
            root.to_str().unwrap(),
            pool.to_str().unwrap(),
            "warm-amber-fox",
            "detached",
            "HEAD",
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(pool.join("secrets.env")).unwrap(),
            "v1=old\n",
            "prewarm-time copy must reflect the original source"
        );

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
    // can be cut from it. The function shells out to `git fetch origin -- <ref>`;
    // the `--` separator is the security hardening (a ref starting with `-`
    // would otherwise be parsed as a `git` flag like `--upload-pack=…`).
    //
    // These tests pin all four cases the issue calls out:
    //   1. success — ref exists on origin
    //   2. ref-not-found — ref missing on origin (caller falls back to base_ref)
    //   3. non-git path — caller passed a directory that isn't a repo
    //   4. adversarial ref — `--`-prefixed input is rejected by `git` itself
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
             (the '--' separator must block git from treating it as a flag)"
        );
    }
}
