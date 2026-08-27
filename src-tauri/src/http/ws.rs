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
use tokio::sync::broadcast;
use tokio_tungstenite::{tungstenite, WebSocketStream};

use crate::http::MaybeTls;

use crate::agent::process::{ProcessRegistryApi, PROCESS_REGISTRY};

/// Should this socket close in response to a revocation signal (issue #502)? A
/// root-token socket (`device_id == None`) owns no device row and is never
/// revocable. On `Lagged` we conservatively close any *device* socket — a
/// still-valid device just reconnects and re-authenticates seamlessly, while a
/// revoked one is correctly dropped even if its exact id scrolled past the
/// buffer. Pulled out as a pure function so the decision is unit-testable
/// without standing up a real WebSocket.
fn revocation_terminates(
    signal: Result<i64, broadcast::error::RecvError>,
    device_id: Option<i64>,
) -> bool {
    let Some(my_id) = device_id else {
        return false;
    };
    match signal {
        Ok(revoked) => revoked == my_id,
        Err(broadcast::error::RecvError::Lagged(_)) => true,
        Err(broadcast::error::RecvError::Closed) => false,
    }
}

/// Server-pushes [`super::events::EventMsg`] JSON to a connected mobile
/// client. The client never sends anything; we ignore inbound frames
/// other than Close. `device_id` is the paired device this socket belongs to
/// (issue #502; `None` for the root token) — revoking it closes the socket.
pub(crate) async fn handle_events_ws_connection(
    ws_stream: WebSocketStream<MaybeTls>,
    device_id: Option<i64>,
) {
    let (mut write, mut read) = ws_stream.split();
    let mut rx = super::events::subscribe();
    let mut revocations = super::revocation::subscribe();

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

    loop {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    Some(Ok(tungstenite::Message::Close(_))) | Some(Err(_)) | None => break,
                    _ => {}
                }
            }
            signal = revocations.recv() => {
                if revocation_terminates(signal, device_id) {
                    tracing::info!("/ws/events terminated by revocation of device {:?}", device_id);
                    break;
                }
            }
        }
    }
    push_task.abort();
    tracing::debug!("/ws/events client disconnected");
}

pub(crate) async fn handle_ws_connection(
    ws_stream: WebSocketStream<MaybeTls>,
    node_id: i64,
    device_id: Option<i64>,
) {
    let (mut write, mut read) = ws_stream.split();

    // Subscribe to revocations FIRST — before the (possibly slow, large-scrollback)
    // initial snapshot send below (issue #502). A broadcast only reaches receivers
    // present at send time and buffers per-receiver, so subscribing up front means a
    // revoke fired *during* the snapshot await is retained and seen at the first
    // `recv()` in the loop, rather than silently lost while we're busy sending.
    let mut revocations = super::revocation::subscribe();

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
        loop {
            match rx.recv().await {
                Ok(data) => {
                    if write
                        .send(tungstenite::Message::Binary(data.into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    // Issue #1238: PTY output outpaced the 1024-slot
                    // broadcast buffer. Re-send the tail of history so the
                    // mobile client doesn't sit on a frozen terminal until
                    // the user manually reconnects. The next `recv()` resumes
                    // at the post-lag cursor; the tail may overlap with bytes
                    // already in xterm.js scrollback — accepted as a
                    // worse-is-better recovery compared to the silent death
                    // that `while let Ok(...)` caused here before the fix.
                    tracing::warn!(
                        "WS for node {} lagged, dropped {} chunks — resending history tail",
                        node_id,
                        skipped
                    );
                    let tail = get_pty_history(node_id);
                    if !tail.is_empty()
                        && write
                            .send(tungstenite::Message::Binary(tail.into()))
                            .await
                            .is_err()
                    {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // `revocations` was subscribed before the snapshot send above so a revoke
    // landing mid-stream closes this socket immediately (issue #502).
    loop {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    Some(Ok(tungstenite::Message::Text(text))) => {
                        match parse_resize_message(&text) {
                            Some(Ok((cols, rows))) => {
                                handle_mobile_resize(node_id, cols, rows);
                            }
                            Some(Err(reason)) => {
                                // Issue #1263: a malformed resize frame is
                                // logged and dropped — never injected into
                                // the PTY as text, where it would land as
                                // garbage on the running shell.
                                tracing::warn!(
                                    "WS resize frame rejected for node {}: {:?}",
                                    node_id,
                                    reason
                                );
                            }
                            None => {
                                forward_mobile_input(node_id, &text);
                            }
                        }
                    }
                    Some(Ok(tungstenite::Message::Binary(data))) => {
                        let text = String::from_utf8_lossy(&data);
                        forward_mobile_input(node_id, &text);
                    }
                    Some(Ok(tungstenite::Message::Close(_))) | Some(Err(_)) | None => break,
                    _ => {}
                }
            }
            signal = revocations.recv() => {
                if revocation_terminates(signal, device_id) {
                    tracing::info!(
                        "WS for node {} terminated by revocation of device {:?}",
                        node_id,
                        device_id
                    );
                    break;
                }
            }
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
        crate::attention_autoclear::disarm(node_id);
        // Routes through SessionLifecycle (issue #132) for the DB write +
        // desktop emit; the mobile broadcast is a separate channel kept
        // below.
        if let Some(app) = super::app_handle() {
            let sink = crate::agent::session_lifecycle::AppSessionLifecycleSink {
                app: &app.clone(),
            };
            let _ = crate::agent::session_lifecycle::on_attention_cleared(&sink, node_id);
        } else {
            let _ = crate::agent::session_lifecycle::on_attention_cleared(
                &crate::agent::session_lifecycle::DbOnlySink,
                node_id,
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

/// Largest cols/rows the mobile client may ask for. Anything larger is
/// almost certainly a corrupt/malicious frame, not a legitimate terminal
/// size — ConPTY tolerates it today but the surface is fragile. Picked
/// well above the largest realistic xterm viewport so this never fires
/// on a real device (issue #1263).
const MAX_RESIZE_DIMENSION: u64 = 1000;

/// Three-state result so the caller can tell "not a resize message" apart
/// from "was a resize message but malformed":
/// - `None` (outer) — caller forwards the text as mobile input (the
///   normal path for non-resize frames).
/// - `Some(Ok((c, r)))` — caller invokes `handle_mobile_resize`.
/// - `Some(Err(reason))` — caller logs + drops the frame; never injects
///   malformed JSON as PTY input. Crucially, the moment `type == "resize"`
///   is confirmed, the parser commits to a resize frame — any subsequent
///   extraction failure (missing fields, wrong types, nulls) MUST return
///   `Some(Err(...))` and NEVER fall through to `None`, which the caller
///   would treat as "not a resize frame" and inject into the PTY as raw
///   keystrokes (review-feedback regression class — issue #1263 review).
type ResizeParseResult = Option<Result<(u16, u16), ResizeError>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResizeError {
    /// `type == "resize"` was confirmed, but cols/rows failed to parse
    /// (missing, null, wrong type, negative, non-integer).
    MalformedPayload,
    ZeroDimension,
    DimensionTooLarge,
}

/// Typed payload: serde enforces `cols` and `rows` are non-negative
/// integers. Negative numbers, strings, nulls, floats, and missing fields
/// all fail deserialization — the caller maps that failure to
/// `ResizeError::MalformedPayload`.
#[derive(serde::Deserialize)]
struct ResizeFrame {
    cols: u64,
    rows: u64,
}

fn parse_resize_message(text: &str) -> ResizeParseResult {
    if !text.starts_with('{') {
        return None;
    }
    // Step 1: parse the outer JSON. If the text isn't valid JSON at all,
    // it can't be a resize frame — caller forwards as input (the normal
    // path for keystrokes that aren't JSON).
    let v: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return None,
    };
    // Step 2: confirm `type == "resize"`. Any other type (or missing /
    // non-string type) means this isn't a resize frame — forward as input.
    // This is the ONLY path that returns `None` after we've seen the `{`.
    if v.get("type").and_then(|t| t.as_str()) != Some("resize") {
        return None;
    }
    // Step 3: type confirmed. ANY extraction failure from here on is a
    // malformed RESIZE frame, not a non-resize frame — `Some(Err(...))`,
    // never `None`. The caller drops these (logs + returns), never
    // forwarding as PTY input.
    let frame: ResizeFrame = match serde_json::from_value(v) {
        Ok(f) => f,
        Err(_) => return Some(Err(ResizeError::MalformedPayload)),
    };
    if frame.cols == 0 || frame.rows == 0 {
        return Some(Err(ResizeError::ZeroDimension));
    }
    if frame.cols > MAX_RESIZE_DIMENSION || frame.rows > MAX_RESIZE_DIMENSION {
        return Some(Err(ResizeError::DimensionTooLarge));
    }
    // Safe: 0 < cols/rows <= 1000 fits u16 exactly (u16::MAX = 65_535).
    Some(Ok((frame.cols as u16, frame.rows as u16)))
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

    #[test]
    fn revocation_terminates_only_the_matching_device() {
        use broadcast::error::RecvError;
        // A root-token socket (None) is never revocable.
        assert!(!revocation_terminates(Ok(5), None));
        // A device socket closes on its own id, ignores others.
        assert!(revocation_terminates(Ok(5), Some(5)));
        assert!(!revocation_terminates(Ok(6), Some(5)));
        // Lagged → conservatively close any device socket (it just reconnects).
        assert!(revocation_terminates(Err(RecvError::Lagged(3)), Some(5)));
        assert!(!revocation_terminates(Err(RecvError::Lagged(3)), None));
        // A closed channel never forces a termination.
        assert!(!revocation_terminates(Err(RecvError::Closed), Some(5)));
    }

    #[tokio::test]
    async fn revoking_the_device_closes_its_live_terminal_ws() {
        // The hard AC: a revoke must drop an already-open socket, not just the
        // next request. A node with no history sends nothing on connect, so the
        // only thing that ends the stream is the revocation signal.
        let node_id = 20055_i64;
        let device_id = 7777_i64;
        ensure_pty_channel(node_id);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = accept_async(MaybeTls::Plain(stream)).await.unwrap();
            handle_ws_connection(ws, node_id, Some(device_id)).await;
        });

        let url = format!("ws://{}/ws/terminal/{}", addr, node_id);
        let (mut ws, _) = connect_async(&url).await.unwrap();

        // Let the handler reach its `revocation::subscribe()` before we fire —
        // a broadcast only reaches receivers present at send time.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        super::super::revocation::revoke(device_id);

        let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match ws.next().await {
                    None | Some(Err(_)) => break true,
                    Some(Ok(m)) if m.is_close() => break true,
                    Some(Ok(_)) => continue,
                }
            }
        })
        .await
        .unwrap_or(false);
        assert!(closed, "revoking the device must terminate its live WS");
    }

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
            handle_ws_connection(ws, node_id, None).await;
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
            handle_ws_connection(ws, node_id, None).await;
        });

        let url = format!("ws://{}/ws/terminal/{}", addr, node_id);
        let (mut ws, _) = connect_async(&url).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        send_pty_output(node_id, b"live data\r\n".to_vec());

        let msg = ws.next().await.unwrap().unwrap();
        assert!(msg.is_binary());
        assert_eq!(msg.into_data(), b"live data\r\n".to_vec());
    }

    // Issue #1238 regression: the WS write_task must survive a broadcast
    // `Lagged` (1024-slot overflow under a flood of PTY output). Pre-fix,
    // `while let Ok(data) = rx.recv().await` exited on the Lagged variant
    // and the task died silently — the socket stayed open but no PTY bytes
    // flowed until the user manually reconnected. Post-fix, the task logs
    // the gap, re-sends the history tail, and keeps forwarding.
    #[tokio::test]
    async fn ws_write_task_survives_broadcast_lag() {
        let node_id = 20006_i64;
        ensure_pty_channel(node_id);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = accept_async(MaybeTls::Plain(stream)).await.unwrap();
            handle_ws_connection(ws, node_id, None).await;
        });

        let url = format!("ws://{}/ws/terminal/{}", addr, node_id);
        let (mut ws, _) = connect_async(&url).await.unwrap();

        // Let the handler subscribe to the broadcast + finish initial-state
        // negotiation before we start flooding.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Overflow the 1024-slot broadcast buffer. The handler's write_task
        // interleaves `rx.recv()` (advances the receiver position) with
        // `write.send()` (blocks once the TCP/WS sink fills). Because the
        // client never calls `ws.next()` the sink fills, `write.send()`
        // blocks, and during the block these sends accumulate past capacity
        // — so the next `rx.recv()` on the handler's receiver returns
        // `RecvError::Lagged`.
        for i in 0..1100 {
            send_pty_output(node_id, format!("flood {}\n", i).into_bytes());
        }

        // Let the runtime service the handler so the Lagged event fires
        // and the recovery (history tail re-send) lands before we look for
        // the marker.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Send a unique marker the test can grep for in the downstream
        // frames. If the write_task survived the Lagged, this is forwarded;
        // if it died (the pre-fix bug), the marker never reaches the client
        // and the loop below runs out the deadline.
        let marker = b"MARKER_AFTER_LAG";
        send_pty_output(node_id, marker.to_vec());

        // Drain the client. Per-iteration timeouts so we keep reading past
        // the history-tail re-send frame(s) without bailing on the first
        // quiet stretch.
        let mut received_marker = false;
        let mut all_data = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(500), ws.next()).await {
                Ok(Some(Ok(msg))) => {
                    let data = msg.into_data();
                    all_data.extend_from_slice(&data);
                    if data.windows(marker.len()).any(|w| w == marker) {
                        received_marker = true;
                        break;
                    }
                }
                Ok(Some(Err(_))) | Ok(None) => break,
                Err(_) => continue, // quiet stretch — keep waiting
            }
        }
        assert!(
            received_marker,
            "WS write_task died after Lagged — marker never reached client. \
             Received {} bytes before deadline.",
            all_data.len()
        );
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
            handle_ws_connection(ws, node_id, None).await;
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
            handle_ws_connection(ws, node_id, None).await;
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

    // -- parse_resize_message (issue #1263) ----------------------------------
    //
    // The validation at the WS boundary pins the "no zero/oversized
    // dimensions reach ConPTY" contract. The three-state return shape
    // (`None` = not a resize frame, `Some(Ok)` = valid dims,
    // `Some(Err)` = malformed resize frame) lets the caller decide
    // between forwarding-as-input, applying, and warning-and-dropping.

    #[test]
    fn parse_resize_accepts_normal_dimensions() {
        assert_eq!(
            parse_resize_message(r#"{"type":"resize","cols":120,"rows":40}"#),
            Some(Ok((120, 40)))
        );
        // Boundary: exactly MAX_RESIZE_DIMENSION is allowed.
        assert_eq!(
            parse_resize_message(r#"{"type":"resize","cols":1000,"rows":1000}"#),
            Some(Ok((1000, 1000)))
        );
    }

    #[test]
    fn parse_resize_rejects_zero_dimensions() {
        // 0×0 (and 0×N, N×0) must NOT reach the PTY — ConPTY tolerates
        // it today but the surface is fragile.
        assert_eq!(
            parse_resize_message(r#"{"type":"resize","cols":0,"rows":0}"#),
            Some(Err(ResizeError::ZeroDimension))
        );
        assert_eq!(
            parse_resize_message(r#"{"type":"resize","cols":80,"rows":0}"#),
            Some(Err(ResizeError::ZeroDimension))
        );
        assert_eq!(
            parse_resize_message(r#"{"type":"resize","cols":0,"rows":24}"#),
            Some(Err(ResizeError::ZeroDimension))
        );
    }

    #[test]
    fn parse_resize_rejects_implausibly_large_dimensions() {
        // > MAX_RESIZE_DIMENSION — almost certainly a corrupt frame.
        assert_eq!(
            parse_resize_message(r#"{"type":"resize","cols":1001,"rows":40}"#),
            Some(Err(ResizeError::DimensionTooLarge))
        );
        assert_eq!(
            parse_resize_message(r#"{"type":"resize","cols":80,"rows":99999}"#),
            Some(Err(ResizeError::DimensionTooLarge))
        );
    }

    #[test]
    fn parse_resize_returns_none_for_non_resize_messages() {
        // Not a resize message at all → caller forwards as input.
        assert_eq!(parse_resize_message(r#"{"type":"keystroke","data":"x"}"#), None);
        // No `type` field at all.
        assert_eq!(parse_resize_message(r#"{"cols":80,"rows":24}"#), None);
        // Non-JSON text input.
        assert_eq!(parse_resize_message("ls\n"), None);
        // Empty string.
        assert_eq!(parse_resize_message(""), None);
    }

    #[test]
    fn parse_resize_handles_malformed_json_gracefully() {
        // Truncated/invalid JSON that DOES start with '{' → not a resize
        // message (the outer None). Caller forwards as input (or, for a
        // brace-prefixed garbage payload, drops — but it never crashes).
        assert_eq!(parse_resize_message("{not json"), None);
    }

    // -- parse_resize_message: malformed-payload cases (review feedback) ------
    //
    // Once `type == "resize"` is confirmed, ANY extraction failure must
    // return `Some(Err(MalformedPayload))` — never `None`, which the caller
    // would forward as PTY input (injection regression). These tests pin
    // every variant serde can surface for a resize frame.

    #[test]
    fn parse_resize_rejects_string_cols_as_malformed_not_forwardable() {
        // `cols:"abc"` — type matches "resize" but cols is the wrong type.
        // Must NOT fall through to `None` (which the caller would dump
        // into the running shell as raw text).
        assert_eq!(
            parse_resize_message(r#"{"type":"resize","cols":"abc","rows":10}"#),
            Some(Err(ResizeError::MalformedPayload))
        );
    }

    #[test]
    fn parse_resize_rejects_missing_fields_as_malformed() {
        // No cols/rows at all. After type confirmation this is a malformed
        // RESIZE frame, not "not a resize frame".
        assert_eq!(
            parse_resize_message(r#"{"type":"resize"}"#),
            Some(Err(ResizeError::MalformedPayload))
        );
    }

    #[test]
    fn parse_resize_rejects_null_cols_as_malformed() {
        assert_eq!(
            parse_resize_message(r#"{"type":"resize","cols":null,"rows":10}"#),
            Some(Err(ResizeError::MalformedPayload))
        );
    }

    #[test]
    fn parse_resize_rejects_negative_cols_as_malformed() {
        // `cols:-1` — serde's u64 rejects negatives at the type level.
        assert_eq!(
            parse_resize_message(r#"{"type":"resize","cols":-1,"rows":10}"#),
            Some(Err(ResizeError::MalformedPayload))
        );
    }

    #[test]
    fn parse_resize_rejects_float_cols_as_malformed() {
        // `cols:1.5` — serde's u64 rejects non-integers.
        assert_eq!(
            parse_resize_message(r#"{"type":"resize","cols":1.5,"rows":10}"#),
            Some(Err(ResizeError::MalformedPayload))
        );
    }

    #[test]
    fn parse_resize_rejects_wrong_type_field_as_malformed() {
        // `type:123` — the type tag itself is the wrong type. This means
        // "not a resize frame" → caller forwards as input (the only
        // correct post-confirmation `None` path).
        assert_eq!(
            parse_resize_message(r#"{"type":123,"cols":80,"rows":24}"#),
            None
        );
    }
}
