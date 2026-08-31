//! Binary PTY-output Channel for Agent Node terminals (issue #1385).
//!
//! The sink type and the pending/live/closed state machine live in
//! [`crate::pty::sink`]. This module is the agent-terminal map plus the
//! Tauri subscribe commands. Build/Run uses a sibling map
//! (`pty::sink::BUILD_RUN`) because both surfaces key by the same node id.
//!
//! `agent-output` with `line` remains for test injection only
//! (`inject_test_output`).
//!
//! The subscription is owned by the Agent Node's terminal, not by one
//! spawned process: it survives process exit, retry, resume, and
//! regenerate. Explicit terminal disposal and Agent Node deletion call
//! `unregister`.

use std::sync::Arc;
use tauri::command;
use tauri::ipc::{Channel, InvokeResponseBody};

use crate::pty::sink::OutputSink;

/// Create-or-get the sink for `session_id`. Called once at reader start
/// so the batcher can hold the `Arc` for the session's lifetime.
pub fn ensure(session_id: i64) -> Arc<OutputSink> {
    crate::pty::sink::AGENT.ensure(session_id)
}

pub fn register(session_id: i64, channel: Channel<InvokeResponseBody>) {
    crate::pty::sink::AGENT.register(session_id, channel);
}

/// Drop the node-scoped sink for `session_id`. Idempotent. Called only when
/// the Agent Node is deleted or its frontend terminal is explicitly disposed.
/// Process exit and replacement must preserve the subscription.
pub fn unregister(session_id: i64) {
    crate::pty::sink::AGENT.unregister(session_id);
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
    fn ensure_returns_the_same_arc_for_a_session() {
        let id = 9_000_211;
        let a = ensure(id);
        let b = ensure(id);
        assert!(Arc::ptr_eq(&a, &b));
        unregister(id);
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
        let id = 9_000_212;
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
    fn process_lifecycle_does_not_unregister_node_output_subscription() {
        let src_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut unregister_sites = Vec::new();
        collect_sink_unregister_sites(&src_root, &src_root, &mut unregister_sites);

        let allowed = [std::path::Path::new("services").join("agent_node.rs")];
        let unexpected: Vec<_> = unregister_sites
            .iter()
            .filter(|(rel, _)| !allowed.iter().any(|ok| rel == ok))
            .cloned()
            .collect();
        assert!(
            unexpected.is_empty(),
            "pty::sink::unregister_node_sinks is node-scoped; process kill / PTY EOF / retry must not call it. Unexpected sites: {unexpected:?}"
        );
        assert!(
            unregister_sites.iter().any(|(rel, _)| rel == &allowed[0]),
            "Agent Node deletion must release the output subscription (unregister_node_sinks)"
        );
    }

    fn collect_sink_unregister_sites(
        dir: &std::path::Path,
        src_root: &std::path::Path,
        out: &mut Vec<(std::path::PathBuf, usize)>,
    ) {
        let entries = std::fs::read_dir(dir).expect("read src tree");
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                collect_sink_unregister_sites(&path, src_root, out);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("read rust file");
            let production = source.split("#[cfg(test)]").next().unwrap_or(&source);
            for (idx, line) in production.lines().enumerate() {
                if line.contains("pty::sink::unregister_node_sinks(") {
                    let rel = path.strip_prefix(src_root).unwrap_or(&path).to_path_buf();
                    out.push((rel, idx + 1));
                }
            }
        }
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
        let id = 9_000_213;
        register(id, ch1);
        register(id, ch2);
        ensure(id).send_owned(b"x".to_vec());
        assert_eq!(first_hits.load(Ordering::SeqCst), 0);
        assert_eq!(second_hits.load(Ordering::SeqCst), 1);
        unregister(id);
    }
}
