//! Bridge that lets the frontend forward `console.*`, `window.error`, and
//! `unhandledrejection` into the same tracing pipeline as the backend so they
//! land in `buildmesh.log`. Without this the WebView2 console is invisible to
//! anyone debugging headlessly.

use tauri::command;

#[command]
pub async fn log_frontend(level: String, message: String) {
    // Cap to avoid runaway log floods if the frontend dumps something huge.
    let truncated = if message.len() > 8192 {
        format!("{}…<truncated {} bytes>", &message[..8192], message.len() - 8192)
    } else {
        message
    };

    match level.as_str() {
        "error" => tracing::error!(target: "frontend", "{}", truncated),
        "warn" => tracing::warn!(target: "frontend", "{}", truncated),
        "info" => tracing::info!(target: "frontend", "{}", truncated),
        "debug" => tracing::debug!(target: "frontend", "{}", truncated),
        other => tracing::info!(target: "frontend", "[level={}] {}", other, truncated),
    }
}
