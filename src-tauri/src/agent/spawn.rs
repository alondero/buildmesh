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
use crate::models::{Provider, SessionStatus};
use portable_pty::{native_pty_system, CommandBuilder, PtyPair, PtySize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Emitter;

/// Parse provider string into Provider enum.
pub fn parse_provider(provider: &str) -> Provider {
    Provider::from_db_str(provider)
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
    let mut recipe = adapter.spawn_recipe(Platform::current());

    match session_id_mode {
        SessionIdMode::Assign(id) => {
            tracing::info!("spawn_agent: assigning session-id {}", id);
            recipe.base_args.push("--session-id".to_string());
            recipe.base_args.push(id.clone());
        }
        SessionIdMode::Resume(id) => {
            tracing::info!("spawn_agent: resuming session {}", id);
            recipe.base_args.push("--resume".to_string());
            recipe.base_args.push(id.clone());
        }
        SessionIdMode::None => {}
    }

    if adapter.supports_model_override() {
        if let Some(model) = model_override.filter(|s| !s.is_empty()) {
            recipe.base_args.push("--model".to_string());
            recipe.base_args.push(model.to_string());
        }
        if let Some(effort) = effort_override.filter(|s| !s.is_empty()) {
            recipe.base_args.push("--effort".to_string());
            recipe.base_args.push(effort.to_string());
        }
    }

    if adapter.supports_prefill() {
        if let Some(text) = prefill.filter(|s| !s.is_empty()) {
            recipe.base_args.push("--prefill".to_string());
            recipe.base_args.push(text.to_string());
        }
    }

    spawn_environment::wrap(recipe, resolved.env_type, &resolved.spawn_path, session_id)
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

    let hook_command = format!(
        "curl -sf -X POST http://localhost:{}/api/attention/$BUILDMESH_SESSION_ID || true",
        crate::http_server::HTTP_PORT_DEFAULT,
    );

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
) {
    PROCESS_REGISTRY.insert(
        session_id,
        AgentProcess {
            child: Arc::new(Mutex::new(child)),
            writer: Arc::new(Mutex::new(writer)),
            master: Arc::new(Mutex::new(master)),
            reader_alive,
        },
    );
}

/// Start the PTY reader thread.
fn start_reader(
    app: tauri::AppHandle,
    session_id: i64,
    reader: Box<dyn std::io::Read + Send>,
    spawned_at: std::time::Instant,
    reader_alive: Arc<AtomicBool>,
) {
    let app_clone = app;
    let reader_alive_clone = reader_alive;

    std::thread::spawn(move || {
        let mut r = reader;
        let mut buf = [0u8; 8192];
        loop {
            match r.read(&mut buf) {
                Ok(0) => {
                    tracing::debug!("PTY EOF received for session {}, reader exiting", session_id);
                    break;
                }
                Ok(n) => {
                    let data = String::from_utf8_lossy(&buf[..n]).to_string();

                    crate::session_naming::on_output(session_id, &data);

                    let _ = app_clone.emit(
                        "agent-output",
                        serde_json::json!({
                            "session_id": session_id,
                            "line": data
                        }),
                    );

                    // Forward to any connected mobile WebSocket clients
                    crate::http_server::send_pty_output(session_id, data.into_bytes());
                }
                Err(e) => {
                    tracing::error!("PTY read error for session {}: {}", session_id, e);
                    break;
                }
            }
        }
        reader_alive_clone.store(false, Ordering::SeqCst);

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

        tracing::debug!("PTY reader thread exited for session {}", session_id);
    });
}

// ---------------------------------------------------------------------------
// Public Tauri command interface
// ---------------------------------------------------------------------------

/// Inner implementation shared by spawn_agent command and auto_resume_sessions.
pub async fn spawn_agent_inner(
    app: &tauri::AppHandle,
    session_id: i64,
    provider: String,
    resume: Option<String>,
    rows: u16,
    cols: u16,
    prefill: Option<String>,
) -> Result<(), String> {
    tracing::info!(
        "spawn_agent_inner: session_id={}, provider={}, resume={:?}, size={}x{}",
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

    // 3. Get node and resolve paths
    let node = db::get_agent_node_by_id(session_id).map_err(|e| {
        let err = format!("spawn_agent: failed to get agent node {}: {}", session_id, e);
        tracing::error!("{}", err);
        err
    })?;
    tracing::info!("spawn_agent_inner: node path={}, env={:?}", node.path, node.env);

    let provider_enum = parse_provider(&provider);
    let adapter = provider_enum.adapter();

    // 4. Determine session ID mode
    let session_id_mode = if adapter.supports_resume() {
        match resume {
            Some(ref id) if !id.is_empty() => SessionIdMode::Resume(id.clone()),
            _ => {
                let cli_uuid = uuid::Uuid::new_v4().to_string();
                db::update_cli_session_id(session_id, &cli_uuid).map_err(|e| e.to_string())?;
                tracing::info!("spawn_agent_inner: assigned cli_session_id={}", cli_uuid);
                SessionIdMode::Assign(cli_uuid)
            }
        }
    } else {
        SessionIdMode::None
    };

    // 5. Read mesh config for use_worktree / model / effort / worktree_mode
    let config = env::read_mesh_spawn_config(&std::path::PathBuf::from(&node.path));
    let use_worktree = config.as_ref().map(|c| c.use_worktree).unwrap_or(true);
    let model_override = config.as_ref().and_then(|c| c.model.as_deref());
    let effort_override = config.as_ref().and_then(|c| c.effort.as_deref());
    let worktree_mode = config
        .as_ref()
        .and_then(|c| c.worktree_mode.as_deref())
        .unwrap_or("detached");

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
        if worktree_mode == "branched" {
            match env::check_source_branch_clean(&node.path) {
                Ok(true) => {}
                Ok(false) => {
                    return Err("Cannot create branched worktree: source branch has uncommitted changes. Commit or discard changes first, or use detached mode.".to_string());
                }
                Err(e) => {
                    tracing::warn!("spawn_agent_inner: failed to check source branch cleanliness: {}", e);
                    return Err("Cannot create branched worktree: failed to verify source branch is clean. Check that git is available and the repository is valid.".to_string());
                }
            }
        }

        let host_path = std::path::Path::new(&resolved.host_path);
        if !host_path.exists() {
            tracing::info!("spawn_agent_inner: worktree {} not found, creating...", wt_name);
            if let Err(e) = env::create_git_worktree(&node.path, &resolved.host_path, wt_name, worktree_mode)
            {
                let msg = format!("Failed to create git worktree: {}", e);
                tracing::error!("spawn_agent_inner: {}", msg);
                return Err(msg);
            }
        }

        if let Err(e) = env::sanitize_git_worktree(&resolved.host_path, resolved.env_type) {
            tracing::warn!("spawn_agent_inner: failed to sanitize worktree .git file: {}", e);
        }
    }

    // 8. Open PTY
    tracing::debug!("spawn_agent_inner: opening PTY system");
    let pair = open_pty_pair(rows, cols)?;

    // 9. Build and spawn command
    let cmd = build_spawn_command(
        &resolved,
        provider_enum,
        &session_id_mode,
        session_id,
        model_override,
        effort_override,
        prefill.as_deref(),
    );

    let child = spawn_child(&pair, cmd).map_err(|e| {
        let err_msg = e.clone();
        let _ = app.emit(
            "provider-error",
            serde_json::json!({
                "session_id": session_id,
                "provider": provider,
                "message": err_msg
            }),
        );
        e
    })?;

    tracing::info!("spawn_agent_inner: process spawned successfully");

    // 10. Setup IO and register
    let reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let master = pair.master;
    let reader_alive = Arc::new(AtomicBool::new(true));

    tracing::info!("spawn_agent_inner: storing agent process for session {}", session_id);
    register_agent(session_id, child, writer, master, reader_alive.clone());
    tracing::info!("spawn_agent_inner: stored agent process");

    // 11. Inject attention hook
    if adapter.requires_attention_hook() {
        inject_attention_hook(std::path::Path::new(&resolved.host_path));
    }

    // 12. Start reader thread
    let spawned_at = std::time::Instant::now();
    tracing::debug!("spawn_agent_inner: starting reader thread for session {}", session_id);
    crate::http_server::ensure_pty_channel(session_id);
    start_reader(app.clone(), session_id, reader, spawned_at, reader_alive);

    tracing::info!("spawn_agent_inner: reader thread spawned, updating node status");
    db::update_agent_node_status(session_id, SessionStatus::Running).map_err(|e| e.to_string())?;
    tracing::info!("spawn_agent_inner: complete");
    Ok(())
}