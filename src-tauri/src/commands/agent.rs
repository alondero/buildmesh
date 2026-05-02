//! Agent spawning and management via PTY

use crate::db;
use crate::models::{EnvType, Provider, SessionStatus};
use portable_pty::{native_pty_system, CommandBuilder, PtyPair, PtySize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{command, AppHandle, Emitter};

struct AgentProcess {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn std::io::Write + Send>,
    #[allow(dead_code)]
    pair: portable_pty::PtyPair,
    reader_alive: Arc<AtomicBool>,
}

/// Thread-safe registry for agent processes.
/// Replaces the global AGENT_PROCESSES HashMap.
struct ProcessRegistry {
    processes: HashMap<i64, AgentProcess>,
}

impl ProcessRegistry {
    fn new() -> Self {
        Self {
            processes: HashMap::new(),
        }
    }

    fn get(&self, session_id: &i64) -> Option<&AgentProcess> {
        self.processes.get(session_id)
    }

    fn insert(&mut self, session_id: i64, agent: AgentProcess) {
        self.processes.insert(session_id, agent);
    }

    fn get_mut(&mut self, session_id: &i64) -> Option<&mut AgentProcess> {
        self.processes.get_mut(session_id)
    }

    fn remove(&mut self, session_id: &i64) -> Option<AgentProcess> {
        self.processes.remove(session_id)
    }

    fn contains(&self, session_id: &i64) -> bool {
        self.processes.contains_key(session_id)
    }
}

static PROCESS_REGISTRY: once_cell::sync::Lazy<Arc<Mutex<ProcessRegistry>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(ProcessRegistry::new())));

static LAST_INPUT_TIME: once_cell::sync::Lazy<Arc<Mutex<HashMap<i64, std::time::Instant>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));


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

/// Get the session and determine if it's WSL.
fn get_session_and_env(session_id: &i64) -> Result<(crate::models::Session, EnvType), String> {
    let session = db::get_session_by_id(*session_id).map_err(|e| {
        let err = format!("spawn_agent: failed to get session {}: {}", session_id, e);
        tracing::error!("{}", err);
        err
    })?;
    let is_wsl = session.env;
    Ok((session, is_wsl))
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
fn open_pty_pair() -> Result<PtyPair, String> {
    let pty_system = native_pty_system();
    pty_system
        .openpty(PtySize::default())
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
    session: &crate::models::Session,
    is_wsl: EnvType,
    provider_enum: Provider,
    session_id_mode: &SessionIdMode,
) -> CommandBuilder {
    let is_macos = cfg!(target_os = "macos");

    let (binary, mut args): (&str, Vec<String>) = if is_macos {
        match provider_enum {
            Provider::Anthropic => ("claude", vec![]),
            _ => (provider_enum.binary(), vec![]),
        }
    } else {
        match provider_enum {
            Provider::Anthropic | Provider::Minimax => {
                ("cwrap", vec![provider_enum.cli_flag().to_string()])
            }
            _ => (provider_enum.binary(), vec![]),
        }
    };

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

    let mut cmd = if is_wsl == EnvType::Wsl {
        tracing::info!("spawn_agent: building WSL command via wsl.exe");
        let mut c = CommandBuilder::new("wsl.exe");
        c.args(["--cd", &session.path, "--", binary]);
        c.args(args);
        c
    } else if is_macos {
        tracing::info!("spawn_agent: building macOS command for {}", binary);
        let mut c = CommandBuilder::new(binary);
        c.args(args);
        c
    } else {
        // Windows direct execution
        let is_cwrap = matches!(provider_enum, Provider::Anthropic | Provider::Minimax);
        if is_cwrap {
            tracing::info!(
                "spawn_agent: building Windows command via cmd.exe /c {}",
                binary
            );
            let mut c = CommandBuilder::new("C:\\Windows\\System32\\cmd.exe");
            c.arg("/c");
            c.arg(binary);
            c.args(args);
            c
        } else {
            tracing::info!("spawn_agent: building direct binary command for {}", binary);
            let mut c = CommandBuilder::new(binary);
            c.args(args);
            c
        }
    };

    cmd.cwd(&session.path);
    cmd
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
    pair: portable_pty::PtyPair,
    reader_alive: Arc<AtomicBool>,
) {
    let mut registry = PROCESS_REGISTRY.lock().unwrap();
    registry.insert(
        session_id,
        AgentProcess {
            child,
            writer,
            pair,
            reader_alive,
        },
    );
}

/// Start the PTY reader thread.
fn start_reader(app: AppHandle, session_id: i64, reader: Box<dyn std::io::Read + Send>, spawned_at: std::time::Instant, provider: crate::models::Provider) {
    let app_clone = app;
    let reader_alive = Arc::new(AtomicBool::new(true));
    let reader_alive_clone = reader_alive.clone();

    std::thread::spawn(move || {
        let mut r = reader;
        let mut buf = [0u8; 8192];
        let mut detector = crate::turn_detector::TurnDetector::new(provider);
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

                    crate::session_namer::append_output(session_id, &data);

                    if detector.contains_prompt(&data) {
                        let suppress = LAST_INPUT_TIME.lock().unwrap()
                            .get(&session_id)
                            .map(|t| t.elapsed() < std::time::Duration::from_secs(3))
                            .unwrap_or(false);

                        if !suppress {
                            crate::session_namer::record_turn(session_id, app_clone.clone());
                            let _ = db::update_session_status(session_id, SessionStatus::AwaitingInput);
                            let _ = app_clone.emit("attention-needed", serde_json::json!({
                                "session_id": session_id
                            }));
                        }
                    }

                    let _ = app_clone.emit(
                        "agent-output",
                        serde_json::json!({
                            "session_id": session_id,
                            "line": data
                        }),
                    );
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
            tracing::warn!("Session {} reader exited after {:?} — likely resume failure", session_id, elapsed);
            let _ = db::update_session_status(session_id, SessionStatus::Error);
            let _ = app_clone.emit("resume-failed", serde_json::json!({
                "session_id": session_id,
                "error": "Agent exited immediately after spawn — session may have expired"
            }));
        } else {
            let _ = db::update_session_status(session_id, SessionStatus::Idle);
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
) -> Result<(), String> {
    tracing::info!(
        "spawn_agent_inner: session_id={}, provider={}, resume={:?}",
        session_id,
        provider,
        resume
    );

    // 1. Check if already running
    if is_agent_already_running(&session_id) {
        return Ok(());
    }

    // 2. Kill any stale process for this session
    tracing::debug!("spawn_agent_inner: killing stale processes for session {}", session_id);
    kill_agent(session_id).await.ok();

    // 3. Get session info
    let (session, is_wsl) = get_session_and_env(&session_id)?;
    tracing::info!("spawn_agent_inner: session path={}, env={:?}", session.path, session.env);

    let provider_enum = parse_provider(&provider);

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

    // 5. Open PTY
    tracing::debug!("spawn_agent_inner: opening PTY system");
    let pair = open_pty_pair()?;

    // 6. Build command
    let cmd = build_spawn_command(&session, is_wsl, provider_enum, &session_id_mode);

    // 7. Spawn
    tracing::info!("spawn_agent_inner: spawning process in {}", session.path);
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
    let reader_alive = Arc::new(AtomicBool::new(true));

    tracing::info!("spawn_agent_inner: storing agent process for session {}", session_id);
    register_agent(session_id, child, writer, pair, reader_alive.clone());
    tracing::info!("spawn_agent_inner: stored agent process");

    // 9. Start reader thread with spawn timestamp for early-exit detection
    let spawned_at = std::time::Instant::now();
    tracing::debug!("spawn_agent_inner: starting reader thread for session {}", session_id);
    start_reader(app.clone(), session_id, reader, spawned_at, provider_enum);

    tracing::info!(
        "spawn_agent_inner: reader thread spawned, updating session status"
    );
    db::update_session_status(session_id, SessionStatus::Running).map_err(|e| e.to_string())?;
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
) -> Result<(), String> {
    spawn_agent_inner(&app, session_id, provider, resume).await
}

/// Auto-resume all suspended sessions that have a stored CLI session ID.
/// Called by the frontend on startup after event listeners are ready.
#[command]
pub async fn auto_resume_sessions(app: AppHandle) -> Result<Vec<i64>, String> {
    let sessions = db::list_suspended_sessions().map_err(|e| e.to_string())?;

    if sessions.is_empty() {
        tracing::info!("auto_resume_sessions: no suspended sessions to resume");
        return Ok(vec![]);
    }

    tracing::info!("auto_resume_sessions: resuming {} sessions", sessions.len());
    let mut resumed: Vec<i64> = Vec::new();

    for session in &sessions {
        let cli_id = match &session.cli_session_id {
            Some(id) if !id.is_empty() => id.clone(),
            _ => {
                tracing::warn!("auto_resume_sessions: session {} has no cli_session_id, skipping", session.id);
                db::update_session_status(session.id, SessionStatus::Idle).ok();
                continue;
            }
        };

        if session.provider != Provider::Anthropic && session.provider != Provider::Minimax {
            tracing::info!("auto_resume_sessions: skipping non-cwrap session {} ({:?})", session.id, session.provider);
            db::update_session_status(session.id, SessionStatus::Idle).ok();
            continue;
        }

        let provider_str = session.provider.to_string();
        match spawn_agent_inner(&app, session.id, provider_str, Some(cli_id)).await {
            Ok(()) => {
                resumed.push(session.id);
                tracing::info!("auto_resume_sessions: resumed session {}", session.id);
            }
            Err(e) => {
                tracing::error!("auto_resume_sessions: failed to resume session {}: {}", session.id, e);
                db::update_session_status(session.id, SessionStatus::Error).ok();
                let _ = app.emit("resume-failed", serde_json::json!({
                    "session_id": session.id,
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
    for (id, mut agent) in registry.processes.drain() {
        agent.child.kill().ok();
        agent.reader_alive.store(false, Ordering::SeqCst);
        tracing::info!("kill_all_agents: killed agent for session {}", id);
    }
}

#[command]
pub async fn write_to_agent(app: AppHandle, session_id: i64, data: String) -> Result<(), String> {
    let mut registry = PROCESS_REGISTRY.lock().unwrap();
    if let Some(agent) = registry.get_mut(&session_id) {
        use std::io::Write;
        agent.writer.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
        agent.writer.flush().map_err(|e| e.to_string())?;
        if data.contains('\n') || data.contains('\r') {
            LAST_INPUT_TIME.lock().unwrap().insert(session_id, std::time::Instant::now());
            db::update_session_status(session_id, SessionStatus::Running).ok();
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
    let mut registry = PROCESS_REGISTRY.lock().unwrap();
    if let Some(mut agent) = registry.remove(&session_id) {
        agent.child.kill().ok();
        agent.reader_alive.store(false, Ordering::SeqCst);
    }
    db::update_session_status(session_id, SessionStatus::Idle).map_err(|e| e.to_string())?;
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