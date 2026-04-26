//! Agent spawning and management via PTY

use crate::db;
use crate::models::{EnvType, Provider, SessionStatus};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{command, AppHandle, Emitter};

struct AgentProcess {
    child: Box<dyn portable_pty::Child + Send>,
    writer: Box<dyn std::io::Write + Send>,
    #[allow(dead_code)]
    pair: portable_pty::PtyPair,
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
    tracing::debug!("spawn_agent called: session_id={}, provider={}, resume={:?}", session_id, provider, resume);

    // Parse provider string to Provider enum
    let provider_enum = match provider.as_str() {
        "minimax" => Provider::Minimax,
        "gemini" => Provider::Gemini,
        "opencode" => Provider::OpenCode,
        _ => Provider::Anthropic,
    };

    let session = db::get_session_by_id(session_id)
        .map_err(|e| e.to_string())?;
    let is_wsl = session.env == EnvType::Wsl;

    tracing::debug!("spawn_agent: session path={}, is_wsl={}", session.path, is_wsl);

    // Kill existing agent if running
    kill_agent(session_id).await.ok();

    tracing::debug!("spawn_agent: opening PTY");
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize::default())
        .map_err(|e| format!("failed to open PTY: {}", e))?;
    tracing::debug!("spawn_agent: PTY opened successfully");

    // Build the command based on provider and environment
    let (binary, args): (&str, Vec<&str>) = match provider_enum {
        Provider::Anthropic | Provider::Minimax => {
            // cwrap takes --anthropic or --minimax flag
            let flag = provider_enum.cli_flag();
            let mut args = vec![flag];
            if let Some(ref session_id) = resume {
                args.push("--resume");
                args.push(session_id);
            }
            ("cwrap", args)
        }
        Provider::Gemini | Provider::OpenCode => {
            // These CLIs don't use cwrap flags
            let mut args = vec![];
            if let Some(ref session_id) = resume {
                args.push("--resume");
                args.push(session_id);
            }
            (provider_enum.binary(), args)
        }
    };

    tracing::debug!("spawn_agent: binary={}, args={:?}", binary, args);

    let mut cmd: CommandBuilder = if is_wsl {
        // For WSL, run via wsl.exe with Unix-style path
        let mut c = CommandBuilder::new("wsl.exe");
        c.args(["--cd", &session.path, "--", binary]);
        c.args(args);
        tracing::debug!("spawn_agent: using WSL command");
        c
    } else {
        // On Windows, cwrap.cmd is a batch script that delegates to bash.
        // Use cmd.exe /c to invoke it directly — avoids PATH-shadowing issues.
        // We pass the full .cmd path so cmd.exe finds it without needing .cmd in PATH.
        let use_cmd_shell = matches!(provider_enum, Provider::Anthropic | Provider::Minimax);
        if use_cmd_shell {
            // Use Git bash directly to run cwrap, bypassing cmd.exe entirely.
            // This avoids the .cmd batch script layer and the issues it causes with PTY.
            let bash_path = "C:\\Program Files\\Git\\bin\\bash.exe";
            let mut c = CommandBuilder::new(bash_path);
            c.arg("-c");
            let mut cwrap_cmd = String::from("cwrap");
            for a in &args {
                cwrap_cmd.push(' ');
                cwrap_cmd.push_str(a);
            }
            c.arg(&cwrap_cmd);
            tracing::debug!("spawn_agent: using {} -c {}", bash_path, cwrap_cmd);
            c
        } else {
            let mut c = CommandBuilder::new(binary);
            c.args(args);
            tracing::debug!("spawn_agent: using direct binary");
            c
        }
    };

    cmd.cwd(&session.path);
    tracing::debug!("spawn_agent: spawning command with cwd={}", session.path);

    let child = pair.slave.spawn_command(cmd)
        .map_err(|e| {
            let err_msg = format!("failed to spawn agent: {}", e);
            tracing::error!("{}", err_msg);
            let _ = app.emit("provider-error", serde_json::json!({
                "session_id": session_id,
                "provider": provider,
                "message": err_msg
            }));
            err_msg
        })?;

    tracing::debug!("spawn_agent: child spawned successfully");

    // Get reader and writer for the PTY
    let reader_for_map = pair.master.try_clone_reader()
        .map_err(|e| format!("failed to get PTY reader: {}", e))?;
    let reader_for_thread = pair.master.try_clone_reader()
        .map_err(|e| format!("failed to get PTY reader for thread: {}", e))?;
    let writer = pair.master.take_writer()
        .map_err(|e| format!("failed to get PTY writer: {}", e))?;

    let session_id_for_reader = session_id;

    {
        let mut processes = AGENT_PROCESSES.lock().unwrap();
        processes.insert(session_id, AgentProcess { child, writer, pair });
    }

    tracing::debug!("spawn_agent: starting PTY reader thread");

    // Spawn a task to read agent output and emit events
    let app_for_reader = app.clone();
    std::thread::spawn(move || {
        tracing::debug!("PTY reader thread started for session {}", session_id_for_reader);
        let mut reader = reader_for_thread;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    tracing::debug!("PTY reader EOF for session {}", session_id_for_reader);
                    break; // EOF
                }
                Ok(n) => {
                    let data = String::from_utf8_lossy(&buf[..n]).to_string();
                    if let Err(e) = app_for_reader.emit("agent-output", serde_json::json!({
                        "session_id": session_id_for_reader,
                        "line": data
                    })) {
                        tracing::error!("Failed to emit agent-output: {}", e);
                    }
                }
                Err(e) => {
                    tracing::error!("Agent PTY read error for session {}: {}", session_id_for_reader, e);
                    break;
                }
            }
        }
    });

    db::update_session_status(session_id, SessionStatus::Running)
        .map_err(|e| e.to_string())?;

    tracing::info!("spawn_agent complete: session_id={}, provider={}", session_id, provider);

    Ok(())
}

/// Send input to the running agent (raw keystrokes, no newline)
#[command]
pub async fn write_to_agent(session_id: i64, data: String) -> Result<(), String> {
    let mut processes = AGENT_PROCESSES.lock().unwrap();
    if let Some(ref mut agent) = processes.get_mut(&session_id) {
        use std::io::Write;
        agent.writer.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
        Ok(())
    } else {
        Err("Agent not running".to_string())
    }
}

/// Send input to the running agent (with newline appended)
#[command]
pub async fn send_to_agent(session_id: i64, input: String) -> Result<(), String> {
    let mut processes = AGENT_PROCESSES.lock().unwrap();
    if let Some(ref mut agent) = processes.get_mut(&session_id) {
        use std::io::Write;
        let input_with_newline = format!("{}\n", input);
        agent.writer.write_all(input_with_newline.as_bytes()).map_err(|e| e.to_string())?;
        Ok(())
    } else {
        Err("Agent not running for this session".to_string())
    }
}

/// Kill the running agent for a session
#[command]
pub async fn kill_agent(session_id: i64) -> Result<(), String> {
    let mut processes = AGENT_PROCESSES.lock().unwrap();
    if let Some(mut agent) = processes.remove(&session_id) {
        agent.child.kill().ok();
    }
    db::update_session_status(session_id, SessionStatus::Idle)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Check if agent is running for a session
#[command]
pub async fn is_agent_running(session_id: i64) -> bool {
    let processes = AGENT_PROCESSES.lock().unwrap();
    processes.contains_key(&session_id)
}
