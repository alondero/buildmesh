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
    /// `pty::sink::unregister_node_sinks(`, skipping anything the
    /// `#[cfg(test)]` gate compiles out of a release build.
    ///
    /// The gate is resolved structurally, by brace depth from each
    /// `#[cfg(test)]` item, not by "everything after the first
    /// `#[cfg(test)]` in the file". A file is free to carry a
    /// `#[cfg(test)]` function above production code — `services/agent_node.rs`
    /// does (issue #2450) — and a positional split silently dropped the real
    /// production call site from the scan, which turned this guard into a
    /// false failure rather than a real one.
    ///
    /// `depth` is signed and updated with `{` minus `}` on every line: a
    /// monotonic counter cannot return to the depth a gate was armed at, so a
    /// `mod tests {` block would leave the gate latched for the rest of the
    /// file and hide every later production call site.
    ///
    /// A gate closes in one of two ways. Either the item had a body — the
    /// depth went positive and unwound back past the armed depth — or it did
    /// not, and the item's own line ended in `;` (`#[cfg(test)] use …;`).
    /// Without the second case the gate would latch on a braceless item and
    /// hide the rest of the file; without the first it would close on a
    /// multi-line signature whose line carries no brace, and the gated body
    /// would be scanned as production.
    fn collect_calls_in_source(
        source: &str,
        path: &std::path::Path,
        src_root: &std::path::Path,
        out: &mut Vec<(std::path::PathBuf, usize)>,
    ) {
        let mut depth = 0i32;
        // Set when a `#[cfg(test)]` attribute is seen, with the depth it was
        // armed at and whether the gated item's body has been entered.
        let mut gate: Option<(i32, bool)> = None;
        // A string literal can span lines, so this is loop-carried: without it
        // the continuation lines of a multi-line literal are read as code and
        // their braces corrupt the depth.
        let mut in_string = false;
        for (idx, line) in source.lines().enumerate() {
            let trimmed = line.trim();
            let code = strip_comment(line);
            let countable = strip_string_literals(code, &mut in_string);

            match gate {
                Some((open_at, mut entered)) => {
                    if trimmed.is_empty() || trimmed.starts_with("#[") || trimmed.starts_with("//")
                    {
                        continue;
                    }
                    let delta = brace_delta(&countable);
                    entered |= delta > 0;
                    depth += delta;
                    let braceless_item = delta == 0 && countable.trim_end().ends_with(';');
                    if (entered && depth <= open_at) || braceless_item {
                        gate = None;
                    }
                }
                None => {
                    if trimmed.starts_with("#[cfg(test)]") {
                        gate = Some((depth, false));
                        continue;
                    }
                    // Match the comment- AND string-stripped line: a commented-out call is not
                    // a live call site, and neither is one inside a string literal —
                    // including this file's own `contains` needle below, which is
                    // exactly why the scan of this file must not match itself.
                    if countable.contains("pty::sink::unregister_node_sinks(") {
                        let rel = path.strip_prefix(src_root).unwrap_or(path).to_path_buf();
                        out.push((rel, idx + 1));
                    }
                    depth += brace_delta(&countable);
                }
            }
        }
    }

    /// The line's code with any trailing `//` comment removed.
    fn strip_comment(line: &str) -> &str {
        line.split("//").next().unwrap_or("")
    }

    /// Drop `"…"` spans so a brace inside a string literal cannot corrupt the
    /// depth. Escaped quotes are skipped. `in_string` is carried across lines
    /// because a literal may span several of them; a `//` comment inside a
    /// literal is not treated as a comment, which only makes the scan more
    /// conservative.
    fn strip_string_literals(code: &str, in_string: &mut bool) -> String {
        let mut out = String::with_capacity(code.len());
        let mut chars = code.chars();
        while let Some(c) = chars.next() {
            if *in_string {
                match c {
                    '\\' => {
                        chars.next();
                    }
                    '"' => *in_string = false,
                    _ => {}
                }
                continue;
            }
            match c {
                '"' => *in_string = true,
                _ => out.push(c),
            }
        }
        out
    }

    /// `{` minus `}` on a line. Signed, so a closing brace can unwind a depth
    /// that an earlier line opened.
    fn brace_delta(code: &str) -> i32 {
        code.matches('{').count() as i32 - code.matches('}').count() as i32
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

    /// The scanner is the guard, so pin its behaviour on synthetic source:
    /// each case below fails if the gate resolution regresses to the
    /// positional split or to an unsigned depth counter.
    ///
    /// The fixtures spell braces as `@{` / `@}` and expand them with
    /// [`fixture`] before scanning. Writing real braces here would put
    /// unbalanced `{` / `}` inside this file's own string literals, and the
    /// scanner — which walks every file under `src/` including this one —
    /// would read them as real code and mis-count its own depth.
    #[test]
    fn source_scanner_separates_production_from_gated_items() {
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
        let fixture = |body: &str| body.replace("@{", "{").replace("@}", "}");

        // A `#[cfg(test)] mod tests {` block must not latch the gate for the
        // rest of the file: the production call after it is still found.
        let after_test_module = fixture(
            "\
fn production_early() @{
    crate::pty::sink::unregister_node_sinks(1);
@}
#[cfg(test)]
mod tests @{
    #[test]
    fn t() @{
        crate::pty::sink::unregister_node_sinks(2);
    @}
@}
fn production_late() @{
    crate::pty::sink::unregister_node_sinks(3);
@}
",
        );
        assert_eq!(
            scan(&after_test_module),
            vec![2, 12],
            "production calls on both sides of a test module must be found; the \
             one inside `mod tests` (line 8) must not"
        );

        // A multi-line `#[cfg(test)] fn` signature carries no braces, so the
        // gate must survive the signature instead of clearing on it and
        // scanning the body as production.
        let multiline_signature = fixture(
            "\
#[cfg(test)]
fn gated(
    a: i32,
) -> i32 @{
    crate::pty::sink::unregister_node_sinks(4);
    a
@}
fn production() @{
    crate::pty::sink::unregister_node_sinks(5);
@}
",
        );
        assert_eq!(
            scan(&multiline_signature),
            vec![9],
            "only the ungated body is a production call site"
        );

        // A commented-out call is not a live call site.
        let commented_out = fixture(
            "\
// crate::pty::sink::unregister_node_sinks(6);
fn f() @{
    let url = \"https://example.test\"; // inert
@}
",
        );
        assert_eq!(scan(&commented_out), Vec::<usize>::new());

        // A braceless gated item (`#[cfg(test)] use …;`) must not latch the
        // gate: the production call below it is still found.
        let braceless_gate = fixture(
            "\
#[cfg(test)]
use std::path::Path;
fn production() @{
    crate::pty::sink::unregister_node_sinks(7);
@}
",
        );
        assert_eq!(
            scan(&braceless_gate),
            vec![4],
            "a `use` gate must close on its own line"
        );
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
