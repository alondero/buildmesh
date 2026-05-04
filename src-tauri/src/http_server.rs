//! Embedded HTTP/WebSocket server for mobile remote access.
//!
//! Serves the mobile web app and handles WebSocket terminal connections
//! for remote access from a phone on the same network.

use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::OnceLock;
use parking_lot::RwLock;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_tungstenite::{accept_async, tungstenite};

use crate::db;

// --- Mobile Web App HTML (served as root) ---

const MOBILE_APP_HTML: &str = include_str!("mobile_app.html");

// --- Server State ---

type PtySenders = HashMap<i64, broadcast::Sender<Vec<u8>>>;

struct HttpServerState {
    _broadcast_tx: broadcast::Sender<Vec<u8>>,
    _pty_outputs: Arc<RwLock<PtySenders>>,
}

static SERVER_STATE: OnceLock<Arc<HttpServerState>> = OnceLock::new();

fn get_server_state() -> Arc<HttpServerState> {
    SERVER_STATE.get().cloned().expect("HTTP server not started")
}

/// Start the HTTP server on the given port.
/// Spawns a background task and returns immediately.
pub fn start_http_server(port: u16) {
    let (broadcast_tx, _) = broadcast::channel(1024);
    let state = Arc::new(HttpServerState {
        _broadcast_tx: broadcast_tx,
        _pty_outputs: Arc::new(RwLock::new(HashMap::new())),
    });
    let _ = SERVER_STATE.set(state.clone());
    drop(state);

    tokio::spawn(async move {
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
                    let state = get_server_state();
                    tokio::spawn(handle_connection(stream, addr, state));
                }
                Err(e) => {
                    tracing::error!("HTTP accept error: {}", e);
                }
            }
        }
    });
}

async fn handle_connection(stream: TcpStream, addr: SocketAddr, state: Arc<HttpServerState>) {
    // Try WebSocket upgrade first
    let ws_stream = match accept_async(stream).await {
        Ok(ws) => ws,
        Err(_e) => {
            // Not a WebSocket upgrade — serve HTTP response
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
                MOBILE_APP_HTML.len(),
                MOBILE_APP_HTML
            );
            if let Ok(mut stream) = tokio::net::TcpStream::connect(addr).await {
                use tokio::io::AsyncWriteExt;
                let _ = stream.write_all(response.as_bytes()).await;
            }
            return;
        }
    };

    // WebSocket connection established
    let (ws_tx, mut ws_rx) = ws_stream.split();
    let mut broadcast_rx = state._broadcast_tx.subscribe();

    // Read task: forward mobile messages to PTY (future use)
    let read_handle = tokio::spawn(async move {
        while let Some(msg) = ws_rx.next().await {
            match msg {
                Ok(tungstenite::Message::Text(text)) => {
                    tracing::debug!("WS text from {}: {} bytes", addr, text.len());
                }
                Ok(tungstenite::Message::Binary(data)) => {
                    tracing::debug!("WS binary from {}: {} bytes", addr, data.len());
                }
                Ok(tungstenite::Message::Close(_)) => {
                    tracing::debug!("WS close from {}", addr);
                    break;
                }
                Err(e) => {
                    tracing::error!("WS read error from {}: {}", addr, e);
                    break;
                }
                _ => {}
            }
        }
    });

    // Write task: forward broadcast to mobile
    let mut ws_tx = ws_tx;
    let write_handle = tokio::spawn(async move {
        loop {
            match broadcast_rx.recv().await {
                Ok(data) => {
                    let msg = tungstenite::Message::Binary(data.into());
                    if ws_tx.send(msg).await.is_err() {
                        break;
                    }
                }
                Err(_) => {
                    break;
                }
            }
        }
    });

    // Wait for either to finish
    tokio::select! {
        _ = read_handle => {}
        _ = write_handle => {}
    }
}

// --- Token Validation ---

fn validate_token(token: &str) -> Option<i64> {
    if let Ok(meshes) = db::list_meshes() {
        for mesh in meshes {
            if let Ok(true) = db::validate_mesh_token(mesh.id, token) {
                return Some(mesh.id);
            }
        }
    }
    None
}

// --- Broadcast Relay for PTY ---

/// Register a PTY output channel for a node.
/// Returns a broadcast receiver that will receive all PTY output.
#[allow(dead_code)]
pub fn register_pty_channel(node_id: i64) -> broadcast::Receiver<Vec<u8>> {
    let state = get_server_state();

    // First try with a read lock
    let sender_opt: Option<broadcast::Sender<Vec<u8>>>;
    {
        let outputs = state._pty_outputs.read();
        sender_opt = outputs.get(&node_id).map(|s| s.clone());
    }
    if let Some(sender) = sender_opt {
        return sender.subscribe();
    }

    // Need to insert — upgrade to write lock
    let rx;
    {
        let mut outputs = state._pty_outputs.write();
        if let Some(sender) = outputs.get(&node_id).map(|s| s.clone()) {
            rx = sender.subscribe();
        } else {
            let (tx, r) = broadcast::channel(1024);
            outputs.insert(node_id, tx);
            rx = r;
        }
    }
    rx
}

/// Broadcast PTY output to all connected mobile clients for a node.
#[allow(dead_code)]
pub fn broadcast_pty_output(node_id: i64, data: &[u8]) {
    let state = get_server_state();
    let sender = {
        let outputs = state._pty_outputs.read();
        outputs.get(&node_id).map(|s| s.clone())
    };
    if let Some(sender) = sender {
        let _ = sender.send(data.to_vec());
    }
}
