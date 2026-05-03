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
use tauri::{command, Emitter};
use tauri::AppHandle;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use std::sync::atomic::{AtomicBool, Ordering};

const TEST_SERVER_PORT: u16 = 1991;
const TEST_SERVER_ADDR: &str = "::"; // Kept as documentation; actual binding uses IpAddr::new

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
    // Windows requires explicit binding to each address family.
    // We bind to 0.0.0.0 (IPv4) first, then spawn a separate task for [::] (IPv6).
    use std::net::{IpAddr, SocketAddr};

    let ipv4_addr: IpAddr = "0.0.0.0".parse().unwrap();
    let ipv6_addr: IpAddr = "::".parse().unwrap();
    let addr_ipv4 = SocketAddr::new(ipv4_addr, TEST_SERVER_PORT);
    let addr_ipv6 = SocketAddr::new(ipv6_addr, TEST_SERVER_PORT);

    tauri::async_runtime::spawn(async move {
        // Primary: IPv4 listener (required for 127.0.0.1 access)
        let listener_ipv4 = match TcpListener::bind(addr_ipv4).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("[test_server] Failed to bind IPv4 on port {}: {}", TEST_SERVER_PORT, e);
                TEST_SERVER_RUNNING.store(false, Ordering::SeqCst);
                return;
            }
        };

        TEST_SERVER_RUNNING.store(true, Ordering::SeqCst);
        tracing::info!("[test_server] HTTP test server listening on http://0.0.0.0:{} (IPv4)", TEST_SERVER_PORT);

        // Optional: also listen on IPv6 for localhost over IPv6
        let app_ipv6 = app_handle.clone();
        let _ = TcpListener::bind(addr_ipv6).await.map(|listener_ipv6| {
            tracing::info!("[test_server] HTTP test server also listening on http://[::]:{} (IPv6)", TEST_SERVER_PORT);
            tauri::async_runtime::spawn(async move {
                loop {
                    match listener_ipv6.accept().await {
                        Ok((mut stream, client_addr)) => {
                            let app = app_ipv6.clone();
                            tauri::async_runtime::spawn(async move {
                                handle_connection(&mut stream, client_addr, &app).await;
                            });
                        }
                        Err(e) => tracing::warn!("[test_server] IPv6 accept error: {}", e),
                    }
                }
            });
        });

        // Main loop: serve IPv4 connections
        loop {
            match listener_ipv4.accept().await {
                Ok((mut stream, client_addr)) => {
                    let app = app_handle.clone();
                    tauri::async_runtime::spawn(async move {
                        handle_connection(&mut stream, client_addr, &app).await;
                    });
                }
                Err(e) => tracing::warn!("[test_server] IPv4 accept error: {}", e),
            }
        }
    });
}

async fn handle_connection(stream: &mut tokio::net::TcpStream, client_addr: SocketAddr, app: &AppHandle) {
    let mut buf = vec![0u8; 16384];
    let n = match stream.read(&mut buf).await {
        Ok(n) if n > 0 => n,
        _ => return,
    };

    let request = String::from_utf8_lossy(&buf[..n]);
    let response = process_request(&request, app);

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

fn process_request(request: &str, app: &AppHandle) -> String {
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
            "create_session" => handle_create_session(&rpc_req.args, app.clone()),
            "list_projects" => handle_list_projects(),
            "list_sessions" => handle_list_sessions(),
            "get_session" => handle_get_session(&rpc_req.args),
            "spawn_agent" => handle_spawn_agent(&rpc_req.args, app),
            "kill_agent" => handle_kill_agent(&rpc_req.args),
            "delete_project" => handle_delete_project(&rpc_req.args),
            "delete_session" => handle_delete_session(&rpc_req.args),
            "archive_session" => handle_archive_session(&rpc_req.args),
            "inject_test_output" => handle_inject_test_output(&rpc_req.args, app.clone()),
            "set_active_session" => handle_set_active_session(&rpc_req.args, app.clone()),
            _ => JsonRpcResponse::error(&format!("Unknown command: {}", rpc_req.cmd)),
        }
    } else if request.starts_with("GET /health") {
        let body = "OK";
        let resp = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: text/plain\r\n\
             Content-Length: {}\r\n\
             Access-Control-Allow-Origin: *\r\n\
             Connection: close\r\n\
             \r\n{}",
            body.len(),
            body
        );
        resp
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

fn handle_create_session(args: &serde_json::Value, app: AppHandle) -> String {
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
        Some(name),
    ) {
        Ok(session) => {
            // Emit event so frontend session store can refetch via invoke()
            let _ = app.emit("session-created", serde_json::json!({ "id": session.id }));
            JsonRpcResponse::success(&session)
        }
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

fn handle_spawn_agent(args: &serde_json::Value, app: &AppHandle) -> String {
    let session_id = args.get("sessionId").and_then(|v| v.as_i64()).unwrap_or(0);
    let provider = args.get("provider").and_then(|v| v.as_str()).unwrap_or("anthropic").to_string();
    let resume = args.get("resume").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from);

    tracing::info!("[test_server] spawn_agent: spawning thread for session_id={}", session_id);

    // Spawn a dedicated thread to avoid panics corrupting the async runtime
    let app_clone = app.clone();
    let result = std::thread::spawn(move || {
        tauri::async_runtime::block_on(crate::commands::agent::spawn_agent(
            app_clone,
            session_id,
            provider,
            resume,
            None,
            None,
        ))
    }).join();

    tracing::info!("[test_server] spawn_agent result: {:?}", result);

    match result {
        Ok(Ok(_)) => {
            tracing::info!("[test_server] spawn_agent returning success");
            JsonRpcResponse::success(&serde_json::json!({ "session_id": session_id }))
        }
        Ok(Err(e)) => {
            tracing::error!("[test_server] spawn_agent error: {}", e);
            JsonRpcResponse::error(&e)
        }
        Err(e) => {
            tracing::error!("[test_server] spawn_agent panicked: {:?}", e);
            JsonRpcResponse::error(&format!("spawn_agent panicked: {:?}", e))
        }
    }
}

fn handle_kill_agent(args: &serde_json::Value) -> String {
    let session_id = args.get("sessionId").and_then(|v| v.as_i64()).unwrap_or(0);

    let result = tauri::async_runtime::block_on(crate::commands::agent::kill_agent(session_id));

    match result {
        Ok(_) => JsonRpcResponse::success(&serde_json::json!({ "session_id": session_id })),
        Err(e) => JsonRpcResponse::error(&e),
    }
}

/// Inject fake terminal output into a session for testing purposes.
/// This bypasses the PTY and directly emits 'agent-output' events.
fn handle_inject_test_output(args: &serde_json::Value, app: AppHandle) -> String {
    let session_id = args.get("sessionId").and_then(|v| v.as_i64()).unwrap_or(0);
    let lines = args.get("lines").and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).map(String::from).collect::<Vec<_>>())
        .unwrap_or_else(|| vec!["Hello from test output!\n".to_string()]);

    tracing::info!("[test_server] inject_test_output: session_id={} lines={}", session_id, lines.len());

    for line in &lines {
        let _ = app.emit("agent-output", serde_json::json!({
            "session_id": session_id,
            "line": line
        }));
    }

    JsonRpcResponse::success(&serde_json::json!({
        "session_id": session_id,
        "lines_injected": lines.len()
    }))
}

fn handle_delete_project(args: &serde_json::Value) -> String {
    let project_id = args.get("projectId").and_then(|v| v.as_i64()).unwrap_or(0);

    match crate::db::delete_project(project_id) {
        Ok(_) => JsonRpcResponse::success(&serde_json::json!({ "project_id": project_id })),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_delete_session(args: &serde_json::Value) -> String {
    let session_id = args.get("sessionId").and_then(|v| v.as_i64()).unwrap_or(0);

    match crate::db::delete_session(session_id) {
        Ok(_) => JsonRpcResponse::success(&serde_json::json!({ "session_id": session_id })),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_archive_session(args: &serde_json::Value) -> String {
    let session_id = args.get("sessionId").and_then(|v| v.as_i64()).unwrap_or(0);

    match crate::db::archive_session(session_id) {
        Ok(_) => JsonRpcResponse::success(&serde_json::json!({ "session_id": session_id })),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_set_active_session(args: &serde_json::Value, app: AppHandle) -> String {
    let session_id = args.get("sessionId").and_then(|v| v.as_i64()).unwrap_or(0);

    tracing::info!("[test_server] set_active_session: session_id={}", session_id);

    // Emit a frontend event that the session store listens to
    let _ = app.emit("session-activated", serde_json::json!({ "session_id": session_id }));

    JsonRpcResponse::success(&serde_json::json!({ "session_id": session_id }))
}