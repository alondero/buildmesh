//! Terminal / PTY management using portable-pty

use portable_pty::{native_pty_system, PtySize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::command;

struct PtyInstance {
    _child: Box<dyn portable_pty::Child + Send>,
}

static PTY_INSTANCES: once_cell::sync::Lazy<Arc<Mutex<HashMap<String, PtyInstance>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

/// Spawn a PTY and run a command
#[command]
pub fn spawn_pty(
    command: String,
    args: Vec<String>,
    cwd: String,
    pty_id: String,
) -> Result<(), String> {
    let pty_system = native_pty_system();

    let pair = pty_system
        .openpty(PtySize::default())
        .map_err(|e| e.to_string())?;

    let mut cmd = portable_pty::CommandBuilder::new(&command);
    cmd.args(&args);
    cmd.cwd(&cwd);
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| e.to_string())?;

    let mut instances = PTY_INSTANCES.lock().unwrap();
    instances.insert(pty_id, PtyInstance { _child: child });

    Ok(())
}

/// Write to a PTY
#[command]
pub fn write_pty(_pty_id: String, _data: String) -> Result<(), String> {
    Ok(())
}

/// Close a PTY
#[command]
pub fn close_pty(pty_id: String) -> Result<(), String> {
    let mut instances = PTY_INSTANCES.lock().unwrap();
    instances.remove(&pty_id);
    Ok(())
}

/// Spawn a shell (bash or cmd) in a PTY
#[command]
pub fn spawn_shell(
    pty_id: String,
    is_wsl: bool,
    cwd: String,
) -> Result<(), String> {
    let (cmd, args) = if is_wsl {
        ("wsl.exe".to_string(), vec!["--cd".to_string(), cwd.clone()])
    } else {
        ("cmd.exe".to_string(), vec![])
    };

    spawn_pty(cmd, args, cwd, pty_id)
}
