//! Prompt delivery with process fencing, paste readiness and Enter acknowledgement.

use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::AppHandle;
use super::evaluator;
use crate::agent::process::AgentProcessRegistry;

const SUBMIT_POLL: Duration = Duration::from_millis(250);

/// How long to wait for the pasted prompt to echo back in PTY output before
/// concluding this provider renders no echo and moving on.
const PASTE_ECHO_DEADLINE: Duration = Duration::from_secs(3);

/// The TUI redraw after a paste must have been quiet this long before Enter
/// is sent (mirrors `launch::MIN_QUIET_MS`'s reasoning at a smaller scale —
/// the box is drawn and the CLI is waiting).
const PASTE_SETTLE_QUIET_MS: u128 = 1_000;

/// Upper bound on waiting for the post-paste redraw to settle.
const PASTE_SETTLE_DEADLINE: Duration = Duration::from_secs(15);
/// Codex on Windows can take several seconds to consume a large bracketed
/// paste. Its input box renders a collapsed marker only after that work ends.
const RENDERED_PASTE_READY_DEADLINE: Duration = Duration::from_secs(30);
/// Complete visible text is useful for short drafts. Long drafts may be
/// collapsed or scrolled out of the TUI; they require Codex's paste marker.
const VISIBLE_PASTE_TEXT_LIMIT: usize = 256;

/// After an Enter keystroke, PTY output must appear within this window for
/// the submit to count as acknowledged.
const ENTER_ACK_WINDOW: Duration = Duration::from_secs(6);

/// Enter keystrokes attempted before the watcher gives up and surfaces the
/// node for human attention.
const MAX_ENTER_ATTEMPTS: u32 = 3;

/// The bytes staged into the PTY input box — WITHOUT the Enter keystroke.
/// Multi-line text is wrapped in bracketed-paste markers so the agent CLI
/// treats it as one pasted block instead of submitting at every newline.
///
/// The Enter is deliberately NOT part of this payload: ink-based TUIs
/// (Claude Code) batch stdin reads, and a `\r` arriving in the same read
/// burst as a bracketed paste is treated as part of the paste — the prompt
/// sits staged in the input box and is never submitted (issue #874, node
/// 2328: the correction was visibly pasted, the run stalled forever).
pub(crate) fn injection_payload(text: &str) -> String {
    if text.contains('\n') {
        format!("\x1b[200~{}\x1b[201~", text)
    } else {
        text.to_string()
    }
}

/// Has the node produced PTY output more recently than `ms_since_mark`
/// milliseconds ago? Pure core of the generic paste-echo check.
pub(crate) fn output_seen_within(ms_since_output: Option<u128>, ms_since_mark: u128) -> bool {
    matches!(ms_since_output, Some(m) if m < ms_since_mark)
}

/// Write a (possibly multi-line) prompt into the node's PTY stdin, then
/// submit it from a background watcher: wait for the paste to echo and the
/// redraw to settle, send Enter as its own write, and — when the target is one
/// the turn evaluator buffers — verify output follows, retrying Enter a bounded
/// number of times. `Ok` means "staged and submission scheduled"; a verified
/// submit that never takes marks the node for human attention instead of
/// stalling silently. A target the evaluator does not buffer gets one Enter and
/// no verification (see [`settle_after_paste`] and
/// [`press_enter_until_output_guarded`]) — the stages that exist to *observe*
/// the submit are skipped rather than faked.
///
/// Deliberately NOT routed through `coordinator::drive::AgentDriver`
/// (whose "no parallel write path" rule targets *Coordinator/scheduler*
/// callers): `send_prompt` is single-line (`{prompt}\n` — a newline mid-
/// template would submit fragments) and its idempotency ledger models
/// retried remote requests, which an in-process turn reaction doesn't
/// have. If `AgentDriver` grows multi-line paste support, converge on it.
pub(crate) fn write_prompt_to_pty(node_id: i64, text: &str, app: &AppHandle) -> Result<(), String> {
    write_prompt_to_pty_guarded(&crate::agent::process::PROCESS_REGISTRY, node_id, text, app, None).map(|_| ())
}

/// The liveness gate for a staged prompt: the **registry**, not the DB status,
/// is the source of truth — a node still reading `pending`/`spawning` can
/// already accept input, and an archived row could not. Split out so the
/// contract is testable without the `AppHandle` the rest of the submit path
/// needs.
fn ensure_prompt_target_alive(registry: &AgentProcessRegistry, node_id: i64) -> Result<(), String> {
    if registry.is_alive(&node_id) {
        Ok(())
    } else {
        Err(format!("node {} has no live agent process", node_id))
    }
}

pub(crate) fn write_prompt_to_pty_guarded(
    registry: &Arc<AgentProcessRegistry>,
    node_id: i64,
    text: &str,
    app: &AppHandle,
    expected_input: Option<&str>,
) -> Result<bool, String> {
    let Some((guarded, readiness)) = stage_prompt_write(registry, node_id, text, expected_input)?
    else {
        return Ok(false);
    };
    if expected_input.is_some() {
        // A continuation is not delivered until the separate Enter write has
        // been acknowledged by fresh PTY output.
        submit_staged_prompt_result(registry, node_id, guarded, &readiness).map(|submitted| submitted.is_some())
    } else {
        let registry = Arc::clone(registry);
        let app = app.clone();
        std::thread::spawn(move || submit_staged_prompt(&registry, node_id, &app, guarded, readiness));
        Ok(true)
    }
}

/// Capture Codex's output cursor before accepting the PTY write. A failed
/// input guard discards the cursor without changing the evaluator's turn mark.
fn stage_prompt_write(
    registry: &Arc<AgentProcessRegistry>,
    node_id: i64,
    text: &str,
    expected_input: Option<&str>,
) -> Result<Option<(Option<String>, PromptReadiness)>, String> {
    ensure_prompt_target_alive(registry, node_id)?;
    let readiness = paste_readiness(node_id, text)?;
    let guarded = if let Some(expected) = expected_input {
        let Some(next) = registry.write_bytes_if_current(
            node_id,
            injection_payload(text).as_bytes(),
            expected,
        )? else {
            return Ok(None);
        };
        Some(next)
    } else {
        registry.write_bytes(node_id, injection_payload(text).as_bytes())?;
        None
    };
    Ok(Some((guarded, readiness)))
}

/// The background half of [`write_prompt_to_pty`]: settle, Enter, verify.
fn submit_staged_prompt(registry: &Arc<AgentProcessRegistry>, node_id: i64, app: &AppHandle, guard: Option<String>, readiness: PromptReadiness) {
    let result = submit_staged_prompt_result(registry, node_id, guard, &readiness);
    match result {
        Ok(Some(attempt)) => tracing::info!(
            "circuit inject({}): staged prompt submitted (Enter attempt {})",
            node_id,
            attempt
        ),
        Ok(None) => {}, // New input owns the draft; never submit it automatically.
        Err(e) => {
            // Loud degrade: a staged-but-unsubmitted prompt is exactly the
            // silent stall of #874 — surface the node instead.
            tracing::warn!(
                "circuit inject({}): staged prompt was never submitted ({}) — \
                 marking the node for human attention",
                node_id,
                e
            );
            crate::commands::attention::mark_attention(node_id, app);
        }
    }
}

/// Wait for the staged paste to land at an idle input box.
///
/// A node the turn evaluator **buffers** ([`evaluator::is_piloted`]) has an
/// output clock, and the wait is signal-driven off it: the paste's echo first
/// (so a provider that renders no echo falls through at the deadline), then a
/// quiet redraw. A node the evaluator never buffers — an ordinary node a human
/// spawned — has nothing to read, so it waits [`PASTE_SETTLE_QUIET_MS`]
/// unconditionally: the same window the observable path waits *to see*, which
/// keeps the decoupled Enter out of the paste burst (#874) without pretending to
/// a signal that does not exist.
///
/// The gate is buffered-vs-not, deliberately NOT "has produced output yet": a
/// piloted node that has not written its first byte is still observable a moment
/// later, and the Autopilot prefill path injects into exactly that state
/// (`node_launch` registers the evaluator before spawning). Reading the clock
/// instead would quietly demote those callers to the unobservable path.
/// Codex and Muse multiline pastes use their rendered input-box marker as the gate;
/// startup output is otherwise indistinguishable from a paste echo here.
#[derive(Debug)]
struct PromptReadiness {
    paste: PasteReadiness,
    receipt: Option<crate::services::muse_watcher::PromptReceipt>,
}

#[derive(Debug)]
enum PasteReadiness {
    Generic,
    RenderedMultiline {
        chars: usize,
        normalized_chars: usize,
        content: String,
        output_cursor: u64,
    },
}

fn paste_readiness(node_id: i64, text: &str) -> Result<PromptReadiness, String> {
    let mut readiness = PromptReadiness { paste: PasteReadiness::Generic, receipt: None };
    if !evaluator::is_piloted(node_id) { return Ok(readiness); }
    let node = crate::db::get_agent_node_by_id(node_id)
        .map_err(|error| format!("could not identify prompt target {node_id}: {error}"))?;
    let harness = crate::preferences::resolve_harness_provider(&node.provider).adapter().id();
    if harness == "muse" {
        readiness.receipt = crate::services::muse_watcher::PromptReceipt::capture(&node, text)?;
    }
    if text.contains('\n') && matches!(harness, "codex" | "muse") {
        let content = crate::circuit::launch::normalize_for_match(text);
        readiness.paste = PasteReadiness::RenderedMultiline {
            chars: text.chars().count(), normalized_chars: text.replace("\r\n", "\n").chars().count(),
            content: if content.len() <= VISIBLE_PASTE_TEXT_LIMIT { content } else { String::new() },
            output_cursor: evaluator::output_cursor(node_id)
                .ok_or_else(|| format!("node {node_id} has no PTY output buffer"))?,
        };
    }
    Ok(readiness)
}

fn rendered_paste_visible(output: &str, chars: usize, normalized_chars: usize, content: &str) -> bool {
    output.contains(&format!("[Pasted Content {chars} chars]"))
        || output.contains(&format!("[Pasted Content {normalized_chars} chars]"))
        || (!content.is_empty()
            && crate::circuit::launch::normalize_for_match(output).contains(content))
}

fn settle_after_paste(
    registry: &AgentProcessRegistry,
    node_id: i64,
    readiness: &PasteReadiness,
) -> Result<(), String> {
    let wrote_at = Instant::now();
    if let PasteReadiness::RenderedMultiline { chars, normalized_chars, content, output_cursor, .. } = readiness {
        while Instant::now() < wrote_at + RENDERED_PASTE_READY_DEADLINE {
            ensure_prompt_target_alive(registry, node_id)?;
            if rendered_paste_visible(
                &evaluator::cleaned_output_since(node_id, *output_cursor),
                *chars,
                *normalized_chars,
                content,
            )
                && evaluator::millis_since_last_output(node_id)
                    .is_some_and(|quiet| quiet >= PASTE_SETTLE_QUIET_MS)
            {
                return Ok(());
            }
            std::thread::sleep(SUBMIT_POLL);
        }
        return Err(format!("The harness did not render the {chars}-character pasted prompt before Enter"));
    }
    if !evaluator::is_piloted(node_id) {
        std::thread::sleep(Duration::from_millis(PASTE_SETTLE_QUIET_MS as u64));
        return Ok(());
    }
    while Instant::now() < wrote_at + PASTE_ECHO_DEADLINE {
        if output_seen_within(
            evaluator::millis_since_last_output(node_id),
            wrote_at.elapsed().as_millis(),
        ) {
            break;
        }
        std::thread::sleep(SUBMIT_POLL);
    }
    let settle_deadline = Instant::now() + PASTE_SETTLE_DEADLINE;
    while Instant::now() < settle_deadline {
        match evaluator::millis_since_last_output(node_id) {
            Some(quiet) if quiet < PASTE_SETTLE_QUIET_MS => std::thread::sleep(SUBMIT_POLL),
            _ => break, // quiet (or no output tracked at all) — settled
        }
    }
    Ok(())
}

fn submit_staged_prompt_result(registry: &Arc<AgentProcessRegistry>, node_id: i64, guard: Option<String>, readiness: &PromptReadiness) -> Result<Option<u32>, String> {
    settle_after_paste(registry, node_id, &readiness.paste)?;
    press_enter_until_output_guarded(registry, node_id, guard, ENTER_ACK_WINDOW, readiness.receipt.as_ref())
}

/// Send Enter and wait for PTY output to acknowledge it, retrying up to
/// [`MAX_ENTER_ATTEMPTS`] times. Returns the attempt number that took.
/// Shared with the launch watcher — a swallowed Enter stalls a prefilled
/// launch the same way it stalls an injection.
pub(crate) fn press_enter_until_output(node_id: i64) -> Result<u32, String> {
    press_enter_until_output_guarded(&crate::agent::process::PROCESS_REGISTRY, node_id, None, ENTER_ACK_WINDOW, None)?
        .ok_or_else(|| "Input changed before submission".into())
}

/// `ack_window` is injected rather than read from [`ENTER_ACK_WINDOW`] directly
/// so a test can drive the retry ladder without paying 6 s per attempt. It is
/// the window an attempt waits for the acknowledgement, not a timeout on the
/// whole submit.
fn press_enter_until_output_guarded(
    registry: &Arc<AgentProcessRegistry>,
    node_id: i64,
    mut guard: Option<String>,
    ack_window: Duration,
    receipt: Option<&crate::services::muse_watcher::PromptReceipt>,
) -> Result<Option<u32>, String> {
    // An Enter can only be *acknowledged* against buffered output, so the retry
    // ladder only exists for a node the evaluator buffers. A node it does not
    // (an ordinary, hand-spawned node — see `settle_after_paste`) gets exactly
    // one Enter: a retry would type extra carriage returns into an agent that is
    // already working on the prompt, and reporting "never submitted" would mark
    // a node the user just handed work to as needing attention, for a
    // submission this path has no way to observe either way.
    let buffered = evaluator::is_piloted(node_id);
    let verifiable = buffered || receipt.is_some();
    for attempt in 1..=MAX_ENTER_ATTEMPTS {
        if attempt > 1 && receipt.is_some_and(|receipt| receipt.accepted()) {
            return Ok(Some(attempt - 1));
        }
        // Compare byte positions rather than rounded millisecond ages: an
        // immediate PTY response can share the same millisecond as this write.
        let output_before = if buffered && receipt.is_none() {
            evaluator::output_cursor(node_id)
                .ok_or_else(|| format!("node {node_id} lost its PTY output buffer"))?
        } else {
            0
        };
        let sent_at = Instant::now();
        if let Some(expected) = guard.as_deref() {
            let Some(next) = registry.write_bytes_if_current(node_id, b"\r", expected)? else { return Ok(None); };
            guard = Some(next);
        } else {
            registry.write_bytes(node_id, b"\r")?;
        }
        if !verifiable {
            return Ok(Some(attempt));
        }
        while Instant::now() < sent_at + ack_window {
            std::thread::sleep(SUBMIT_POLL);
            let acknowledged = match receipt {
                Some(receipt) => receipt.accepted(),
                None => evaluator::output_cursor(node_id).is_some_and(|current| current > output_before),
            };
            if acknowledged {
                return Ok(Some(attempt));
            }
        }
        tracing::warn!(
            "circuit inject({}): Enter attempt {}/{} has no submission acknowledgement",
            node_id,
            attempt,
            MAX_ENTER_ATTEMPTS
        );
    }
    if receipt.is_some() {
        return Err("Muse did not record acceptance of the staged prompt; inspect its input box before retrying".into());
    }
    Err(format!(
        "no PTY output followed any of {} Enter keystrokes",
        MAX_ENTER_ATTEMPTS
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn injection_payload_never_carries_the_enter_keystroke() {
        assert!(!injection_payload("line one\nline two").contains('\r'));
        assert!(!injection_payload("single line prompt").contains('\r'));
    }

    #[test]
    fn multiline_payload_is_bracketed_paste_wrapped() {
        let p = injection_payload("a\nb");
        assert!(p.starts_with("\x1b[200~"));
        assert!(p.ends_with("\x1b[201~"));
        assert!(p.contains("a\nb"));
    }

    #[test]
    fn single_line_payload_is_written_verbatim() {
        assert_eq!(injection_payload("do the thing"), "do the thing");
    }

    #[test]
    fn codex_multiline_waits_for_paste_render_before_enter() {
        let id = -930_024;
        let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
        evaluator::register(id);
        let readiness = PromptReadiness { paste: PasteReadiness::RenderedMultiline {
            chars: 3279,
            normalized_chars: 3279,
            content: "reviewthistestwithallremainingcontext".into(),
            output_cursor: evaluator::output_cursor(id).unwrap(),
        }, receipt: None };
        let registry_for_submit = Arc::clone(&registry);
        let submit = std::thread::spawn(move || {
            submit_staged_prompt_result(&registry_for_submit, id, None, &readiness)
        });

        evaluator::on_output(id, "Codex startup redraw; review this test");
        assert_eq!(
            writes.recv_timeout(Duration::from_millis(1500)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout),
            "startup output must not acknowledge a paste that Codex has not rendered"
        );
        evaluator::on_output(id, "[Pasted Content 3279 chars]");
        assert_eq!(
            writes.recv_timeout(Duration::from_secs(3)).unwrap(),
            b"\r".to_vec(),
        );
        evaluator::on_output(id, "task started");
        assert_eq!(submit.join().unwrap().unwrap(), Some(1));
        evaluator::unregister(id);
        registry.kill_session(id);
    }

    #[test]
    fn codex_paste_readiness_accepts_short_visible_text() {
        assert!(rendered_paste_visible("› review the diff", 15, 15, "reviewthediff"));
        assert!(!rendered_paste_visible("› review the", 15, 15, "reviewthediff"));
        assert!(!rendered_paste_visible("Codex startup redraw", 3279, 3279, "reviewthistest"));
        assert!(rendered_paste_visible("[Pasted Content 14 chars]", 15, 14, ""));
        assert!(!rendered_paste_visible("[Pasted Content 13 chars]", 15, 14, ""));
    }

    #[test]
    fn live_codex_node_selects_rendered_paste_gate() {
        crate::db::test_support::ensure_db_for_tests();
        let path = std::env::temp_dir().join(format!("codex-paste-selection-{}", std::process::id()));
        let path = path.to_string_lossy();
        let mesh = crate::db::create_mesh("codex paste selection", &path).unwrap();
        let node = crate::db::create_agent_node(
            mesh.id, "reviewer", &path, "main", crate::models::EnvType::Windows,
            "codex", None, None, None, None, false, None, None, None,
        ).unwrap();
        evaluator::register(node.id);

        assert!(matches!(
            paste_readiness(node.id, "review the change\nwith context").unwrap().paste,
            PasteReadiness::RenderedMultiline { .. }
        ));
        let proxied = crate::db::create_agent_node(
            mesh.id, "proxied reviewer", &path, "main", crate::models::EnvType::Windows,
            "codex:minimax", None, None, None, None, false, None, None, None,
        ).unwrap();
        evaluator::register(proxied.id);
        assert!(matches!(
            paste_readiness(proxied.id, "review the change\nwith context").unwrap().paste,
            PasteReadiness::RenderedMultiline { .. }
        ));
        let (registry, writes) = crate::agent::process::testing::capturing_registry(proxied.id);
        let prompt = "review the change\r\nwith context";
        evaluator::on_output(proxied.id, "old [Pasted Content 30 chars]");
        let (_, readiness) = stage_prompt_write(&registry, proxied.id, prompt, None).unwrap().unwrap();
        assert_eq!(writes.recv_timeout(Duration::from_secs(1)).unwrap(), injection_payload(prompt).into_bytes());
        let PasteReadiness::RenderedMultiline { chars, normalized_chars, content, output_cursor, .. } = readiness.paste else {
            panic!("proxied Codex must use the rendered paste gate");
        };
        assert_eq!((chars, normalized_chars), (31, 30));
        assert!(!rendered_paste_visible(&evaluator::cleaned_output_since(proxied.id, output_cursor), chars, normalized_chars, &content));
        evaluator::on_output(proxied.id, "[Pasted Content 30 chars]");
        assert!(rendered_paste_visible(&evaluator::cleaned_output_since(proxied.id, output_cursor), chars, normalized_chars, &content));
        let long_prompt = format!("review this change\n{}", "x".repeat(7_000));
        let PasteReadiness::RenderedMultiline { content, chars, normalized_chars, .. } = paste_readiness(proxied.id, &long_prompt).unwrap().paste else {
            panic!("long Codex prompts must retain the paste gate");
        };
        assert!(content.is_empty(), "a scrolled or truncated prompt cannot prove paste completion");
        assert!(!rendered_paste_visible("reviewthischange", chars, normalized_chars, &content));
        assert!(rendered_paste_visible(&format!("[Pasted Content {chars} chars]"), chars, normalized_chars, &content));
        registry.kill_session(proxied.id);
        evaluator::register(-930_099);
        assert!(
            paste_readiness(-930_099, "review the change\nwith context")
                .unwrap_err().contains("could not identify prompt target"),
            "a failed provider lookup must not silently use the early-Enter path"
        );
        evaluator::unregister(-930_099);
        evaluator::unregister(node.id);
        evaluator::unregister(proxied.id);
    }

    #[test]
    fn live_muse_node_waits_for_rendered_multiline_paste() {
        crate::db::test_support::ensure_db_for_tests();
        let path = std::env::temp_dir().join("muse-paste-selection");
        let path = path.to_string_lossy();
        let mesh = crate::db::create_mesh("muse paste selection", &path).unwrap();
        let node = crate::db::create_agent_node(
            mesh.id, "source", &path, "main", crate::models::EnvType::Windows,
            "muse", None, None, None, None, false, None, None, None,
        ).unwrap();
        evaluator::register(node.id);
        assert!(matches!(paste_readiness(node.id, "Review feedback\nApply the changes").unwrap().paste,
            PasteReadiness::RenderedMultiline { .. }));
        crate::db::update_cli_session_id(node.id, "nonexistent-muse-receipt-session").unwrap();
        assert!(paste_readiness(node.id, "Apply the review findings").unwrap_err().contains("session log is unavailable"),
            "single-line follow-ups also require the established session receipt");
        evaluator::unregister(node.id);
        assert!(matches!(paste_readiness(node.id, "Manual\nfollow-up").unwrap().paste, PasteReadiness::Generic),
            "ordinary unbuffered nodes retain their existing submission path");
    }

    #[test]
    fn muse_enter_ignores_redraw_until_the_matching_prompt_is_accepted() {
        let id = -930_026;
        let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
        evaluator::register(id);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let record = |prompt: &str| format!("{}\n", serde_json::json!({
            "payload_type":"runtime.session", "payload":{"kind":"run",
                "event":{"kind":"started","prompt":prompt}}
        }));
        std::fs::write(&path, record("feedback\nfix it")).unwrap();
        let receipt = crate::services::muse_watcher::PromptReceipt::from_log(path.clone(), "feedback\r\nfix it").unwrap();
        assert!(!receipt.accepted(), "an earlier matching prompt is not this submission");
        let registry_for_submit = Arc::clone(&registry);
        let submit = std::thread::spawn(move || {
            press_enter_until_output_guarded(&registry_for_submit, id, None, Duration::from_millis(300), Some(&receipt))
        });
        assert_eq!(writes.recv_timeout(Duration::from_secs(2)).unwrap(), b"\r");
        evaluator::on_output(id, "[Pasted Content 15 chars] redraw");
        // The second Enter proves redraw alone did not satisfy the first one.
        assert_eq!(writes.recv_timeout(Duration::from_secs(2)).unwrap(), b"\r");
        use std::io::Write;
        let mut log = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        log.write_all(record("feedback\nfix it").as_bytes()).unwrap();
        log.flush().unwrap();
        assert_eq!(submit.join().unwrap().unwrap(), Some(2));
        evaluator::unregister(id);
        registry.kill_session(id);
    }

    #[test]
    fn codex_paste_wait_stops_when_process_dies() {
        let id = -930_025;
        let (registry, _writes) = crate::agent::process::testing::capturing_registry(id);
        evaluator::register(id);
        let readiness = PromptReadiness { paste: PasteReadiness::RenderedMultiline {
            chars: 100,
            normalized_chars: 100,
            content: String::new(),
            output_cursor: evaluator::output_cursor(id).unwrap(),
        }, receipt: None };
        registry.kill_session(id);
        let started = Instant::now();
        let error = settle_after_paste(&registry, id, &readiness.paste).unwrap_err();
        assert!(error.contains("no live agent process"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(1));
        evaluator::unregister(id);
    }

    #[test]
    fn output_seen_within_only_counts_output_newer_than_the_mark() {
        // Output 200ms ago, mark set 500ms ago → the output followed the mark.
        assert!(output_seen_within(Some(200), 500));
        // Output 800ms ago predates a 500ms-old mark → not an acknowledgement.
        assert!(!output_seen_within(Some(800), 500));
        // No output tracked at all → never an acknowledgement.
        assert!(!output_seen_within(None, 10_000));
    }

    // ── submit posture (#874): what a target that cannot be observed gets ───
    //
    // The staged-submit path verifies its Enter against the evaluator's output
    // clock, which exists only for nodes the evaluator buffers. The terminal's
    // "Handover to node" injects into a target a human spawned, which is never
    // buffered — and that case previously ran the retry ladder anyway, typing
    // two extra carriage returns into an agent already working on the prompt and
    // then reporting "never submitted" (which marks the node for attention).

    /// Everything the path under test wrote, drained until the channel goes
    /// quiet. Asserted as a whole so a missing or extra Enter fails loudly
    /// instead of hanging the suite on a blocking `recv`.
    fn drained(rx: &std::sync::mpsc::Receiver<Vec<u8>>) -> Vec<Vec<u8>> {
        let mut chunks = Vec::new();
        while let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
            chunks.push(chunk);
        }
        chunks
    }

    #[test]
    fn a_target_with_no_output_clock_submits_with_exactly_one_enter() {
        let id = -930_020;
        let (registry, rx) = crate::agent::process::testing::capturing_registry(id);
        assert!(
            !evaluator::is_piloted(id),
            "fixture precondition: this target is not buffered by the evaluator"
        );

        assert_eq!(
            press_enter_until_output_guarded(&registry, id, None, Duration::from_millis(50), None)
                .expect("an unobservable submit cannot fail: nothing can be compared"),
            Some(1),
        );
        assert_eq!(
            drained(&rx),
            vec![b"\r".to_vec()],
            "one Enter, and no retry ladder behind it"
        );
        registry.kill_session(id);
    }

    /// The ladder must survive for a buffered node — including one that has not
    /// produced output yet, which is the state `node_launch` injects its prefill
    /// into (it registers the evaluator before spawning). Keying the posture off
    /// "has output yet" instead of "is buffered" would silently demote that
    /// caller to a single blind Enter.
    #[test]
    fn a_buffered_but_silent_target_retries_then_fails_loudly() {
        let id = -930_021;
        let (registry, rx) = crate::agent::process::testing::capturing_registry(id);
        evaluator::register(id);
        assert!(
            evaluator::is_piloted(id) && evaluator::millis_since_last_output(id).is_none(),
            "fixture precondition: buffered, but no output to acknowledge against"
        );

        let err = press_enter_until_output_guarded(&registry, id, None, Duration::from_millis(50), None)
            .expect_err("nothing acknowledges the Enter ⇒ the path must not claim success");
        assert!(err.contains("no PTY output followed"), "{err}");
        assert_eq!(
            drained(&rx),
            vec![b"\r".to_vec(); MAX_ENTER_ATTEMPTS as usize],
            "a buffered target keeps its retry ladder"
        );
        evaluator::unregister(id);
        registry.kill_session(id);
    }

    #[test]
    fn a_staged_prompt_needs_a_live_process_not_a_db_row() {
        let id = -930_022;
        let (registry, _rx) = crate::agent::process::testing::capturing_registry(id);
        assert!(ensure_prompt_target_alive(&registry, id).is_ok());

        let err = ensure_prompt_target_alive(&registry, -930_099)
            .expect_err("a node with no registry entry has no live process");
        assert!(err.contains("no live agent process"), "{err}");
        registry.kill_session(id);
    }

    // ── in-flight guard: queue, don't drop (#874 candidate 3) ──────────────

}
