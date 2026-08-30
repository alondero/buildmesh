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
//! `TerminalWriter` on the frontend still rAF-batches for display
//! (issues #303 / #1122).

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;
use tauri::command;
use tauri::ipc::{Channel, InvokeResponseBody};

static OUTPUT_CHANNELS: Lazy<Mutex<HashMap<i64, Channel<InvokeResponseBody>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub fn register(session_id: i64, channel: Channel<InvokeResponseBody>) {
    OUTPUT_CHANNELS.lock().unwrap().insert(session_id, channel);
}

pub fn unregister(session_id: i64) {
    OUTPUT_CHANNELS.lock().unwrap().remove(&session_id);
}

/// Push `data` on the session's binary Channel. Returns true if a Channel
/// was registered and the send succeeded — the caller should skip the
/// JSON-event fallback in that case.
pub fn send_raw(session_id: i64, data: &[u8]) -> bool {
    let channel = {
        let guard = OUTPUT_CHANNELS.lock().unwrap();
        guard.get(&session_id).cloned()
    };
    let Some(channel) = channel else {
        return false;
    };
    channel
        .send(InvokeResponseBody::Raw(data.to_vec()))
        .is_ok()
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
    use std::sync::Arc;

    #[test]
    fn send_raw_false_when_unregistered() {
        assert!(!send_raw(9_000_001, b"x"));
    }

    #[test]
    fn send_raw_delivers_to_registered_channel() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let rec = received.clone();
        let ch = Channel::new(move |body| {
            if let InvokeResponseBody::Raw(bytes) = body {
                rec.lock().unwrap().extend_from_slice(&bytes);
            }
            Ok(())
        });
        let id = 9_000_002;
        register(id, ch);
        assert!(send_raw(id, b"hello"));
        assert_eq!(&*received.lock().unwrap(), b"hello");
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
        let id = 9_000_003;
        register(id, ch1);
        register(id, ch2);
        assert!(send_raw(id, b"x"));
        assert_eq!(first_hits.load(Ordering::SeqCst), 0);
        assert_eq!(second_hits.load(Ordering::SeqCst), 1);
        unregister(id);
    }

    #[test]
    fn unsubscribe_is_idempotent() {
        unregister(9_000_004);
        unregister(9_000_004);
        assert!(!send_raw(9_000_004, b"x"));
    }
}
