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

    // Kill existing agent if running
    kill_agent(session_id).await.ok();

    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize::default())
        .map_err(|e| format!("failed to open PTY: {}", e))?;

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

    let mut cmd: CommandBuilder = if is_wsl {
        // For WSL, run via wsl.exe with Unix-style path
        let mut c = CommandBuilder::new("wsl.exe");
        c.args(["--cd", &session.path, "--", binary]);
        c.args(args);
        c
    } else {
        // On Windows, cwrap is a .cmd batch script — must use cmd.exe (fully qualified to avoid MSYS2 path resolution)
        let use_cmd_shell = matches!(provider_enum, Provider::Anthropic | Provider::Minimax);
        if use_cmd_shell {
            let mut c = CommandBuilder::new("C:\\Windows\\System32\\cmd.exe");
            c.arg("/c");
            c.arg("cwrap");
            for a in &args {
                c.arg(a);
            }
            c
        } else {
            let mut c = CommandBuilder::new(binary);
            c.args(args);
            c
        }
    };

    cmd.cwd(&session.path);

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
        processes.insert(session_id, AgentProcess { child, writer });
    }

    // Spawn a task to read agent output and emit events
    let app_for_reader = app.clone();
    std::thread::spawn(move || {
        let mut reader = reader_for_thread;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF
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
                    tracing::error!("Agent PTY read error: {}", e);
                    break;
                }
            }
        }
    });

    db::update_session_status(session_id, SessionStatus::Running)
        .map_err(|e| e.to_string())?;

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
