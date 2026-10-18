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

    /// Walk a `.rs` file and record the lines that call
    /// `pty::sink::unregister_node_sinks(` in code a release build compiles.
    ///
    /// Production is the text before the file's first `#[cfg(test)]`. That split
    /// is only sound while gated items come last, so this function does not try
    /// to be clever about gates: it relies on a layout invariant, and
    /// `no_gated_item_precedes_a_production_sink_call` asserts the invariant on
    /// every file the walk visits. An earlier version of this guard resolved
    /// gates with a brace-depth state machine, which is a Rust tokenizer
    /// written in the wrong place — the layout it had to cope with was itself
    /// created by `#[cfg(test)]` wrappers sitting mid-file in
    /// `services/agent_node.rs`, and those now live in a `*_tests` module
    /// (issue #2450).
    fn collect_calls_in_source(
        source: &str,
        path: &std::path::Path,
        src_root: &std::path::Path,
        out: &mut Vec<(std::path::PathBuf, usize)>,
    ) {
        let production = production_prefix(source);
        let mut in_string = false;
        for (idx, line) in production.lines().enumerate() {
            // Match the noise-stripped line: a commented-out call is not a live
            // call site, and neither is the needle quoted in a string literal.
            if strip_noise(line, &mut in_string).contains("pty::sink::unregister_node_sinks(") {
                let rel = path.strip_prefix(src_root).unwrap_or(path).to_path_buf();
                out.push((rel, idx + 1));
            }
        }
    }

    /// Everything before the file's first `#[cfg(test)]` attribute line.
    ///
    /// The gate is matched in attribute position — a line whose trimmed text
    /// starts with `#[cfg(test)]` — not as a bare substring. A doc comment that
    /// *names* the gate (several in this file do) must not truncate production,
    /// or the scan silently loses every call site after it.
    fn production_prefix(source: &str) -> &str {
        let mut offset = 0usize;
        for line in source.split_inclusive('\n') {
            if line.trim_start().starts_with("#[cfg(test)]") {
                return &source[..offset];
            }
            offset += line.len();
        }
        source
    }

    /// The line with comments and string literals removed, in one left-to-right
    /// pass.
    ///
    /// Order matters and is the whole point: stripping `//` first would cut a
    /// line at the `//` inside `"https://…"`, sever the closing quote, and leave
    /// the scanner believing a string is still open. `in_string` is loop-carried
    /// because a literal may span lines.
    fn strip_noise(line: &str, in_string: &mut bool) -> String {
        let mut out = String::with_capacity(line.len());
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if *in_string {
                match c {
                    '\\' => i += 1,
                    '"' => *in_string = false,
                    _ => {}
                }
                i += 1;
                continue;
            }
            match c {
                '"' => *in_string = true,
                '/' if chars.get(i + 1) == Some(&'/') => break,
                _ => out.push(c),
            }
            i += 1;
        }
        out
    }

    /// This file's own path, as `collect_sink_unregister_sites` sees it: the
    /// `src/`-relative `file!()` joined onto the walk root. The scan skips it
    /// so the guard does not match its own needle literal and fixtures.
    fn self_path() -> std::path::PathBuf {
        // `file!()` is crate-root-relative (`src/agent/output.rs`), so joining it
        // onto the manifest root reproduces the path the walk hands us.
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file!())
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
            // Exempt this file. Its own source necessarily contains the needle
            // literal (in `collect_calls_in_source` and in the synthetic
            // fixtures below), and a source scanner cannot reliably parse the
            // source that implements it — the fixtures are deliberately
            // brace-free and quote-delimited, which is exactly the shape the
            // scanner's own string/depth bookkeeping has to reason about.
            // `source_scanner_separates_production_from_gated_items` is the
            // guard for the scanner itself: it pins the behaviour on synthetic
            // input, so the exemption removes a self-reference, not coverage.
            if path.ends_with(self_path()) {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("read rust file");
            collect_calls_in_source(&source, &path, src_root, out);
        }
    }

    /// The layout invariant `collect_calls_in_source` depends on, asserted on
    /// every file the walk visits.
    ///
    /// Production is found by taking the text before the file's first
    /// `#[cfg(test)]` attribute, so a gated item placed above a production call
    /// site would hide it. Rather than make the scanner understand gates, this
    /// fails and names the file: move the gated item into a `*_tests` module at
    /// the bottom of its file. The one call site in the tree is in
    /// `services/agent_node.rs`, above that file's test modules.
    ///
    /// This file is skipped for the same reason the walk skips it: its own
    /// source quotes the needle.
    #[test]
    fn sink_call_sites_are_not_followed_by_a_gated_item() {
        let src_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut queue = vec![src_root.clone()];
        while let Some(dir) = queue.pop() {
            for entry in std::fs::read_dir(&dir).expect("read src tree") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    queue.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs")
                    || path.ends_with(self_path())
                {
                    continue;
                }
                let source = std::fs::read_to_string(&path).expect("read rust file");
                let production = production_prefix(&source);
                let rel = path
                    .strip_prefix(&src_root)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                for (idx, line) in source.lines().enumerate() {
                    if !line.contains("pty::sink::unregister_node_sinks(") {
                        continue;
                    }
                    let byte_at: usize = source.lines().take(idx).map(|l| l.len() + 1).sum();
                    if byte_at >= production.len() && production.len() < source.len() {
                        offenders.push(format!(
                            "{rel}:{} — this call sits after the file's first \
                             #[cfg(test)], so the production scan cannot see it",
                            idx + 1
                        ));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "a gated item must not sit above a call the guard is meant to find \
             (issue #2450): {offenders:?}"
        );
    }

    /// `strip_noise` removes comments and string literals in one pass, so a
    /// `//` inside a literal cannot cut the line and leave a string "open".
    #[test]
    fn noise_stripping_handles_a_comment_start_inside_a_literal() {
        let scan = |source: &str| -> Vec<usize> {
            let mut out = Vec::new();
            collect_calls_in_source(
                source,
                std::path::Path::new("probe.rs"),
                std::path::Path::new(""),
                &mut out,
            );
            out.into_iter().map(|(_, line)| line).collect()
        };

        // A URL literal on line 2 must not swallow line 3's call. Stripping
        // comments first would split line 2 at `//` in `https://`, drop the
        // closing quote, and hide everything after.
        let url_then_call = "\
fn f() {
    let u = \"https://example.test/path\";
    crate::pty::sink::unregister_node_sinks(1);
}
";
        assert_eq!(scan(url_then_call), vec![3]);

        // A commented-out call is not a live call site, and a call quoted in a
        // literal is not either.
        let inert = "\
// crate::pty::sink::unregister_node_sinks(2);
fn f() {
    let doc = \"see pty::sink::unregister_node_sinks(3) for details\";
}
";
        assert_eq!(scan(inert), Vec::<usize>::new());

        // Everything past the first `#[cfg(test)]` is test code by construction.
        let gated = "\
fn production() {
    crate::pty::sink::unregister_node_sinks(4);
}
#[cfg(test)]
mod tests {
    fn t() {
        crate::pty::sink::unregister_node_sinks(5);
    }
}
";
        assert_eq!(scan(gated), vec![2]);
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
