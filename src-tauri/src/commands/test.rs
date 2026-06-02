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
const _TEST_SERVER_ADDR: &str = "::"; // Kept as documentation; actual binding uses IpAddr::new

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

#[allow(dead_code)]
#[command]
pub fn is_test_server_running() -> bool {
    TEST_SERVER_RUNNING.load(Ordering::SeqCst)
}

/// Start the HTTP test server. Uses `tauri::async_runtime::spawn` so the task
/// runs on the Tauri-managed tokio runtime, not the global one.
pub fn start_test_server(app_handle: AppHandle, port_offset: u16) {
    // Windows requires explicit binding to each address family.
    // We bind to 0.0.0.0 (IPv4) first, then spawn a separate task for [::] (IPv6).
    use std::net::{IpAddr, SocketAddr};

    // Dev profile binds 2991 so it never contends with the stable hub's 1991.
    let port = TEST_SERVER_PORT + port_offset;
    let ipv4_addr: IpAddr = "0.0.0.0".parse().unwrap();
    let ipv6_addr: IpAddr = "::".parse().unwrap();
    let addr_ipv4 = SocketAddr::new(ipv4_addr, port);
    let addr_ipv6 = SocketAddr::new(ipv6_addr, port);

    tauri::async_runtime::spawn(async move {
        // Primary: IPv4 listener (required for 127.0.0.1 access)
        let listener_ipv4 = match TcpListener::bind(addr_ipv4).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("[test_server] Failed to bind IPv4 on port {}: {}", port, e);
                TEST_SERVER_RUNNING.store(false, Ordering::SeqCst);
                return;
            }
        };

        TEST_SERVER_RUNNING.store(true, Ordering::SeqCst);
        tracing::info!("[test_server] HTTP test server listening on http://0.0.0.0:{} (IPv4)", port);

        // Optional: also listen on IPv6 for localhost over IPv6
        let app_ipv6 = app_handle.clone();
        let _ = TcpListener::bind(addr_ipv6).await.map(|listener_ipv6| {
            tracing::info!("[test_server] HTTP test server also listening on http://[::]:{} (IPv6)", port);
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
            "get_worktree_close_safety" => handle_get_worktree_close_safety(&rpc_req.args),
            "archive_session" => handle_archive_session(&rpc_req.args),
            "inject_test_output" => handle_inject_test_output(&rpc_req.args, app.clone()),
            "set_active_session" => handle_set_active_session(&rpc_req.args, app.clone()),
            "get_mesh_properties" => handle_get_mesh_properties(&rpc_req.args),
            "update_mesh_field" => handle_update_mesh_field(&rpc_req.args),
            "update_worktree_base_ref" => handle_update_worktree_base_ref(&rpc_req.args),
            "remove_worktree_base_ref" => handle_remove_worktree_base_ref(&rpc_req.args),
            "update_mesh_use_worktree" => handle_update_mesh_use_worktree(&rpc_req.args),
            "get_root_token" => handle_get_root_token(),
            "spawn_handover_agent" => handle_spawn_handover_agent(&rpc_req.args, app.clone()),
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

    match crate::db::create_mesh(name, &std::env::temp_dir().to_string_lossy()) {
        Ok(mesh) => JsonRpcResponse::success(&mesh),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_create_session(args: &serde_json::Value, app: AppHandle) -> String {
    let mesh_id = args.get("projectId").and_then(|v| v.as_i64()).unwrap_or(0);
    let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("Session");
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("/tmp");
    let branch = args.get("branch").and_then(|v| v.as_str()).unwrap_or("main");

    use crate::models::{EnvType, Provider};

    match crate::db::create_agent_node(
        mesh_id,
        name,
        path,
        branch,
        EnvType::Windows,
        Provider::Anthropic,
        Some(name),
        None,
    ) {
        Ok(node) => {
            // Emit event so frontend session store can refetch via invoke()
            let _ = app.emit("session-created", serde_json::json!({ "id": node.id }));
            JsonRpcResponse::success(&node)
        }
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_list_projects() -> String {
    match crate::db::list_meshes() {
        Ok(meshes) => JsonRpcResponse::success(&meshes),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_list_sessions() -> String {
    match crate::db::list_agent_nodes() {
        Ok(nodes) => JsonRpcResponse::success(&nodes),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_get_session(args: &serde_json::Value) -> String {
    let session_id = args.get("sessionId")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    match crate::db::get_agent_node_by_id(session_id) {
        Ok(node) => JsonRpcResponse::success(&node),
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

fn handle_spawn_handover_agent(args: &serde_json::Value, app: AppHandle) -> String {
    let mesh_id = args.get("meshId").and_then(|v| v.as_i64()).unwrap_or(0);
    let prefill = args.get("prefill").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let provider = args.get("provider").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from);

    tracing::info!("[test_server] spawn_handover_agent: mesh_id={}, prefill_len={}", mesh_id, prefill.len());

    let app_clone = app.clone();
    let result = std::thread::spawn(move || {
        tauri::async_runtime::block_on(crate::commands::agent::spawn_handover_agent(
            app_clone,
            mesh_id,
            prefill,
            provider,
        ))
    }).join();

    match result {
        Ok(Ok(node)) => {
            let _ = app.emit("session-created", serde_json::json!({ "id": node.id }));
            JsonRpcResponse::success(&node)
        }
        Ok(Err(e)) => {
            tracing::error!("[test_server] spawn_handover_agent error: {}", e);
            JsonRpcResponse::error(&e)
        }
        Err(e) => {
            tracing::error!("[test_server] spawn_handover_agent panicked: {:?}", e);
            JsonRpcResponse::error(&format!("spawn_handover_agent panicked: {:?}", e))
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

    match crate::db::delete_mesh(project_id) {
        Ok(_) => JsonRpcResponse::success(&serde_json::json!({ "project_id": project_id })),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_delete_session(args: &serde_json::Value) -> String {
    let session_id = args.get("sessionId").and_then(|v| v.as_i64()).unwrap_or(0);
    let remove_worktree = args.get("removeWorktree").and_then(|v| v.as_bool()).unwrap_or(false);

    match crate::services::agent_node::delete(session_id, remove_worktree) {
        Ok(_) => JsonRpcResponse::success(&serde_json::json!({ "session_id": session_id })),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_get_worktree_close_safety(args: &serde_json::Value) -> String {
    let session_id = args.get("sessionId").and_then(|v| v.as_i64()).unwrap_or(0);

    match crate::services::agent_node::get_worktree_close_safety(session_id) {
        Ok(safety) => JsonRpcResponse::success(&safety),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}

fn handle_archive_session(args: &serde_json::Value) -> String {
    let session_id = args.get("sessionId").and_then(|v| v.as_i64()).unwrap_or(0);

    match crate::db::archive_agent_node(session_id) {
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

fn handle_get_mesh_properties(args: &serde_json::Value) -> String {
    let mesh_id = args.get("mesh_id").and_then(|v| v.as_i64()).unwrap_or(0);

    match tauri::async_runtime::block_on(crate::commands::mesh_config::get_mesh_properties(mesh_id)) {
        Ok(config) => JsonRpcResponse::success(&config),
        Err(e) => JsonRpcResponse::error(&e),
    }
}

fn handle_update_mesh_field(args: &serde_json::Value) -> String {
    let mesh_id = args.get("mesh_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let section = args.get("section").and_then(|v| v.as_str()).unwrap_or("");
    let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let value = args.get("value").and_then(|v| v.as_str()).unwrap_or("");

    match tauri::async_runtime::block_on(crate::commands::mesh_config::update_mesh_field(mesh_id, section.to_string(), key.to_string(), value.to_string())) {
        Ok(_) => JsonRpcResponse::success(&serde_json::json!({ "mesh_id": mesh_id })),
        Err(e) => JsonRpcResponse::error(&e),
    }
}

fn handle_update_worktree_base_ref(args: &serde_json::Value) -> String {
    let mesh_id = args.get("mesh_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let base_ref = args.get("base_ref").and_then(|v| v.as_str()).unwrap_or("fresh").to_string();

    match tauri::async_runtime::block_on(crate::commands::mesh_config::update_worktree_base_ref(mesh_id, base_ref)) {
        Ok(_) => JsonRpcResponse::success(&serde_json::json!({ "mesh_id": mesh_id })),
        Err(e) => JsonRpcResponse::error(&e),
    }
}

fn handle_remove_worktree_base_ref(args: &serde_json::Value) -> String {
    let mesh_id = args.get("mesh_id").and_then(|v| v.as_i64()).unwrap_or(0);

    match tauri::async_runtime::block_on(crate::commands::mesh_config::remove_worktree_base_ref(mesh_id)) {
        Ok(_) => JsonRpcResponse::success(&serde_json::json!({ "mesh_id": mesh_id })),
        Err(e) => JsonRpcResponse::error(&e),
    }
}

fn handle_update_mesh_use_worktree(args: &serde_json::Value) -> String {
    let mesh_id = args.get("mesh_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let use_worktree = args.get("use_worktree").and_then(|v| v.as_bool()).unwrap_or(true);

    match tauri::async_runtime::block_on(crate::commands::mesh_config::update_mesh_use_worktree(mesh_id, use_worktree)) {
        Ok(_) => JsonRpcResponse::success(&serde_json::json!({ "mesh_id": mesh_id, "use_worktree": use_worktree })),
        Err(e) => JsonRpcResponse::error(&e),
    }
}

/// Expose the remote-access root token to Playwright mobile e2e specs so
/// they can construct `/v2?token=...` URLs without scraping the SQLite DB
/// from outside. The Tauri command of the same name does the same thing
/// for the desktop QR-code modal; we go through the test server here only
/// because Playwright runs outside the Tauri webview.
fn handle_get_root_token() -> String {
    match crate::db::get_or_create_root_token() {
        Ok(token) => JsonRpcResponse::success(&serde_json::json!({ "token": token })),
        Err(e) => JsonRpcResponse::error(&e.to_string()),
    }
}
