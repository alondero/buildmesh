//! Agent spawning and management via PTY

use crate::db;
use crate::models::{EnvType, Provider, SessionStatus};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{command, AppHandle, Emitter};

struct AgentProcess {
    child: Box<dyn portable_pty::Child + Send>,
    writer: Box<dyn std::io::Write + Send>,
    #[allow(dead_code)]
    pair: portable_pty::PtyPair,
    reader_alive: Arc<AtomicBool>,
}

static AGENT_PROCESSES: once_cell::sync::Lazy<Arc<Mutex<HashMap<i64, AgentProcess>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

/// Spawn a new agent for the given session with the specified provider
#[command]
pub async fn spawn_agent(
    app: AppHandle,
    session_id: i64,
    provider: String,
    resume: Option<String>,
) -> Result<(), String> {
    tracing::info!("spawn_agent: session_id={}, provider={}, resume={:?}", session_id, provider, resume);

    // 1. Check if already running
    {
        let processes = AGENT_PROCESSES.lock().unwrap();
        if let Some(agent) = processes.get(&session_id) {
            if agent.reader_alive.load(Ordering::SeqCst) {
                tracing::info!("spawn_agent: session {} is already running, skipping spawn", session_id);
                return Ok(());
            }
        }
    }

    // 2. Kill any stale process for this session
    tracing::debug!("spawn_agent: killing stale processes for session {}", session_id);
    kill_agent(session_id).await.ok();

    // 3. Get session info
    let session = db::get_session_by_id(session_id).map_err(|e| {
        let err = format!("spawn_agent: failed to get session {}: {}", session_id, e);
        tracing::error!("{}", err);
        err
    })?;
    let is_wsl = session.env == EnvType::Wsl;
    
    tracing::info!("spawn_agent: session path={}, env={:?}", session.path, session.env);

    let provider_enum = match provider.as_str() {
        "minimax" => Provider::Minimax,
        "gemini" => Provider::Gemini,
        "opencode" => Provider::OpenCode,
        _ => Provider::Anthropic,
    };

    // 4. Open PTY
    tracing::debug!("spawn_agent: opening PTY system");
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize::default())
        .map_err(|e| {
            let err = format!("failed to open PTY: {}", e);
            tracing::error!("{}", err);
            err
        })?;

    // 5. Build command
    let (binary, mut args): (&str, Vec<String>) = match provider_enum {
        Provider::Anthropic | Provider::Minimax => {
            ("cwrap", vec![provider_enum.cli_flag().to_string()])
        }
        _ => (provider_enum.binary(), vec![])
    };

    if let Some(ref res_id) = resume {
        if !res_id.is_empty() {
            tracing::info!("spawn_agent: adding resume flag for session {}", res_id);
            args.push("--resume".to_string());
            args.push(res_id.clone());
        }
    }

    let mut cmd = if is_wsl {
        tracing::info!("spawn_agent: building WSL command via wsl.exe");
        let mut c = CommandBuilder::new("wsl.exe");
        c.args(["--cd", &session.path, "--", binary]);
        c.args(args);
        c
    } else {
        // Windows direct execution
        let is_cwrap = matches!(provider_enum, Provider::Anthropic | Provider::Minimax);
        if is_cwrap {
            tracing::info!("spawn_agent: building Windows command via cmd.exe /c {}", binary);
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

    // 6. Spawn
    tracing::info!("spawn_agent: spawning process in {}", session.path);
    let mut child = pair.slave.spawn_command(cmd).map_err(|e| {
        let err_msg = format!("failed to spawn agent: {}", e);
        tracing::error!("{}", err_msg);
        let _ = app.emit("provider-error", serde_json::json!({
            "session_id": session_id,
            "provider": provider,
            "message": err_msg
        }));
        err_msg
    })?;

    tracing::info!("spawn_agent: process spawned successfully");

    // 7. Setup IO
    let reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let reader_alive = Arc::new(AtomicBool::new(true));

    {
        let mut processes = AGENT_PROCESSES.lock().unwrap();
        processes.insert(session_id, AgentProcess { 
            child, 
            writer, 
            pair, 
            reader_alive: reader_alive.clone() 
        });
    }

    // 8. Start reader thread
    let app_clone = app.clone();
    let reader_alive_clone = reader_alive.clone();
    tracing::debug!("spawn_agent: starting reader thread for session {}", session_id);
    std::thread::spawn(move || {
        let mut r = reader;
        let mut buf = [0u8; 8192];
        let session_id_regex = regex::Regex::new(r"session[_-]id[:\s]+([a-zA-Z0-9\-_]+)").unwrap();
        
        loop {
            match r.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let data = String::from_utf8_lossy(&buf[..n]).to_string();
                    
                    // Try to capture session ID if not already known
                    if let Some(caps) = session_id_regex.captures(&data) {
                        if let Some(cli_id) = caps.get(1) {
                            let cli_id_str = cli_id.as_str().to_string();
                            let _ = db::update_cli_session_id(session_id, &cli_id_str);
                        }
                    }

                    let _ = app_clone.emit("agent-output", serde_json::json!({
                        "session_id": session_id,
                        "line": data
                    }));
                }
                Err(_) => break,
            }
        }
        reader_alive_clone.store(false, Ordering::SeqCst);
        tracing::debug!("PTY reader thread exited for session {}", session_id);
    });

    db::update_session_status(session_id, SessionStatus::Running).map_err(|e| e.to_string())?;
    Ok(())
}

#[command]
pub async fn write_to_agent(session_id: i64, data: String) -> Result<(), String> {
    let mut processes = AGENT_PROCESSES.lock().unwrap();
    if let Some(agent) = processes.get_mut(&session_id) {
        use std::io::Write;
        agent.writer.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
        agent.writer.flush().map_err(|e| e.to_string())?;
        Ok(())
    } else {
        Err("Agent not running".to_string())
    }
}

#[command]
pub async fn send_to_agent(session_id: i64, input: String) -> Result<(), String> {
    write_to_agent(session_id, format!("{}\n", input)).await
}

#[command]
pub async fn kill_agent(session_id: i64) -> Result<(), String> {
    let mut processes = AGENT_PROCESSES.lock().unwrap();
    if let Some(mut agent) = processes.remove(&session_id) {
        agent.child.kill().ok();
        agent.reader_alive.store(false, Ordering::SeqCst);
    }
    db::update_session_status(session_id, SessionStatus::Idle).map_err(|e| e.to_string())?;
    Ok(())
}

#[command]
pub async fn is_agent_running(session_id: i64) -> bool {
    let processes = AGENT_PROCESSES.lock().unwrap();
    processes.get(&session_id)
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
    let processes = AGENT_PROCESSES.lock().unwrap();
    processes.iter().map(|(id, agent)| {
        AgentDebugState {
            session_id: *id,
            is_alive: agent.reader_alive.load(Ordering::SeqCst),
        }
    }).collect()
}
