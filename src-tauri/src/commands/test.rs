//! HTTP test server for Playwright E2E tests
//!
//! Problem: Playwright connects via HTTP to the Vite dev server, but `invoke()`
//! from `@tauri-apps/api/core` requires `window.__TAURI_INTERNALS__` which is only
//! set in a Tauri webview. HTTP requests never get this global.
//!
//! Solution: This module spawns a minimal tokio TCP server on port 1991 that
//! receives JSON-RPC-like calls and directly calls the underlying db/command
//! functions. Playwright tests use `fetch('http://localhost:1991/invoke', ...)`
//! to exercise the real Rust backend without needing Tauri APIs in the browser.

use serde::{Deserialize, Serialize};
use tauri::command;
use tauri::AppHandle;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use std::sync::atomic::{AtomicBool, Ordering};

const TEST_SERVER_PORT: u16 = 1991;
const TEST_SERVER_ADDR: &str = "127.0.0.1";

static TEST_SERVER_RUNNING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Serialize, Deserialize)]
struct JsonRpcRequest {
    cmd: String,
    args: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    ok: bool,
    data: Option<serde_json::Value>,
    error: Option<String>,
}

impl JsonRpcResponse {
    fn success<T: Serialize>(data: &T) -> String {
        let data = serde_json::to_value(data).unwrap();
        serde_json::to_string(&JsonRpcResponse { ok: true, data: Some(data), error: None }).unwrap()
    }

    fn error(msg: &str) -> String {
        serde_json::to_string(&JsonRpcResponse { ok: false, data: None, error: Some(msg.to_string()) }).unwrap()
    }
}

#[command]
pub fn is_test_server_running() -> bool {
    TEST_SERVER_RUNNING.load(Ordering::SeqCst)
}

/// Start the HTTP test server. Uses `tauri::async_runtime::spawn` so the task
/// runs on the Tauri-managed tokio runtime, not the global one.
pub fn start_test_server(app_handle: AppHandle) {
    let addr: SocketAddr = format!("{}:{}", TEST_SERVER_ADDR, TEST_SERVER_PORT).parse().unwrap();

    tauri::async_runtime::spawn(async move {
        let listener = match TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("[test_server] Failed to bind on port {}: {}", TEST_SERVER_PORT, e);
                TEST_SERVER_RUNNING.store(false, Ordering::SeqCst);
                return;
            }
        };

        TEST_SERVER_RUNNING.store(true, Ordering::SeqCst);
        tracing::info!("[test_server] HTTP test server listening on http://{}:{}", TEST_SERVER_ADDR, TEST_SERVER_PORT);

        loop {
            match listener.accept().await {
                Ok((mut stream, client_addr)) => {
                    let app = app_handle.clone();
                    tauri::async_runtime::spawn(async move {
                        handle_connection(&mut stream, client_addr, &app).await;
                    });
                }
                Err(e) => {
                    tracing::warn!("[test_server] Accept error: {}", e);
                }
            }
        }
    });
}

async fn handle_connection(stream: &mut tokio::net::TcpStream, client_addr: SocketAddr, _app: &AppHandle) {
    let mut buf = vec![0u8; 16384];
    let n = match stream.read(&mut buf).await {
        Ok(n) if n > 0 => n,
        _ => return,
    };

    let request = String::from_utf8_lossy(&buf[..n]);
    let response = process_request(&request);

    let resp = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Connection: close\r\n\
         \r\n{}",
        response.len(),
        response
    );

    if let Err(e) = stream.write_all(resp.as_bytes()).await {
        tracing::warn!("[test_server] Failed to send response to {}: {}", client_addr, e);
    }
}

fn process_request(request: &str) -> String {
    // Route based on request line
    if request.starts_with("POST /invoke") {
        let body = match request.find("\r\n\r\n") {
            Some(pos) => request[pos + 4..].trim(),
            None => return JsonRpcResponse::error("No request body"),
        };

        let rpc_req: JsonRpcRequest = match serde_json::from_str(body) {
            Ok(r) => r,
            Err(e) => return JsonRpcResponse::error(&format!("Invalid JSON: {}", e)),
        };

        tracing::debug!("[test_server] Handling: cmd={}", rpc_req.cmd);

        match rpc_req.cmd.as_str() {
            "create_test_project" => handle_create_test_project(&rpc_req.args),
            "create_session" => handle_create_session(&rpc_req.args),
            "list_projects" => handle_list_projects(),
            "list_sessions" => handle_list_sessions(),
            "get_session" => handle_get_session(&rpc_req.args),
            _ => JsonRpcResponse::error(&format!("Unknown command: {}", rpc_req.cmd)),
        }
    } else if request.starts_with("GET /health") {
        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK".to_string()
    } else {
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_string()
    }
}

fn handle_create_test_project(args: &serde_json::Value) -> String {
    let name = args.get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Test Project");

    match crate::db::create_project(name, &std::env::temp_dir().to_string_lossy()) {
        Ok(project) => JsonRpcResponse::success(&project),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_create_session(args: &serde_json::Value) -> String {
    let project_id = args.get("projectId").and_then(|v| v.as_i64()).unwrap_or(0);
    let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("Session");
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("/tmp");
    let branch = args.get("branch").and_then(|v| v.as_str()).unwrap_or("main");

    use crate::models::{EnvType, Provider};

    match crate::db::create_session(
        project_id,
        name,
        path,
        branch,
        EnvType::Windows,
        Provider::Anthropic,
    ) {
        Ok(session) => JsonRpcResponse::success(&session),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_list_projects() -> String {
    match crate::db::list_projects() {
        Ok(projects) => JsonRpcResponse::success(&projects),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_list_sessions() -> String {
    match crate::db::list_sessions() {
        Ok(sessions) => JsonRpcResponse::success(&sessions),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_get_session(args: &serde_json::Value) -> String {
    let session_id = args.get("sessionId")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    match crate::db::get_session_by_id(session_id) {
        Ok(session) => JsonRpcResponse::success(&session),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}