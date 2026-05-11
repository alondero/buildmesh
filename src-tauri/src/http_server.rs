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
use tokio_tungstenite::{accept_async, tungstenite};

use tauri::Emitter;
use crate::db;

// --- App Handle for emitting Tauri events from HTTP handlers ---

static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

pub const HTTP_PORT: u16 = 1992;

// --- Mobile Web App HTML (served as root) ---

const MOBILE_APP_HTML: &str = include_str!("mobile_app.html");

// --- Server State ---

struct HttpServerState {
    app: tauri::AppHandle,
    _broadcast_tx: broadcast::Sender<Vec<u8>>,
    pty_outputs: Arc<RwLock<HashMap<i64, broadcast::Sender<Vec<u8>>>>>,
}

static SERVER_STATE: OnceLock<Arc<HttpServerState>> = OnceLock::new();

fn get_server_state() -> Arc<HttpServerState> {
    SERVER_STATE.get().cloned().expect("HTTP server not started")
}

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
    let _ = APP_HANDLE.set(app.clone());
    let (broadcast_tx, _) = broadcast::channel(1024);
    let state = Arc::new(HttpServerState {
        app,
        _broadcast_tx: broadcast_tx,
        pty_outputs: Arc::new(RwLock::new(HashMap::new())),
    });
    let _ = SERVER_STATE.set(state.clone());
    drop(state);

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

        let stream = lines.into_inner();
        match accept_async(stream).await {
            Ok(ws_stream) => {
                tracing::info!("WebSocket connected for node {}", node_id);
                tauri::async_runtime::spawn(handle_ws_connection(ws_stream, node_id));
            }
            Err(e) => {
                tracing::error!("WebSocket upgrade failed: {}", e);
            }
        }
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

async fn handle_ws_connection(
    ws_stream: tokio_tungstenite::WebSocketStream<TcpStream>,
    node_id: i64,
) {
    let (mut write, mut read) = ws_stream.split();

    // Subscribe to live broadcast before taking snapshot to avoid gaps
    let mut rx = subscribe_pty(node_id);

    // Replay scrollback buffer so mobile sees existing terminal content
    let snapshot = get_scrollback_snapshot(node_id);
    if !snapshot.is_empty() {
        if write.send(tungstenite::Message::Binary(snapshot.into())).await.is_err() {
            return;
        }
    }


    let write_task = tauri::async_runtime::spawn(async move {
        while let Ok(data) = rx.recv().await {
            if write.send(tungstenite::Message::Binary(data.into())).await.is_err() {
                break;
            }
        }
    });

    while let Some(msg) = read.next().await {
        match msg {
            Ok(tungstenite::Message::Text(text)) => {
                forward_mobile_input(node_id, &text);
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
    if let Err(e) = crate::commands::agent::write_to_pty(node_id, text.as_bytes()) {
        tracing::warn!("Mobile input forward failed for {}: {}", node_id, e);
        return;
    }
    if text.bytes().any(|b| b == b'\n' || b == b'\r') {
        let _ = db::update_agent_node_status(node_id, crate::models::SessionStatus::Running);
        if let Some(app) = APP_HANDLE.get() {
            let _ = app.emit("attention-cleared", serde_json::json!({ "session_id": node_id }));
        }
    }
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

static KNOWN_NODES: OnceLock<Arc<RwLock<HashMap<i64, broadcast::Sender<Vec<u8>>>>>> = OnceLock::new();

fn get_known_nodes() -> &'static Arc<RwLock<HashMap<i64, broadcast::Sender<Vec<u8>>>>> {
    KNOWN_NODES.get_or_init(|| {
        Arc::new(RwLock::new(HashMap::new()))
    })
}

pub fn ensure_pty_channel(node_id: i64) {
    let nodes = get_known_nodes();
    let mut locked = nodes.write();
    if !locked.contains_key(&node_id) {
        let (tx, _) = broadcast::channel(1024);
        locked.insert(node_id, tx);
    }
}

pub fn subscribe_pty(node_id: i64) -> broadcast::Receiver<Vec<u8>> {
    let nodes = get_known_nodes();
    let mut locked = nodes.write();
    let sender = locked.entry(node_id).or_insert_with(|| {
        let (tx, _) = broadcast::channel(1024);
        tx
    }).clone();
    sender.subscribe()
}

pub fn send_pty_output(node_id: i64, data: Vec<u8>) {
    // Buffer for scrollback replay before sending (avoids clone)
    {
        let buffers = get_scrollback();
        let mut locked = buffers.write();
        let buf = locked.entry(node_id).or_insert_with(|| VecDeque::with_capacity(PTY_SCROLLBACK_MAX));
        buf.extend(&data);
        if buf.len() > PTY_SCROLLBACK_MAX {
            let excess = buf.len() - PTY_SCROLLBACK_MAX;
            buf.drain(..excess);
        }
    }

    let nodes = get_known_nodes();
    let sender = {
        let locked = nodes.read();
        locked.get(&node_id).cloned()
    };
    if let Some(sender) = sender {
        let _ = sender.send(data);
    }
}

// --- Scrollback Buffer for Mobile Replay ---

const PTY_SCROLLBACK_MAX: usize = 128 * 1024;

static SCROLLBACK: OnceLock<Arc<RwLock<HashMap<i64, VecDeque<u8>>>>> = OnceLock::new();

fn get_scrollback() -> &'static Arc<RwLock<HashMap<i64, VecDeque<u8>>>> {
    SCROLLBACK.get_or_init(|| Arc::new(RwLock::new(HashMap::new())))
}

fn get_scrollback_snapshot(node_id: i64) -> Vec<u8> {
    let buffers = get_scrollback();
    let locked = buffers.read();
    match locked.get(&node_id) {
        Some(buf) => {
            let (a, b) = buf.as_slices();
            let mut v = Vec::with_capacity(buf.len());
            v.extend_from_slice(a);
            v.extend_from_slice(b);
            v
        }
        None => Vec::new(),
    }
}

/// Clear the scrollback buffer for a node. Called on agent kill.
pub fn clear_scrollback(node_id: i64) {
    let buffers = get_scrollback();
    let mut locked = buffers.write();
    locked.remove(&node_id);
}

#[cfg(test)]
mod tests {
    use super::*;

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
        ensure_pty_channel(5555);
        let _rx = subscribe_pty(5555);
        send_pty_output(5555, vec![0x48, 0x65, 0x6c, 0x6c, 0x6f]);
        send_pty_output(5555, vec![0x20, 0x57, 0x6f, 0x72, 0x6c, 0x64]);
        let snapshot = get_scrollback_snapshot(5555);
        assert_eq!(snapshot, b"Hello World");
    }

    #[test]
    fn scrollback_respects_max_size() {
        ensure_pty_channel(4444);
        let _rx = subscribe_pty(4444);
        let chunk = vec![0x41; PTY_SCROLLBACK_MAX];
        send_pty_output(4444, chunk);
        send_pty_output(4444, vec![0x42, 0x43]);
        let snapshot = get_scrollback_snapshot(4444);
        assert_eq!(snapshot.len(), PTY_SCROLLBACK_MAX);
        assert_eq!(snapshot[snapshot.len() - 1], 0x43);
        assert_eq!(snapshot[snapshot.len() - 2], 0x42);
    }

    #[test]
    fn scrollback_empty_for_unknown_node() {
        let snapshot = get_scrollback_snapshot(3333);
        assert!(snapshot.is_empty());
    }

    #[test]
    fn clear_scrollback_removes_buffer() {
        ensure_pty_channel(2222);
        let _rx = subscribe_pty(2222);
        send_pty_output(2222, vec![1, 2, 3]);
        clear_scrollback(2222);
        let snapshot = get_scrollback_snapshot(2222);
        assert!(snapshot.is_empty());
    }
}
