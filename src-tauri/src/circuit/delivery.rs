//! Prompt delivery with process fencing, paste readiness and Enter acknowledgement.

use super::evaluator;
use crate::agent::process::{AgentProcessRegistry, InputDisposition, InputWriteError};
use crate::agent::provider::PasteGatePolicy;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::AppHandle;

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
/// Give a slow console reader/redraw two extra original 30-second intervals.
/// Guarded delivery synchronously blocks the shared circuit dispatcher for up
/// to 90 seconds here. This bounded delay in surfacing Unverified is accepted
/// to recover an already staged paste without risking a duplicate prompt.
const RENDERED_PASTE_TOTAL_BUDGET: Duration = Duration::from_secs(90);
/// Complete visible text is useful for short drafts. Longer drafts may be
/// collapsed or scrolled out of the TUI; unless the harness's adapter
/// declares [`PasteGatePolicy::RenderedWithTailAnchor`] they require the
/// harness's paste marker.
const VISIBLE_PASTE_TEXT_LIMIT: usize = 256;
/// A harness that draws a mid-size paste in full collapses only the largest
/// into a marker (Muse probed in a real PTY: 839 chars drawn, 1,509
/// collapsed; Codex showed a ~600-char inline paste in a partial 0.160.0
/// frame — issue #2061), so a draft past [`VISIBLE_PASTE_TEXT_LIMIT`] is
/// confirmed by its last 64 normalized characters instead. Run 343 waited
/// out the whole budget for a marker Muse never printed. Counted after
/// `normalize_for_match`, not in display columns.
const TAIL_ANCHOR_CHARS: usize = 64;

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
    write_prompt_to_pty_guarded(
        &crate::agent::process::PROCESS_REGISTRY,
        node_id,
        text,
        app,
        None,
    )
    .map(|_| ())
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

/// With expected_input, delivery blocks the circuit dispatcher for at most
/// RENDERED_PASTE_TOTAL_BUDGET of rendered-paste readiness, then up to
/// MAX_ENTER_ATTEMPTS * ENTER_ACK_WINDOW for submission acknowledgement.
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
        submit_staged_prompt_result(registry, node_id, guarded, &readiness)
            .map(|submitted| submitted.is_some())
    } else {
        let registry = Arc::clone(registry);
        let app = app.clone();
        std::thread::spawn(move || {
            submit_staged_prompt(&registry, node_id, &app, guarded, readiness)
        });
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
        // `Ok(None)` is only ever "the guard was lost" — the draft now belongs
        // to someone else. Backpressure arrives as an `Err` and is retried by
        // `retry_backpressured` instead, so a momentarily full queue can never
        // be misread as lost draft ownership and silently discard the prompt
        // (issue #1530).
        let Some(next) = retry_backpressured(|| {
            registry.write_bytes_if_current(node_id, injection_payload(text).as_bytes(), expected)
        })?
        else {
            return Ok(None);
        };
        Some(next)
    } else {
        retry_backpressured(|| {
            match registry.write_bytes(node_id, injection_payload(text).as_bytes()) {
                Ok(outcome) if outcome.is_accepted() => Ok(()),
                // A closed queue is terminal, so it must not consume the retry
                // budget: retrying a dead writer is pure latency before the same
                // error. The unguarded path has no `write_bytes_if_current` twin, so
                // the disposition is checked here rather than being flattened into
                // an `Option` alongside the guard-lost signal.
                Ok(outcome) if outcome.disposition == InputDisposition::Closed => {
                    Err(InputWriteError::Closed)
                }
                Ok(_) => Err(InputWriteError::Backpressured),
                Err(_) => Err(InputWriteError::Closed),
            }
        })?;
        None
    };
    Ok(Some((guarded, readiness)))
}

/// Retry an input write while the PTY input queue is refusing it.
///
/// A `Backpressured` write was *never queued* — the decoder did not advance and
/// no telemetry fired — so re-issuing the identical bytes cannot duplicate a
/// paste. That makes a bounded retry both safe and necessary here: the circuit
/// already blocks for up to [`RENDERED_PASTE_TOTAL_BUDGET`] (and
/// [`ENTER_ACK_WINDOW`] per Enter attempt), so a few hundred milliseconds of
/// backoff is free, while giving up surfaces through the existing error arm
/// that calls `mark_attention` — the loud path, rather than a prompt left
/// staged in the agent's input box forever (issue #1530).
fn retry_backpressured<T>(
    mut write: impl FnMut() -> Result<T, InputWriteError>,
) -> Result<T, String> {
    const ATTEMPTS: u32 = 5;
    const BACKOFF: Duration = Duration::from_millis(40);
    for attempt in 0..ATTEMPTS {
        match write() {
            Ok(value) => return Ok(value),
            Err(InputWriteError::Backpressured) if attempt + 1 < ATTEMPTS => {
                std::thread::sleep(BACKOFF * (attempt + 1));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("PTY input queue did not drain".to_string())
}

/// The background half of [`write_prompt_to_pty`]: settle, Enter, verify.
fn submit_staged_prompt(
    registry: &Arc<AgentProcessRegistry>,
    node_id: i64,
    app: &AppHandle,
    guard: Option<String>,
    readiness: PromptReadiness,
) {
    let result = submit_staged_prompt_result(registry, node_id, guard, &readiness);
    match result {
        Ok(Some(attempt)) => tracing::info!(
            "circuit inject({}): staged prompt submitted (Enter attempt {})",
            node_id,
            attempt
        ),
        Ok(None) => {} // New input owns the draft; never submit it automatically.
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
    let mut readiness = PromptReadiness {
        paste: PasteReadiness::Generic,
        receipt: None,
    };
    if !evaluator::is_circuit_piloted(node_id) {
        return Ok(readiness);
    }
    let node = crate::db::get_agent_node_by_id(node_id)
        .map_err(|error| format!("could not identify prompt target {node_id}: {error}"))?;
    let resolved = crate::preferences::resolve_harness_provider(&node.provider);
    let adapter = resolved.adapter();
    // The session-log receipt is a Muse-watcher service integration, not
    // paste confirmation: it stays keyed on the harness id while the gate
    // below consults the adapter-declared policy (issue #2061).
    if adapter.id() == "muse" {
        readiness.receipt = crate::services::muse_watcher::PromptReceipt::capture(&node, text)?;
    }
    let policy = adapter.paste_gate_policy();
    if text.contains('\n') && !matches!(policy, PasteGatePolicy::Generic) {
        let content = crate::circuit::launch::normalize_for_match(text);
        readiness.paste = PasteReadiness::RenderedMultiline {
            chars: text.chars().count(),
            normalized_chars: text.replace("\r\n", "\n").chars().count(),
            content: visible_paste_proof(policy, content),
            output_cursor: evaluator::output_cursor(node_id)
                .ok_or_else(|| format!("node {node_id} has no PTY output buffer"))?,
        };
    }
    Ok(readiness)
}

/// The normalized text whose presence in fresh output proves a multiline paste
/// landed, or empty when only the paste marker can.
///
/// The paste is read in order, so the draft's tail appearing means the text
/// before it was accepted; a composer that scrolls keeps the tail in view. It
/// cannot tell a fresh redraw from a stale one that repaints an earlier prompt
/// with an identical ending, which the quiet-output gate only narrows.
fn visible_paste_proof(policy: PasteGatePolicy, normalized: String) -> String {
    let chars = normalized.chars().count();
    if chars <= VISIBLE_PASTE_TEXT_LIMIT {
        return normalized;
    }
    if policy != PasteGatePolicy::RenderedWithTailAnchor {
        return String::new();
    }
    let skip = chars.saturating_sub(TAIL_ANCHOR_CHARS);
    normalized.chars().skip(skip).collect()
}

fn rendered_paste_visible(
    output: &str,
    chars: usize,
    normalized_chars: usize,
    content: &str,
) -> bool {
    // ConPTY inserts padding and line breaks when the composer wraps a marker.
    // Preserve punctuation and the full count so partial/different pastes fail.
    let compact: String = output.chars().filter(|c| !c.is_whitespace()).collect();
    compact.contains(&format!("[PastedContent{chars}chars]"))
        || compact.contains(&format!("[PastedContent{normalized_chars}chars]"))
        || (!content.is_empty()
            && crate::circuit::launch::normalize_for_match(output).contains(content))
}

/// Wait for the staged paste to land at an idle input box without writing input.
///
/// Rendered-gate multiline pastes require a complete matching paste echo (a
/// marker, full visible text for short drafts, or for tail-anchor harnesses
/// the draft's tail — see [`visible_paste_proof`]) and a quiet redraw within one
/// RENDERED_PASTE_TOTAL_BUDGET (90 seconds in production).
/// Polling rechecks liveness and guard every SUBMIT_POLL; progress is logged at
/// each third of the budget. rendered_paste_budget is injected so tests can
/// exercise late echoes and exhaustion without spending 90 seconds waiting.
/// Ok(false) means the guard lost draft ownership: the caller must not Enter.
/// Exhaustion or process death returns Err, keeping delivery unverified.
///
/// Generic buffered targets wait for a paste echo (or PASTE_ECHO_DEADLINE), then
/// a quiet redraw (bounded by PASTE_SETTLE_DEADLINE). Generic unbuffered targets
/// wait PASTE_SETTLE_QUIET_MS unconditionally, separating Enter from the paste
/// burst (#874). Buffer registration, rather than output already being present,
/// selects the observable path so a newly launched piloted node can still echo.
/// Those generic branches do not consult guard here; the subsequent
/// press_enter_until_output_guarded atomically fences every Enter write.
fn settle_after_paste(
    registry: &AgentProcessRegistry,
    node_id: i64,
    readiness: &PasteReadiness,
    guard: Option<&str>,
    rendered_paste_budget: Duration,
) -> Result<bool, String> {
    let wrote_at = Instant::now();
    if let PasteReadiness::RenderedMultiline {
        chars,
        normalized_chars,
        content,
        output_cursor,
        ..
    } = readiness
    {
        let mut paste_seen = false;
        let deadline = wrote_at + rendered_paste_budget;
        let progress_interval = rendered_paste_budget / 3;
        let mut next_progress = wrote_at + progress_interval;
        while Instant::now() < deadline {
            ensure_prompt_target_alive(registry, node_id)?;
            if guard.is_some_and(|expected| !registry.input_is_current(node_id, expected)) {
                return Ok(false);
            }
            // Latch a complete echo before later redraws evict it from the
            // bounded tail. Once latched, only the quiet clock is consulted.
            if !paste_seen {
                paste_seen = rendered_paste_visible(
                    &evaluator::cleaned_output_since(node_id, *output_cursor),
                    *chars,
                    *normalized_chars,
                    content,
                );
            }
            if paste_seen
                && evaluator::millis_since_last_output(node_id)
                    .is_some_and(|quiet| quiet >= PASTE_SETTLE_QUIET_MS)
            {
                return Ok(true);
            }
            if Instant::now() >= next_progress {
                tracing::info!("circuit inject({node_id}): paste readiness still pending after {:.1}s of {rendered_paste_budget:?}; rechecking the existing draft", wrote_at.elapsed().as_secs_f64());
                next_progress += progress_interval;
            }
            std::thread::sleep(SUBMIT_POLL.min(deadline.saturating_duration_since(Instant::now())));
        }
        return Err(format!("The harness did not confirm the {chars}-character pasted prompt before Enter after {:.1}s (readiness budget {rendered_paste_budget:?})", wrote_at.elapsed().as_secs_f64()));
    }
    if !evaluator::is_circuit_piloted(node_id) {
        std::thread::sleep(Duration::from_millis(PASTE_SETTLE_QUIET_MS as u64));
        return Ok(true);
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
    Ok(true)
}

fn submit_staged_prompt_result(
    registry: &Arc<AgentProcessRegistry>,
    node_id: i64,
    guard: Option<String>,
    readiness: &PromptReadiness,
) -> Result<Option<u32>, String> {
    if !settle_after_paste(
        registry,
        node_id,
        &readiness.paste,
        guard.as_deref(),
        RENDERED_PASTE_TOTAL_BUDGET,
    )? {
        return Ok(None);
    }
    press_enter_until_output_guarded(
        registry,
        node_id,
        guard,
        ENTER_ACK_WINDOW,
        readiness.receipt.as_ref(),
    )
}

/// Send Enter and wait for PTY output to acknowledge it, retrying up to
/// [`MAX_ENTER_ATTEMPTS`] times. Returns the attempt number that took.
/// Shared with the launch watcher — a swallowed Enter stalls a prefilled
/// launch the same way it stalls an injection.
pub(crate) fn press_enter_until_output(node_id: i64) -> Result<u32, String> {
    press_enter_until_output_guarded(
        &crate::agent::process::PROCESS_REGISTRY,
        node_id,
        None,
        ENTER_ACK_WINDOW,
        None,
    )?
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
    let buffered = evaluator::is_circuit_piloted(node_id);
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
            // A backpressured Enter is retried, not treated as a lost guard.
            // Pre-#1530 a refused Enter returned `Ok(None)`, which
            // `press_enter_until_output_guarded` reported as "new input owns the
            // draft" — leaving the prompt staged in the agent's input box
            // forever with no `mark_attention` and no error anywhere.
            let Some(next) =
                retry_backpressured(|| registry.write_bytes_if_current(node_id, b"\r", expected))?
            else {
                return Ok(None);
            };
            guard = Some(next);
        } else {
            retry_backpressured(|| match registry.write_bytes(node_id, b"\r") {
                Ok(outcome) if outcome.is_accepted() => Ok(()),
                // Terminal: no point spending the retry budget on a dead writer.
                Ok(outcome) if outcome.disposition == InputDisposition::Closed => {
                    Err(InputWriteError::Closed)
                }
                Ok(_) => Err(InputWriteError::Backpressured),
                Err(_) => Err(InputWriteError::Closed),
            })?;
        }
        if !verifiable {
            return Ok(Some(attempt));
        }
        while Instant::now() < sent_at + ack_window {
            std::thread::sleep(SUBMIT_POLL);
            let acknowledged = match receipt {
                Some(receipt) => receipt.accepted(),
                None => {
                    evaluator::output_cursor(node_id).is_some_and(|current| current > output_before)
                }
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

    /// Move a node this test just created into a process-unique id range.
    ///
    /// Every isolated test database restarts its autoincrement ids at 1
    /// (issue #2048), while the evaluator's node map is process-global and
    /// keyed by node id. Two delivery tests running in parallel would
    /// otherwise register the same id and feed each other's PTY output into
    /// one buffer. Nothing references the node yet — the test created it a
    /// moment ago — so rewriting the primary key is safe.
    fn unique_node_id(node_id: i64) -> i64 {
        static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);
        let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst) * 1_000_000 + 1;
        crate::db::write_conn()
            .execute(
                "UPDATE agent_nodes SET id = ?1 WHERE id = ?2",
                rusqlite::params![unique, node_id],
            )
            .expect("reserve a process-unique node id");
        unique
    }

    #[test]
    fn codex_multiline_waits_for_paste_render_before_enter() {
        let _db = crate::db::test_support::isolated();
        let path = std::env::temp_dir().join("codex-guarded-paste");
        let path = path.to_string_lossy();
        let mesh = crate::db::create_mesh("guarded Codex paste", &path).unwrap();
        let node = crate::db::create_agent_node(
            mesh.id,
            "source",
            &path,
            "main",
            crate::models::EnvType::Windows,
            "codex",
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
        )
        .unwrap();
        let id = unique_node_id(node.id);
        let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
        evaluator::register(id);
        let prompt = format!("feedback\n{}", "x".repeat(9398));
        evaluator::on_output(id, "old [Pasted Content 9407 chars]");
        let expected = registry.input_stamp(id).unwrap();
        let (guard, readiness) = stage_prompt_write(&registry, id, &prompt, Some(&expected))
            .unwrap()
            .unwrap();
        assert_eq!(
            writes.recv_timeout(Duration::from_secs(1)).unwrap(),
            injection_payload(&prompt).into_bytes()
        );
        assert!(
            registry.input_stamp(id).is_none(),
            "report admission must still reject the staged draft"
        );
        assert!(
            registry.input_is_current(id, guard.as_deref().unwrap()),
            "the post-write stamp must own its nonempty draft"
        );
        let registry_for_submit = Arc::clone(&registry);
        let submit = std::thread::spawn(move || {
            submit_staged_prompt_result(&registry_for_submit, id, guard, &readiness)
        });

        evaluator::on_output(id, "Codex startup redraw; review this test");
        assert_eq!(
            writes.recv_timeout(Duration::from_millis(1500)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout),
            "startup output must not acknowledge a paste that Codex has not rendered"
        );
        evaluator::on_output(
            id,
            "\x1b[38;5;6m[Pasted Content     \x1b[m  \x1b[38;5;6m9407 chars]\x1b[K\x1b[m",
        );
        assert_eq!(
            writes.recv_timeout(Duration::from_secs(3)).unwrap(),
            b"\r".to_vec(),
        );
        evaluator::on_output(id, "task started");
        assert_eq!(submit.join().unwrap().unwrap(), Some(1));
        assert_eq!(
            writes.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty),
            "exactly one separate Enter is allowed"
        );
        evaluator::unregister(id);
        registry.kill_session(id);
    }

    #[test]
    fn codex_paste_readiness_accepts_short_visible_text() {
        assert!(rendered_paste_visible(
            "› review the diff",
            15,
            15,
            "reviewthediff"
        ));
        assert!(!rendered_paste_visible(
            "› review the",
            15,
            15,
            "reviewthediff"
        ));
        assert!(!rendered_paste_visible(
            "Codex startup redraw",
            3279,
            3279,
            "reviewthistest"
        ));
        assert!(rendered_paste_visible(
            "[Pasted Content 14 chars]",
            15,
            14,
            ""
        ));
        assert!(!rendered_paste_visible(
            "[Pasted Content 13 chars]",
            15,
            14,
            ""
        ));
    }

    #[test]
    fn codex_paste_readiness_matches_wrapped_terminal_marker() {
        let id = -930_027;
        evaluator::register(id);
        evaluator::on_output(id, "old [Pasted Content 9407 chars]");
        let cursor = evaluator::output_cursor(id).unwrap();
        assert!(!rendered_paste_visible(
            &evaluator::cleaned_output_since(id, cursor),
            9407,
            9407,
            ""
        ));
        for output in [
            "\x1b[36m[Pasted Content \x1b[0m\r\n  9407 chars]",
            // Captured from Codex 0.160.0 in a 22-column Windows ConPTY.
            "\x1b[38;5;6m[Pasted Content     \x1b[m  \x1b[38;5;6m9407 chars]\x1b[K\x1b[m",
        ] {
            let cursor = evaluator::output_cursor(id).unwrap();
            evaluator::on_output(id, output);
            assert!(
                rendered_paste_visible(
                    &evaluator::cleaned_output_since(id, cursor),
                    9407,
                    9407,
                    ""
                ),
                "terminal line wrapping must not hide a complete matching paste marker"
            );
        }
        assert!(!rendered_paste_visible(
            "[Pasted Content\r\n 940 chars]",
            9407,
            9407,
            ""
        ));
        assert!(!rendered_paste_visible(
            "[Pasted Content 9407 chars",
            9407,
            9407,
            ""
        ));
        evaluator::unregister(id);
    }

    #[test]
    fn codex_paste_readiness_rechecks_late_echo_without_repeating_input() {
        let id = -930_028;
        let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
        evaluator::register(id);
        let expected = registry.input_stamp(id).unwrap();
        let prompt = format!("feedback\n{}", "x".repeat(9398));
        let guard = registry
            .write_bytes_if_current(id, injection_payload(&prompt).as_bytes(), &expected)
            .unwrap()
            .unwrap();
        assert_eq!(
            writes.recv_timeout(Duration::from_secs(1)).unwrap(),
            injection_payload(&prompt).into_bytes()
        );
        let readiness = PasteReadiness::RenderedMultiline {
            chars: 9407,
            normalized_chars: 9407,
            content: String::new(),
            output_cursor: evaluator::output_cursor(id).unwrap(),
        };
        let registry_for_wait = Arc::clone(&registry);
        let wait = std::thread::spawn(move || {
            settle_after_paste(
                &registry_for_wait,
                id,
                &readiness,
                Some(&guard),
                Duration::from_secs(5),
            )
        });
        assert_eq!(writes.recv_timeout(Duration::from_secs(2)), Err(std::sync::mpsc::RecvTimeoutError::Timeout),
            "an idle draft without a complete echo must receive no input while readiness is rechecked");
        evaluator::on_output(id, "[Pasted Content\r\n 9407 chars]");
        assert!(
            wait.join().unwrap().unwrap(),
            "a late complete echo within the total budget must be accepted"
        );
        assert_eq!(
            writes.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty),
            "readiness retries must not paste again or send Enter"
        );
        evaluator::unregister(id);
        registry.kill_session(id);
    }

    #[test]
    fn codex_paste_readiness_stops_on_changed_input_or_exhaustion() {
        let id = -930_029;
        let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
        evaluator::register(id);
        let expected = registry.input_stamp(id).unwrap();
        let prompt = format!("feedback\n{}", "x".repeat(9398));
        let guard = registry
            .write_bytes_if_current(id, injection_payload(&prompt).as_bytes(), &expected)
            .unwrap()
            .unwrap();
        assert_eq!(
            writes.recv_timeout(Duration::from_secs(1)).unwrap(),
            injection_payload(&prompt).into_bytes()
        );
        let readiness = PasteReadiness::RenderedMultiline {
            chars: 9407,
            normalized_chars: 9407,
            content: String::new(),
            output_cursor: evaluator::output_cursor(id).unwrap(),
        };
        let error = settle_after_paste(
            &registry,
            id,
            &readiness,
            Some(&guard),
            Duration::from_millis(1),
        )
        .unwrap_err();
        assert!(
            error.contains("after ") && error.contains("s (readiness budget 1ms)"),
            "{error}"
        );
        assert_eq!(
            writes.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty),
            "an idle agent without a complete echo must not receive blind input"
        );
        registry.write_bytes(id, b"human draft").unwrap();
        assert_eq!(
            writes.recv_timeout(Duration::from_secs(1)).unwrap(),
            b"human draft"
        );
        assert!(!settle_after_paste(
            &registry,
            id,
            &readiness,
            Some(&guard),
            Duration::from_secs(1)
        )
        .unwrap());
        assert_eq!(writes.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
        evaluator::unregister(id);
        registry.kill_session(id);
    }

    #[test]
    fn live_codex_node_selects_rendered_paste_gate() {
        let _db = crate::db::test_support::isolated();
        let path =
            std::env::temp_dir().join(format!("codex-paste-selection-{}", std::process::id()));
        let path = path.to_string_lossy();
        let mesh = crate::db::create_mesh("codex paste selection", &path).unwrap();
        let node = crate::db::create_agent_node(
            mesh.id,
            "reviewer",
            &path,
            "main",
            crate::models::EnvType::Windows,
            "codex",
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
        )
        .unwrap();
        let node_id = unique_node_id(node.id);
        evaluator::register(node_id);

        assert!(matches!(
            paste_readiness(node_id, "review the change\nwith context")
                .unwrap()
                .paste,
            PasteReadiness::RenderedMultiline { .. }
        ));
        let proxied = crate::db::create_agent_node(
            mesh.id,
            "proxied reviewer",
            &path,
            "main",
            crate::models::EnvType::Windows,
            "codex:minimax",
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
        )
        .unwrap();
        let proxied_id = unique_node_id(proxied.id);
        evaluator::register(proxied_id);
        assert!(matches!(
            paste_readiness(proxied_id, "review the change\nwith context")
                .unwrap()
                .paste,
            PasteReadiness::RenderedMultiline { .. }
        ));
        let (registry, writes) = crate::agent::process::testing::capturing_registry(proxied_id);
        let prompt = "review the change\r\nwith context";
        evaluator::on_output(proxied_id, "old [Pasted Content 30 chars]");
        let (_, readiness) = stage_prompt_write(&registry, proxied_id, prompt, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            writes.recv_timeout(Duration::from_secs(1)).unwrap(),
            injection_payload(prompt).into_bytes()
        );
        let PasteReadiness::RenderedMultiline {
            chars,
            normalized_chars,
            content,
            output_cursor,
            ..
        } = readiness.paste
        else {
            panic!("proxied Codex must use the rendered paste gate");
        };
        assert_eq!((chars, normalized_chars), (31, 30));
        assert!(!rendered_paste_visible(
            &evaluator::cleaned_output_since(proxied_id, output_cursor),
            chars,
            normalized_chars,
            &content
        ));
        evaluator::on_output(proxied_id, "[Pasted Content 30 chars]");
        assert!(rendered_paste_visible(
            &evaluator::cleaned_output_since(proxied_id, output_cursor),
            chars,
            normalized_chars,
            &content
        ));
        let long_prompt = format!("review this change\n{}", "x".repeat(7_000));
        let PasteReadiness::RenderedMultiline {
            content,
            chars,
            normalized_chars,
            ..
        } = paste_readiness(proxied_id, &long_prompt).unwrap().paste
        else {
            panic!("long Codex prompts must retain the paste gate");
        };
        assert_eq!(
            content,
            "x".repeat(TAIL_ANCHOR_CHARS),
            "a long Codex prompt is confirmed by its tail (issue #2061)"
        );
        assert!(!rendered_paste_visible(
            "reviewthischange",
            chars,
            normalized_chars,
            &content
        ));
        assert!(rendered_paste_visible(
            &format!("composerdrawn{content}"),
            chars,
            normalized_chars,
            &content
        ));
        assert!(rendered_paste_visible(
            &format!("[Pasted Content {chars} chars]"),
            chars,
            normalized_chars,
            &content
        ));
        registry.kill_session(proxied_id);
        evaluator::register(-930_099);
        assert!(
            paste_readiness(-930_099, "review the change\nwith context")
                .unwrap_err()
                .contains("could not identify prompt target"),
            "a failed provider lookup must not silently use the early-Enter path"
        );
        evaluator::unregister(-930_099);
        evaluator::unregister(node_id);
        evaluator::unregister(proxied_id);
    }

    #[test]
    fn live_muse_node_waits_for_rendered_multiline_paste() {
        let _db = crate::db::test_support::isolated();
        let path = std::env::temp_dir().join("muse-paste-selection");
        let path = path.to_string_lossy();
        let mesh = crate::db::create_mesh("muse paste selection", &path).unwrap();
        let node = crate::db::create_agent_node(
            mesh.id,
            "source",
            &path,
            "main",
            crate::models::EnvType::Windows,
            "muse",
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
        )
        .unwrap();
        let node_id = unique_node_id(node.id);
        evaluator::register(node_id);
        assert!(matches!(
            paste_readiness(node_id, "Review feedback\nApply the changes")
                .unwrap()
                .paste,
            PasteReadiness::RenderedMultiline { .. }
        ));
        crate::db::update_cli_session_id(node_id, "nonexistent-muse-receipt-session").unwrap();
        assert!(
            paste_readiness(node_id, "Apply the review findings")
                .unwrap_err()
                .contains("session log is unavailable"),
            "single-line follow-ups also require the established session receipt"
        );
        evaluator::unregister(node_id);
        assert!(
            matches!(
                paste_readiness(node_id, "Manual\nfollow-up").unwrap().paste,
                PasteReadiness::Generic
            ),
            "ordinary unbuffered nodes retain their existing submission path"
        );
    }

    /// Stand-in for run 343's 831-character `publish` prompt (that run's own text
    /// was not retained): a 600-character multiline draft in the same size band.
    /// Muse Code 1.3.0 draws such a paste in full with no `[Pasted Content N
    /// chars]` marker, and the gate used to demand the marker past the
    /// visible-text limit, so the staged prompt sat unsent for the 90 s budget.
    const MUSE_MIDSIZE_PROMPT: &str = concat!(
        "Publish the work from this session as a pull request.\n",
        "\n",
        "- Push the current branch to origin and open a PR against `main`.\n",
        "- Title it with a Conventional Commit summary of the change.\n",
        "- In the body, explain what changed and why, list the checks you ran, and\n",
        "  link the issue with a `Closes #1234` line if one exists.\n",
        "- Do not merge the PR; a reviewer will pick it up next.\n",
        "\n",
        "When the PR is open, reply with its URL and a one-paragraph summary of what\n",
        "you verified, so the reviewer knows what has already been covered. If any gate\n",
        "is red, say so plainly instead of opening the PR and explain the failure.",
    );

    /// The final composer redraw from a real Muse Code 1.3.0 capture of
    /// `MUSE_MIDSIZE_PROMPT` in a 79x57 Windows ConPTY (box rules and status
    /// line abbreviated): wrapped mid-sentence ("If any" / "gate"), indented,
    /// positioned with cursor escapes, and carrying no paste marker.
    const MUSE_MIDSIZE_COMPOSER_FRAME: &str = concat!(
        "\x1b[8;1H\x1b[J\x1b[8;1H\x1b[2m\x1b[38;2;103;108;116;49m\u{2500}\u{2500} \x1b[22m",
        "\x1b[38;2;138;144;152;49mVoice input (Alt+V to start)\x1b[2m\u{2500}\u{2500}\u{2500}\u{2500}",
        "\x1b[9;1H\x1b[22m\x1b[38;2;90;160;255;49m\u{276f} \x1b[38;2;204;211;219;49m",
        "Publish the work from this session as a pull request.",
        "\x1b[10;1H\x1b[38;2;103;108;116;49m  ",
        "\x1b[11;1H  \x1b[38;2;204;211;219;49m- Push the current branch to origin and open a PR against `main`.",
        "\x1b[12;1H\x1b[38;2;103;108;116;49m  \x1b[38;2;204;211;219;49m- Title it with a Conventional Commit summary of the change.",
        "\x1b[13;1H\x1b[38;2;103;108;116;49m  \x1b[38;2;204;211;219;49m- In the body, explain what changed and why, list the checks you ran, and",
        "\x1b[14;1H\x1b[38;2;103;108;116;49m  \x1b[38;2;204;211;219;49m  link the issue with a `Closes #1234` line if one exists.",
        "\x1b[15;1H\x1b[38;2;103;108;116;49m  \x1b[38;2;204;211;219;49m- Do not merge the PR; a reviewer will pick it up next.",
        "\x1b[16;1H\x1b[38;2;103;108;116;49m  ",
        "\x1b[17;1H  \x1b[38;2;204;211;219;49mWhen the PR is open, reply with its URL and a one-paragraph summary of what",
        "\x1b[18;1H\x1b[38;2;103;108;116;49m  \x1b[38;2;204;211;219;49myou verified, so the reviewer knows what has already been covered. If any ",
        "\x1b[19;1H\x1b[38;2;103;108;116;49m  \x1b[38;2;204;211;219;49mgate",
        "\x1b[20;1H\x1b[38;2;103;108;116;49m  \x1b[38;2;204;211;219;49mis red, say so plainly instead of opening the PR and explain the failure.",
        "\x1b[21;1H\x1b[2m\x1b[38;2;103;108;116;49m\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
        "\x1b[22;1H\x1b[22m  \x1b[38;2;90;160;255;49mmuse-spark-1.3-contributor\x1b[38;2;138;144;152;49m \u{b7} ",
        "\x1b[38;2;90;160;255;49mhigh\x1b[38;2;138;144;152;49m \u{b7} Auto-review\x1b[39m\x1b[49m\x1b[59m\x1b[0m",
    );

    /// Create a muse node in this test's own database and return its id.
    ///
    /// The caller installs the database with `db::test_support::isolated()` and
    /// holds that guard for the rest of the test (issue #2048): a helper that
    /// installed it and dropped the guard on return would leave the rest of the
    /// test reading the process-global database.
    fn muse_publisher_node(label: &str) -> i64 {
        let path = std::env::temp_dir().join(label);
        let path = path.to_string_lossy();
        let mesh = crate::db::create_mesh(label, &path).unwrap();
        crate::db::create_agent_node(
            mesh.id,
            "publisher",
            &path,
            "main",
            crate::models::EnvType::Windows,
            "muse",
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
        )
        .unwrap()
        .id
    }

    #[test]
    fn only_tail_anchor_harnesses_confirm_a_long_draft_by_their_tail() {
        let long = "word".repeat(100);
        let tail = |policy| visible_paste_proof(policy, long.clone());
        assert_eq!(
            tail(PasteGatePolicy::RenderedWithTailAnchor),
            "word".repeat(TAIL_ANCHOR_CHARS / 4)
        );
        assert_eq!(
            tail(PasteGatePolicy::RenderedMarkerOnly),
            "",
            "a collapsed draft carries no visible tail: only its marker can confirm it"
        );
        assert_eq!(
            tail(PasteGatePolicy::Generic),
            "",
            "the generic path never takes a tail proof"
        );
        // Under the limit every policy still requires the complete text.
        assert_eq!(
            visible_paste_proof(
                PasteGatePolicy::RenderedWithTailAnchor,
                "short draft".into()
            ),
            "short draft"
        );
        assert_eq!(
            visible_paste_proof(PasteGatePolicy::RenderedMarkerOnly, "short draft".into()),
            "short draft"
        );
        assert_eq!(
            visible_paste_proof(PasteGatePolicy::Generic, "short draft".into()),
            "short draft"
        );
        // The cut is by character, so a multi-byte tail is never split mid-codepoint.
        let accented = "é".repeat(VISIBLE_PASTE_TEXT_LIMIT + 1);
        assert_eq!(
            visible_paste_proof(PasteGatePolicy::RenderedWithTailAnchor, accented),
            "é".repeat(TAIL_ANCHOR_CHARS)
        );
    }

    #[test]
    fn the_full_text_limit_counts_characters_not_bytes() {
        // 200 characters but 400 bytes: under the limit, so every policy keeps the full text.
        let accented = "é".repeat(200);
        assert!(
            accented.len() > VISIBLE_PASTE_TEXT_LIMIT,
            "fixture precondition: over the limit in bytes only"
        );
        assert_eq!(
            visible_paste_proof(PasteGatePolicy::RenderedMarkerOnly, accented.clone()),
            accented
        );
        assert_eq!(
            visible_paste_proof(PasteGatePolicy::RenderedWithTailAnchor, accented.clone()),
            accented
        );
        // One character past the limit is the boundary for marker-only harnesses.
        let at_limit = "é".repeat(VISIBLE_PASTE_TEXT_LIMIT);
        assert_eq!(
            visible_paste_proof(PasteGatePolicy::RenderedMarkerOnly, at_limit.clone()),
            at_limit
        );
        assert_eq!(
            visible_paste_proof(
                PasteGatePolicy::RenderedMarkerOnly,
                "é".repeat(VISIBLE_PASTE_TEXT_LIMIT + 1)
            ),
            ""
        );
    }

    #[test]
    fn muse_midsize_paste_rendered_in_full_is_confirmed_by_its_tail() {
        let _db = crate::db::test_support::isolated();
        let id = muse_publisher_node("muse-midsize-paste");
        let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
        evaluator::register(id);
        assert!(
            crate::circuit::launch::normalize_for_match(MUSE_MIDSIZE_PROMPT).chars().count() > VISIBLE_PASTE_TEXT_LIMIT,
            "fixture precondition: past the full-text limit, where only a marker used to be accepted"
        );
        let (_, readiness) = stage_prompt_write(&registry, id, MUSE_MIDSIZE_PROMPT, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            writes.recv_timeout(Duration::from_secs(1)).unwrap(),
            injection_payload(MUSE_MIDSIZE_PROMPT).into_bytes()
        );

        let registry_for_wait = Arc::clone(&registry);
        let wait = std::thread::spawn(move || {
            settle_after_paste(
                &registry_for_wait,
                id,
                &readiness.paste,
                None,
                Duration::from_secs(5),
            )
        });
        evaluator::on_output(id, MUSE_MIDSIZE_COMPOSER_FRAME);
        assert!(
            wait.join()
                .unwrap()
                .expect("a prompt Muse rendered in full must be confirmed, not time out"),
            "the staged prompt is ready for Enter"
        );
        assert_eq!(
            writes.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty),
            "confirming the paste must not write anything"
        );
        evaluator::unregister(id);
        registry.kill_session(id);
    }

    #[test]
    fn muse_midsize_paste_is_not_confirmed_until_its_tail_is_drawn() {
        let _db = crate::db::test_support::isolated();
        let id = muse_publisher_node("muse-midsize-partial-paste");
        evaluator::register(id);
        let PasteReadiness::RenderedMultiline {
            chars,
            normalized_chars,
            content,
            ..
        } = paste_readiness(id, MUSE_MIDSIZE_PROMPT).unwrap().paste
        else {
            panic!("a multiline Muse prompt must use the rendered paste gate");
        };
        // Read each frame back as production does: output since a cursor, escapes stripped.
        let confirmed_by = |frame: &str| {
            let cursor = evaluator::output_cursor(id).unwrap();
            evaluator::on_output(id, frame);
            rendered_paste_visible(
                &evaluator::cleaned_output_since(id, cursor),
                chars,
                normalized_chars,
                &content,
            )
        };
        assert!(confirmed_by(MUSE_MIDSIZE_COMPOSER_FRAME));
        // Everything but the last row: the head of the draft is drawn, the paste is not complete.
        let partial = MUSE_MIDSIZE_COMPOSER_FRAME
            .split("\x1b[20;1H")
            .next()
            .unwrap();
        assert!(
            !confirmed_by(partial),
            "a head-only redraw must not acknowledge a paste that is still arriving"
        );
        // A different draft with the same opening is not this paste either.
        assert!(!confirmed_by(
            &MUSE_MIDSIZE_COMPOSER_FRAME.replace("explain the failure", "do something else")
        ));
        evaluator::unregister(id);
    }

    /// Stand-in for the #2061 Codex gap: a mid-size multiline draft in the
    /// same size band as run 343's 831-character Muse prompt. A partial frame
    /// from Codex 0.160.0 (79x57 Windows ConPTY) showed a ~600-character paste
    /// drawn inline with no `[Pasted Content N chars]` marker, but a clean
    /// capture was blocked by a hooks-review dialog — so
    /// `CODEX_MIDSIZE_COMPOSER_FRAME` is synthetic (the full draft text inside
    /// redraw chrome, no marker), pinning the rendering shape the tail rule
    /// relies on rather than a byte-exact recording.
    const CODEX_MIDSIZE_PROMPT: &str = concat!(
        "Review the changes on this branch and leave line-level feedback.\n",
        "\n",
        "- Compare against `main` and focus on behavior changes, not style.\n",
        "- For each finding, quote the exact lines and explain the failure mode.\n",
        "- Check the new tests exercise the changed paths: name the test and the\n",
        "  assertion that would fail without the fix.\n",
        "- If a check is red, paste the failing output instead of approving.\n",
        "\n",
        "When the review is complete, reply with APPROVE or REQUEST-CHANGES and\n",
        "the full finding list, so the author knows what remains before merge.\n",
        "Do not push any commits yourself.",
    );

    /// Synthetic Codex composer redraw of `CODEX_MIDSIZE_PROMPT`: cursor
    /// addressing and line clears around the full draft text, `›` input
    /// prefixes, and no paste marker anywhere.
    const CODEX_MIDSIZE_COMPOSER_FRAME: &str = concat!(
        "\x1b[5;1H\x1b[J\x1b[5;1H› Review the changes on this branch and leave line-level feedback.\x1b[K",
        "\x1b[6;1H\x1b[K",
        "\x1b[7;1H› - Compare against `main` and focus on behavior changes, not style.\x1b[K",
        "\x1b[8;1H› - For each finding, quote the exact lines and explain the failure mode.\x1b[K",
        "\x1b[9;1H› - Check the new tests exercise the changed paths: name the test and the\x1b[K",
        "\x1b[10;1H›   assertion that would fail without the fix.\x1b[K",
        "\x1b[11;1H› - If a check is red, paste the failing output instead of approving.\x1b[K",
        "\x1b[12;1H\x1b[K",
        "\x1b[13;1H› When the review is complete, reply with APPROVE or REQUEST-CHANGES and\x1b[K",
        "\x1b[14;1H› the full finding list, so the author knows what remains before merge.\x1b[K",
        "\x1b[15;1H› Do not push any commits yourself.\x1b[K",
        "\x1b[16;1H\x1b[2mesc to cancel · enter to send\x1b[0m",
    );

    /// Earlier prompt sharing exactly the ending of `CODEX_MIDSIZE_PROMPT`
    /// (its last two sentences) and nothing else, for the stale-redraw
    /// limit pin: a redraw of this draft after a newer write carries the
    /// newer draft's whole tail proof.
    const CODEX_EARLIER_SAME_ENDING_PROMPT: &str = concat!(
        "Summarize yesterday's incidents for the status page.\n",
        "\n",
        "- List each incident with its start time and duration.\n",
        "- Note which ones paged and which stayed silent.\n",
        "\n",
        "So the author knows what remains before merge. Do not push any commits yourself.",
    );

    /// That earlier prompt's composer redraw: cursor chrome around A's full
    /// text, no paste marker. It carries B's tail and none of B's head.
    const CODEX_EARLIER_REDRAW_FRAME: &str = concat!(
        "\x1b[5;1H\x1b[J› Summarize yesterday's incidents for the status page.\x1b[K",
        "\x1b[6;1H\x1b[K",
        "\x1b[7;1H› - List each incident with its start time and duration.\x1b[K",
        "\x1b[8;1H› - Note which ones paged and which stayed silent.\x1b[K",
        "\x1b[9;1H\x1b[K",
        "\x1b[10;1H› So the author knows what remains before merge. Do not push any commits yourself.\x1b[K",
        "\x1b[11;1H\x1b[2mesc to cancel · enter to send\x1b[0m",
    );

    /// Create a Codex node in this test's own database and return a
    /// process-unique id for it (see `unique_node_id`).
    fn codex_worker_node(label: &str) -> i64 {
        let path = std::env::temp_dir().join(format!("{}-{}", label, std::process::id()));
        let path = path.to_string_lossy();
        let mesh = crate::db::create_mesh(label, &path).unwrap();
        let node = crate::db::create_agent_node(
            mesh.id,
            "worker",
            &path,
            "main",
            crate::models::EnvType::Windows,
            "codex",
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
        )
        .unwrap();
        unique_node_id(node.id)
    }

    #[test]
    fn codex_midsize_paste_rendered_in_full_is_confirmed_by_its_tail() {
        let _db = crate::db::test_support::isolated();
        let id = codex_worker_node("codex-midsize-paste");
        let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
        evaluator::register(id);
        assert!(
            crate::circuit::launch::normalize_for_match(CODEX_MIDSIZE_PROMPT).chars().count() > VISIBLE_PASTE_TEXT_LIMIT,
            "fixture precondition: past the full-text limit, where only a marker used to be accepted"
        );
        let (_, readiness) = stage_prompt_write(&registry, id, CODEX_MIDSIZE_PROMPT, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            writes.recv_timeout(Duration::from_secs(1)).unwrap(),
            injection_payload(CODEX_MIDSIZE_PROMPT).into_bytes()
        );

        let registry_for_wait = Arc::clone(&registry);
        let wait = std::thread::spawn(move || {
            settle_after_paste(
                &registry_for_wait,
                id,
                &readiness.paste,
                None,
                Duration::from_secs(2),
            )
        });
        evaluator::on_output(id, CODEX_MIDSIZE_COMPOSER_FRAME);
        assert!(
            wait.join()
                .unwrap()
                .expect("a prompt Codex rendered in full must be confirmed, not time out"),
            "the staged prompt is ready for Enter"
        );
        assert_eq!(
            writes.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty),
            "confirming the paste must not write anything"
        );
        evaluator::unregister(id);
        registry.kill_session(id);
    }

    #[test]
    fn codex_midsize_paste_is_not_confirmed_until_its_tail_is_drawn() {
        let _db = crate::db::test_support::isolated();
        let id = codex_worker_node("codex-midsize-partial-paste");
        evaluator::register(id);
        let PasteReadiness::RenderedMultiline {
            chars,
            normalized_chars,
            content,
            ..
        } = paste_readiness(id, CODEX_MIDSIZE_PROMPT).unwrap().paste
        else {
            panic!("a multiline Codex prompt must use the rendered paste gate");
        };
        // Read each frame back as production does: output since a cursor, escapes stripped.
        let confirmed_by = |frame: &str| {
            let cursor = evaluator::output_cursor(id).unwrap();
            evaluator::on_output(id, frame);
            rendered_paste_visible(
                &evaluator::cleaned_output_since(id, cursor),
                chars,
                normalized_chars,
                &content,
            )
        };
        assert!(confirmed_by(CODEX_MIDSIZE_COMPOSER_FRAME));
        // Everything but the last row: the head of the draft is drawn, the paste is not complete.
        let partial = CODEX_MIDSIZE_COMPOSER_FRAME
            .split("\x1b[15;1H")
            .next()
            .unwrap();
        assert!(
            !confirmed_by(partial),
            "a head-only redraw must not acknowledge a paste that is still arriving"
        );
        // A different draft with the same opening is not this paste either.
        assert!(!confirmed_by(&CODEX_MIDSIZE_COMPOSER_FRAME.replace(
            "Do not push any commits yourself",
            "Do something else instead"
        )));
        evaluator::unregister(id);
    }

    #[test]
    fn paste_confirmation_policy_is_declared_per_harness() {
        use crate::agent::provider::{adapters, AgentProvider, PasteGatePolicy};
        // Every adapter declares its paste-gate policy, so no per-harness
        // string comparison in delivery.rs can drift out of sync when a new
        // harness is added: extend this table alongside the new adapter.
        // Tail-anchor harnesses draw mid-size pastes inline (issues
        // #2060/#2061); every other harness keeps the default Generic gate.
        let policies: &[(&str, PasteGatePolicy)] = &[
            ("agy", adapters::AGY.paste_gate_policy()),
            ("anthropic", adapters::ANTHROPIC.paste_gate_policy()),
            ("cline", adapters::CLINE.paste_gate_policy()),
            ("codex", adapters::CODEX.paste_gate_policy()),
            ("commandcode", adapters::COMMANDCODE.paste_gate_policy()),
            ("cursor", adapters::CURSOR.paste_gate_policy()),
            ("dsh", adapters::DSH.paste_gate_policy()),
            ("freebuff", adapters::FREEBUFF.paste_gate_policy()),
            ("grok", adapters::GROK.paste_gate_policy()),
            ("kimi", adapters::KIMI.paste_gate_policy()),
            ("mcode", adapters::MCODE.paste_gate_policy()),
            ("muse", adapters::MUSE.paste_gate_policy()),
            ("opencode", adapters::OPENCODE.paste_gate_policy()),
            ("terminal", adapters::TERMINAL.paste_gate_policy()),
        ];
        assert_eq!(policies.len(), 14, "one row per harness adapter");
        for (name, policy) in policies {
            let expected = if *name == "codex" || *name == "muse" {
                PasteGatePolicy::RenderedWithTailAnchor
            } else {
                PasteGatePolicy::Generic
            };
            assert_eq!(*policy, expected, "paste-gate policy for {name}");
        }
    }

    #[test]
    fn a_stale_redraw_with_an_identical_ending_is_a_known_tail_match_limit() {
        // Issue #2061 item 3: the tail proof cannot tell a fresh redraw from
        // a stale one repainting an EARLIER prompt that shares only the
        // ending. A and B below share exactly their last 64 normalized
        // characters and differ everywhere else; staging B while A's redraw
        // lands after the write still satisfies the match. Output-cursor
        // scoping plus the 1 s quiet gate narrow this but do not remove it.
        // Pinned here so a future hardening (a prompt-specific token, or
        // checking the composer region only) visibly flips the final
        // assertion to `assert!(!...)`.
        let _db = crate::db::test_support::isolated();
        let id = codex_worker_node("codex-stale-redraw-limit");
        evaluator::register(id);
        let PasteReadiness::RenderedMultiline {
            chars,
            normalized_chars,
            content,
            ..
        } = paste_readiness(id, CODEX_MIDSIZE_PROMPT).unwrap().paste
        else {
            panic!("a multiline Codex prompt must use the rendered paste gate");
        };
        // The earlier prompt shares B's tail and nothing else: if its redraw
        // also carried B's head, this would re-test the happy path instead
        // of the stale-redraw limit.
        let b_normalized = crate::circuit::launch::normalize_for_match(CODEX_MIDSIZE_PROMPT);
        let b_head: String = b_normalized.chars().take(30).collect();
        let a_normalized =
            crate::circuit::launch::normalize_for_match(CODEX_EARLIER_SAME_ENDING_PROMPT);
        assert!(
            a_normalized.ends_with(&content),
            "fixture precondition: the earlier prompt shares B's tail"
        );
        assert!(
            !a_normalized.contains(&b_head),
            "fixture precondition: the earlier prompt shares ONLY the tail"
        );
        // A's redraw lands after B's write: the matcher sees B's tail in it.
        let cursor = evaluator::output_cursor(id).unwrap();
        evaluator::on_output(id, CODEX_EARLIER_REDRAW_FRAME);
        assert!(
            rendered_paste_visible(
                &evaluator::cleaned_output_since(id, cursor),
                chars,
                normalized_chars,
                &content
            ),
            "known limit: a stale redraw sharing only the ending satisfies the tail match"
        );
        evaluator::unregister(id);
    }

    #[test]
    fn tail_anchor_survives_narrow_terminal_fragmentation() {
        // Issue #2061 item 5: a 22-column ConPTY capture is known to insert
        // padding inside `[Pasted Content N chars]` markers. The tail anchor
        // must survive the same fragmentation when a narrow composer wraps
        // mid-word — `normalize_for_match` drops whitespace, so wrap padding
        // and line breaks inside the tail region cannot hide it.
        let normalized = crate::circuit::launch::normalize_for_match(CODEX_MIDSIZE_PROMPT);
        let proof = visible_paste_proof(PasteGatePolicy::RenderedWithTailAnchor, normalized);
        assert_eq!(proof.chars().count(), TAIL_ANCHOR_CHARS);
        // The split lands inside the tail: the fragmented span is part of the proof.
        let tail_span = crate::circuit::launch::normalize_for_match("any commits yourself");
        assert!(
            proof.contains(&tail_span),
            "fixture precondition: the narrow wrap splits the tail itself"
        );
        let _db = crate::db::test_support::isolated();
        let id = codex_worker_node("codex-narrow-tail");
        evaluator::register(id);
        let PasteReadiness::RenderedMultiline {
            chars,
            normalized_chars,
            content,
            ..
        } = paste_readiness(id, CODEX_MIDSIZE_PROMPT).unwrap().paste
        else {
            panic!("a multiline Codex prompt must use the rendered paste gate");
        };
        assert_eq!(
            content, proof,
            "the gate anchors on the same tail with or without fragmentation"
        );
        let fragmented = CODEX_MIDSIZE_COMPOSER_FRAME
            .replace("any commits yourself.", "any\x1b[K\r\n   commits yourself.");
        let cursor = evaluator::output_cursor(id).unwrap();
        evaluator::on_output(id, &fragmented);
        assert!(
            rendered_paste_visible(
                &evaluator::cleaned_output_since(id, cursor),
                chars,
                normalized_chars,
                &content
            ),
            "ConPTY padding and line breaks inside the tail must not hide it"
        );
        evaluator::unregister(id);
    }

    #[test]
    fn tail_rule_applies_at_any_size_without_a_collapse_cutoff() {
        // Issue #2061 item 4: the exact collapse point is unmeasured (Muse
        // drew 839 chars and collapsed 1,509; chars vs lines vs width is
        // unknown), so the gate does not encode one — marker OR tail confirms
        // at any size. A collapsed draft carries no visible tail, so only its
        // marker can confirm it.
        for size in [839, 1_509, 9_407] {
            let draft = format!("prompt\n{}", "z".repeat(size));
            let normalized = crate::circuit::launch::normalize_for_match(&draft);
            assert!(
                normalized.chars().count() > VISIBLE_PASTE_TEXT_LIMIT,
                "fixture precondition"
            );
            assert_eq!(
                visible_paste_proof(PasteGatePolicy::RenderedWithTailAnchor, normalized),
                "z".repeat(TAIL_ANCHOR_CHARS),
                "no collapse cutoff at size {size}",
            );
        }
        let draft = format!("prompt\n{}", "z".repeat(1_509));
        let normalized = crate::circuit::launch::normalize_for_match(&draft);
        assert_eq!(
            visible_paste_proof(PasteGatePolicy::RenderedMarkerOnly, normalized),
            "",
            "marker-only harnesses keep marker-only at every size",
        );
    }

    #[test]
    fn muse_enter_ignores_redraw_until_the_matching_prompt_is_accepted() {
        let id = -930_026;
        let (registry, writes) = crate::agent::process::testing::capturing_registry(id);
        evaluator::register(id);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let record = |prompt: &str| {
            format!(
                "{}\n",
                serde_json::json!({
                    "payload_type":"runtime.session", "payload":{"kind":"run",
                        "event":{"kind":"started","prompt":prompt}}
                })
            )
        };
        std::fs::write(&path, record("feedback\nfix it")).unwrap();
        let receipt = crate::services::muse_watcher::PromptReceipt::from_log(
            path.clone(),
            "feedback\r\nfix it",
        )
        .unwrap();
        assert!(
            !receipt.accepted(),
            "an earlier matching prompt is not this submission"
        );
        let registry_for_submit = Arc::clone(&registry);
        let submit = std::thread::spawn(move || {
            press_enter_until_output_guarded(
                &registry_for_submit,
                id,
                None,
                Duration::from_millis(300),
                Some(&receipt),
            )
        });
        assert_eq!(writes.recv_timeout(Duration::from_secs(2)).unwrap(), b"\r");
        evaluator::on_output(id, "[Pasted Content 15 chars] redraw");
        // The second Enter proves redraw alone did not satisfy the first one.
        assert_eq!(writes.recv_timeout(Duration::from_secs(2)).unwrap(), b"\r");
        use std::io::Write;
        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        log.write_all(record("feedback\nfix it").as_bytes())
            .unwrap();
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
        let readiness = PromptReadiness {
            paste: PasteReadiness::RenderedMultiline {
                chars: 100,
                normalized_chars: 100,
                content: String::new(),
                output_cursor: evaluator::output_cursor(id).unwrap(),
            },
            receipt: None,
        };
        registry.kill_session(id);
        let started = Instant::now();
        let error = settle_after_paste(
            &registry,
            id,
            &readiness.paste,
            None,
            RENDERED_PASTE_TOTAL_BUDGET,
        )
        .unwrap_err();
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
            !evaluator::is_circuit_piloted(id),
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
            evaluator::is_circuit_piloted(id) && evaluator::millis_since_last_output(id).is_none(),
            "fixture precondition: buffered, but no output to acknowledge against"
        );

        let err =
            press_enter_until_output_guarded(&registry, id, None, Duration::from_millis(50), None)
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
