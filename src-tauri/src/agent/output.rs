//! Binary PTY-output sink (issue #1385).
//!
//! Production agent output is pushed over a per-session Tauri `Channel` as
//! raw bytes (`InvokeResponseBody::Raw`), so the webview receives a
//! `Uint8Array` without Base64 or JSON. The `agent-output` event (base64
//! `data` / UTF-8 `line`) remains as the fallback for:
//!
//! - the window before the frontend has subscribed
//! - test injection (`inject_test_output` emits `line` payloads)
//!
//! The batcher thread holds an `Arc<OutputSink>` for its session, so the
//! send hot path never takes the global map lock. Subscribe/unsubscribe
//! only touch the map.

use once_cell::sync::Lazy;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::command;
use tauri::ipc::{Channel, InvokeResponseBody};

/// Per-session slot the batcher clones at spawn time. `send` takes a
/// read lock on this slot only — not the global map — so N agents do
/// not serialise on one `Mutex<HashMap>`.
pub struct OutputSink {
    channel: RwLock<Option<Channel<InvokeResponseBody>>>,
}

impl OutputSink {
    fn new() -> Self {
        Self {
            channel: RwLock::new(None),
        }
    }

    /// Push `data` on the session's binary Channel.
    ///
    /// Returns `true` when a Channel is registered, **even if this send
    /// failed**. The JSON-event fallback must not fire in that case:
    /// `Channel::send` may have already delivered the bytes to JS before
    /// returning `Err`, and emitting the event would duplicate (and
    /// corrupt) the terminal stream.
    pub fn send(&self, data: &[u8]) -> bool {
        let channel = self.channel.read().clone();
        let Some(channel) = channel else {
            return false;
        };
        let _ = channel.send(InvokeResponseBody::Raw(data.to_vec()));
        true
    }

    fn set(&self, channel: Channel<InvokeResponseBody>) {
        *self.channel.write() = Some(channel);
    }

    fn clear(&self) {
        *self.channel.write() = None;
    }
}

static SINKS: Lazy<RwLock<HashMap<i64, Arc<OutputSink>>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

/// Create-or-get the sink for `session_id`. Called once at reader start
/// so the batcher can hold the `Arc` for the session's lifetime.
pub fn ensure(session_id: i64) -> Arc<OutputSink> {
    let mut map = SINKS.write();
    map.entry(session_id)
        .or_insert_with(|| Arc::new(OutputSink::new()))
        .clone()
}

pub fn register(session_id: i64, channel: Channel<InvokeResponseBody>) {
    ensure(session_id).set(channel);
}

pub fn unregister(session_id: i64) {
    if let Some(sink) = SINKS.write().remove(&session_id) {
        sink.clear();
    }
}

/// Subscribe this webview to raw PTY bytes for `session_id`. Replaces any
/// previous Channel for the same session (a remounted terminal re-subscribes
/// with a fresh callback). Fast in-memory map insert — plain sync command
/// so it does not occupy a tokio worker (issue #1380).
#[command]
pub fn subscribe_agent_output(session_id: i64, on_chunk: Channel<InvokeResponseBody>) {
    register(session_id, on_chunk);
}

/// Drop the binary Channel for `session_id`. Idempotent: a second call
/// (or a call with no prior subscribe) is a no-op. Called from
/// `TerminalRegistry.dispose` when the node is deleted.
#[command]
pub fn unsubscribe_agent_output(session_id: i64) {
    unregister(session_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[test]
    fn send_false_when_unregistered() {
        let sink = OutputSink::new();
        assert!(!sink.send(b"x"));
    }

    #[test]
    fn send_delivers_to_registered_channel() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let rec = received.clone();
        let ch = Channel::new(move |body| {
            if let InvokeResponseBody::Raw(bytes) = body {
                rec.lock().unwrap().extend_from_slice(&bytes);
            }
            Ok(())
        });
        let sink = OutputSink::new();
        sink.set(ch);
        assert!(sink.send(b"hello"));
        assert_eq!(&*received.lock().unwrap(), b"hello");
    }

    #[test]
    fn send_true_when_channel_registered_even_if_send_fails() {
        // Dual-write guard: a registered Channel that errors on send
        // must still suppress the JSON fallback. Otherwise the same
        // bytes can land on both the Channel callback and `agent-output`.
        let ch = Channel::new(|_| Err(tauri::Error::WebviewNotFound));
        let sink = OutputSink::new();
        sink.set(ch);
        assert!(
            sink.send(b"x"),
            "registered Channel must suppress JSON fallback even on send Err"
        );
    }

    #[test]
    fn ensure_returns_the_same_arc_for_a_session() {
        let id = 9_000_101;
        let a = ensure(id);
        let b = ensure(id);
        assert!(Arc::ptr_eq(&a, &b));
        unregister(id);
    }

    #[test]
    fn register_replaces_previous_channel() {
        let first_hits = Arc::new(AtomicUsize::new(0));
        let second_hits = Arc::new(AtomicUsize::new(0));
        let f = first_hits.clone();
        let s = second_hits.clone();
        let ch1 = Channel::new(move |_| {
            f.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        let ch2 = Channel::new(move |_| {
            s.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        let id = 9_000_102;
        register(id, ch1);
        register(id, ch2);
        assert!(ensure(id).send(b"x"));
        assert_eq!(first_hits.load(Ordering::SeqCst), 0);
        assert_eq!(second_hits.load(Ordering::SeqCst), 1);
        unregister(id);
    }

    #[test]
    fn unregister_is_idempotent_and_clears_the_slot() {
        let id = 9_000_103;
        let sink = ensure(id);
        let ch = Channel::new(|_| Ok(()));
        sink.set(ch);
        unregister(id);
        unregister(id);
        assert!(!sink.send(b"x"), "cleared sink must fall back to JSON");
    }
}
