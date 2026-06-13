//! Agent spawning — the `spawn_agent_inner` function and its helpers.
//!
//! Provider-specific recipe lives behind `Provider::adapter()` (see `agent/provider`).
//! OS-specific wrapping lives in `spawn_environment`.
//! PTY-specific helpers (open_pty_pair, spawn_child) live in `process.rs`.

use crate::agent::process::{AgentProcess, PROCESS_REGISTRY};
use crate::agent::provider::Platform;
use crate::agent::spawn_environment;
use crate::db;
use crate::env;
use crate::models::{AgentNode, EnvType, Provider, SessionStatus};
use base64::Engine;
use portable_pty::{native_pty_system, CommandBuilder, PtyPair, PtySize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Emitter;

/// Default `worktree_mode` when the mesh config leaves it unset. The UI
/// default in `MeshPropertiesPanel.tsx` and the Rust constant below must
/// agree (paired-constants pattern — see [[feedback_cross-language-default-coupling]]).
/// See `docs/knowledge-primer.md` (Worktree Support) for the branched-vs-detached rationale.
pub const DEFAULT_WORKTREE_MODE: &str = "branched";

/// Binary name the cwrap provider launcher resolves to (Anthropic/Minimax/Kimi).
/// Mirrors the `binary: "cwrap"` declared in those adapters' spawn recipes; kept
/// here as the single point the prefill-transport gate matches against.
const CWRAP_BINARY: &str = "cwrap";

/// Environment variable carrying prefill text to a cwrap-launched provider.
///
/// cwrap reads `$BUILDMESH_PREFILL` and forwards it to `claude --prefill`. We use
/// the environment rather than a CLI arg because on Windows the cwrap providers
/// are launched through `cwrap.cmd` → `cmd.exe`, whose command line is truncated
/// at the first newline — so a multi-line prefill passed as an argv element loses
/// everything after line one (the exact "only the first line is pre-filled"
/// symptom). The process environment block is inherited intact by every shell
/// layer, so the full text survives. See `build_spawn_command`.
pub const PREFILL_ENV_VAR: &str = "BUILDMESH_PREFILL";

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
/// A bare carriage return reaching an agent's TUI input (notably cwrap → ConPTY
/// on Windows) is interpreted as Enter, submitting the prompt after the first
/// line — so an issue-seeded agent only ever sees its first line. macOS (`claude`
/// spawned directly) tolerates CRLF, which is why this only bit Windows.
fn normalize_prefill_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Build the spawn command by composing the provider's recipe with the runtime environment.
pub fn build_spawn_command(
    resolved: &env::ResolvedPath,
    provider_enum: Provider,
    session_id_mode: &SessionIdMode,
    session_id: i64,
    model_override: Option<&str>,
    effort_override: Option<&str>,
    prefill: Option<&str>,
) -> CommandBuilder {
    let adapter = provider_enum.adapter();
    let platform = Platform::current();

    let mut recipe = match session_id_mode {
        SessionIdMode::Resume(id) => {
            if let Some(resume_recipe) = adapter.spawn_recipe_for_resume(platform, id) {
                tracing::info!("spawn_agent: using resume recipe for session {}", id);
                resume_recipe
            } else {
                let mut r = adapter.spawn_recipe(platform);
                let args = adapter.resume_args(id);
                if !args.is_empty() {
                    tracing::info!("spawn_agent: resuming session {}", id);
                    r.base_args.extend(args);
                }
                r
            }
        }
        SessionIdMode::Assign(id) => {
            let mut r = adapter.spawn_recipe(platform);
            let args = adapter.session_assign_args(id);
            if !args.is_empty() {
                tracing::info!("spawn_agent: assigning session-id {}", id);
                r.base_args.extend(args);
            }
            r
        }
        SessionIdMode::None => adapter.spawn_recipe(platform),
    };

    if adapter.supports_model_override() {
        if let Some(model) = model_override.filter(|s| !s.is_empty()) {
            recipe.base_args.extend(adapter.model_args(model));
        }
        if let Some(effort) = effort_override.filter(|s| !s.is_empty()) {
            recipe.base_args.extend(adapter.effort_args(effort));
        }
    }

    // Prefill transport. cwrap providers launched on a non-WSL host receive the
    // prefill through `$BUILDMESH_PREFILL` rather than a `--prefill` CLI arg,
    // because the `cwrap.cmd` → cmd.exe launcher on Windows truncates a
    // multi-line argv at the first newline (see PREFILL_ENV_VAR). WSL keeps the
    // CLI arg: `wsl.exe` passes multi-line argv through intact, and a Windows env
    // var does not cross into the WSL environment without `WSLENV`. Direct
    // `claude`/`codex` spawns (macOS, Codex) also stay on the CLI arg.
    let mut prefill_via_env: Option<String> = None;
    if adapter.supports_prefill() {
        if let Some(text) = prefill.filter(|s| !s.is_empty()) {
            let normalized = normalize_prefill_newlines(text);
            if recipe.binary == CWRAP_BINARY && resolved.env_type != EnvType::Wsl {
                prefill_via_env = Some(normalized);
            } else {
                recipe.base_args.extend(adapter.prefill_args(&normalized));
            }
        }
    }

    let mut cmd =
        spawn_environment::wrap(recipe, resolved.env_type, &resolved.spawn_path, session_id);
    if let Some(text) = prefill_via_env {
        cmd.env(PREFILL_ENV_VAR, text);
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

/// Ensures the Notification hook exists in `{project}/.claude/settings.local.json`.
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

    let expected_hooks = serde_json::json!({
        "Notification": [{
            "matcher": "idle_prompt",
            "hooks": [{
                "type": "command",
                "command": hook_command
            }]
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
fn register_agent(
    session_id: i64,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn std::io::Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    reader_alive: Arc<AtomicBool>,
    job: Option<crate::process_util::JobHandle>,
) {
    PROCESS_REGISTRY.insert(
        session_id,
        AgentProcess {
            child: Arc::new(Mutex::new(child)),
            writer: Arc::new(Mutex::new(writer)),
            master: Arc::new(Mutex::new(master)),
            reader_alive,
            job,
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

/// Start the PTY reader thread.
fn start_reader(
    app: tauri::AppHandle,
    session_id: i64,
    needs_session_capture: bool,
    reader: Box<dyn std::io::Read + Send>,
    spawned_at: std::time::Instant,
    reader_alive: Arc<AtomicBool>,
    is_plain_terminal: bool,
) {
    let app_clone = app;
    let reader_alive_clone = reader_alive;
    let session_captured = AtomicBool::new(!needs_session_capture);

    std::thread::spawn(move || {
        pump_pty_output(reader, |data| {
            let text = String::from_utf8_lossy(data);
            crate::session_naming::on_output(session_id, &text);

            if !session_captured.load(Ordering::Relaxed) {
                if let Some(uuid) = crate::session_capture::try_extract_session_id(&text) {
                    let _ = db::update_cli_session_id(session_id, uuid);
                    session_captured.store(true, Ordering::Relaxed);
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
            // Detect early exit (likely a failed --resume)
            let elapsed = spawned_at.elapsed();
            if elapsed < std::time::Duration::from_secs(3) {
                tracing::warn!(
                    "Node {} reader exited after {:?} — likely resume failure",
                    session_id,
                    elapsed
                );
                let _ = db::update_agent_node_status(session_id, SessionStatus::Error);
                let _ = app_clone.emit(
                    "resume-failed",
                    serde_json::json!({
                        "session_id": session_id,
                        "error": "Agent exited immediately after spawn — session may have expired"
                    }),
                );
            } else {
                let _ = db::update_agent_node_status(session_id, SessionStatus::Idle);
            }
        }

        tracing::debug!("PTY reader thread exited for session {}", session_id);
    });
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

    // 1. Check if already running
    if is_agent_already_running(&session_id) {
        return Ok(());
    }

    // 2. Kill any stale process for this session
    tracing::debug!("spawn_agent_inner: killing stale processes for session {}", session_id);
    crate::commands::agent::kill_agent(session_id).await.ok();

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
                    SessionIdMode::Assign(cli_uuid)
                }
            }
        }
    } else {
        SessionIdMode::None
    };

    // 5. Read mesh config for use_worktree / model / effort / worktree_mode
    let config = env::read_mesh_config(&std::path::PathBuf::from(&node.path));
    let use_worktree = config.as_ref().map(|c| c.use_worktree).unwrap_or(true);
    let model_override = config.as_ref().and_then(|c| c.model.as_deref());
    let effort_override = config.as_ref().and_then(|c| c.effort.as_deref());
    let worktree_mode = config
        .as_ref()
        .and_then(|c| c.worktree_mode.as_deref())
        .unwrap_or(DEFAULT_WORKTREE_MODE);
    let base_ref = config
        .as_ref()
        .and_then(|c| c.base_ref.as_deref())
        .unwrap_or("origin/main");

    // 6. Compute spawn path
    let spawn_worktree_name = if use_worktree {
        node.worktree_name.as_deref()
    } else {
        tracing::info!("spawn_agent_inner: use_worktree=false, using repo root directly");
        None
    };

    let resolved = env::resolve_agent_path(&node.path, spawn_worktree_name);
    tracing::info!(
        "spawn_agent_inner: resolved spawn_path={}, host_path={}, env={:?}",
        resolved.spawn_path,
        resolved.host_path,
        resolved.env_type
    );

    // 7. Create worktree if needed
    if let Some(wt_name) = spawn_worktree_name {
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
            emit_sync_outcome_event(app, session_id, &node.path, sync_result);

            tracing::info!("spawn_agent_inner: worktree {} not found, creating...", wt_name);
            // The checkout can take seconds on a large repo; spawn_blocking keeps
            // it off the async runtime's worker threads (same as fetch_origin above).
            let create_args = (
                node.path.clone(),
                resolved.host_path.clone(),
                wt_name.to_string(),
                worktree_mode.to_string(),
                base_ref.to_string(),
            );
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
            if let Err(e) = created {
                let msg = format!("Failed to create git worktree: {}", e);
                tracing::error!("spawn_agent_inner: {}", msg);
                return Err(msg);
            }
        }

        if let Err(e) = crate::git::worktree::sanitize_git_worktree(&resolved.host_path, resolved.env_type) {
            tracing::warn!("spawn_agent_inner: failed to sanitize worktree .git file: {}", e);
        }
    }

    // 8. Open PTY
    tracing::debug!("spawn_agent_inner: opening PTY system");
    let pair = open_pty_pair(rows, cols)?;

    // 9. Build and spawn command
    let cmd = build_spawn_command(
        &resolved,
        provider,
        &session_id_mode,
        session_id,
        model_override,
        effort_override,
        prefill.as_deref(),
    );

    let child = spawn_child(&pair, cmd).inspect_err(|e| {
        let _ = app.emit(
            "provider-error",
            serde_json::json!({
                "session_id": session_id,
                "provider": provider,
                "message": e
            }),
        );
    })?;

    tracing::info!("spawn_agent_inner: process spawned successfully");

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

    // 10. Setup IO and register
    let reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let master = pair.master;
    let reader_alive = Arc::new(AtomicBool::new(true));

    tracing::info!("spawn_agent_inner: storing agent process for session {}", session_id);
    register_agent(session_id, child, writer, master, reader_alive.clone(), job);
    tracing::info!("spawn_agent_inner: stored agent process");

    // 11. Inject attention hook
    crate::agent::workspace_trust::ensure_trusted(&resolved);
    if adapter.requires_attention_hook() {
        inject_attention_hook(std::path::Path::new(&resolved.host_path));
    }

    // 12. Start reader thread
    let spawned_at = std::time::Instant::now();
    tracing::debug!("spawn_agent_inner: starting reader thread for session {}", session_id);
    crate::http_server::ensure_pty_channel(session_id);
    let needs_session_capture = adapter.self_assigns_session_id() && node.cli_session_id.is_none();
    start_reader(
        app.clone(),
        session_id,
        needs_session_capture,
        reader,
        spawned_at,
        reader_alive,
        adapter.is_plain_terminal(),
    );

    tracing::info!("spawn_agent_inner: reader thread spawned, updating node status");
    db::update_agent_node_status(session_id, SessionStatus::Running).map_err(|e| e.to_string())?;
    tracing::info!("spawn_agent_inner: complete");
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
                    "session_id": session_id,
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
                    "session_id": session_id,
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
                    "session_id": session_id,
                    "mesh_path": mesh_path,
                    "outcome": "fetch_failed",
                    "message": message,
                }),
            )
        }
    };
    let _ = app.emit(event_name, payload);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the spawn-time fallback. Mirrors the TS constant
    /// `DEFAULT_WORKTREE_MODE` exported from `MeshPropertiesPanel.tsx`; the
    /// two are coupled by convention, not by code.
    #[test]
    fn default_worktree_mode_is_branched() {
        assert_eq!(DEFAULT_WORKTREE_MODE, "branched");
    }
}
