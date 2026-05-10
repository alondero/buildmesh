//! Embedded HTTP/WebSocket server for mobile remote access.
//!
//! Serves the mobile web app and handles WebSocket terminal connections
//! for remote access from a phone on the same network.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use futures_util::{SinkExt, StreamExt};
use parking_lot::RwLock;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
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
    tracing::debug!("[DEBUG-pty] fulfill_snapshot: request_id={}, data_len={}", request_id, data.len());
    let mut requests = get_snapshot_requests().write();
    if let Some(tx) = requests.remove(request_id) {
        if tx.send(data).is_err() {
            tracing::warn!("fulfill_snapshot: receiver already dropped for {}", request_id);
        }
    } else {
        tracing::debug!("[DEBUG-pty] fulfill_snapshot: no pending request found for {}", request_id);
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

    tracing::debug!("[DEBUG-pty] request_terminal_snapshot: emitting for node {}, request_id={}", node_id, request_id);
    let _ = app.emit("serialize-terminal-request", serde_json::json!({
        "node_id": node_id,
        "request_id": request_id,
    }));

    match tokio::time::timeout(std::time::Duration::from_millis(500), rx).await {
        Ok(Ok(data)) => {
            tracing::debug!("[DEBUG-pty] request_terminal_snapshot: got snapshot ({} bytes) for node {}", data.len(), node_id);
            Some(data)
        }
        Ok(Err(e)) => {
            tracing::debug!("[DEBUG-pty] request_terminal_snapshot: channel closed for node {}: {}", node_id, e);
            let mut requests = get_snapshot_requests().write();
            requests.remove(&request_id);
            None
        }
        Err(_) => {
            tracing::debug!("[DEBUG-pty] request_terminal_snapshot: TIMEOUT for node {} after 500ms", node_id);
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
        tracing::info!("[DEBUG-pty] WS path HIT: {} (token present: {})", parts[1], token.is_some());
        let ws_token = token.clone();
        let path = parts[1];
        tracing::debug!("[DEBUG-pty] headers before WS upgrade: {}", headers.replace("\r\n", "|"));

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

        let mut stream = lines.into_inner();

        // Drain any remaining buffered data before accept_async.
        let mut drain_buf = [0u8; 4096];
        let did_drain = stream.read(&mut drain_buf).await;
        let drain_bytes = if did_drain.is_ok() { did_drain.unwrap() } else { 0 };
        tracing::debug!("[DEBUG-pty] drain read {} bytes before accept_async", drain_bytes);

        tracing::debug!("[DEBUG-pty] calling accept_async for node {}", node_id);
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), accept_async(stream)).await;
        match result {
            Ok(Ok(ws_stream)) => {
                tracing::info!("WebSocket connected for node {}", node_id);
                tracing::debug!("[DEBUG-pty] WS upgrade SUCCESS, spawning handle_ws_connection task for node {}", node_id);
                tauri::async_runtime::spawn(handle_ws_connection(ws_stream, node_id));
            }
            Ok(Err(e)) => {
                tracing::error!("WebSocket upgrade failed: {}", e);
            }
            Err(_) => {
                tracing::error!("WebSocket upgrade timed out after 5s for node {}", node_id);
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
    tracing::info!("[DEBUG-pty] >>> handle_ws_connection ENTERED for node {}", node_id);

    // Subscribe BEFORE requesting snapshot to avoid missing data between snapshot and live stream
    let mut rx = subscribe_pty(node_id);

    let app = get_server_state().app.clone();
    tracing::debug!("[DEBUG-pty] handle_ws_connection: requesting snapshot for node {}", node_id);
    if let Some(snapshot) = request_terminal_snapshot(&app, node_id).await {
        tracing::debug!("[DEBUG-pty] handle_ws_connection: snapshot received, {} bytes, sending to mobile", snapshot.len());
        if !snapshot.is_empty() {
            if write.send(tungstenite::Message::Text(snapshot.into())).await.is_err() {
                tracing::debug!("[DEBUG-pty] handle_ws_connection: failed to send snapshot for node {}", node_id);
                return;
            }
        }
    } else {
        tracing::debug!("[DEBUG-pty] handle_ws_connection: no snapshot for node {} (timeout or error)", node_id);
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
                tracing::debug!("WS received text from mobile: {:?}", &text[..text.len().min(32)]);
            }
            Ok(tungstenite::Message::Binary(data)) => {
                tracing::debug!("WS received binary {} bytes from mobile", data.len());
            }
            Ok(tungstenite::Message::Close(_)) => break,
            Err(e) => {
                tracing::error!("WS read error: {}", e);
                break;
            }
            _ => {}
        }
    }

    write_task.abort();
    tracing::debug!("WS connection closed for node {}", node_id);
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
        tracing::debug!("[DEBUG-pty] ensure_pty_channel: created NEW channel for node {}", node_id);
    } else {
        tracing::debug!("[DEBUG-pty] ensure_pty_channel: channel already exists for node {}", node_id);
    }
}

pub fn subscribe_pty(node_id: i64) -> broadcast::Receiver<Vec<u8>> {
    let nodes = get_known_nodes();
    let mut locked = nodes.write();
    let sender = locked.entry(node_id).or_insert_with(|| {
        let (tx, _) = broadcast::channel(1024);
        tracing::debug!("[DEBUG-pty] subscribe_pty: created channel for node {} (was missing)", node_id);
        tx
    }).clone();
    tracing::debug!("[DEBUG-pty] subscribe_pty: subscribing to node {} ({} active subscribers)", node_id, sender.len());
    sender.subscribe()
}

pub fn send_pty_output(node_id: i64, data: Vec<u8>) {
    let nodes = get_known_nodes();
    let sender = {
        let locked = nodes.read();
        locked.get(&node_id).cloned()
    };
    if let Some(sender) = sender {
        let msg_len = data.len();
        let _ = sender.send(data);
        tracing::debug!("[DEBUG-pty] send_pty_output: sent {} bytes to node {} (subscribers: {})", msg_len, node_id, sender.len());
    } else {
        tracing::debug!("[DEBUG-pty] send_pty_output: NO channel found for node {}", node_id);
    }
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
}
