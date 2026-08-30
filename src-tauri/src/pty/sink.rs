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

use once_cell::sync::Lazy;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::ipc::{Channel, InvokeResponseBody};

/// Cap on bytes buffered before the frontend has subscribed. Matches
/// `TerminalWriter.MAX_PENDING_BYTES` — past this, older bytes are dropped
/// because xterm's scrollback cannot retain more anyway.
const PENDING_CAP: usize = 4 * 1024 * 1024;

enum SinkState {
    /// Frontend has not subscribed yet. Append-only; flushed on `set`.
    Pending(Vec<u8>),
    /// Live Channel. `send` uses this handle in place — no clone.
    Live(Channel<InvokeResponseBody>),
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
            inner: RwLock::new(SinkState::Pending(Vec::new())),
        }
    }

    /// Deliver `data` without cloning the Channel. Always the Channel
    /// path (or a pending buffer): the caller must not also emit JSON.
    pub fn send_owned(&self, data: Vec<u8>) {
        {
            let inner = self.inner.read();
            if let SinkState::Live(channel) = &*inner {
                let _ = channel.send(InvokeResponseBody::Raw(data));
                return;
            }
            if matches!(&*inner, SinkState::Closed) {
                return;
            }
        }
        let mut inner = self.inner.write();
        match &mut *inner {
            SinkState::Live(channel) => {
                let _ = channel.send(InvokeResponseBody::Raw(data));
            }
            SinkState::Pending(buf) => {
                buf.extend_from_slice(&data);
                let excess = buf.len().saturating_sub(PENDING_CAP);
                if excess > 0 {
                    buf.drain(..excess);
                }
            }
            SinkState::Closed => {}
        }
    }

    pub(crate) fn set(&self, channel: Channel<InvokeResponseBody>) {
        let mut inner = self.inner.write();
        let pending = match &mut *inner {
            SinkState::Pending(buf) => std::mem::take(buf),
            SinkState::Live(_) | SinkState::Closed => Vec::new(),
        };
        if !pending.is_empty() {
            let _ = channel.send(InvokeResponseBody::Raw(pending));
        }
        *inner = SinkState::Live(channel);
    }

    pub(crate) fn close(&self) {
        *self.inner.write() = SinkState::Closed;
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
        self.ensure(session_id).set(channel);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    fn collecting_channel(received: &Arc<Mutex<Vec<u8>>>) -> Channel<InvokeResponseBody> {
        let rec = received.clone();
        Channel::new(move |body| {
            if let InvokeResponseBody::Raw(bytes) = body {
                rec.lock().unwrap().extend_from_slice(&bytes);
            }
            Ok(())
        })
    }

    fn counting_channel(hits: &Arc<AtomicUsize>) -> Channel<InvokeResponseBody> {
        let h = hits.clone();
        Channel::new(move |_| {
            h.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }

    #[test]
    fn send_before_subscribe_flushes_in_order_on_register() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = OutputSink::new();
        sink.send_owned(b"hel".to_vec());
        sink.send_owned(b"lo".to_vec());
        sink.set(collecting_channel(&received));
        assert_eq!(&*received.lock().unwrap(), b"hello");
    }

    #[test]
    fn send_after_subscribe_goes_to_channel_without_cloning_per_chunk() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = OutputSink::new();
        sink.set(collecting_channel(&received));
        sink.send_owned(b"ab".to_vec());
        sink.send_owned(b"cd".to_vec());
        assert_eq!(&*received.lock().unwrap(), b"abcd");
    }

    #[test]
    fn closed_sink_drops_further_sends() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = OutputSink::new();
        sink.set(collecting_channel(&received));
        sink.send_owned(b"keep".to_vec());
        sink.close();
        sink.send_owned(b"drop".to_vec());
        assert_eq!(&*received.lock().unwrap(), b"keep");
    }

    #[test]
    fn ensure_returns_the_same_arc_for_a_session() {
        let id = 9_000_201;
        let a = AGENT.ensure(id);
        let b = AGENT.ensure(id);
        assert!(Arc::ptr_eq(&a, &b));
        AGENT.unregister(id);
    }

    #[test]
    fn register_replaces_previous_channel() {
        let first_hits = Arc::new(AtomicUsize::new(0));
        let second_hits = Arc::new(AtomicUsize::new(0));
        let id = 9_000_202;
        AGENT.register(id, counting_channel(&first_hits));
        AGENT.register(id, counting_channel(&second_hits));
        AGENT.ensure(id).send_owned(b"x".to_vec());
        assert_eq!(first_hits.load(Ordering::SeqCst), 0);
        assert_eq!(second_hits.load(Ordering::SeqCst), 1);
        AGENT.unregister(id);
    }

    #[test]
    fn unregister_is_idempotent() {
        let id = 9_000_203;
        let sink = AGENT.ensure(id);
        AGENT.unregister(id);
        AGENT.unregister(id);
        sink.send_owned(b"x".to_vec()); // closed — no panic
    }

    #[test]
    fn replacement_reader_reuses_the_live_channel() {
        // Retry / resume / regenerate (agent) or a replacement Build/Run
        // reader call `ensure`. That must be the same Live sink the
        // terminal already subscribed, not a fresh Pending buffer the
        // frontend never sees.
        let received = Arc::new(Mutex::new(Vec::new()));
        let id = 9_000_204;
        BUILD_RUN.register(id, collecting_channel(&received));
        let first_reader = BUILD_RUN.ensure(id);
        first_reader.send_owned(b"boot".to_vec());

        let replacement_reader = BUILD_RUN.ensure(id);
        assert!(
            Arc::ptr_eq(&first_reader, &replacement_reader),
            "a new process incarnation must reuse the session-scoped sink"
        );
        replacement_reader.send_owned(b"-ok".to_vec());
        assert_eq!(&*received.lock().unwrap(), b"boot-ok");
        BUILD_RUN.unregister(id);
    }

    #[test]
    fn unregister_starts_a_fresh_pending_sink() {
        let first = Arc::new(Mutex::new(Vec::new()));
        let second = Arc::new(Mutex::new(Vec::new()));
        let id = 9_000_205;
        BUILD_RUN.register(id, collecting_channel(&first));
        BUILD_RUN.unregister(id);

        BUILD_RUN.ensure(id).send_owned(b"late".to_vec());
        assert!(
            first.lock().unwrap().is_empty(),
            "bytes after unregister must not reach the disposed Channel"
        );

        BUILD_RUN.register(id, collecting_channel(&second));
        assert_eq!(
            &*second.lock().unwrap(),
            b"late",
            "ensure after unregister is a new Pending sink flushed on the next subscribe"
        );
        BUILD_RUN.unregister(id);
    }

    #[test]
    fn agent_and_build_run_maps_do_not_cross_talk() {
        // Same numeric id, two surfaces. The reason the maps are separate.
        let id = 9_000_206;
        let agent_got = Arc::new(Mutex::new(Vec::new()));
        let build_got = Arc::new(Mutex::new(Vec::new()));
        AGENT.register(id, collecting_channel(&agent_got));
        BUILD_RUN.register(id, collecting_channel(&build_got));

        AGENT.ensure(id).send_owned(b"agent".to_vec());
        BUILD_RUN.ensure(id).send_owned(b"build".to_vec());

        assert_eq!(&*agent_got.lock().unwrap(), b"agent");
        assert_eq!(&*build_got.lock().unwrap(), b"build");
        AGENT.unregister(id);
        BUILD_RUN.unregister(id);
    }
}
