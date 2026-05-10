//! Agent spawning and management via PTY

use crate::db;
use crate::env;
use crate::models::{EnvType, Provider, SessionStatus};
use portable_pty::{native_pty_system, CommandBuilder, PtyPair, PtySize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{command, AppHandle, Emitter};

struct AgentProcess {
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    writer: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
    master: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    reader_alive: Arc<AtomicBool>,
}

/// Thread-safe registry for agent processes.
struct ProcessRegistry {
    processes: HashMap<i64, Arc<AgentProcess>>,
}

impl ProcessRegistry {
    fn new() -> Self {
        Self {
            processes: HashMap::new(),
        }
    }

    fn get(&self, session_id: &i64) -> Option<Arc<AgentProcess>> {
        self.processes.get(session_id).cloned()
    }

    fn insert(&mut self, session_id: i64, agent: AgentProcess) {
        self.processes.insert(session_id, Arc::new(agent));
    }

    fn remove(&mut self, session_id: &i64) -> Option<Arc<AgentProcess>> {
        self.processes.remove(session_id)
    }

    fn contains(&self, session_id: &i64) -> bool {
        self.processes.contains_key(session_id)
    }
}

pub(crate) static PROCESS_REGISTRY: once_cell::sync::Lazy<Arc<Mutex<ProcessRegistry>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(ProcessRegistry::new())));


// ---------------------------------------------------------------------------
// Helper functions for spawn_agent
// ---------------------------------------------------------------------------

/// Check if an agent is already running for this session.
fn is_agent_already_running(session_id: &i64) -> bool {
    let registry = PROCESS_REGISTRY.lock().unwrap();
    if let Some(agent) = registry.get(session_id) {
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

/// Parse provider string into Provider enum.
fn parse_provider(provider: &str) -> Provider {
    match provider {
        "minimax" => Provider::Minimax,
        "gemini" => Provider::Gemini,
        "opencode" => Provider::OpenCode,
        _ => Provider::Anthropic,
    }
}

/// Open a PTY pair using the native PTY system.
fn open_pty_pair(rows: u16, cols: u16) -> Result<PtyPair, String> {
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
enum SessionIdMode {
    /// Fresh session: pass --session-id <uuid> to Claude
    Assign(String),
    /// Resuming: pass --resume <uuid> to Claude
    Resume(String),
    /// No session ID handling (non-Anthropic providers)
    None,
}

/// Build the spawn command based on provider and environment.
fn build_spawn_command(
    node: &crate::models::AgentNode,
    resolved: &env::ResolvedPath,
    provider_enum: Provider,
    session_id_mode: &SessionIdMode,
    session_id: i64,
    model_override: Option<&str>,
    effort_override: Option<&str>,
) -> CommandBuilder {
    let is_macos = cfg!(target_os = "macos");
    let is_wsl = resolved.env_type == EnvType::Wsl;

    let (binary, mut args): (&str, Vec<String>) = if is_macos {
        match provider_enum {
            Provider::Anthropic => ("claude", vec!["--dangerously-skip-permissions".to_string()]),
            Provider::Gemini => (provider_enum.binary(), vec!["--yolo".to_string()]),
            _ => (provider_enum.binary(), vec![]),
        }
    } else {
        match provider_enum {
            Provider::Anthropic | Provider::Minimax => {
                ("cwrap", vec![provider_enum.cli_flag().to_string()])
            }
            Provider::Gemini => (provider_enum.binary(), vec!["--yolo".to_string()]),
            _ => (provider_enum.binary(), vec![]),
        }
    };

    // Add -w for git worktree support on cwrap providers (Anthropic, Minimax)
    // This creates a dedicated worktree per node, preventing concurrent node conflicts.
    // When worktree_name is set (node's hyphenated name), pass it explicitly so cwrap
    // can find the same node data on resume.
    //
    // On Resume: skip -w if the worktree directory already exists. The worktree was
    // already created during the first spawn (when SessionIdMode::Assign was used).
    // Passing -w on resume causes cwrap to call `git worktree add` again for the same
    // worktree, which fails with "worktree already checked out".
    // We now spawn the process directly inside the worktree directory on resume, so
    // claude will correctly use the worktree as the project root and find its session data.
    let is_cwrap = matches!(provider_enum, Provider::Anthropic | Provider::Minimax);
    if is_cwrap {
        match session_id_mode {
            SessionIdMode::Assign(_) => {
                // Fresh spawn: always pass -w so cwrap creates the worktree
                tracing::info!("spawn_agent: enabling worktree support (-w) for fresh spawn");
                if let Some(ref wt_name) = node.worktree_name {
                    args.push("-w".to_string());
                    args.push(wt_name.clone());
                    tracing::info!("spawn_agent: using explicit worktree name: {}", wt_name);
                } else {
                    // Backward compat: old nodes without worktree_name get -w without explicit name
                    args.push("-w".to_string());
                    tracing::info!("spawn_agent: no explicit worktree name, using auto-generated");
                }
            }
            SessionIdMode::Resume(_) => {
                // Resume: the worktree already exists and we are running directly inside it.
                // We MUST NOT pass -w here, otherwise claude will attempt `git worktree add` again
                // and fail with "worktree already checked out".
                tracing::info!("spawn_agent: omitting -w for resume (running directly in worktree)");
            }
            SessionIdMode::None => {}
        }
    }

    match session_id_mode {
        SessionIdMode::Assign(id) => {
            tracing::info!("spawn_agent: assigning session-id {}", id);
            args.push("--session-id".to_string());
            args.push(id.clone());
        }
        SessionIdMode::Resume(id) => {
            tracing::info!("spawn_agent: resuming session {}", id);
            args.push("--resume".to_string());
            args.push(id.clone());
        }
        SessionIdMode::None => {}
    }

    // Add --model and --effort for cwrap providers when configured
    if is_cwrap {
        if let Some(model) = model_override {
            args.push("--model".to_string());
            args.push(model.to_string());
        }
        if let Some(effort) = effort_override {
            args.push("--effort".to_string());
            args.push(effort.to_string());
        }
    }

    let mut cmd = if is_wsl {
        tracing::info!("spawn_agent: building WSL command via wsl.exe");
        let mut c = CommandBuilder::new("wsl.exe");
        c.args(["--cd", &resolved.spawn_path, "--", binary]);
        c.args(args);
        c
    } else if is_macos {
        tracing::info!("spawn_agent: building macOS command for {}", binary);
        let mut c = CommandBuilder::new(binary);
        c.args(args);
        c
    } else {
        // Windows: cwrap-wrapped providers (Anthropic, Minimax) use cmd.exe /c for
        // ConPTY compatibility and console suppression. Non-cwrap providers (gemini,
        // opencode) are typically Node.js shims that resolve correctly via direct exec.
        if is_cwrap {
            tracing::info!("spawn_agent: building Windows powershell.exe for cwrap provider {}", binary);
            // Use -NoLogo to suppress banner, -Command to run cwrap directly without cmd.exe layer.
            // PowerShell has native ConPTY support and provides better compatibility with
            // modern Claude Code sessions compared to cmd.exe /c wrapping.
            let mut c = CommandBuilder::new("powershell.exe");
            // Build the command line as a single string: "cwrap --minimax -w foo --session-id bar"
            let combined = format!("{} {}", binary, args.join(" "));
            c.args(["-NoLogo", "-Command", &combined]);
            c
        } else {
            tracing::info!("spawn_agent: building Windows direct command for {}", binary);
            let mut c = CommandBuilder::new(binary);
            c.args(args);
            c
        }
    };

    cmd.cwd(&resolved.spawn_path);
    cmd.env("BUILDMESH_SESSION_ID", session_id.to_string());
    cmd.env("BUILDMESH_PORT", crate::http_server::HTTP_PORT.to_string());
    // Fix Windows git worktree limitation: the .git file in worktrees contains a Unix
    // path that Git on Windows can't resolve. Setting GIT_DIR explicitly bypasses it.
    // GIT_WORK_TREE tells git which working tree to operate on.
    cmd.env("GIT_DIR", format!("{}/.git", node.path));
    cmd.env("GIT_WORK_TREE", &resolved.spawn_path);
    cmd
}

/// Ensures the Notification hook exists in `{project}/.claude/settings.local.json`.
/// Uses $BUILDMESH_SESSION_ID env var so one hook works for all agents in the project.
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
        crate::http_server::HTTP_PORT,
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
                tracing::info!(
                    "inject_attention_hook: wrote hook at {:?}",
                    settings_path
                );
            }
        }
        Err(e) => tracing::warn!("inject_attention_hook: serialize failed: {}", e),
    }
}

/// Spawn the child process.
fn spawn_child(pair: &PtyPair, cmd: CommandBuilder) -> Result<Box<dyn portable_pty::Child + Send + Sync>, String> {
    pair.slave
        .spawn_command(cmd)
        .map_err(|e| format!("failed to spawn agent: {}", e))
}

/// Register the agent process in the registry.
fn register_agent(
    session_id: i64,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn std::io::Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    reader_alive: Arc<AtomicBool>,
) {
    let mut registry = PROCESS_REGISTRY.lock().unwrap();
    registry.insert(
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
fn start_reader(app: AppHandle, session_id: i64, reader: Box<dyn std::io::Read + Send>, spawned_at: std::time::Instant) {
    let app_clone = app;
    let reader_alive = Arc::new(AtomicBool::new(true));
    let reader_alive_clone = reader_alive.clone();

    std::thread::spawn(move || {
        let mut r = reader;
        let mut buf = [0u8; 8192];
        loop {
            match r.read(&mut buf) {
                Ok(0) => {
                    tracing::debug!(
                        "PTY EOF received for session {}, reader exiting",
                        session_id
                    );
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
            tracing::warn!("Node {} reader exited after {:?} — likely resume failure", session_id, elapsed);
            let _ = db::update_agent_node_status(session_id, SessionStatus::Error);
            let _ = app_clone.emit("resume-failed", serde_json::json!({
                "session_id": session_id,
                "error": "Agent exited immediately after spawn — session may have expired"
            }));
        } else {
            let _ = db::update_agent_node_status(session_id, SessionStatus::Idle);
        }

        tracing::debug!("PTY reader thread exited for session {}", session_id);
    });
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

/// Inner implementation shared by spawn_agent command and auto_resume_sessions.
async fn spawn_agent_inner(
    app: &AppHandle,
    session_id: i64,
    provider: String,
    resume: Option<String>,
    rows: u16,
    cols: u16,
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
    kill_agent(session_id).await.ok();

    // 3. Get node and resolve paths for the environment
    let node = db::get_agent_node_by_id(session_id).map_err(|e| {
        let err = format!("spawn_agent: failed to get agent node {}: {}", session_id, e);
        tracing::error!("{}", err);
        err
    })?;
    tracing::info!("spawn_agent_inner: node path={}, env={:?}", node.path, node.env);

    let provider_enum = parse_provider(&provider);
    let is_cwrap = matches!(provider_enum, Provider::Anthropic | Provider::Minimax);

    // 4. Determine session ID mode for cwrap-wrapped providers (Anthropic, Minimax)
    // Both use cwrap which forwards --session-id and --resume to the underlying CLI
    let session_id_mode = if provider_enum == Provider::Anthropic || provider_enum == Provider::Minimax {
        match resume {
            Some(ref id) if !id.is_empty() => SessionIdMode::Resume(id.clone()),
            _ => {
                // Fresh session: generate UUID and store it immediately
                let cli_uuid = uuid::Uuid::new_v4().to_string();
                db::update_cli_session_id(session_id, &cli_uuid).map_err(|e| e.to_string())?;
                tracing::info!("spawn_agent_inner: assigned cli_session_id={}", cli_uuid);
                SessionIdMode::Assign(cli_uuid)
            }
        }
    } else {
        SessionIdMode::None
    };

    // Determine the target CWD for spawning.
    // For cwrap providers on a fresh spawn (Assign), cwrap itself creates the worktree via git, 
    // so it MUST be run from the git repo root. 
    // On resume (Resume), the worktree already exists, so we run directly in it.
    // For non-cwrap providers, we always run them directly in the worktree.
    let spawn_worktree_name = if is_cwrap {
        if matches!(session_id_mode, SessionIdMode::Assign(_)) {
            None
        } else {
            node.worktree_name.as_deref()
        }
    } else {
        node.worktree_name.as_deref()
    };

    // Resolve paths: includes worktree subdirectory (if applicable) + environment-aware spawn path
    let resolved = env::resolve_agent_path(&node.path, spawn_worktree_name);
    tracing::info!(
        "spawn_agent_inner: resolved spawn_path={}, host_path={}, env={:?}",
        resolved.spawn_path, resolved.host_path, resolved.env_type
    );

    // If we are setting the CWD to a worktree directory, we MUST verify it exists on disk first.
    // If it doesn't exist (e.g. deleted or failed to create), the shell will silently fall back
    // to the user's home directory and cause confusing "Accessing workspace: C:\Users\..." messages.
    if spawn_worktree_name.is_some() {
        let host_path = std::path::Path::new(&resolved.host_path);
        if !host_path.exists() {
            let msg = format!("Worktree directory not found: {}. It may have been deleted or failed to create previously. Please archive/delete this agent node and create a new one.", resolved.host_path);
            tracing::error!("spawn_agent_inner: {}", msg);
            return Err(msg);
        }
    }

    // 5. Open PTY
    tracing::debug!("spawn_agent_inner: opening PTY system");
    let pair = open_pty_pair(rows, cols)?;

    // 6. Read mesh config for model/effort overrides
    let mesh = db::get_mesh_by_id(node.mesh_id).map_err(|e| e.to_string())?;
    let config = crate::commands::build_run::parse_mesh_config_for_spawn(&std::path::PathBuf::from(&mesh.path));
    let model_override = config.as_ref().and_then(|c| c.model.as_deref());
    let effort_override = config.as_ref().and_then(|c| c.effort.as_deref());

    // 7. Build command
    let cmd = build_spawn_command(&node, &resolved, provider_enum, &session_id_mode, session_id, model_override, effort_override);

    // Log the CWD path for PTY child (this is the key fix from commit 99290c4)
    tracing::info!(
        "spawn_agent_inner: CWD= worktree_name={:?}",
        node.worktree_name
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

    // 8. Setup IO and register
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let master = pair.master;
    let reader_alive = Arc::new(AtomicBool::new(true));

    tracing::info!("spawn_agent_inner: storing agent process for session {}", session_id);
    register_agent(session_id, child, writer, master, reader_alive.clone());
    tracing::info!("spawn_agent_inner: stored agent process");

    // 9. Ensure attention hook exists in project root (idempotent)
    if provider_enum == Provider::Anthropic || provider_enum == Provider::Minimax {
        inject_attention_hook(std::path::Path::new(&node.path));
    }

    // 10. Start reader thread with spawn timestamp for early-exit detection
    let spawned_at = std::time::Instant::now();
    tracing::debug!("spawn_agent_inner: starting reader thread for session {}", session_id);
    // Ensure mobile broadcast channel exists before reader starts
    crate::http_server::ensure_pty_channel(session_id);
    start_reader(app.clone(), session_id, reader, spawned_at);

    tracing::info!(
        "spawn_agent_inner: reader thread spawned, updating node status"
    );
    db::update_agent_node_status(session_id, SessionStatus::Running).map_err(|e| e.to_string())?;
    tracing::info!("spawn_agent_inner: complete");
    Ok(())
}

/// Spawn a new agent for the given session with the specified provider
#[command]
pub async fn spawn_agent(
    app: AppHandle,
    session_id: i64,
    provider: String,
    resume: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<(), String> {
    let r = rows.unwrap_or(24);
    let c = cols.unwrap_or(80);
    spawn_agent_inner(&app, session_id, provider, resume, r, c).await
}

/// Auto-resume all suspended sessions that have a stored CLI session ID.
/// Called by the frontend on startup after event listeners are ready.
#[command]
pub async fn auto_resume_sessions(app: AppHandle) -> Result<Vec<i64>, String> {
    let nodes = db::list_suspended_nodes().map_err(|e| e.to_string())?;

    if nodes.is_empty() {
        tracing::info!("auto_resume_sessions: no suspended sessions to resume");
        return Ok(vec![]);
    }

    tracing::info!("auto_resume_sessions: resuming {} sessions", nodes.len());
    let mut resumed: Vec<i64> = Vec::new();

    for node in &nodes {
        let cli_id = match &node.cli_session_id {
            Some(id) if !id.is_empty() => id.clone(),
            _ => {
                tracing::warn!("auto_resume_sessions: node {} has no cli_session_id, skipping", node.id);
                db::update_agent_node_status(node.id, SessionStatus::Idle).ok();
                continue;
            }
        };

        if node.provider != Provider::Anthropic && node.provider != Provider::Minimax {
            tracing::info!("auto_resume_sessions: skipping non-cwrap node {} ({:?})", node.id, node.provider);
            db::update_agent_node_status(node.id, SessionStatus::Idle).ok();
            continue;
        }

        let provider_str = node.provider.to_string();
        // Default to 80x24 for auto-resume since we don't know the size yet
        match spawn_agent_inner(&app, node.id, provider_str, Some(cli_id), 24, 80).await {
            Ok(()) => {
                resumed.push(node.id);
                tracing::info!("auto_resume_sessions: resumed node {}", node.id);
            }
            Err(e) => {
                tracing::error!("auto_resume_sessions: failed to resume node {}: {}", node.id, e);
                db::update_agent_node_status(node.id, SessionStatus::Error).ok();
                let _ = app.emit("resume-failed", serde_json::json!({
                    "session_id": node.id,
                    "error": e
                }));
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    Ok(resumed)
}

/// Kill all running agent processes. Used during graceful shutdown.
pub fn kill_all_agents() {
    let mut registry = PROCESS_REGISTRY.lock().unwrap();
    for (id, agent) in registry.processes.drain() {
        agent.child.lock().unwrap().kill().ok();
        agent.reader_alive.store(false, Ordering::SeqCst);
        tracing::info!("kill_all_agents: killed agent for session {}", id);
    }
}

#[command]
pub async fn resize_agent(session_id: i64, rows: u16, cols: u16) -> Result<(), String> {
    let agent = {
        let registry = PROCESS_REGISTRY.lock().unwrap();
        registry.get(&session_id)
    };

    if let Some(agent) = agent {
        let master = agent.master.lock().unwrap();
        master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }).map_err(|e| e.to_string())?;
        Ok(())
    } else {
        Err("Agent not running".to_string())
    }
}

#[command]
pub async fn write_to_agent(app: AppHandle, session_id: i64, data: String) -> Result<(), String> {
    let agent = {
        let registry = PROCESS_REGISTRY.lock().unwrap();
        registry.get(&session_id)
    };

    if let Some(agent) = agent {
        {
            let mut writer = agent.writer.lock().unwrap();
            use std::io::Write;
            writer.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
            writer.flush().map_err(|e| e.to_string())?;
        }

        if data.contains('\n') || data.contains('\r') {
            db::update_agent_node_status(session_id, SessionStatus::Running).ok();
            let _ = app.emit("attention-cleared", serde_json::json!({ "session_id": session_id }));
        }
        Ok(())
    } else {
        Err("Agent not running".to_string())
    }
}

#[command]
pub async fn send_to_agent(app: AppHandle, session_id: i64, input: String) -> Result<(), String> {
    write_to_agent(app, session_id, format!("{}\n", input)).await
}

#[command]
pub async fn kill_agent(session_id: i64) -> Result<(), String> {
    let agent = {
        let mut registry = PROCESS_REGISTRY.lock().unwrap();
        registry.remove(&session_id)
    };

    if let Some(agent) = agent {
        agent.child.lock().unwrap().kill().ok();
        agent.reader_alive.store(false, Ordering::SeqCst);
    }
    db::update_agent_node_status(session_id, SessionStatus::Idle).map_err(|e| e.to_string())?;
    Ok(())
}

#[command]
pub async fn is_agent_running(session_id: i64) -> bool {
    let registry = PROCESS_REGISTRY.lock().unwrap();
    registry
        .get(&session_id)
        .map(|a| a.reader_alive.load(Ordering::SeqCst))
        .unwrap_or(false)
}

#[derive(serde::Serialize)]
pub struct AgentDebugState {
    pub session_id: i64,
    pub is_alive: bool,
}

#[command]
pub async fn debug_list_agents() -> Vec<AgentDebugState> {
    let registry = PROCESS_REGISTRY.lock().unwrap();
    registry
        .processes
        .iter()
        .map(|(id, agent)| AgentDebugState {
            session_id: *id,
            is_alive: agent.reader_alive.load(Ordering::SeqCst),
        })
        .collect()
}

/// Snapshot of all relevant state at the time of a crash, for post-mortem diagnosis.
/// Call this via invoke('debug_crash_snapshot') immediately after a crash to get
/// a consistent view of what the backend was doing.
#[derive(serde::Serialize)]
pub struct CrashSnapshot {
    pub process_registry_ids: Vec<i64>,
    pub session_count: usize,
    pub renamed_sessions: usize,
    pub buffers_size_bytes: usize,
    pub turn_counters_entries: usize,
}

#[command]
pub async fn debug_crash_snapshot() -> CrashSnapshot {
    let registry = PROCESS_REGISTRY.lock().unwrap();
    let process_ids: Vec<i64> = registry.processes.keys().copied().collect();

    drop(registry);

    let session_count = db::list_agent_nodes().map(|s| s.len()).unwrap_or(0);
    let renamed_count = crate::session_naming::renamed_sessions_count();
    let buffers_size = crate::session_naming::buffers_size_bytes();
    let turn_counter_count = crate::session_naming::turn_counter_count();

    CrashSnapshot {
        process_registry_ids: process_ids,
        session_count,
        renamed_sessions: renamed_count,
        buffers_size_bytes: buffers_size,
        turn_counters_entries: turn_counter_count,
    }
}