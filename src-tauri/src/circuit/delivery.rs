//! Prompt delivery with process fencing, paste readiness and Enter acknowledgement.

use super::evaluator;
use crate::agent::process::{AgentProcessRegistry, InputDisposition, InputWriteError};
use crate::agent::provider::{AgentProvider, PasteGatePolicy};
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
/// declares tail-anchor support they require the harness's paste marker.
pub(crate) const VISIBLE_PASTE_TEXT_LIMIT: usize = 256;
/// A harness that draws a mid-size paste in full collapses only the largest
/// into a marker (Muse probed in a real PTY: 839 chars drawn, 1,509
/// collapsed; Codex showed a ~600-char inline paste in a partial 0.160.0
/// frame — issue #2061), so a draft past [`VISIBLE_PASTE_TEXT_LIMIT`] is
/// confirmed by its last 64 normalized characters instead. Run 343 waited
/// out the whole budget for a marker Muse never printed. Counted after
/// `normalize_for_match`, not in display columns.
///
/// Shared with [`crate::circuit::launch`]: a composer that scrolls keeps the
/// tail of the staged text in view, so this span is how a long draft is
/// confirmed. The two paths share the cutoff and the width, not the policy —
/// this constant applies only to an adapter that declares a rendered gate
/// taking a tail anchor (`Generic` collects no paste evidence at all and
/// submits on timing), while the prefill path has no adapter-declared policy
/// and always applies it.
pub(crate) const TAIL_ANCHOR_CHARS: usize = 64;
/// Bound ambiguous reconstruction per output snapshot. Exhaustion leaves the
/// paste unconfirmed; it never authorizes Enter or extends the readiness budget.
const MAX_PASTE_MATCH_STATES: usize = 16_384;

/// After an Enter keystroke, PTY output must appear within this window for
/// the submit to count as acknowledged.
const ENTER_ACK_WINDOW: Duration = Duration::from_secs(6);

/// Enter keystrokes attempted before the watcher gives up and surfaces the
/// node for human attention.
const MAX_ENTER_ATTEMPTS: u32 = 3;

/// True when the prompt contains newlines that require bracketed-paste staging.
fn is_multiline_prompt(text: &str) -> bool {
    text.contains('\n') || text.contains('\r')
}

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
    if is_multiline_prompt(text) {
        format!("\x1b[200~{}\x1b[201~", text)
    } else {
        text.to_string()
    }
}

/// The prompt text with the line endings the target harness's composer needs
/// inside a bracketed paste (`AgentProvider::paste_requires_cr_newlines`).
///
/// Windows ConPTY delivers paste bytes as key events: LF is dropped, so lines
/// glue together, and CRLF submits the first line and queues the rest. A
/// terminal selection reaches here as CRLF (xterm.js joins with `\r\n` on
/// Windows), so a Grok handover used to arrive as one message per line. CR
/// is the ending xterm.js itself emits for a manual paste.
/// This transformation is idempotent: repeating it on already converted text
/// produces the identical string without further changes.
pub(crate) fn paste_text_for(adapter: &dyn AgentProvider, text: &str) -> String {
    if adapter.paste_requires_cr_newlines() {
        text.replace("\r\n", "\r").replace('\n', "\r")
    } else {
        text.to_string()
    }
}

/// A prompt whose line endings and payload formatting have been resolved for
/// its target harness (`AgentProvider::paste_requires_cr_newlines`).
///
/// Constructing a `PreparedPrompt` is the single owner of newline conversion.
/// For Generic-policy harnesses, this guarantees that the text hashed for the
/// evidence ledger (`submission_digest`) and the bytes staged into the PTY
/// (`injection_payload`) operate on the exact same transformed text. Specialized
/// harnesses with custom composer sanitation (such as Codex's split-marker handling)
/// additionally transform the staged body by stripping terminal CSI escape
/// sequences and dropping non-tab/non-newline control characters; their line
/// endings stay whatever [`PreparedPrompt::prepare`] resolved, because the
/// composer's own paste count is newline-normalized and so does not observe the
/// ending that ConPTY needs (issue #2199).
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PreparedPrompt {
    node_id: i64,
    text: String,
}

impl std::fmt::Debug for PreparedPrompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedPrompt")
            .field("node_id", &self.node_id)
            .field(
                "text",
                &format_args!("<redacted: {} chars>", self.text.chars().count()),
            )
            .finish()
    }
}

impl PreparedPrompt {
    /// Prepare prompt text for delivery to `node_id`. Resolves the target node's
    /// harness provider from the database to normalize newlines (converting LF / CRLF
    /// to CR for harnesses like Grok).
    ///
    /// If the target is circuit-piloted, database lookup failure fails loudly;
    /// otherwise, for any non-circuit node whose lookup fails (whether an unpersisted
    /// standalone node or a transient database read error), a warning is logged and
    /// the text is kept as given to preserve delivery best-effort.
    pub(crate) fn prepare(node_id: i64, text: &str) -> Result<Self, String> {
        let text = match crate::db::get_agent_node_by_id(node_id) {
            Ok(node) => {
                let resolved = crate::preferences::resolve_harness_provider(&node.provider);
                paste_text_for(resolved.adapter(), text)
            }
            Err(error) => {
                if evaluator::is_circuit_piloted(node_id) {
                    return Err(format!(
                        "could not identify prompt target {node_id}: {error}"
                    ));
                }
                // Best effort for unpersisted non-circuit nodes (e.g. terminal handovers
                // to nodes running outside database supervision, or on transient DB read error):
                // keep the raw text as given rather than dropping the delivery entirely.
                tracing::warn!(
                    "could not identify prompt target {node_id} for newline normalization: {error}"
                );
                text.to_string()
            }
        };
        Ok(Self { node_id, text })
    }

    #[cfg(test)]
    pub(crate) fn from_raw(node_id: i64, text: impl Into<String>) -> Self {
        Self {
            node_id,
            text: text.into(),
        }
    }

    pub(crate) fn node_id(&self) -> i64 {
        self.node_id
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
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
    let prepared = PreparedPrompt::prepare(node_id, text)?;
    write_prompt_to_pty_guarded(
        &crate::agent::process::PROCESS_REGISTRY,
        &prepared,
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
    prompt: &PreparedPrompt,
    app: &AppHandle,
    expected_input: Option<&str>,
) -> Result<bool, String> {
    let node_id = prompt.node_id();
    let Some((guarded, readiness)) = stage_prompt_write(registry, prompt, expected_input)? else {
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
    prompt: &PreparedPrompt,
    expected_input: Option<&str>,
) -> Result<Option<(Option<String>, PromptReadiness)>, String> {
    let node_id = prompt.node_id();
    ensure_prompt_target_alive(registry, node_id)?;
    let mut readiness = paste_readiness(node_id, prompt.text())?;
    let payload = match &mut readiness.paste {
        PasteReadiness::RenderedMultiline {
            split_marker_prompt: Some(rendered),
            ..
        } => {
            // Codex sanitizes each burst separately. Transform the body before
            // the PTY write so a split CSI cannot change what the proof counts.
            // The proof itself (CountedPasteText) is newline-normalized, so the
            // CR endings ConPTY requires leave the marker count untouched.
            injection_payload(&std::mem::take(&mut rendered.payload))
        }
        _ => injection_payload(prompt.text()),
    };
    let guarded = if let Some(expected) = expected_input {
        // `Ok(None)` is only ever "the guard was lost" — the draft now belongs
        // to someone else. Backpressure arrives as an `Err` and is retried by
        // `retry_backpressured` instead, so a momentarily full queue can never
        // be misread as lost draft ownership and silently discard the prompt
        // (issue #1530).
        let Some(next) = retry_backpressured(|| {
            registry.write_bytes_if_current(node_id, payload.as_bytes(), expected)
        })?
        else {
            return Ok(None);
        };
        Some(next)
    } else {
        retry_backpressured(|| {
            match registry.write_bytes(node_id, payload.as_bytes()) {
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
        split_marker_prompt: Option<RenderedPastePrompt>,
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
    if is_multiline_prompt(text) && !matches!(policy, PasteGatePolicy::Generic) {
        let split_marker_prompt = (policy == PasteGatePolicy::RenderedWithSplitMarker)
            .then(|| RenderedPastePrompt::new(text));
        let (chars, normalized_chars, content) = match &split_marker_prompt {
            Some(prompt) => {
                let chars = prompt.text.offsets.len() - 1;
                (chars, chars, prompt.text.normalized.clone())
            }
            None => (
                text.chars().count(),
                text.replace("\r\n", "\n").chars().count(),
                crate::circuit::launch::normalize_for_match(text),
            ),
        };
        readiness.paste = PasteReadiness::RenderedMultiline {
            chars,
            normalized_chars,
            content: visible_paste_proof(policy, content),
            split_marker_prompt,
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
    if !matches!(
        policy,
        PasteGatePolicy::RenderedWithTailAnchor | PasteGatePolicy::RenderedWithSplitMarker
    ) {
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

#[derive(Debug)]
struct RenderedPastePrompt {
    text: CountedPasteText,
    /// The sanitized body staged into the PTY, carrying the line endings the
    /// harness's composer needs. Drained by [`stage_prompt_write`].
    payload: String,
}

/// Strip terminal control text Codex's `sanitize_user_text` removes, keeping
/// the line endings the caller supplied.
///
/// Codex sanitizes each paste burst independently, so this runs before the
/// bracketed-paste markers are added: a CSI cut at a burst boundary cannot then
/// leave printable fragments behind. Newlines and tabs survive, as in the
/// composer.
fn codex_sanitized_text(prompt: &str) -> String {
    let mut chars = prompt.chars().peekable();
    let mut sanitized = String::with_capacity(prompt.len());
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.next_if_eq(&'[').is_some() {
            // An unfinished CSI consumes the rest, matching the composer.
            let _ = chars.by_ref().find(|ch| ('@'..='~').contains(ch));
        } else if matches!(ch, '\n' | '\r' | '\t') || !ch.is_control() {
            sanitized.push(ch);
        }
    }
    sanitized
}

/// The text Codex's composer holds and counts: CRLF and bare CR normalized to
/// LF, then sanitized, exactly as `apply_paste` does before counting its
/// `[Pasted Content N chars]` marker (Codex 0.160.1 / 0.162.1).
///
/// This is the *proof*, not the payload. Because the composer collapses every
/// line ending to one LF before counting, the count is identical whether the
/// PTY delivered CR, CRLF or LF — which is why the staged body can carry the
/// CR endings ConPTY needs without moving this number (issue #2199).
fn codex_composer_text(prompt: &str) -> String {
    codex_sanitized_text(prompt)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

#[derive(Debug)]
struct CountedPasteText {
    normalized: String,
    // Map original Unicode-character boundaries to normalized byte offsets.
    // Ignored whitespace/punctuation may give several boundaries one offset.
    offsets: Vec<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct RenderedPasteMarker {
    chars: usize,
    ordinal: usize,
}

impl RenderedPasteMarker {
    fn interpretations(self, after: &str) -> Vec<(Self, &str)> {
        let mut interpretations = vec![(self, after)];
        // '#42' may be actual inline prompt text, so defer interpreting an
        // external ordinal until the expected text resolves the ambiguity.
        if self.ordinal == 1 && after.starts_with('#') {
            let digits = after[1..].bytes().take_while(u8::is_ascii_digit).count();
            for boundary in 1..=digits {
                match after[1..1 + boundary].parse::<usize>() {
                    Ok(ordinal) if ordinal >= 2 => {
                        interpretations.push((Self { ordinal, ..self }, &after[1 + boundary..]));
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        }
        interpretations
    }
}

struct PasteMatchState {
    index: usize,
    start: usize,
    used: Vec<RenderedPasteMarker>,
}

struct PasteMatchBudget {
    deadline: Instant,
    remaining_states: usize,
}

impl PasteMatchBudget {
    fn new(deadline: Instant) -> Self {
        Self {
            deadline,
            remaining_states: MAX_PASTE_MATCH_STATES,
        }
    }

    fn within_deadline(&self) -> bool {
        Instant::now() < self.deadline
    }

    fn queue_state(&mut self) -> bool {
        if self.remaining_states == 0 || !self.within_deadline() {
            return false;
        }
        self.remaining_states -= 1;
        true
    }
}

impl CountedPasteText {
    fn new(text: &str) -> Self {
        let mut normalized = String::new();
        let mut offsets = vec![0];
        let mut buffer = [0; 4];
        for ch in text.chars() {
            normalized.push_str(&crate::circuit::launch::normalize_for_match(
                ch.encode_utf8(&mut buffer),
            ));
            offsets.push(normalized.len());
        }
        Self {
            normalized,
            offsets,
        }
    }

    fn matches_segments(
        &self,
        markers: &[RenderedPasteMarker],
        inline: &[String],
        budget: &mut PasteMatchBudget,
    ) -> bool {
        let total = self.offsets.len() - 1;
        for last in (0..markers.len()).rev() {
            // A burst may be followed by more inline text. Ignore subsequent
            // redraw chrome only after the expected ending has been matched.
            let mut states = Vec::new();
            let mut seen = std::collections::HashMap::new();
            for (marker, after) in markers[last].interpretations(&inline[last + 1]) {
                for end in 0..=total {
                    if !budget.within_deadline() {
                        return false;
                    }
                    let tail = &self.normalized[self.offsets[end]..];
                    if (end != total && tail.is_empty()) || !after.starts_with(tail) {
                        continue;
                    }
                    if let Some(start) = end.checked_sub(marker.chars) {
                        if !budget.queue_state() {
                            return false;
                        }
                        states.push(PasteMatchState {
                            index: last,
                            start,
                            used: if markers[..last]
                                .iter()
                                .any(|earlier| earlier.chars == marker.chars)
                            {
                                vec![marker]
                            } else {
                                Vec::new()
                            },
                        });
                    }
                }
            }
            while let Some(PasteMatchState { index, start, used }) = states.pop() {
                if !budget.within_deadline() {
                    return false;
                }
                // At one normalized offset, a larger raw boundary admits all
                // earlier boundaries available to a smaller one. Keep only
                // that dominating state; retain zero separately for full count.
                let key = (index, self.offsets[start], start == 0, used.clone());
                if seen.get(&key).is_some_and(|&largest| largest >= start) {
                    continue;
                }
                seen.insert(key, start);
                let prefix = &self.normalized[..self.offsets[start]];
                if start == 0 || (!prefix.is_empty() && inline[index].ends_with(prefix)) {
                    return true;
                }
                if index == 0 {
                    continue;
                }
                for (marker, between) in markers[index - 1].interpretations(&inline[index]) {
                    // Fresh output can redraw one placeholder several times.
                    // Each count/ordinal identifies one burst, not more text.
                    if used
                        .iter()
                        .any(|later| later.chars == marker.chars && later.ordinal <= marker.ordinal)
                        || !prefix.ends_with(between)
                    {
                        continue;
                    }
                    // The text between adjacent markers must be the expected
                    // segment. Explore only boundaries differing in characters
                    // that normalization removes, never arbitrary count slack.
                    let offset = prefix.len() - between.len();
                    let boundaries = &self.offsets[..=start];
                    let first = boundaries.partition_point(|&value| value < offset);
                    let after = boundaries.partition_point(|&value| value <= offset);
                    let mut starts = Vec::new();
                    for end in first..after {
                        if !budget.within_deadline() {
                            return false;
                        }
                        if let Some(start) = end.checked_sub(marker.chars) {
                            if starts.last().is_some_and(|&previous| {
                                previous != 0 && self.offsets[previous] == self.offsets[start]
                            }) {
                                *starts.last_mut().unwrap() = start;
                            } else {
                                starts.push(start);
                            }
                        }
                    }
                    for start in starts {
                        if !budget.queue_state() {
                            return false;
                        }
                        // Only the smallest later ordinal matters, and only
                        // for counts that can still recur earlier. Otherwise
                        // numeric-label alternatives create useless histories.
                        let earlier = &markers[..index - 1];
                        let mut used = used.clone();
                        used.retain(|later| {
                            later.chars != marker.chars
                                && earlier.iter().any(|previous| previous.chars == later.chars)
                        });
                        if earlier
                            .iter()
                            .any(|previous| previous.chars == marker.chars)
                        {
                            used.push(marker);
                        }
                        used.sort_unstable_by_key(|marker| marker.chars);
                        states.push(PasteMatchState {
                            index: index - 1,
                            start,
                            used,
                        });
                    }
                }
            }
        }
        false
    }
}

impl RenderedPastePrompt {
    /// `prompt` is the already-prepared text: [`PreparedPrompt::prepare`] has
    /// resolved its line endings from the adapter's paste policy, so Codex's
    /// staged body carries CR inside the bracketed paste.
    fn new(prompt: &str) -> Self {
        let composer_text = codex_composer_text(prompt);
        Self {
            text: CountedPasteText::new(&composer_text),
            payload: codex_sanitized_text(prompt),
        }
    }

    fn visible(&self, output: &str, deadline: Instant) -> bool {
        self.visible_with_budget(output, &mut PasteMatchBudget::new(deadline))
    }

    fn visible_with_budget(&self, output: &str, budget: &mut PasteMatchBudget) -> bool {
        static MARKER: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
            // Codex puts duplicate-size ordinals after the closing bracket.
            regex::Regex::new(r"\[PastedContent([0-9]+)chars\]").unwrap()
        });
        let compact: String = output.chars().filter(|c| !c.is_whitespace()).collect();
        let mut markers = Vec::new();
        let mut inline = Vec::new();
        let mut previous_end = 0;
        for marker in MARKER.captures_iter(&compact) {
            let Ok(chars) = marker[1].parse::<usize>() else {
                return false;
            };
            if chars == 0 {
                return false;
            }
            let range = marker.get(0).unwrap().range();
            inline.push(crate::circuit::launch::normalize_for_match(
                &compact[previous_end..range.start],
            ));
            markers.push(RenderedPasteMarker { chars, ordinal: 1 });
            previous_end = range.end;
        }
        if markers.is_empty() {
            return false;
        }
        inline.push(crate::circuit::launch::normalize_for_match(
            &compact[previous_end..],
        ));
        self.text.matches_segments(&markers, &inline, budget)
    }
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
        split_marker_prompt,
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
                let output = evaluator::cleaned_output_since(node_id, *output_cursor);
                paste_seen = rendered_paste_visible(&output, *chars, *normalized_chars, content)
                    || split_marker_prompt
                        .as_ref()
                        .is_some_and(|prompt| prompt.visible(&output, deadline));
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
#[path = "delivery_tests.rs"]
mod tests;
