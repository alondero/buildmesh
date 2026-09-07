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
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tauri::ipc::{Channel, InvokeResponseBody};

/// Cap on bytes buffered before the frontend has subscribed. Matches
/// `TerminalWriter.MAX_PENDING_BYTES` -- past this, older bytes are dropped
/// because xterm's scrollback cannot retain more anyway.
const PENDING_CAP: usize = 4 * 1024 * 1024;

enum SinkState {
    /// Frontend has not subscribed yet. Append-only; flushed on `set`.
    /// `VecDeque` so overflow drops the oldest bytes in O(1) per pop
    /// instead of memmoving a multi-MiB `Vec::drain` tail each batcher
    /// chunk (see the 4 MiB trap when a noisy build script dumps output
    /// before the frontend mounts).
    Pending(VecDeque<u8>),
    /// Live Channel. `send` uses this handle in place -- no clone.
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
            inner: RwLock::new(SinkState::Pending(VecDeque::new())),
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
                buf.extend(data);
                while buf.len() > PENDING_CAP {
                    buf.pop_front();
                }
            }
            SinkState::Closed => {}
        }
    }

    pub(crate) fn set(&self, channel: Channel<InvokeResponseBody>) {
        let mut inner = self.inner.write();
        let pending: Vec<u8> = match &mut *inner {
            SinkState::Pending(buf) => std::mem::take(buf).into(),
            SinkState::Live(_) => Vec::new(),
            // Closed is a terminal state (terminal disposed or node
            // deleted). Do not resurrect it into Live -- the prior
            // subscription was deliberately torn down and any new
            // caller here is a stale reference. Drop the channel.
            SinkState::Closed => return,
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

    #[test]
    fn pending_buffer_truncates_to_cap_without_panicking_or_reordering() {
        // Reproduces the noisy-build-pre-mount scenario: a batcher pushes
        // chunks well past PENDING_CAP before the frontend subscribes.
        // The retained tail must be the LAST PENDING_CAP bytes in arrival
        // order, with no panic, no deadlock, and no interleaving between
        // chunks that arrived in separate send_owned calls.
        let id = 9_000_207;
        let sink = OutputSink::new();
        let chunk = vec![0xAB_u8; 64 * 1024]; // 64 KiB -- typical batcher batch
        let chunk_count = (PENDING_CAP / chunk.len()) + 8; // ~overflow by 8 chunks
        for n in 0..chunk_count {
            // Stamp each chunk with its index in the low byte of its tail so
            // we can detect reordering after truncation.
            let mut stamped = chunk.clone();
            let last = stamped.len() - 1;
            stamped[last] = n as u8;
            sink.send_owned(stamped);
        }

        let received = Arc::new(Mutex::new(Vec::new()));
        sink.set(collecting_channel(&received));

        let got = received.lock().unwrap().clone();
        assert_eq!(
            got.len(),
            PENDING_CAP,
            "subscription flush must deliver exactly the kept tail, no more"
        );
        // First retained byte must come from a mid-stream chunk (the leading
        // bytes of the very first chunks were dropped). We can verify
        // ordering by walking the trailing stamp and asserting the indexes
        // are strictly ascending and contiguous.
        let stamps: Vec<u8> = got
            .iter()
            .copied()
            .filter(|b| *b < chunk_count as u8)
            .collect();
        assert!(
            stamps.windows(2).all(|w| w[0] < w[1]),
            "chunk stamps must remain in arrival order after truncation"
        );
        // The first stamp is the lowest non-dropped chunk index. Its
        // preceding bytes (all 0xAB except the last) must be the bulk of
        // the chunk -- the truncation can only have lopped off whole
        // chunks plus possibly a partial tail of one.
        assert!(
            stamps[0] >= (chunk_count - (PENDING_CAP / chunk.len()) - 1) as u8,
            "truncation must not retain more than PENDING_CAP worth of chunks"
        );

        BUILD_RUN.unregister(id);
    }

    #[test]
    fn pending_buffer_single_chunk_larger_than_cap_keeps_only_its_tail() {
        // A single oversize send (e.g. one mega-payload from a noisy tool)
        // must keep the LAST PENDING_CAP bytes of that single chunk --
        // dropping the leading bytes, not the trailing ones.
        let id = 9_000_208;
        let sink = OutputSink::new();
        let big: Vec<u8> = (0..(PENDING_CAP * 2) as u32)
            .map(|i| (i & 0xFF) as u8)
            .collect();
        sink.send_owned(big.clone());

        let received = Arc::new(Mutex::new(Vec::new()));
        sink.set(collecting_channel(&received));

        let got = received.lock().unwrap().clone();
        assert_eq!(got.len(), PENDING_CAP);
        let expected_tail: Vec<u8> = big[big.len() - PENDING_CAP..].to_vec();
        assert_eq!(
            got, expected_tail,
            "single oversize chunk must retain only its trailing PENDING_CAP bytes"
        );

        BUILD_RUN.unregister(id);
    }

    #[test]
    fn set_on_closed_sink_does_not_resurrect_into_live() {
        // Closed is documented terminal. A subsequent set() (e.g. a stale
        // subscribe callback landing after disposal) must NOT bring the
        // sink back to Live and leak bytes through the new channel.
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = OutputSink::new();
        sink.set(collecting_channel(&received));
        sink.send_owned(b"keep".to_vec());
        sink.close();
        sink.set(collecting_channel(&received)); // must be a no-op
        sink.send_owned(b"drop".to_vec());
        assert_eq!(
            &*received.lock().unwrap(),
            b"keep",
            "set on a closed sink must not transition back to Live"
        );
    }
}
