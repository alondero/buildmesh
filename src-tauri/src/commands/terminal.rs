//! Terminal / PTY management using portable-pty

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{command, AppHandle, Emitter};

struct PtyInstance {
    _child: Box<dyn portable_pty::Child + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Option<Box<dyn std::io::Write + Send>>,
}

static PTY_INSTANCES: once_cell::sync::Lazy<Arc<Mutex<HashMap<String, PtyInstance>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

/// Spawn a PTY output reader that emits events to the frontend
fn spawn_pty_output_reader(app: AppHandle, pty_id: String, reader: Box<dyn std::io::Read + Send>) {
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    let data = String::from_utf8_lossy(&buf[..n]).to_string();
                    if let Err(e) = app.emit("pty-output", serde_json::json!({
                        "pty_id": pty_id,
                        "data": data
                    })) {
                        tracing::error!("Failed to emit pty-output: {}", e);
                    }
                }
                Err(e) => {
                    tracing::error!("PTY read error: {}", e);
                    break;
                }
            }
        }
    });
}

/// Spawn a PTY and run a command
#[command]
pub fn spawn_pty(
    app: AppHandle,
    command: String,
    args: Vec<String>,
    cwd: String,
    pty_id: String,
) -> Result<(), String> {
    let pty_system = native_pty_system();

    let pair = pty_system
        .openpty(PtySize::default())
        .map_err(|e| e.to_string())?;

    let master = pair.master;
    let mut cmd = CommandBuilder::new(&command);
    cmd.args(&args);
    cmd.cwd(&cwd);
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| e.to_string())?;

    // Get reader and writer for the PTY
    let reader = master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = master.take_writer().map_err(|e| e.to_string())?;

    let pty_id_clone = pty_id.clone();

    {
        let mut instances = PTY_INSTANCES.lock().unwrap();
        instances.insert(pty_id_clone.clone(), PtyInstance {
            _child: child,
            master,
            writer: Some(writer),
        });
    }

    // Spawn the output reader thread
    spawn_pty_output_reader(app, pty_id_clone, reader);

    Ok(())
}

/// Write to a PTY
#[command]
pub fn write_pty(pty_id: String, data: String) -> Result<(), String> {
    let mut instances = PTY_INSTANCES.lock().unwrap();
    let pty = instances.get_mut(&pty_id).ok_or("PTY not found")?;

    if let Some(ref mut writer) = pty.writer {
        use std::io::Write;
        writer.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
        Ok(())
    } else {
        Err("PTY writer not available".to_string())
    }
}

#[command]
pub fn resize_pty(pty_id: String, rows: u16, cols: u16) -> Result<(), String> {
    let mut instances = PTY_INSTANCES.lock().unwrap();
    let pty = instances.get_mut(&pty_id).ok_or("PTY not found")?;

    pty.master.resize(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }).map_err(|e| e.to_string())?;

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
    app: AppHandle,
    pty_id: String,
    is_wsl: bool,
    cwd: String,
) -> Result<(), String> {
    let (cmd, args) = if is_wsl {
        ("wsl.exe".to_string(), vec!["--cd".to_string(), cwd.clone()])
    } else {
        ("cmd.exe".to_string(), vec![])
    };

    spawn_pty(app, cmd, args, cwd, pty_id)
}
