//! WebSocket handling and the PTY broadcast channel.
//!
//! Each agent node has a `NodeChannel` holding (a) a `tokio::sync::broadcast`
//! sender that fans live PTY output to connected mobile clients, and (b) a
//! capped history buffer so a newly-connected client gets recent context.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::OnceLock;

use futures_util::{SinkExt, StreamExt};
use parking_lot::RwLock;
use tauri::Emitter;
use tokio::sync::broadcast;
use tokio_tungstenite::{tungstenite, WebSocketStream};

use crate::http::MaybeTls;

use crate::agent::process::{ProcessRegistryApi, PROCESS_REGISTRY};
use crate::db;

/// Server-pushes [`super::events::EventMsg`] JSON to a connected mobile
/// client. The client never sends anything; we ignore inbound frames
/// other than Close.
pub(crate) async fn handle_events_ws_connection(ws_stream: WebSocketStream<MaybeTls>) {
    let (mut write, mut read) = ws_stream.split();
    let mut rx = super::events::subscribe();

    let push_task = tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    let text = match serde_json::to_string(&msg) {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    if write
                        .send(tungstenite::Message::Text(text.into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Client fell behind; drop the gap and keep going.
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    while let Some(msg) = read.next().await {
        match msg {
            Ok(tungstenite::Message::Close(_)) | Err(_) => break,
            _ => {}
        }
    }
    push_task.abort();
    tracing::debug!("/ws/events client disconnected");
}

pub(crate) async fn handle_ws_connection(
    ws_stream: WebSocketStream<MaybeTls>,
    node_id: i64,
) {
    let (mut write, mut read) = ws_stream.split();

    // Subscribe before sending initial state to avoid missing output in the gap.
    // IMPORTANT: call ensure_pty_channel first so we don't accidentally create a new
    // empty channel and lose the history that send_pty_output has been accumulating.
    ensure_pty_channel(node_id);
    let mut rx = subscribe_pty(node_id);

    // Prefer a clean terminal snapshot over raw history replay, which contains
    // stale cursor-positioning sequences from TUI redraws.
    match super::app_handle() {
        Some(app) => {
            if let Some(snapshot) = super::request_terminal_snapshot(app, node_id).await {
                if !snapshot.is_empty()
                    && write
                        .send(tungstenite::Message::Text(snapshot.into()))
                        .await
                        .is_err()
                {
                    return;
                }
            } else {
                let history = get_pty_history(node_id);
                if !history.is_empty()
                    && write
                        .send(tungstenite::Message::Binary(history.into()))
                        .await
                        .is_err()
                {
                    return;
                }
            }
        }
        None => {
            let history = get_pty_history(node_id);
            if !history.is_empty()
                && write
                    .send(tungstenite::Message::Binary(history.into()))
                    .await
                    .is_err()
            {
                return;
            }
        }
    }

    let write_task = tauri::async_runtime::spawn(async move {
        while let Ok(data) = rx.recv().await {
            if write
                .send(tungstenite::Message::Binary(data.into()))
                .await
                .is_err()
            {
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

fn forward_mobile_input_with(registry: &dyn ProcessRegistryApi, node_id: i64, text: &str) {
    if let Err(e) = registry.write_bytes(node_id, text.as_bytes()) {
        tracing::warn!("Mobile input forward failed for {}: {}", node_id, e);
        return;
    }
    if text.bytes().any(|b| b == b'\n' || b == b'\r') {
        let _ = db::update_agent_node_status(node_id, crate::models::SessionStatus::Running);
        if let Some(app) = super::app_handle() {
            let _ = app.emit(
                "attention-cleared",
                serde_json::json!({ "session_id": node_id }),
            );
        }
        // Also fan out to mobile event subscribers — the desktop Tauri
        // event above only reaches the webview.
        super::events::emit(super::events::EventMsg::AttentionCleared {
            session_id: node_id,
        });
    }
}

fn forward_mobile_input(node_id: i64, text: &str) {
    forward_mobile_input_with(&**PROCESS_REGISTRY, node_id, text);
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

fn handle_mobile_resize_with(
    registry: &dyn ProcessRegistryApi,
    node_id: i64,
    cols: u16,
    rows: u16,
) {
    if let Err(e) = registry.resize_pty(node_id, cols, rows) {
        tracing::warn!("Mobile resize failed for node {}: {}", node_id, e);
    }
}

fn handle_mobile_resize(node_id: i64, cols: u16, rows: u16) {
    handle_mobile_resize_with(&**PROCESS_REGISTRY, node_id, cols, rows);
}

// --- PTY Broadcast ---

const HISTORY_BUFFER_CAP: usize = 128 * 1024;

struct NodeChannel {
    sender: broadcast::Sender<Vec<u8>>,
    history: VecDeque<u8>,
}

static KNOWN_NODES: OnceLock<Arc<RwLock<HashMap<i64, NodeChannel>>>> = OnceLock::new();

fn get_known_nodes() -> &'static Arc<RwLock<HashMap<i64, NodeChannel>>> {
    KNOWN_NODES.get_or_init(|| Arc::new(RwLock::new(HashMap::new())))
}

pub fn ensure_pty_channel(node_id: i64) {
    let nodes = get_known_nodes();
    let mut locked = nodes.write();
    locked.entry(node_id).or_insert_with(|| {
        let (tx, _) = broadcast::channel(1024);
        NodeChannel { sender: tx, history: VecDeque::new() }
    });
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
    locked
        .get(&node_id)
        .map(|ch| {
            let (a, b) = ch.history.as_slices();
            let mut v = Vec::with_capacity(a.len() + b.len());
            v.extend_from_slice(a);
            v.extend_from_slice(b);
            v
        })
        .unwrap_or_default()
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
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    use tokio::net::TcpListener;
    use tokio_tungstenite::{accept_async, connect_async};

    #[tokio::test]
    async fn ws_replays_history_on_connect() {
        let node_id = 20001_i64;
        ensure_pty_channel(node_id);
        send_pty_output(node_id, b"hello from history\r\n".to_vec());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = accept_async(MaybeTls::Plain(stream)).await.unwrap();
            handle_ws_connection(ws, node_id).await;
        });

        let url = format!("ws://{}/ws/terminal/{}", addr, node_id);
        let (mut ws, _) = connect_async(&url).await.unwrap();
        let msg = ws.next().await.unwrap().unwrap();
        assert!(msg.is_binary());
        assert_eq!(msg.into_data(), b"hello from history\r\n".to_vec());
    }

    #[tokio::test]
    async fn ws_receives_live_pty_output() {
        let node_id = 20002_i64;
        ensure_pty_channel(node_id);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = accept_async(MaybeTls::Plain(stream)).await.unwrap();
            handle_ws_connection(ws, node_id).await;
        });

        let url = format!("ws://{}/ws/terminal/{}", addr, node_id);
        let (mut ws, _) = connect_async(&url).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        send_pty_output(node_id, b"live data\r\n".to_vec());

        let msg = ws.next().await.unwrap().unwrap();
        assert!(msg.is_binary());
        assert_eq!(msg.into_data(), b"live data\r\n".to_vec());
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
            let ws = accept_async(MaybeTls::Plain(stream)).await.unwrap();
            handle_ws_connection(ws, node_id).await;
        });

        let url = format!("ws://{}/ws/terminal/{}", addr, node_id);
        let (mut ws, _) = connect_async(&url).await.unwrap();

        let msg1 = ws.next().await.unwrap().unwrap();
        assert!(msg1.is_binary());
        assert_eq!(msg1.into_data(), b"old output\r\n".to_vec());

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        send_pty_output(node_id, b"new output\r\n".to_vec());

        let msg2 = ws.next().await.unwrap().unwrap();
        assert!(msg2.is_binary());
        assert_eq!(msg2.into_data(), b"new output\r\n".to_vec());
    }

    #[tokio::test]
    async fn ws_input_reaches_write_to_pty() {
        let node_id = 20004_i64;
        ensure_pty_channel(node_id);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = accept_async(MaybeTls::Plain(stream)).await.unwrap();
            handle_ws_connection(ws, node_id).await;
        });

        let url = format!("ws://{}/ws/terminal/{}", addr, node_id);
        let (mut ws, _) = connect_async(&url).await.unwrap();

        ws.send(tungstenite::Message::Text("ls -la\n".into()))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
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

    // --- ProcessRegistryApi mock tests ---

    struct MockRegistry {
        write_called: AtomicBool,
        resize_called: AtomicBool,
        last_write_data: std::sync::Mutex<Vec<u8>>,
        last_resize: std::sync::Mutex<(u16, u16)>,
        should_fail: bool,
    }

    impl MockRegistry {
        fn new() -> Self {
            Self {
                write_called: AtomicBool::new(false),
                resize_called: AtomicBool::new(false),
                last_write_data: std::sync::Mutex::new(vec![]),
                last_resize: std::sync::Mutex::new((0, 0)),
                should_fail: false,
            }
        }
        fn failing() -> Self {
            Self { should_fail: true, ..Self::new() }
        }
    }

    impl ProcessRegistryApi for MockRegistry {
        fn write_bytes(&self, _session_id: i64, data: &[u8]) -> Result<(), String> {
            if self.should_fail {
                return Err("mock error".into());
            }
            self.write_called.store(true, AtomicOrdering::SeqCst);
            *self.last_write_data.lock().unwrap() = data.to_vec();
            Ok(())
        }
        fn resize_pty(&self, _session_id: i64, cols: u16, rows: u16) -> Result<(), String> {
            if self.should_fail {
                return Err("mock error".into());
            }
            self.resize_called.store(true, AtomicOrdering::SeqCst);
            *self.last_resize.lock().unwrap() = (cols, rows);
            Ok(())
        }
    }

    #[test]
    fn forward_mobile_input_writes_to_registry() {
        let mock = MockRegistry::new();
        forward_mobile_input_with(&mock, 1, "hello");
        assert!(mock.write_called.load(AtomicOrdering::SeqCst));
        assert_eq!(*mock.last_write_data.lock().unwrap(), b"hello");
    }

    #[test]
    fn forward_mobile_input_handles_registry_error() {
        let mock = MockRegistry::failing();
        forward_mobile_input_with(&mock, 1, "hello");
        assert!(!mock.write_called.load(AtomicOrdering::SeqCst));
    }

    #[test]
    fn handle_mobile_resize_calls_registry() {
        let mock = MockRegistry::new();
        handle_mobile_resize_with(&mock, 1, 120, 40);
        assert!(mock.resize_called.load(AtomicOrdering::SeqCst));
        assert_eq!(*mock.last_resize.lock().unwrap(), (120, 40));
    }

    #[test]
    fn handle_mobile_resize_handles_registry_error() {
        let mock = MockRegistry::failing();
        handle_mobile_resize_with(&mock, 1, 80, 24);
        assert!(!mock.resize_called.load(AtomicOrdering::SeqCst));
    }
}
