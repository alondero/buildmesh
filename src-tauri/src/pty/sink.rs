//! Per-session binary PTY-output sink (issue #1385 / #1393).
//!
//! Production PTY bytes go **only** over a Tauri `Channel`
//! (`InvokeResponseBody::Raw`). They are never mixed with a JSON event:
//! those two IPC paths have no ordering, and a subscribe that lands
//! mid-stream would let chunk 2 on the Channel overtake chunk 1 on the
//! event bus (split ANSI). Bytes that arrive before subscribe are held
//! on the sink and flushed in order when the Channel is registered.
//!
//! Agent terminals and Build/Run terminals share the same numeric
//! session/node id space, so they **must not** share a map: agent bytes
//! would paint into a Build/Run xterm (and vice versa). [`AGENT`] and
//! [`BUILD_RUN`] are two independent maps of the same [`OutputSink`]
//! type.

use super::output_window::OutputWindow;
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use std::collections::{HashMap, VecDeque};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tauri::ipc::{Channel, InvokeResponseBody};

/// Cap on bytes buffered before the frontend has subscribed. Matches
/// `TerminalWriter.MAX_PENDING_BYTES` -- past this, older bytes are dropped
/// because xterm's scrollback cannot retain more anyway.
const PENDING_CAP: usize = 4 * 1024 * 1024;

const BUFFERED_COLOUR_QUERIES: [&[u8]; 4] = [
    b"\x1b]10;?\x07",
    b"\x1b]10;?\x1b\\",
    b"\x1b]11;?\x07",
    b"\x1b]11;?\x1b\\",
];

fn filter_buffered_colour_queries(pending: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut replay = Vec::with_capacity(pending.len());
    let mut index = 0;

    while index < pending.len() {
        let remaining = &pending[index..];
        if let Some(query) = BUFFERED_COLOUR_QUERIES
            .iter()
            .find(|query| remaining.starts_with(query))
        {
            index += query.len();
            continue;
        }

        if BUFFERED_COLOUR_QUERIES
            .iter()
            .any(|query| query.len() > remaining.len() && query.starts_with(remaining))
        {
            return (replay, remaining.to_vec());
        }

        replay.push(pending[index]);
        index += 1;
    }

    (replay, Vec::new())
}

struct LiveOutput {
    window: OutputWindow,
    pending_colour_query_prefix: parking_lot::Mutex<Vec<u8>>,
}

impl LiveOutput {
    fn new(
        channel: Channel<InvokeResponseBody>,
        pending_colour_query_prefix: Vec<u8>,
        replay: Vec<u8>,
    ) -> Self {
        Self {
            window: OutputWindow::new(channel, replay),
            pending_colour_query_prefix: parking_lot::Mutex::new(pending_colour_query_prefix),
        }
    }

    fn send(&self, data: Vec<u8>, is_current: &dyn Fn() -> bool) {
        let data = {
            let mut prefix = self.pending_colour_query_prefix.lock();
            if !is_current() {
                return;
            }
            if prefix.is_empty() {
                data
            } else {
                let mut candidate = std::mem::take(&mut *prefix);
                candidate.extend(data);
                if let Some(query) = BUFFERED_COLOUR_QUERIES
                    .iter()
                    .find(|query| candidate.starts_with(query))
                {
                    candidate[query.len()..].to_vec()
                } else if BUFFERED_COLOUR_QUERIES
                    .iter()
                    .any(|query| query.starts_with(&candidate))
                {
                    *prefix = candidate;
                    return;
                } else {
                    candidate
                }
            }
        };
        // Filtering owns only a short CPU section. Never retain its mutex
        // while waiting for another producer or frontend decode credit.
        self.window.send(data, is_current);
    }
}

enum SinkState {
    /// Frontend has not subscribed yet. Append-only; flushed on `set`.
    /// `VecDeque` so overflow drops the oldest bytes in O(1) per pop
    /// instead of memmoving a multi-MiB `Vec::drain` tail each batcher
    /// chunk (see the 4 MiB trap when a noisy build script dumps output
    /// before the frontend mounts).
    Pending(VecDeque<u8>),
    /// Shared live window; batchers wait outside the sink lock.
    Live(Arc<LiveOutput>),
    /// Terminal disposed or node deleted. Further sends are dropped.
    /// Process exit must not enter this state.
    Closed,
}

/// Per-session slot the batcher clones at spawn time.
pub struct OutputSink {
    inner: RwLock<SinkState>,
}

impl OutputSink {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(SinkState::Pending(VecDeque::new())),
        }
    }

    /// Deliver `data` without cloning the Channel. Always the Channel
    /// path (or a pending buffer): the caller must not also emit JSON.
    pub fn send_owned(&self, data: Vec<u8>) {
        self.send_while(data, &|| true);
    }

    fn send_while(&self, data: Vec<u8>, is_current: &dyn Fn() -> bool) {
        if !is_current() {
            return;
        }
        let live = {
            let mut inner = self.inner.write();
            match &mut *inner {
                SinkState::Live(live) => Arc::clone(live),
                SinkState::Pending(buf) => {
                    buf.extend(data);
                    while buf.len() > PENDING_CAP {
                        buf.pop_front();
                    }
                    return;
                }
                SinkState::Closed => return,
            }
        };
        live.send(data, is_current);
    }

    /// Each process owns a cancellation flag, while the terminal owns the Channel.
    pub fn producer(self: &Arc<Self>, cancelled: Arc<AtomicBool>) -> OutputProducer {
        OutputProducer {
            sink: Arc::clone(self),
            cancelled,
        }
    }

    fn wake_producers(&self) {
        if let SinkState::Live(live) = &*self.inner.read() {
            live.window.wake();
        }
    }

    pub(crate) fn set(&self, channel: Channel<InvokeResponseBody>) {
        let mut inner = self.inner.write();
        let (pending, live_prefix): (Vec<u8>, Vec<u8>) = match &mut *inner {
            SinkState::Pending(buf) => (std::mem::take(buf).into(), Vec::new()),
            SinkState::Live(live) => {
                live.window.close();
                (
                    Vec::new(),
                    std::mem::take(&mut *live.pending_colour_query_prefix.lock()),
                )
            }
            // Closed is a terminal state (terminal disposed or node
            // deleted). Do not resurrect it into Live -- the prior
            // subscription was deliberately torn down and any new
            // caller here is a stale reference. Drop the channel.
            SinkState::Closed => return,
        };
        let (replay, replay_prefix) = filter_buffered_colour_queries(&pending);
        let pending_colour_query_prefix = if live_prefix.is_empty() {
            replay_prefix
        } else {
            live_prefix
        };
        let live = Arc::new(LiveOutput::new(
            channel,
            pending_colour_query_prefix,
            replay,
        ));
        live.window.start();
        *inner = SinkState::Live(live);
    }

    pub(crate) fn close(&self) {
        let mut inner = self.inner.write();
        if let SinkState::Live(live) = &*inner {
            live.window.close();
        }
        *inner = SinkState::Closed;
    }
}

/// A cancelled process may drain its old bounded reader queue, but cannot
/// enter the live Channel or wait for credit belonging to a new process.
#[derive(Clone)]
pub struct OutputProducer {
    sink: Arc<OutputSink>,
    cancelled: Arc<AtomicBool>,
}
impl OutputProducer {
    pub fn send_owned(&self, data: Vec<u8>) {
        self.sink
            .send_while(data, &|| !self.cancelled.load(Ordering::Acquire));
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.sink.wake_producers();
    }
}

impl Default for OutputSink {
    fn default() -> Self {
        Self::new()
    }
}

/// Independent map of session-id → [`OutputSink`].
pub struct OutputSinks {
    map: RwLock<HashMap<i64, Arc<OutputSink>>>,
}

impl OutputSinks {
    pub fn new() -> Self {
        Self {
            map: RwLock::new(HashMap::new()),
        }
    }

    /// Create-or-get the sink for `session_id`. Called once at reader start
    /// so the batcher can hold the `Arc` for the session's lifetime.
    pub fn ensure(&self, session_id: i64) -> Arc<OutputSink> {
        let mut map = self.map.write();
        map.entry(session_id)
            .or_insert_with(|| Arc::new(OutputSink::new()))
            .clone()
    }

    pub fn register(&self, session_id: i64, channel: Channel<InvokeResponseBody>) {
        // Keep registration atomic with removal: a captured sink must not be
        // closed between lookup and set, silently losing the new Channel.
        let mut map = self.map.write();
        map.entry(session_id)
            .or_insert_with(|| Arc::new(OutputSink::new()))
            .set(channel);
    }

    /// Wake batchers after their process cancellation flags have been set.
    pub fn wake_producers(&self, session_id: i64) {
        let sink = self.map.read().get(&session_id).cloned();
        if let Some(sink) = sink {
            sink.wake_producers();
        }
    }

    /// Acknowledge only the matching subscription, never a replacement Channel.
    pub fn acknowledge(&self, session_id: i64, channel_id: u32, sequence: u64) {
        let sink = self.map.read().get(&session_id).cloned();
        if let Some(sink) = sink {
            let live = match &*sink.inner.read() {
                SinkState::Live(live) if live.window.id() == channel_id => Some(Arc::clone(live)),
                _ => None,
            };
            if let Some(live) = live {
                live.window.acknowledge(sequence);
            }
        }
    }

    /// Cancellation is scoped to a Channel so delayed teardown cannot close its replacement.
    pub fn unregister_channel(&self, session_id: i64, channel_id: u32) {
        let mut map = self.map.write();
        let closed = map.get(&session_id).is_some_and(|sink| {
            let mut inner = sink.inner.write();
            if let SinkState::Live(live) = &*inner {
                if live.window.id() == channel_id {
                    live.window.close();
                    *inner = SinkState::Closed;
                    return true;
                }
            }
            false
        });
        if closed {
            map.remove(&session_id);
        }
    }

    /// Drop the session-scoped sink. Idempotent. Called only when the
    /// owning terminal is disposed or the Agent Node is deleted.
    /// Process exit and replacement must preserve the subscription.
    pub fn unregister(&self, session_id: i64) {
        if let Some(sink) = self.map.write().remove(&session_id) {
            sink.close();
        }
    }
}

impl Default for OutputSinks {
    fn default() -> Self {
        Self::new()
    }
}

/// Agent-terminal output sinks (issue #1385).
pub static AGENT: Lazy<OutputSinks> = Lazy::new(OutputSinks::new);

/// Build/Run-terminal output sinks (issue #1393). Separate from [`AGENT`]
/// because both surfaces key by the same node id.
pub static BUILD_RUN: Lazy<OutputSinks> = Lazy::new(OutputSinks::new);

/// Drop the node-scoped sinks for `session_id` across both Agent and
/// Build/Run maps. Idempotent. Called only when the Agent Node is
/// deleted -- process exit, retry, resume, and regenerate deliberately
/// preserve the subscription (issue #1405). Callers must use this
/// helper instead of reaching into [`AGENT`] / [`BUILD_RUN`] directly
/// so both maps stay in lock-step with the node lifecycle.
pub fn unregister_node_sinks(session_id: i64) {
    AGENT.unregister(session_id);
    BUILD_RUN.unregister(session_id);
}

#[cfg(test)]
#[path = "sink_tests.rs"]
mod tests;
