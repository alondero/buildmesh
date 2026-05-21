//! Embedded HTTP/WebSocket server for mobile remote access.
//!
//! Serves the mobile web app and handles WebSocket terminal connections
//! for remote access from a phone on the same network.

pub mod assets;
pub mod request;
pub mod routes;
pub mod ws;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;
use tauri::Emitter;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;

// --- App handle for emitting Tauri events from HTTP handlers ---

static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

pub(crate) fn app_handle() -> Option<&'static tauri::AppHandle> {
    APP_HANDLE.get()
}

const HTTP_PORT_START: u16 = 1992;
const HTTP_PORT_END: u16 = 1994;

/// Default/expected port for the attention webhook hook.
/// The actual server may bind 1993 or 1994 if 1992 is taken; the hook will
/// silently no-op on the wrong port since `|| true` is appended.
pub const HTTP_PORT_DEFAULT: u16 = HTTP_PORT_START;

// --- Snapshot request/response (used by ws.rs to seed initial terminal state) ---

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
            tracing::warn!(
                "fulfill_snapshot: receiver already dropped for {}",
                request_id
            );
        }
    }
}

pub(crate) async fn request_terminal_snapshot(
    app: &tauri::AppHandle,
    node_id: i64,
) -> Option<String> {
    let seq = SNAPSHOT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let request_id = format!("snap-{}-{}", node_id, seq);

    let (tx, rx) = oneshot::channel();
    {
        let mut requests = get_snapshot_requests().write();
        requests.insert(request_id.clone(), tx);
    }

    let _ = app.emit(
        "serialize-terminal-request",
        serde_json::json!({
            "node_id": node_id,
            "request_id": request_id,
        }),
    );

    match tokio::time::timeout(std::time::Duration::from_millis(500), rx).await {
        Ok(Ok(data)) => Some(data),
        _ => {
            let mut requests = get_snapshot_requests().write();
            requests.remove(&request_id);
            None
        }
    }
}

/// Start the HTTP server, trying ports 1992→1993→1994 until one binds.
/// Emits a `remote-access-port` event with the actual port used so the
/// QR code modal can update without recompiling.
pub fn start_http_server(app: tauri::AppHandle) {
    let _ = APP_HANDLE.set(app.clone());

    tauri::async_runtime::spawn(async move {
        let port = try_bind_ports(HTTP_PORT_START, HTTP_PORT_END).await;

        if let Some(port) = port {
            tracing::info!("HTTP server listening on http://0.0.0.0:{}", port);
            let _ = app.emit("remote-access-port", serde_json::json!({ "port": port }));
        } else {
            tracing::error!(
                "Failed to bind HTTP server on any port {}–{}",
                HTTP_PORT_START,
                HTTP_PORT_END
            );
        }
    });
}

async fn try_bind_ports(start: u16, end: u16) -> Option<u16> {
    for port in start..=end {
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        if let Ok(listener) = TcpListener::bind(&addr).await {
            let port = addr.port();
            tauri::async_runtime::spawn(async move {
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
            return Some(port);
        } else {
            tracing::debug!("Port {} already in use, trying next", port);
        }
    }
    None
}

async fn handle_connection(stream: TcpStream, _addr: SocketAddr) {
    let mut lines = tokio::io::BufStream::new(stream);
    let mut request_line = String::new();
    if lines.read_line(&mut request_line).await.is_err() {
        return;
    }

    let request_line = request_line.trim().to_string();
    let parts: Vec<String> = request_line
        .split_whitespace()
        .map(String::from)
        .collect();
    if parts.len() < 2 {
        return;
    }
    let method = parts[0].as_str();
    let path_with_query = parts[1].clone();

    let token = request::extract_token_from_path(&path_with_query);

    let mut headers = String::new();
    while headers.lines().count() == 0 || !headers.ends_with("\r\n\r\n") {
        if lines.read_line(&mut headers).await.is_err() {
            break;
        }
        if headers.trim().is_empty() {
            break;
        }
    }

    // WebSocket upgrade: GET /ws/terminal/{nodeId}?token=xxx
    if method == "GET" && path_with_query.starts_with("/ws/terminal/") {
        let node_id: Option<i64> = path_with_query
            .split('/')
            .nth(3)
            .and_then(|s| s.split('?').next().unwrap_or(s).parse().ok());

        let Some(node_id) = node_id else {
            let _ = request::write_status_only(&mut lines, "400 Bad Request").await;
            return;
        };

        if !request::validate_token(token) {
            let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
            return;
        }

        let Some(ws_key) = request::extract_header_value(&headers, "Sec-WebSocket-Key") else {
            let _ = request::write_status_only(&mut lines, "400 Bad Request").await;
            return;
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
        tauri::async_runtime::spawn(ws::handle_ws_connection(ws_stream, node_id));
        return;
    }

    let path_without_query = path_with_query
        .split('?')
        .next()
        .unwrap_or(&path_with_query)
        .to_string();

    // Attention webhook: POST /api/attention/{session_id}
    // Called by Claude Code's Stop hook — no token required (localhost-only).
    if method == "POST" && path_without_query.starts_with("/api/attention/") {
        routes::attention::handle_post(&mut lines, &path_without_query).await;
        return;
    }

    // POST /api/nodes/create
    if method == "POST" && path_without_query == "/api/nodes/create" {
        if !request::validate_token(token) {
            let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
            return;
        }
        let content_length: usize = request::extract_header_value(&headers, "Content-Length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        routes::nodes::create(&mut lines, content_length).await;
        return;
    }

    // GET /v2[/...] — new mobile SPA served from the rust-embed bundle.
    // Same auth as the legacy `/` for now (root token); cookie migration
    // lands in stage 3.
    if method == "GET" && (path_without_query == "/v2" || path_without_query.starts_with("/v2/")) {
        if !request::validate_token(token) {
            let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
            return;
        }
        let _ = assets::serve_v2(&mut lines, &path_without_query).await;
        return;
    }

    // GET /api/*
    if method == "GET" && path_without_query.starts_with("/api/") {
        if !request::validate_token(token) {
            let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
            return;
        }
        let body = match path_without_query.as_str() {
            "/api/nodes" => routes::nodes::list_json(),
            "/api/providers" => routes::providers::list_json(),
            "/api/meshes" => routes::meshes::list_json(),
            _ => r#"{"error":"not found"}"#.to_string(),
        };
        let _ = request::write_json(&mut lines, "200 OK", &body).await;
        return;
    }

    // Default: serve the mobile HTML root.
    if !request::validate_token(token) {
        let _ = request::write_status_only(&mut lines, "401 Unauthorized").await;
        return;
    }
    let _ = assets::serve_mobile_root(&mut lines).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    async fn attention_post(path: &str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle_connection(stream, peer).await;
        });

        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request = format!(
            "POST {} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
            path
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 1024];
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream.read(&mut buf),
        )
        .await
        .expect("attention webhook hung")
        .expect("read failed");

        buf.truncate(n);
        let status_line = String::from_utf8_lossy(&buf);
        status_line
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn attention_webhook_returns_503_when_app_handle_unset() {
        // Locks the hot-path contract: the Claude Code Stop hook POSTs to
        // /api/attention/{session_id} without a token. In tests APP_HANDLE
        // is never initialised, so the handler short-circuits to 503 — but
        // a refactor that drops the path-parsing or short-circuit would
        // surface as a different status code or a hang.
        let status = attention_post("/api/attention/42").await;
        assert_eq!(status, 503, "expected 503 when APP_HANDLE is not set in tests");
    }

    #[tokio::test]
    async fn attention_webhook_returns_400_for_unparseable_session_id() {
        let status = attention_post("/api/attention/not-an-int").await;
        assert_eq!(status, 400, "expected 400 for non-integer session id");
    }

    async fn get_request(path: &str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle_connection(stream, peer).await;
        });

        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
            path
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 1024];
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream.read(&mut buf),
        )
        .await
        .expect("request hung")
        .expect("read failed");

        buf.truncate(n);
        String::from_utf8_lossy(&buf)
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn v2_root_returns_401_without_token() {
        assert_eq!(get_request("/v2").await, 401);
    }

    #[tokio::test]
    async fn v2_asset_returns_401_without_token() {
        assert_eq!(get_request("/v2/assets/index.js").await, 401);
    }

    #[tokio::test]
    async fn ws_upgrade_does_not_hang() {
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
        )
        .await;

        assert!(result.is_ok(), "handle_connection hung on WebSocket upgrade (regression)");
    }
}
