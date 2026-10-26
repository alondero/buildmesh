//! WebSocket handling and the PTY broadcast channel.
//!
//! Each agent node has a `NodeChannel` holding (a) a `tokio::sync::broadcast`
//! sender that fans live PTY output to connected mobile clients, and (b) a
//! capped history buffer so a newly-connected client gets recent context.
//!
//! Two lifecycle events touch that map and they are not the same event
//! (issue #2019):
//! - **Process exit / restart** clears the retained bytes but *keeps* the
//!   channel ([`clear_scrollback`]). A restarted agent reuses the same node
//!   id, and the terminal context a phone reconnects to is intentional.
//! - **Permanent node deletion** retires the whole entry
//!   ([`retire_pty_channel`]), releasing the sender, the history and the
//!   slot allocation. Only [`crate::agent::node_teardown`]'s deletion variant
//!   may do that; a retired id is fenced so a reconnecting client cannot
//!   resurrect the entry it was meant to reclaim.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::OnceLock;

use futures_util::{SinkExt, StreamExt};
use parking_lot::{Mutex, RwLock};
use tokio::sync::broadcast;
use tokio_tungstenite::{tungstenite, tungstenite::Bytes, WebSocketStream};

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

    let push = async move {
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
    };
    tokio::pin!(push);

    loop {
        tokio::select! {
            _ = &mut push => break,
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

    // Refuse a node that no longer exists, *before* creating anything (issue
    // #2019). Channel creation is otherwise unconditional, so a phone
    // auto-reconnecting to a node deleted while the app was closed — when the
    // in-memory fence is still empty — would conjure a fresh channel per
    // reconnect and no deletion would ever reclaim it. The row is the
    // authority; the fence only closes the in-process window where the delete
    // commits between this check and the create.
    //
    // A failed lookup is treated as "gone": a phone should reconnect, not hold
    // a socket open against a node this process cannot confirm.
    match crate::db::agent_node_exists(node_id) {
        Ok(true) => {}
        Ok(false) => {
            tracing::info!("/ws/terminal refused for deleted node {}", node_id);
            return;
        }
        Err(error) => {
            tracing::warn!("/ws/terminal could not confirm node {}: {}", node_id, error);
            return;
        }
    }

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

    let output = async move {
        loop {
            match rx.recv().await {
                Ok(data) => {
                    // `data` is a `Bytes` chunk: cloning it out of the
                    // broadcast is a refcount bump, and handing it to the
                    // frame is a move — no copy on the socket path, and no
                    // per-subscriber clone of the payload (issue #2019).
                    if write
                        .send(tungstenite::Message::Binary(data))
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
    };
    tokio::pin!(output);

    // `revocations` was subscribed before the snapshot send above so a revoke
    // landing mid-stream closes this socket immediately (issue #502).
    loop {
        tokio::select! {
            _ = &mut output => break,
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

    tracing::debug!("WS connection closed for node {}", node_id);
}

/// Test seam for [`write_mobile_input`]. The production wrapper
/// resolves the lifecycle sink from the global app handle, but unit
/// tests inject a mock sink here so the autoclear side-effects can be
/// asserted on without standing up a real SQLite database.
///
/// The registry forwards accepted bytes and reports decoded input activity.
/// Only a submission outside bracketed paste clears attention. Transport
/// replies, partial control packets and rejected writes cannot clear it.
pub(crate) fn write_mobile_input_with_sink(
    registry: &dyn ProcessRegistryApi,
    lifecycle_sink: &dyn crate::agent::session_lifecycle::SessionLifecycleSink,
    node_id: i64,
    text: &str,
) -> Result<crate::agent::process::InputOutcome, String> {
    let outcome = registry.write_input(node_id, text.as_bytes())?;
    // Only an accepted write may clear attention (issue #1530). A backpressured
    // buffer never reached the agent, so treating it as a resumed turn would
    // paint a node as running for input that does not exist.
    if outcome.is_accepted() {
        crate::agent::process::watch_for_interrupt(node_id, text.as_bytes());
        run_autoclear_side_effects(lifecycle_sink, node_id, outcome.activity);
    }
    Ok(outcome)
}

/// Write a raw keystroke sequence to a node's PTY and run the attention
/// autoclear side-effect when the registry observes a submitted prompt.
///
/// Production wrapper around [`write_mobile_input_with_sink`]. The
/// autoclear predicate and side-effects are covered by the `_with_sink`
/// unit tests — this wrapper is a thin dispatch by design (the only
/// production logic it carries is sink resolution from the global
/// app handle).
///
/// Returns the registry's write error verbatim so the HTTP `/api/nodes/{id}/input`
/// route can surface PTY-write failures as a 5xx — the WS path can swallow them
/// because a long-lived socket gets to try again, but a one-shot HTTP tap must
/// report success/failure to the caller. The autoclear side-effects are
/// best-effort (the WS broadcasts + DB lifecycle emits are themselves infallible);
/// an accepted `y\r` submission clears the awaiting_input flag exactly as
/// a typed Enter would, while pasted newlines remain draft content.
///
/// Sink resolution: production always has a live `app_handle` (set by
/// `lib.rs::setup`), so the `AppSessionLifecycleSink` branch fires
/// desktop events (`agent-lifecycle`, `attention-cleared`). The
/// `DbOnlySink` fallback only fires for the unit-test seam (no
/// `app_handle` is reachable in tests) and the post-shutdown drain
/// where the app is tearing down.
pub(crate) fn write_mobile_input(
    registry: &dyn ProcessRegistryApi,
    node_id: i64,
    text: &str,
) -> Result<crate::agent::process::InputOutcome, String> {
    if let Some(app) = super::app_handle() {
        let sink = crate::agent::session_lifecycle::AppSessionLifecycleSink { app };
        write_mobile_input_with_sink(registry, &sink, node_id, text)
    } else {
        write_mobile_input_with_sink(
            registry,
            &crate::agent::session_lifecycle::DbOnlySink,
            node_id,
            text,
        )
    }
}

/// The autoclear predicate + side-effects, isolated so the test seam
/// (`write_mobile_input_with_sink`) can drive them with a mock sink.
///
/// CR/LF in the payload means "user submitted" — the autoclear hypothesis
/// fires regardless of whether the submission is meaningful. Bare text stays
/// in the buffer; the predicate never runs.
fn run_autoclear_side_effects(
    sink: &dyn crate::agent::session_lifecycle::SessionLifecycleSink,
    node_id: i64,
    activity: crate::agent::process::InputActivity,
) {
    if activity.user_input {
        crate::attention_autoclear::disarm(node_id);
    }
    if !activity.submitted {
        return;
    }
    // `commit_submitted_input` owns the shell rule and the DB write + desktop emit
    // (issues #132, #535); only a committed resume reaches the mobile
    // broadcast below, the same condition the desktop path publishes on.
    let Ok(Some(_)) = crate::agent::process::commit_submitted_input(sink, node_id) else {
        return;
    };
    // Also fan out to mobile event subscribers — the desktop Tauri event
    // above only reaches the webview.
    super::events::emit(super::events::EventMsg::AttentionCleared {
        session_id: node_id,
    });
}

fn forward_mobile_input_with(registry: &dyn ProcessRegistryApi, node_id: i64, text: &str) {
    // Routes through the production `write_mobile_input` so the WS path
    // gets the same event dispatch as the HTTP path. Tests for this
    // helper use bare text ("hello") which has no CR/LF — the autoclear
    // side-effect never runs, so the sink type (`AppSessionLifecycleSink`
    // in production, `DbOnlySink` in the test seam) is irrelevant.
    match write_mobile_input(registry, node_id, text) {
        Ok(outcome) if outcome.is_accepted() => {}
        // Issue #1530: the socket cannot acknowledge an inbound write, so the
        // only honest channel back to the phone is this broadcast. Without it
        // a stalled agent would swallow keystrokes invisibly.
        Ok(_) => {
            // Read the depth through the injected registry so this path is
            // testable with a mock, rather than off the process-global one.
            let (queued_messages, queued_bytes) = registry.input_queue(node_id);
            tracing::warn!(
                node_id,
                queued_messages,
                queued_bytes,
                "mobile input stalled: PTY input queue full, {} bytes refused",
                text.len()
            );
            super::events::emit(super::events::EventMsg::TerminalInputStalled {
                session_id: node_id,
                // A saturating cast rather than `as`: the queue is capped at
                // 1 MiB, but the cast must not wrap if that ever changes — a
                // wrapped byte count would tell the phone the queue is nearly
                // empty while it is in fact refusing input.
                queued_bytes: u32::try_from(queued_bytes).unwrap_or(u32::MAX),
                queued_messages: queued_messages.clamp(0, i32::MAX as usize) as i32,
            });
        }
        Err(e) => {
            tracing::warn!("Mobile input forward failed for {}: {}", node_id, e);
        }
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
//
// One process-global map of per-node fanouts (issue #2019). The ring payload is
// `Bytes`, so a chunk is allocated once and shared by the ring and every
// receiver: a slow client no longer costs a private copy of each batch, and the
// socket path moves the chunk into the frame without copying it.
//
// The history buffer is deliberately *not* one of those sharers — it is a byte
// `VecDeque`, and `record_history` copies into it. Sharing it with the ring
// would mean storing chunk segments and trimming at segment granularity, which
// trades an exact `HISTORY_BUFFER_CAP` for one 128 KiB-bounded memcpy per
// batch. Only the ring-to-receiver sharing is claimed here because only that one
// scales with the number of subscribers.

/// Retained bytes a newly-connected client replays, and the tail re-sent after
/// a slow client lags. Unchanged by issue #2019: this is the intentional
/// terminal context, so *retain it on process exit*.
const HISTORY_BUFFER_CAP: usize = 128 * 1024;

/// Largest payload a single fanout slot may carry. One PTY write is split at
/// this boundary, so the ring's worst-case retention is a byte budget rather
/// than "slots × however big a batch happened to be".
const PTY_MAX_CHUNK_BYTES: usize = 32 * 1024;

/// Bytes of undrained live output one node may hold for its slowest
/// subscriber. The `PTY_FANOUT_SLOTS` below turn this into an exact bound.
///
/// Pre-#2019 the ring was 1024 slots of *variable-size* batches that every
/// receiver cloned: up to 1024 × 32 KiB = 32 MiB per node, and the clone
/// landed again for every reconnecting socket. The budget plus `Bytes` sharing
/// makes retention independent of subscriber count and caps it at 2 MiB.
const PTY_FANOUT_BYTE_BUDGET: usize = 2 * 1024 * 1024;

/// Slot count derived from the budget: `slots × PTY_MAX_CHUNK_BYTES` is the
/// worst case a subscriber that never drains can pin.
const PTY_FANOUT_SLOTS: usize = PTY_FANOUT_BYTE_BUDGET / PTY_MAX_CHUNK_BYTES;

/// How many retired node ids are remembered as a resurrection fence.
///
/// `agent_nodes.id` is `INTEGER PRIMARY KEY AUTOINCREMENT`, so a deleted id is
/// never handed to a different node and the fence cannot block a live one.
/// Bounded FIFO keeps the fence itself from becoming the next leak: the cost of
/// evicting the oldest tombstone is at most one empty channel for a node
/// deleted longer ago than every other deletion.
const RETIRED_NODE_MEMORY: usize = 4096;

struct NodeChannel {
    sender: broadcast::Sender<Bytes>,
    history: VecDeque<u8>,
}

static KNOWN_NODES: OnceLock<Arc<RwLock<HashMap<i64, NodeChannel>>>> = OnceLock::new();

/// Ids whose channel has been retired by permanent node deletion.
///
/// `ids` answers membership in O(1) and `order` supplies FIFO eviction, so the
/// lookup stays cheap even though it runs while the caller holds the
/// `KNOWN_NODES` write lock — the same lock every live PTY write needs. A linear
/// scan here would stall output for every node for the duration of a connect.
struct RetiredFence {
    ids: HashSet<i64>,
    order: VecDeque<i64>,
}

impl RetiredFence {
    fn contains(&self, node_id: i64) -> bool {
        self.ids.contains(&node_id)
    }

    /// Remember a deletion, evicting the oldest tombstone past the cap.
    fn record(&mut self, node_id: i64) {
        if self.ids.insert(node_id) {
            self.order.push_back(node_id);
        }
        while self.order.len() > RETIRED_NODE_MEMORY {
            if let Some(oldest) = self.order.pop_front() {
                self.ids.remove(&oldest);
            }
        }
    }
}

static RETIRED_NODES: OnceLock<Mutex<RetiredFence>> = OnceLock::new();

fn get_known_nodes() -> &'static Arc<RwLock<HashMap<i64, NodeChannel>>> {
    KNOWN_NODES.get_or_init(|| Arc::new(RwLock::new(HashMap::new())))
}

fn get_retired_nodes() -> &'static Mutex<RetiredFence> {
    RETIRED_NODES.get_or_init(|| {
        Mutex::new(RetiredFence {
            ids: HashSet::new(),
            order: VecDeque::new(),
        })
    })
}

/// Has this node's channel been retired?
///
/// Only the two creation paths ([`ensure_pty_channel`] and [`subscribe_pty`])
/// consult this, and both hold the `KNOWN_NODES` write lock while they do.
/// [`send_pty_output`] needs no check: it never creates an entry, and a delete
/// that already committed removed the entry it would have found.
///
/// A tombstone is a *belt*, not the authority. [`db::agent_node_exists`](crate::db::agent_node_exists)
/// is what refuses a deleted id across process restarts and fence eviction;
/// this only closes the window where a delete commits between that check and
/// the create. That is why it may be bounded and lossy without reopening the
/// leak.
fn is_retired(node_id: i64) -> bool {
    get_retired_nodes().lock().contains(node_id)
}

fn new_node_channel() -> NodeChannel {
    let (sender, _) = broadcast::channel(PTY_FANOUT_SLOTS);
    NodeChannel {
        sender,
        history: VecDeque::new(),
    }
}

pub fn ensure_pty_channel(node_id: i64) {
    let nodes = get_known_nodes();
    let mut locked = nodes.write();
    // Checked under the map lock, and `retire_pty_channel` takes these two
    // locks in the same order while holding both, so a create either happens
    // entirely before a delete (and is then removed) or entirely after it (and
    // sees the tombstone). There is no interleaving that leaves a channel
    // behind for a retired id.
    if is_retired(node_id) {
        return;
    }
    locked.entry(node_id).or_insert_with(new_node_channel);
}

pub fn subscribe_pty(node_id: i64) -> broadcast::Receiver<Bytes> {
    let nodes = get_known_nodes();
    let mut locked = nodes.write();
    if is_retired(node_id) {
        // A retired node gets a closed receiver rather than a fresh entry, so
        // the socket's write loop ends on `RecvError::Closed` and the phone
        // sees a clean close instead of an immortal empty terminal (and the
        // entry the retirement just reclaimed does not come back).
        let (sender, receiver) = broadcast::channel(1);
        drop(sender);
        return receiver;
    }
    locked
        .entry(node_id)
        .or_insert_with(new_node_channel)
        .sender
        .subscribe()
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

/// Append one PTY write to the retained history, keeping the most recent
/// [`HISTORY_BUFFER_CAP`] bytes.
///
/// An oversized write is trimmed *before* it is copied in: extending first and
/// draining after spiked the buffer to the size of the whole write, so a single
/// multi-megabyte batch briefly held twice the cap (issue #2019).
fn record_history(channel: &mut NodeChannel, data: &[u8]) {
    if data.len() >= HISTORY_BUFFER_CAP {
        let tail = &data[data.len() - HISTORY_BUFFER_CAP..];
        channel.history.clear();
        channel.history.extend(tail.iter().copied());
        return;
    }
    channel.history.extend(data.iter().copied());
    let excess = channel.history.len().saturating_sub(HISTORY_BUFFER_CAP);
    if excess > 0 {
        channel.history.drain(..excess);
    }
}

/// Split one PTY write into slot-sized [`Bytes`] chunks, allocating once per
/// chunk. The PTY batcher already coalesces to 32 KiB, so ordinary traffic
/// yields exactly one chunk; the split is what keeps the byte budget a hard
/// bound when some future producer hands us a whole build log in one call.
/// Byte boundaries are preserved verbatim, so a subscriber concatenating the
/// chunks sees the original stream.
fn fanout_chunks(data: &[u8]) -> Vec<Bytes> {
    data.chunks(PTY_MAX_CHUNK_BYTES)
        .map(Bytes::copy_from_slice)
        .collect()
}

pub fn send_pty_output(node_id: i64, data: impl AsRef<[u8]>) {
    let data = data.as_ref();
    let chunks = fanout_chunks(data);
    if chunks.is_empty() {
        return;
    }
    // One write-lock acquisition for the whole batch: history order and fanout
    // order agree, and the send happens after the lock is dropped.
    let sender = {
        let mut locked = get_known_nodes().write();
        match locked.get_mut(&node_id) {
            Some(channel) => {
                record_history(channel, data);
                Some(channel.sender.clone())
            }
            // Never create here. Output for an unknown node is dropped, and a
            // retired node stays retired — the entry retirement reclaimed must
            // not reappear because a producer's last write raced the delete.
            None => None,
        }
    };
    if let Some(sender) = sender {
        for chunk in chunks {
            // No subscriber is the normal case (the phone may be asleep); the
            // history copy above is what survives until it connects.
            let _ = sender.send(chunk);
        }
    }
}

/// Drop the retained bytes for a node whose *process* ended or was killed,
/// keeping its channel so a restart reuses the same fanout and a reconnecting
/// client still gets live output. Permanent node deletion uses
/// [`retire_pty_channel`] instead — clearing the map on process exit would
/// throw away the retained terminal context that is intentional (issue #2019).
pub fn clear_scrollback(node_id: i64) {
    let nodes = get_known_nodes();
    let mut locked = nodes.write();
    if let Some(channel) = locked.get_mut(&node_id) {
        // A fresh deque, not `clear()`: the cap-sized ring buffer is released
        // with the bytes it held rather than kept as a dormant allocation for
        // a process that has already gone.
        channel.history = VecDeque::new();
    }
}

/// Retire a permanently deleted node's channel: drop the entry, which releases
/// the sender, its ring slots and the retained history in one move, then fence
/// the id so nothing recreates it (issue #2019).
///
/// Call this only from permanent deletion — a process exit or restart must
/// keep its channel (see [`clear_scrollback`]).
pub fn retire_pty_channel(node_id: i64) {
    // Both locks are held across the removal and the tombstone, in the same
    // order the creation paths use (map, then fence). Releasing the map guard
    // first would reopen the leak: a create landing between the two would
    // insert a fresh channel for an id the fence is about to mark deleted, and
    // nothing would ever retire that entry. `drop(nodes)` happens on scope
    // exit — no early return may skip it.
    let mut nodes = get_known_nodes().write();
    let retired = nodes.remove(&node_id);
    get_retired_nodes().lock().record(node_id);
    drop(nodes);

    if retired.is_some() {
        tracing::debug!(node_id, "retired PTY broadcast channel for deleted node");
    }
}

/// Live state of `node_id`'s channel: retained history length and the capacity
/// of the ring that buffer owns. `None` when the node has no entry at all.
///
/// Test seam for the reclamation assertions in the #2019 tests — it reads the
/// real map and the real history buffer, so a test cannot pass against
/// structures the production path stopped using. The fanout's slot count is
/// not reported because `broadcast::Sender` exposes no capacity accessor; the
/// budget tests compare measured retention against [`PTY_FANOUT_SLOTS`]
/// instead, which still fails if the ring is ever built with another count.
#[cfg(test)]
fn pty_channel_state(node_id: i64) -> Option<(usize, usize)> {
    let nodes = get_known_nodes();
    let locked = nodes.read();
    locked
        .get(&node_id)
        .map(|channel| (channel.history.len(), channel.history.capacity()))
}

#[cfg(test)]
#[path = "ws_tests.rs"]
mod tests;
