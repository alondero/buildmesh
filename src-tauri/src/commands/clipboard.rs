//! Clipboard access via native OS APIs.
//!
//! On macOS 14+, calling navigator.clipboard.readText() from a WKWebView triggers
//! a system clipboard-permission popup. Routing reads through this Rust command
//! (using pbpaste) bypasses that popup entirely.

use tauri::command;

// Offloaded via `run_blocking` (issue #762 convention): `pbpaste` is a
// subprocess we *wait on* (`.output()`), which would otherwise park a
// bounded tokio worker for the whole clipboard read.
#[command]
pub async fn read_clipboard() -> Result<String, String> {
    crate::commands::run_blocking("read_clipboard", read_clipboard_blocking).await
}

/// Sync core for [`read_clipboard`].
fn read_clipboard_blocking() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("pbpaste")
            .output()
            .map_err(|e| format!("pbpaste failed: {}", e))?;
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Signal JS to fall back to navigator.clipboard.readText()
        Err("not-macos".to_string())
    }
}
