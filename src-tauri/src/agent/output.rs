//! Binary PTY-output sink (issue #1385).
//!
//! Production PTY bytes go **only** over a per-session Tauri `Channel`
//! (`InvokeResponseBody::Raw`). They are never mixed with the JSON
//! `agent-output` event: those two IPC paths have no ordering, and a
//! subscribe that lands mid-stream would let chunk 2 on the Channel
//! overtake chunk 1 on the event bus (split ANSI). Bytes that arrive
//! before `subscribe_agent_output` are held on the sink and flushed
//! in order when the Channel is registered.
//!
//! `agent-output` with `line` remains for test injection only
//! (`inject_test_output`).
//!
//! The batcher holds an `Arc<OutputSink>` for the session, so the send
//! path never takes the global map lock. The subscription is owned by the
//! Agent Node's terminal, not by one spawned process: it survives process
//! exit, retry, resume, and regenerate. Explicit terminal disposal and
//! Agent Node deletion call `unregister`.

use once_cell::sync::Lazy;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::command;
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
    /// Agent Node deleted or frontend terminal disposed. Further sends
    /// are dropped. Process exit must not enter this state.
    Closed,
}

/// Per-session slot the batcher clones at spawn time.
pub struct OutputSink {
    inner: RwLock<SinkState>,
}

impl OutputSink {
    fn new() -> Self {
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

    fn set(&self, channel: Channel<InvokeResponseBody>) {
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

    fn close(&self) {
        *self.inner.write() = SinkState::Closed;
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

/// Drop the node-scoped sink for `session_id`. Idempotent. Called only when
/// the Agent Node is deleted or its frontend terminal is explicitly disposed.
/// Process exit and replacement must preserve the subscription.
pub fn unregister(session_id: i64) {
    if let Some(sink) = SINKS.write().remove(&session_id) {
        sink.close();
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

/// Drop the binary Channel for `session_id`. Idempotent.
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
    fn send_before_subscribe_flushes_in_order_on_register() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let rec = received.clone();
        let ch = Channel::new(move |body| {
            if let InvokeResponseBody::Raw(bytes) = body {
                rec.lock().unwrap().extend_from_slice(&bytes);
            }
            Ok(())
        });
        let sink = OutputSink::new();
        sink.send_owned(b"hel".to_vec());
        sink.send_owned(b"lo".to_vec());
        sink.set(ch);
        assert_eq!(&*received.lock().unwrap(), b"hello");
    }

    #[test]
    fn send_after_subscribe_goes_to_channel_without_cloning_per_chunk() {
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
        sink.send_owned(b"ab".to_vec());
        sink.send_owned(b"cd".to_vec());
        assert_eq!(&*received.lock().unwrap(), b"abcd");
    }

    #[test]
    fn closed_sink_drops_further_sends() {
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
        sink.send_owned(b"keep".to_vec());
        sink.close();
        sink.send_owned(b"drop".to_vec());
        assert_eq!(&*received.lock().unwrap(), b"keep");
    }

    #[test]
    fn ensure_returns_the_same_arc_for_a_session() {
        let id = 9_000_201;
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
        let id = 9_000_202;
        register(id, ch1);
        register(id, ch2);
        ensure(id).send_owned(b"x".to_vec());
        assert_eq!(first_hits.load(Ordering::SeqCst), 0);
        assert_eq!(second_hits.load(Ordering::SeqCst), 1);
        unregister(id);
    }

    #[test]
    fn unregister_is_idempotent() {
        let id = 9_000_203;
        let sink = ensure(id);
        unregister(id);
        unregister(id);
        sink.send_owned(b"x".to_vec()); // closed — no panic
    }

    fn collecting_channel(received: &Arc<Mutex<Vec<u8>>>) -> Channel<InvokeResponseBody> {
        let rec = received.clone();
        Channel::new(move |body| {
            if let InvokeResponseBody::Raw(bytes) = body {
                rec.lock().unwrap().extend_from_slice(&bytes);
            }
            Ok(())
        })
    }

    #[test]
    fn replacement_reader_reuses_the_live_channel() {
        // Retry / resume / regenerate start a new PTY reader that calls
        // `ensure`. That must be the same Live sink the terminal already
        // subscribed, not a fresh Pending buffer the frontend never sees.
        let received = Arc::new(Mutex::new(Vec::new()));
        let id = 9_000_204;
        register(id, collecting_channel(&received));
        let first_reader = ensure(id);
        first_reader.send_owned(b"boot".to_vec());

        let replacement_reader = ensure(id);
        assert!(
            Arc::ptr_eq(&first_reader, &replacement_reader),
            "a new process incarnation must reuse the node-scoped sink"
        );
        replacement_reader.send_owned(b"-ok".to_vec());
        assert_eq!(&*received.lock().unwrap(), b"boot-ok");
        unregister(id);
    }

    #[test]
    fn unregister_starts_a_fresh_pending_sink() {
        let first = Arc::new(Mutex::new(Vec::new()));
        let second = Arc::new(Mutex::new(Vec::new()));
        let id = 9_000_205;
        register(id, collecting_channel(&first));
        unregister(id);

        ensure(id).send_owned(b"late".to_vec());
        assert!(
            first.lock().unwrap().is_empty(),
            "bytes after unregister must not reach the disposed Channel"
        );

        register(id, collecting_channel(&second));
        assert_eq!(
            &*second.lock().unwrap(),
            b"late",
            "ensure after unregister is a new Pending sink flushed on the next subscribe"
        );
        unregister(id);
    }

    #[test]
    fn process_lifecycle_does_not_unregister_node_output_subscription() {
        let src_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut unregister_sites = Vec::new();
        collect_output_unregister_sites(&src_root, &src_root, &mut unregister_sites);

        let allowed = [
            std::path::Path::new("agent").join("output.rs"),
            std::path::Path::new("services").join("agent_node.rs"),
        ];
        let unexpected: Vec<_> = unregister_sites
            .iter()
            .filter(|(rel, _)| !allowed.iter().any(|ok| rel == ok))
            .cloned()
            .collect();
        assert!(
            unexpected.is_empty(),
            "output::unregister is node-scoped; process kill / PTY EOF / retry must not call it. Unexpected sites: {unexpected:?}"
        );
        assert!(
            unregister_sites
                .iter()
                .any(|(rel, _)| rel == &allowed[1]),
            "Agent Node deletion must release the output subscription"
        );
    }

    fn collect_output_unregister_sites(
        dir: &std::path::Path,
        src_root: &std::path::Path,
        out: &mut Vec<(std::path::PathBuf, usize)>,
    ) {
        let entries = std::fs::read_dir(dir).expect("read src tree");
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                collect_output_unregister_sites(&path, src_root, out);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("read rust file");
            let production = source.split("#[cfg(test)]").next().unwrap_or(&source);
            for (idx, line) in production.lines().enumerate() {
                if line.contains("output::unregister(") {
                    let rel = path
                        .strip_prefix(src_root)
                        .unwrap_or(&path)
                        .to_path_buf();
                    out.push((rel, idx + 1));
                }
            }
        }
    }
}
