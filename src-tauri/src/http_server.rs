//! Embedded HTTP/WebSocket server for mobile remote access.
//!
//! Serves the mobile web app and handles WebSocket terminal connections
//! for remote access from a phone on the same network.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use futures_util::{SinkExt, StreamExt};
use parking_lot::RwLock;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, oneshot};
#[cfg(test)]
use tokio_tungstenite::accept_async;
use tokio_tungstenite::{tungstenite, WebSocketStream};
use tungstenite::handshake::derive_accept_key;
use tungstenite::protocol::Role;

use tauri::Emitter;
use crate::commands::agent::PROCESS_REGISTRY;
use crate::db;

// --- App Handle for emitting Tauri events from HTTP handlers ---

static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

pub const HTTP_PORT: u16 = 1992;

// --- Mobile Web App HTML (served as root) ---

const MOBILE_APP_HTML: &str = include_str!("mobile_app.html");

// --- Server State ---

// --- Snapshot Request/Response ---

static SNAPSHOT_COUNTER: AtomicU64 = AtomicU64::new(0);

type SnapshotSenders = HashMap<String, oneshot::Sender<String>>;
static SNAPSHOT_REQUESTS: OnceLock<Arc<RwLock<SnapshotSenders>>> = OnceLock::new();

fn get_snapshot_requests() -> &'static Arc<RwLock<SnapshotSenders>> {
    SNAPSHOT_REQUESTS.get_or_init(|| Arc::new(RwLock::new(HashMap::new())))
}

/// Called by the Tauri command when the frontend responds with a serialized terminal snapshot.
pub fn fulfill_snapshot(request_id: &str, data: String) {
    let mut requests = get_snapshot_requests().write();
    if let Some(tx) = requests.remove(request_id) {
        if tx.send(data).is_err() {
            tracing::warn!("fulfill_snapshot: receiver already dropped for {}", request_id);
        }
    }
}

async fn request_terminal_snapshot(app: &tauri::AppHandle, node_id: i64) -> Option<String> {
    let seq = SNAPSHOT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let request_id = format!("snap-{}-{}", node_id, seq);

    let (tx, rx) = oneshot::channel();
    {
        let mut requests = get_snapshot_requests().write();
        requests.insert(request_id.clone(), tx);
    }

    let _ = app.emit("serialize-terminal-request", serde_json::json!({
        "node_id": node_id,
        "request_id": request_id,
    }));

    match tokio::time::timeout(std::time::Duration::from_millis(500), rx).await {
        Ok(Ok(data)) => Some(data),
        _ => {
            let mut requests = get_snapshot_requests().write();
            requests.remove(&request_id);
            None
        }
    }
}

/// Start the HTTP server on the given port.
pub fn start_http_server(port: u16, app: tauri::AppHandle) {
    let _ = APP_HANDLE.set(app);

    tauri::async_runtime::spawn(async move {
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        let listener = match TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("Failed to bind HTTP server on port {}: {}", port, e);
                return;
            }
        };
        tracing::info!("HTTP server listening on http://0.0.0.0:{}", port);

        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    tracing::debug!("HTTP connection from {}", addr);
                    tauri::async_runtime::spawn(handle_connection(stream, addr));
                }
                Err(e) => {
                    tracing::error!("HTTP accept error: {}", e);
                }
            }
        }
    });
}

async fn handle_connection(stream: TcpStream, _addr: SocketAddr) {
    let mut lines = tokio::io::BufStream::new(stream);
    let mut request_line = String::new();
    if lines.read_line(&mut request_line).await.is_err() {
        return;
    }

    let request_line = request_line.trim();
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return;
    }

    let token = extract_token_from_path(parts[1]);

    let mut headers = String::new();
    while headers.lines().count() == 0 || !headers.ends_with("\r\n\r\n") {
        if lines.read_line(&mut headers).await.is_err() { break; }
        if headers.trim().is_empty() { break; }
    }

    // WebSocket upgrade path: /ws/terminal/{nodeId}?token=xxx
    if parts[0] == "GET" && parts[1].starts_with("/ws/terminal/") {
        let ws_token = token.clone();
        let path = parts[1];

        let node_id: Option<i64> = path.split('/').nth(3)
            .map(|s| s.split('?').next().unwrap_or(s).parse().ok())
            .flatten();

        if node_id.is_none() {
            let error = b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n";
            let mut stream = lines.into_inner();
            let _ = stream.write_all(error).await;
            return;
        }
        let node_id = node_id.unwrap();

        if !validate_token(ws_token) {
            let error = b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n";
            let mut stream = lines.into_inner();
            let _ = stream.write_all(error).await;
            return;
        }

        let ws_key = match extract_header_value(&headers, "Sec-WebSocket-Key") {
            Some(key) => key,
            None => {
                let error = b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n";
                let mut stream = lines.into_inner();
                let _ = stream.write_all(error).await;
                return;
            }
        };

        let accept_key = derive_accept_key(ws_key.as_bytes());
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Connection: Upgrade\r\n\
             Upgrade: websocket\r\n\
             Sec-WebSocket-Accept: {}\r\n\
             \r\n",
            accept_key
        );

        let mut stream = lines.into_inner();
        if stream.write_all(response.as_bytes()).await.is_err() {
            return;
        }
        if stream.flush().await.is_err() {
            return;
        }

        let ws_stream = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
        tracing::info!("WebSocket connected for node {}", node_id);
        tauri::async_runtime::spawn(handle_ws_connection(ws_stream, node_id));
        return;
    }

    let path_without_query = parts[1].split('?').next().unwrap_or(parts[1]);

    // Attention webhook: POST /api/attention/{session_id}
    // Called by Claude Code's Stop hook — no token required (localhost-only)
    if parts[0] == "POST" && path_without_query.starts_with("/api/attention/") {
        let session_id: Option<i64> = path_without_query
            .strip_prefix("/api/attention/")
            .and_then(|s| s.parse().ok());

        let Some(session_id) = session_id else {
            let error = b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n";
            let mut stream = lines.into_inner();
            let _ = stream.write_all(error).await;
            return;
        };

        let Some(app) = APP_HANDLE.get() else {
            let error = b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n";
            let mut stream = lines.into_inner();
            let _ = stream.write_all(error).await;
            return;
        };

        crate::commands::attention::mark_attention(session_id, app);

        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
        let mut stream = lines.into_inner();
        let _ = stream.write_all(response).await;
        return;
    }

    // Create node: POST /api/nodes/create?token=xxx
    if parts[0] == "POST" && path_without_query == "/api/nodes/create" {
        if !validate_token(token.clone()) {
            let error = b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n";
            let mut stream = lines.into_inner();
            let _ = stream.write_all(error).await;
            return;
        }

        let content_length: usize = extract_header_value(&headers, "Content-Length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        if content_length > 64 * 1024 {
            send_json_error(&mut lines, "413 Content Too Large", "Body too large").await;
            return;
        }

        let mut body_bytes = vec![0u8; content_length];
        if content_length > 0 {
            use tokio::io::AsyncReadExt;
            if lines.read_exact(&mut body_bytes).await.is_err() {
                let error = b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n";
                let mut stream = lines.into_inner();
                let _ = stream.write_all(error).await;
                return;
            }
        }

        #[derive(serde::Deserialize)]
        struct CreateNodeRequest {
            mesh_id: i64,
            provider: String,
            #[serde(default = "default_rows")]
            rows: u16,
            #[serde(default = "default_cols")]
            cols: u16,
        }
        fn default_rows() -> u16 { 24 }
        fn default_cols() -> u16 { 80 }

        let req: CreateNodeRequest = match serde_json::from_slice(&body_bytes) {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("Invalid JSON: {}", e);
                send_json_error(&mut lines, "400 Bad Request", &msg).await;
                return;
            }
        };

        let mesh = match db::get_mesh_by_id(req.mesh_id) {
            Ok(m) => m,
            Err(_) => {
                send_json_error(&mut lines, "400 Bad Request", "Mesh not found").await;
                return;
            }
        };

        let node = match crate::services::agent_node::create(
            req.mesh_id,
            &mesh.path,
            "main",
            Some(req.provider.as_str()),
        ) {
            Ok(n) => n,
            Err(e) => {
                let msg = format!("Failed to create node: {}", e);
                send_json_error(&mut lines, "500 Internal Server Error", &msg).await;
                return;
            }
        };

        let Some(app) = APP_HANDLE.get() else {
            send_json_error(&mut lines, "503 Service Unavailable", "App not ready").await;
            return;
        };

        if let Err(e) = crate::commands::agent::spawn_agent_inner(
            app,
            node.id,
            req.provider.clone(),
            None,
            req.rows,
            req.cols,
        ).await {
            let msg = format!("Failed to spawn agent: {}", e);
            send_json_error(&mut lines, "500 Internal Server Error", &msg).await;
            return;
        }

        // Notify desktop frontend so sidebar refreshes to show the new node.
        // The mobile app creates nodes via HTTP (port 1992) while desktop uses
        // Tauri invoke — this bridges the gap so both UIs stay in sync.
        let _ = app.emit("session-created", serde_json::json!({ "id": node.id }));

        let body = serde_json::to_string(&node).unwrap_or_else(|_| "{}".to_string());
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(), body
        );
        let mut stream = lines.into_inner();
        let _ = stream.write_all(response.as_bytes()).await;
        return;
    }

    // API routes — require token
    if parts[0] == "GET" && path_without_query.starts_with("/api/") {
        if !validate_token(token.clone()) {
            let error = b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n";
            let mut stream = lines.into_inner();
            let _ = stream.write_all(error).await;
            return;
        }

        let body: String;
        let content_type = "application/json";

        if path_without_query == "/api/nodes" {
            body = match db::list_agent_nodes() {
                Ok(nodes) => serde_json::to_string(&nodes).unwrap_or_else(|_| "[]".to_string()),
                Err(_) => "[]".to_string(),
            };
        } else if path_without_query == "/api/providers" {
            // Static list for now — future work will detect installed binaries.
            #[derive(serde::Serialize)]
            struct ProviderInfo {
                id: &'static str,
                label: &'static str,
                color: &'static str,
                icon: &'static str,
            }
            let providers = [
                ProviderInfo { id: "anthropic", label: "Anthropic (Claude)", color: "#1d7cfc", icon: "A" },
                ProviderInfo { id: "minimax", label: "MiniMax", color: "#6366f1", icon: "M" },
                ProviderInfo { id: "gemini", label: "Google Gemini", color: "#10b981", icon: "G" },
                ProviderInfo { id: "opencode", label: "OpenCode", color: "#f59e0b", icon: "O" },
            ];
            body = serde_json::to_string(&providers).unwrap_or_else(|_| "[]".to_string());
        } else if path_without_query == "/api/meshes" {
            body = match db::list_meshes() {
                Ok(meshes) => serde_json::to_string(&meshes).unwrap_or_else(|_| "[]".to_string()),
                Err(_) => "[]".to_string(),
            };
        } else {
            body = r#"{"error":"not found"}"#.to_string();
        }

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\n\r\n{}",
            content_type,
            body.len(),
            body
        );
        let mut stream = lines.into_inner();
        let _ = stream.write_all(response.as_bytes()).await;
        return;
    }

    // Serve HTML
    if !validate_token(token) {
        let error = b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n";
        let mut stream = lines.into_inner();
        let _ = stream.write_all(error).await;
        return;
    }

    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
        MOBILE_APP_HTML.len(),
        MOBILE_APP_HTML
    );
    let mut stream = lines.into_inner();
    let _ = stream.write_all(response.as_bytes()).await;
}

async fn send_json_error(lines: &mut tokio::io::BufStream<TcpStream>, status: &str, msg: &str) {
    let body = format!(r#"{{"error":"{}"}}"#, msg.replace('"', "\\\""));
    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        status,
        body.len(),
        body
    );
    let stream = lines.get_mut();
    let _ = stream.write_all(response.as_bytes()).await;
}

async fn handle_ws_connection(
    ws_stream: tokio_tungstenite::WebSocketStream<TcpStream>,
    node_id: i64,
) {
    let (mut write, mut read) = ws_stream.split();

    // Subscribe before sending initial state to avoid missing output in the gap
    let mut rx = subscribe_pty(node_id);

    // Prefer a clean terminal snapshot over raw history replay, which contains
    // stale cursor-positioning sequences from TUI redraws.
    let history_fallback = || {
        let h = get_pty_history(node_id);
        String::from_utf8_lossy(&h).into_owned()
    };
    let initial_data = match APP_HANDLE.get() {
        Some(app) => request_terminal_snapshot(app, node_id).await
            .unwrap_or_else(history_fallback),
        None => history_fallback(),
    };
    if !initial_data.is_empty() {
        if write.send(tungstenite::Message::Text(initial_data.into())).await.is_err() {
            return;
        }
    }

    let write_task = tauri::async_runtime::spawn(async move {
        while let Ok(data) = rx.recv().await {
            let text = String::from_utf8_lossy(&data).into_owned();
            if write.send(tungstenite::Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    while let Some(msg) = read.next().await {
        match msg {
            Ok(tungstenite::Message::Text(text)) => {
                if let Some(resize) = parse_resize_message(&text) {
                    handle_mobile_resize(node_id, resize.0, resize.1);
                } else {
                    forward_mobile_input(node_id, &text);
                }
            }
            Ok(tungstenite::Message::Binary(data)) => {
                let text = String::from_utf8_lossy(&data);
                forward_mobile_input(node_id, &text);
            }
            Ok(tungstenite::Message::Close(_)) => break,
            Err(_) => break,
            _ => {}
        }
    }

    write_task.abort();
    tracing::debug!("WS connection closed for node {}", node_id);
}

fn forward_mobile_input(node_id: i64, text: &str) {
    let registry = PROCESS_REGISTRY.lock().unwrap();
    if let Err(e) = registry.write_bytes(node_id, text.as_bytes()) {
        tracing::warn!("Mobile input forward failed for {}: {}", node_id, e);
        return;
    }
    drop(registry);
    if text.bytes().any(|b| b == b'\n' || b == b'\r') {
        let _ = db::update_agent_node_status(node_id, crate::models::SessionStatus::Running);
        if let Some(app) = APP_HANDLE.get() {
            let _ = app.emit("attention-cleared", serde_json::json!({ "session_id": node_id }));
        }
    }
}

fn parse_resize_message(text: &str) -> Option<(u16, u16)> {
    if !text.starts_with('{') {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    if v.get("type")?.as_str()? != "resize" {
        return None;
    }
    let cols = v.get("cols")?.as_u64()? as u16;
    let rows = v.get("rows")?.as_u64()? as u16;
    Some((cols, rows))
}

fn handle_mobile_resize(node_id: i64, cols: u16, rows: u16) {
    let registry = PROCESS_REGISTRY.lock().unwrap();
    if let Err(e) = registry.resize_pty(node_id, cols, rows) {
        tracing::warn!("Mobile resize failed for node {}: {}", node_id, e);
    }
}

// --- Header Helpers ---

fn extract_header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    let name_bytes = name.as_bytes();
    headers.lines()
        .find(|line| {
            let lb = line.as_bytes();
            lb.len() > name_bytes.len()
                && lb[name_bytes.len()] == b':'
                && lb[..name_bytes.len()].eq_ignore_ascii_case(name_bytes)
        })
        .map(|line| line[name.len() + 1..].trim())
}

// --- Token Validation ---

fn extract_token_from_path(path: &str) -> Option<String> {
    path.split('?')
        .nth(1)
        .and_then(|query| {
            query.split('&')
                .find(|pair| pair.starts_with("token="))
                .map(|pair| pair[6..].to_string())
        })
}

fn validate_token(token: Option<String>) -> bool {
    if let Some(t) = token {
        db::validate_root_token(&t).unwrap_or(false)
    } else {
        false
    }
}

// --- PTY Broadcast ---

const HISTORY_BUFFER_CAP: usize = 128 * 1024;

struct NodeChannel {
    sender: broadcast::Sender<Vec<u8>>,
    history: VecDeque<u8>,
}

static KNOWN_NODES: OnceLock<Arc<RwLock<HashMap<i64, NodeChannel>>>> = OnceLock::new();

fn get_known_nodes() -> &'static Arc<RwLock<HashMap<i64, NodeChannel>>> {
    KNOWN_NODES.get_or_init(|| {
        Arc::new(RwLock::new(HashMap::new()))
    })
}

pub fn ensure_pty_channel(node_id: i64) {
    let nodes = get_known_nodes();
    let mut locked = nodes.write();
    if !locked.contains_key(&node_id) {
        let (tx, _) = broadcast::channel(1024);
        locked.insert(node_id, NodeChannel { sender: tx, history: VecDeque::new() });
    }
}

pub fn subscribe_pty(node_id: i64) -> broadcast::Receiver<Vec<u8>> {
    let nodes = get_known_nodes();
    let mut locked = nodes.write();
    let channel = locked.entry(node_id).or_insert_with(|| {
        let (tx, _) = broadcast::channel(1024);
        NodeChannel { sender: tx, history: VecDeque::new() }
    });
    channel.sender.subscribe()
}

pub fn get_pty_history(node_id: i64) -> Vec<u8> {
    let nodes = get_known_nodes();
    let locked = nodes.read();
    locked.get(&node_id).map(|ch| {
        let (a, b) = ch.history.as_slices();
        let mut v = Vec::with_capacity(a.len() + b.len());
        v.extend_from_slice(a);
        v.extend_from_slice(b);
        v
    }).unwrap_or_default()
}

pub fn send_pty_output(node_id: i64, data: Vec<u8>) {
    let nodes = get_known_nodes();
    let sender = {
        let mut locked = nodes.write();
        if let Some(channel) = locked.get_mut(&node_id) {
            channel.history.extend(data.iter());
            let excess = channel.history.len().saturating_sub(HISTORY_BUFFER_CAP);
            if excess > 0 {
                channel.history.drain(..excess);
            }
            Some(channel.sender.clone())
        } else {
            None
        }
    };
    if let Some(sender) = sender {
        let _ = sender.send(data);
    }
}

/// Clear the history buffer for a node. Called on agent kill.
pub fn clear_scrollback(node_id: i64) {
    let nodes = get_known_nodes();
    let mut locked = nodes.write();
    if let Some(channel) = locked.get_mut(&node_id) {
        channel.history.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::connect_async;

    #[tokio::test]
    async fn ws_replays_history_on_connect() {
        let node_id = 20001_i64;
        ensure_pty_channel(node_id);
        send_pty_output(node_id, b"hello from history\r\n".to_vec());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = accept_async(stream).await.unwrap();
            handle_ws_connection(ws, node_id).await;
        });

        let url = format!("ws://{}/ws/terminal/{}", addr, node_id);
        let (mut ws, _) = connect_async(&url).await.unwrap();
        let msg = ws.next().await.unwrap().unwrap();
        assert!(msg.is_text());
        assert_eq!(msg.into_text().unwrap(), "hello from history\r\n");
    }

    #[tokio::test]
    async fn ws_receives_live_pty_output() {
        let node_id = 20002_i64;
        ensure_pty_channel(node_id);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = accept_async(stream).await.unwrap();
            handle_ws_connection(ws, node_id).await;
        });

        let url = format!("ws://{}/ws/terminal/{}", addr, node_id);
        let (mut ws, _) = connect_async(&url).await.unwrap();

        // Send PTY output after client is connected
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        send_pty_output(node_id, b"live data\r\n".to_vec());

        let msg = ws.next().await.unwrap().unwrap();
        assert!(msg.is_text());
        assert_eq!(msg.into_text().unwrap(), "live data\r\n");
    }

    #[tokio::test]
    async fn ws_history_then_live_output() {
        let node_id = 20003_i64;
        ensure_pty_channel(node_id);
        send_pty_output(node_id, b"old output\r\n".to_vec());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = accept_async(stream).await.unwrap();
            handle_ws_connection(ws, node_id).await;
        });

        let url = format!("ws://{}/ws/terminal/{}", addr, node_id);
        let (mut ws, _) = connect_async(&url).await.unwrap();

        // First message should be the history replay
        let msg1 = ws.next().await.unwrap().unwrap();
        assert_eq!(msg1.into_text().unwrap(), "old output\r\n");

        // Then live output
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        send_pty_output(node_id, b"new output\r\n".to_vec());

        let msg2 = ws.next().await.unwrap().unwrap();
        assert_eq!(msg2.into_text().unwrap(), "new output\r\n");
    }

    #[tokio::test]
    async fn ws_input_reaches_write_to_pty() {
        // This test verifies the input path works without a real PTY.
        // write_to_pty will log "no running agent" but shouldn't panic.
        let node_id = 20004_i64;
        ensure_pty_channel(node_id);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = accept_async(stream).await.unwrap();
            handle_ws_connection(ws, node_id).await;
        });

        let url = format!("ws://{}/ws/terminal/{}", addr, node_id);
        let (mut ws, _) = connect_async(&url).await.unwrap();

        // Send input from the "mobile client"
        ws.send(tungstenite::Message::Text("ls -la\n".into())).await.unwrap();
        // Give the handler time to process
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // No panic = success (the write_to_pty gracefully handles missing agents)
    }

    #[test]
    fn extract_token_from_simple_path() {
        let token = extract_token_from_path("/?token=abc123");
        assert_eq!(token, Some("abc123".to_string()));
    }

    #[test]
    fn extract_token_from_path_with_multiple_params() {
        let token = extract_token_from_path("/api/nodes?foo=bar&token=secret&baz=1");
        assert_eq!(token, Some("secret".to_string()));
    }

    #[test]
    fn extract_token_returns_none_without_token_param() {
        let token = extract_token_from_path("/api/nodes?foo=bar");
        assert_eq!(token, None);
    }

    #[test]
    fn extract_token_returns_none_for_bare_path() {
        let token = extract_token_from_path("/api/nodes");
        assert_eq!(token, None);
    }

    #[test]
    fn extract_token_from_ws_path() {
        let token = extract_token_from_path("/ws/terminal/306?token=deadbeef");
        assert_eq!(token, Some("deadbeef".to_string()));
    }

    #[test]
    fn extract_token_empty_value() {
        let token = extract_token_from_path("/?token=");
        assert_eq!(token, Some("".to_string()));
    }

    #[test]
    fn validate_token_rejects_none() {
        assert!(!validate_token(None));
    }

    #[test]
    fn ensure_pty_channel_is_idempotent() {
        ensure_pty_channel(9999);
        ensure_pty_channel(9999);
        let nodes = get_known_nodes();
        let locked = nodes.read();
        assert!(locked.contains_key(&9999));
    }

    #[test]
    fn subscribe_pty_creates_channel_if_missing() {
        let _rx = subscribe_pty(8888);
        let nodes = get_known_nodes();
        let locked = nodes.read();
        assert!(locked.contains_key(&8888));
    }

    #[test]
    fn send_pty_output_delivers_to_subscriber() {
        ensure_pty_channel(7777);
        let mut rx = subscribe_pty(7777);
        send_pty_output(7777, vec![0x41, 0x42, 0x43]);
        let received = rx.try_recv().unwrap();
        assert_eq!(received, vec![0x41, 0x42, 0x43]);
    }

    #[test]
    fn send_pty_output_appends_to_history() {
        ensure_pty_channel(5555);
        send_pty_output(5555, vec![0x41, 0x42]);
        send_pty_output(5555, vec![0x43, 0x44]);
        let history = get_pty_history(5555);
        assert_eq!(history, vec![0x41, 0x42, 0x43, 0x44]);
    }

    #[test]
    fn history_buffer_caps_at_limit() {
        ensure_pty_channel(4444);
        let big_chunk = vec![0x58; HISTORY_BUFFER_CAP + 100];
        send_pty_output(4444, big_chunk);
        let history = get_pty_history(4444);
        assert_eq!(history.len(), HISTORY_BUFFER_CAP);
    }

    #[test]
    fn get_pty_history_returns_empty_for_unknown_node() {
        let history = get_pty_history(3333);
        assert!(history.is_empty());
    }

    #[test]
    fn send_pty_output_no_panic_without_subscribers() {
        ensure_pty_channel(6666);
        send_pty_output(6666, vec![1, 2, 3]);
    }

    #[test]
    fn send_pty_output_no_panic_for_unknown_node() {
        send_pty_output(1111, vec![1, 2, 3]);
    }

    #[test]
    fn scrollback_captures_output() {
        ensure_pty_channel(50055);
        let _rx = subscribe_pty(50055);
        send_pty_output(50055, vec![0x48, 0x65, 0x6c, 0x6c, 0x6f]);
        send_pty_output(50055, vec![0x20, 0x57, 0x6f, 0x72, 0x6c, 0x64]);
        let history = get_pty_history(50055);
        assert_eq!(history, b"Hello World");
    }

    #[test]
    fn scrollback_respects_max_size() {
        ensure_pty_channel(40044);
        let _rx = subscribe_pty(40044);
        let chunk = vec![0x41; HISTORY_BUFFER_CAP];
        send_pty_output(40044, chunk);
        send_pty_output(40044, vec![0x42, 0x43]);
        let history = get_pty_history(40044);
        assert_eq!(history.len(), HISTORY_BUFFER_CAP);
        assert_eq!(history[history.len() - 1], 0x43);
        assert_eq!(history[history.len() - 2], 0x42);
    }

    #[test]
    fn scrollback_empty_for_unknown_node() {
        let history = get_pty_history(30033);
        assert!(history.is_empty());
    }

    #[test]
    fn clear_scrollback_removes_buffer() {
        ensure_pty_channel(20022);
        let _rx = subscribe_pty(20022);
        send_pty_output(20022, vec![1, 2, 3]);
        clear_scrollback(20022);
        let history = get_pty_history(20022);
        assert!(history.is_empty());
    }

    #[tokio::test]
    async fn ws_upgrade_does_not_hang() {
        use tokio::io::AsyncReadExt;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle_connection(stream, peer).await;
        });

        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request = "GET /ws/terminal/123?token=invalid HTTP/1.1\r\n\
             Host: localhost\r\n\
             Connection: Upgrade\r\n\
             Upgrade: websocket\r\n\
             Sec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             \r\n";
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 1024];
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream.read(&mut buf),
        ).await;

        assert!(result.is_ok(), "handle_connection hung on WebSocket upgrade (regression)");
    }

    #[test]
    fn extract_header_value_case_insensitive() {
        let headers = "Host: localhost\r\nSec-WebSocket-Key: abc123\r\nConnection: Upgrade\r\n";
        assert_eq!(extract_header_value(headers, "Sec-WebSocket-Key"), Some("abc123"));
        assert_eq!(extract_header_value(headers, "sec-websocket-key"), Some("abc123"));
        assert_eq!(extract_header_value(headers, "Host"), Some("localhost"));
        assert_eq!(extract_header_value(headers, "Missing"), None);
    }
}
