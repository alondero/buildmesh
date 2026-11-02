//! Automated Agent Node prompts: readiness, owned paste and bounded submission.
//! Callers choose evidence explicitly; Circuit progression stays with its worker.

use crate::agent::process::{AgentProcessRegistry, InputWriteError};
use crate::agent::provider::{AgentProvider, PasteGatePolicy};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod observation;
pub(crate) use observation::OutputObservation;

/// Callers choose observation explicitly; delivery does not infer Circuit ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObservationRequirement {
    /// Ordinary prompts keep a timed paste gap and exactly one unobserved Enter.
    Unobserved,
    /// Adapter paste evidence, then a session receipt or fresh PTY acknowledgement.
    Required,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeliveryMode {
    LaterTurn,
    OrdinaryStartup,
    VerifiedStartup,
    Prefilled,
    AutoSubmitted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeliveryProgress {
    NotStaged,
    TextStaged,
    EnterSent {
        attempts: u32,
    },
    /// Fresh PTY output followed Enter; a redraw can satisfy this evidence.
    EnterAcknowledged {
        attempts: u32,
    },
    /// A matching native session receipt establishes harness acceptance.
    AcceptanceObserved {
        attempts: u32,
    },
    /// The CLI launch owns submission; delivery sends no extra Enter.
    AutoSubmitted,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DeliveryStop {
    Superseded,
    ProcessExited,
    Unverified(String),
    TransportFailure(InputWriteError),
}

impl std::fmt::Display for DeliveryStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Superseded => f.write_str("Terminal input or process ownership changed; inspect the existing draft before retrying"),
            Self::ProcessExited => f.write_str("The agent process exited before delivery finished"),
            Self::Unverified(reason) => f.write_str(reason),
            Self::TransportFailure(error) => write!(f, "Could not write to the agent terminal: {error}"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DeliveryOutcome {
    pub progress: DeliveryProgress,
    pub stop: Option<DeliveryStop>,
}

impl DeliveryOutcome {
    /// Require Circuit acknowledgement while distinguishing
    /// PTY activity from native acceptance in the returned progress.
    pub(crate) fn require_acknowledgement(self) -> Result<(), String> {
        let reason = match (&self.progress, &self.stop) {
            (
                DeliveryProgress::AcceptanceObserved { .. }
                | DeliveryProgress::EnterAcknowledged { .. },
                None,
            ) => return Ok(()),
            (_, Some(stop)) => stop.to_string(),
            (DeliveryProgress::NotStaged, None) => "The prompt was not staged".into(),
            (DeliveryProgress::TextStaged, None) => {
                "The prompt is staged, but Enter was not sent".into()
            }
            (DeliveryProgress::EnterSent { .. }, None) => {
                "Enter was sent, but the harness did not acknowledge it".into()
            }
            (DeliveryProgress::AutoSubmitted, None) => {
                "The launch submitted the prompt without observed acknowledgement".into()
            }
        };
        Err(format!("Prompt delivery remained unverified: {reason}"))
    }
}

pub(crate) struct DeliveryRequest<'a> {
    pub prompt: &'a PreparedPrompt,
    pub mode: DeliveryMode,
    pub observation: ObservationRequirement,
    /// The process generation and input version captured by the caller.
    pub expected_input: Option<&'a str>,
    /// Durable caller ownership (e.g. an attempt fence). Errors never authorize input.
    pub still_current: &'a dyn Fn() -> Result<bool, String>,
}

trait DeliveryIo {
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
    fn alive(&self) -> bool;
    fn input_is_current(&self, stamp: &str) -> bool;
    fn write(&self, bytes: &[u8], stamp: &str) -> Result<Option<String>, InputWriteError>;
    fn activity(&self) -> observation::Activity;
    fn output(&self, since: u64) -> observation::Snapshot;
    fn receipt_accepted(&self) -> Option<bool>;
    fn prepare_receipt(&self, _prompt: &PreparedPrompt) -> Result<(), String> {
        Ok(())
    }
}

struct LiveDelivery<'a> {
    registry: &'a AgentProcessRegistry,
    node_id: i64,
    output: Arc<OutputObservation>,
    adapter: &'a dyn AgentProvider,
    capture_receipt: bool,
    receipt: std::cell::RefCell<Option<Box<dyn crate::agent::provider::PromptAcceptanceReceipt>>>,
}

impl DeliveryIo for LiveDelivery<'_> {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
    fn alive(&self) -> bool {
        self.registry.is_alive(&self.node_id)
    }
    fn input_is_current(&self, stamp: &str) -> bool {
        self.registry.input_is_current(self.node_id, stamp)
    }
    fn write(&self, bytes: &[u8], stamp: &str) -> Result<Option<String>, InputWriteError> {
        self.registry
            .write_bytes_if_current(self.node_id, bytes, stamp)
    }
    fn activity(&self) -> observation::Activity {
        self.output.activity(self.now())
    }
    fn output(&self, since: u64) -> observation::Snapshot {
        self.output.snapshot(since, self.now())
    }
    fn receipt_accepted(&self) -> Option<bool> {
        self.receipt
            .borrow()
            .as_ref()
            .map(|receipt| receipt.accepted())
    }
    fn prepare_receipt(&self, prompt: &PreparedPrompt) -> Result<(), String> {
        if self.capture_receipt {
            *self.receipt.borrow_mut() = self
                .adapter
                .prompt_acceptance_receipt(self.node_id, prompt.text())?;
        }
        Ok(())
    }
}

/// Execute one automated delivery. Blocking callers run this on a worker thread.
/// The process registry remains the sole input transport.
pub(crate) fn deliver(
    registry: &AgentProcessRegistry,
    adapter: &dyn AgentProvider,
    request: DeliveryRequest<'_>,
) -> DeliveryOutcome {
    let progress = DeliveryProgress::NotStaged;
    let node_id = request.prompt.node_id();
    let Some(process) = registry.get(&node_id) else {
        return DeliveryOutcome {
            progress,
            stop: Some(DeliveryStop::ProcessExited),
        };
    };
    deliver_to_process(registry, &process, adapter, request)
}

fn deliver_to_process(
    registry: &AgentProcessRegistry,
    process: &crate::agent::process::AgentProcess,
    adapter: &dyn AgentProvider,
    request: DeliveryRequest<'_>,
) -> DeliveryOutcome {
    let progress = DeliveryProgress::NotStaged;
    let node_id = request.prompt.node_id();
    let stamp = match request
        .expected_input
        .map(str::to_owned)
        .map(Ok)
        .unwrap_or_else(|| process.input_stamp_result().map(|stamp| stamp.encode()))
    {
        Ok(stamp) => stamp,
        Err(_) => {
            return DeliveryOutcome {
                progress,
                stop: Some(DeliveryStop::Superseded),
            }
        }
    };
    let io = LiveDelivery {
        registry,
        node_id,
        output: Arc::clone(&process.delivery_output),
        adapter,
        capture_receipt: request.observation == ObservationRequirement::Required
            && !matches!(
                request.mode,
                DeliveryMode::Prefilled | DeliveryMode::AutoSubmitted
            ),
        receipt: Default::default(),
    };
    execute(&io, adapter, &request, stamp)
}

fn check_owner(
    io: &impl DeliveryIo,
    request: &DeliveryRequest<'_>,
    stamp: &str,
) -> Result<(), DeliveryStop> {
    if !io.alive() {
        return Err(DeliveryStop::ProcessExited);
    }
    if !io.input_is_current(stamp) {
        return Err(DeliveryStop::Superseded);
    }
    match (request.still_current)() {
        Ok(true) => Ok(()),
        Ok(false) => Err(DeliveryStop::Superseded),
        Err(error) => Err(DeliveryStop::Unverified(error)),
    }
}

/// A refused queue write has not staged anything. Only that refusal is retried;
/// the operation never re-pastes an uncertain draft.
fn write_owned(
    io: &impl DeliveryIo,
    request: &DeliveryRequest<'_>,
    stamp: &mut String,
    bytes: &[u8],
) -> Result<(), DeliveryStop> {
    for attempt in 0..5 {
        check_owner(io, request, stamp)?;
        match io.write(bytes, stamp) {
            Ok(Some(next)) => {
                *stamp = next;
                return Ok(());
            }
            Ok(None) => return Err(DeliveryStop::Superseded),
            Err(InputWriteError::Backpressured) if attempt < 4 => {
                io.sleep(Duration::from_millis(40 * (attempt + 1)))
            }
            Err(error) => return Err(DeliveryStop::TransportFailure(error)),
        }
    }
    unreachable!("the final write attempt returns its outcome")
}

fn wait_ready(
    io: &impl DeliveryIo,
    adapter: &dyn AgentProvider,
    request: &DeliveryRequest<'_>,
    stamp: &str,
) -> Result<(), DeliveryStop> {
    let (budget, cadence) = match request.mode {
        DeliveryMode::OrdinaryStartup => (Duration::from_secs(45), SUBMIT_POLL),
        DeliveryMode::VerifiedStartup | DeliveryMode::Prefilled => {
            (Duration::from_secs(300), Duration::from_millis(500))
        }
        _ => return check_owner(io, request, stamp),
    };
    let deadline = io.now() + budget;
    let marker = marker_hint_for_prefill(request.prompt.text());
    loop {
        check_owner(io, request, stamp)?;
        let output = io.output(0);
        let ready = match request.mode {
            DeliveryMode::OrdinaryStartup => {
                output
                    .activity
                    .quiet
                    .is_some_and(|quiet| quiet.as_millis() >= MIN_QUIET_MS)
                    && adapter.ready_for_initial_prompt(&output.text)
            }
            DeliveryMode::VerifiedStartup => adapter.ready_for_initial_prompt(&output.text),
            DeliveryMode::Prefilled => output.activity.quiet.is_some_and(|quiet| {
                ready_to_submit(
                    &normalize_for_match(&output.text),
                    &marker,
                    quiet.as_millis(),
                )
            }),
            _ => unreachable!(),
        };
        if ready || (io.now() >= deadline && request.mode == DeliveryMode::OrdinaryStartup) {
            return Ok(());
        }
        if io.now() >= deadline {
            return Err(DeliveryStop::Unverified(
                "Harness startup did not become ready for the initial prompt; inspect the terminal"
                    .into(),
            ));
        }
        io.sleep(cadence.min(deadline.saturating_duration_since(io.now())));
    }
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

fn paste_readiness(
    adapter: &dyn AgentProvider,
    text: &str,
    observation: ObservationRequirement,
    output_cursor: u64,
) -> PasteReadiness {
    let policy = adapter.paste_gate_policy();
    if observation == ObservationRequirement::Unobserved
        || !is_multiline_prompt(text)
        || policy == PasteGatePolicy::Generic
    {
        return PasteReadiness::Generic;
    }
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
            normalize_for_match(text),
        ),
    };
    PasteReadiness::RenderedMultiline {
        chars,
        normalized_chars,
        content: visible_paste_proof(policy, content),
        split_marker_prompt,
        output_cursor,
    }
}

fn settle_after_paste(
    io: &impl DeliveryIo,
    request: &DeliveryRequest<'_>,
    stamp: &str,
    readiness: &PasteReadiness,
    rendered_budget: Duration,
) -> Result<(), DeliveryStop> {
    let started = io.now();
    if let PasteReadiness::RenderedMultiline {
        chars,
        normalized_chars,
        content,
        split_marker_prompt,
        output_cursor,
    } = readiness
    {
        let deadline = started + rendered_budget;
        let mut seen = false;
        loop {
            check_owner(io, request, stamp)?;
            let quiet = if seen {
                io.activity().quiet
            } else {
                let output = io.output(*output_cursor);
                seen = rendered_paste_visible(&output.text, *chars, *normalized_chars, content)
                    || split_marker_prompt.as_ref().is_some_and(|prompt| {
                        let now = || io.now();
                        prompt.visible_with_budget(
                            &output.text,
                            &mut PasteMatchBudget::with_clock(deadline, &now),
                        )
                    });
                output.activity.quiet
            };
            if seen && quiet.is_some_and(|quiet| quiet.as_millis() >= PASTE_SETTLE_QUIET_MS) {
                return Ok(());
            }
            if io.now() >= deadline {
                return Err(DeliveryStop::Unverified(format!("The harness did not confirm the {chars}-character pasted prompt before Enter (readiness budget {rendered_budget:?})")));
            }
            io.sleep(SUBMIT_POLL.min(deadline.saturating_duration_since(io.now())));
        }
    }
    if request.observation == ObservationRequirement::Unobserved {
        io.sleep(Duration::from_millis(PASTE_SETTLE_QUIET_MS as u64));
        return check_owner(io, request, stamp);
    }
    while io.now() < started + PASTE_ECHO_DEADLINE {
        check_owner(io, request, stamp)?;
        if output_seen_within(
            io.activity().quiet.map(|quiet| quiet.as_millis()),
            io.now().saturating_duration_since(started).as_millis(),
        ) {
            break;
        }
        io.sleep(SUBMIT_POLL);
    }
    let deadline = io.now() + PASTE_SETTLE_DEADLINE;
    while io.now() < deadline {
        check_owner(io, request, stamp)?;
        if io
            .activity()
            .quiet
            .is_none_or(|quiet| quiet.as_millis() >= PASTE_SETTLE_QUIET_MS)
        {
            return Ok(());
        }
        io.sleep(SUBMIT_POLL);
    }
    Ok(())
}

fn submit(
    io: &impl DeliveryIo,
    request: &DeliveryRequest<'_>,
    stamp: &mut String,
    progress: &mut DeliveryProgress,
    ack_window: Duration,
) -> Result<(), DeliveryStop> {
    for attempt in 1..=MAX_ENTER_ATTEMPTS {
        check_owner(io, request, stamp)?;
        if attempt > 1 && io.receipt_accepted() == Some(true) {
            *progress = DeliveryProgress::AcceptanceObserved {
                attempts: attempt - 1,
            };
            return Ok(());
        }
        let cursor = io.activity().cursor;
        write_owned(io, request, stamp, b"\r")?;
        *progress = DeliveryProgress::EnterSent { attempts: attempt };
        if request.observation == ObservationRequirement::Unobserved {
            return Ok(());
        }
        let deadline = io.now() + ack_window;
        loop {
            check_owner(io, request, stamp)?;
            let receipt = io.receipt_accepted();
            let accepted = receipt.unwrap_or_else(|| io.activity().cursor > cursor);
            if accepted {
                *progress = if receipt == Some(true) {
                    DeliveryProgress::AcceptanceObserved { attempts: attempt }
                } else {
                    DeliveryProgress::EnterAcknowledged { attempts: attempt }
                };
                return Ok(());
            }
            if io.now() >= deadline {
                break;
            }
            io.sleep(SUBMIT_POLL.min(deadline.saturating_duration_since(io.now())));
        }
    }
    Err(DeliveryStop::Unverified("No acknowledgement followed the bounded Enter attempts; inspect the existing draft before retrying".into()))
}

fn execute(
    io: &impl DeliveryIo,
    adapter: &dyn AgentProvider,
    request: &DeliveryRequest<'_>,
    mut stamp: String,
) -> DeliveryOutcome {
    let mut progress = DeliveryProgress::NotStaged;
    let result = (|| {
        wait_ready(io, adapter, request, &stamp)?;
        if request.mode == DeliveryMode::AutoSubmitted {
            progress = DeliveryProgress::AutoSubmitted;
            return Ok(());
        }
        if request.mode != DeliveryMode::Prefilled {
            // Capture after boot has established the native session identity,
            // before writing this prompt (an older matching receipt is stale).
            io.prepare_receipt(request.prompt)
                .map_err(DeliveryStop::Unverified)?;
            let readiness = paste_readiness(
                adapter,
                request.prompt.text(),
                request.observation,
                io.activity().cursor,
            );
            let body = match &readiness {
                PasteReadiness::RenderedMultiline {
                    split_marker_prompt: Some(prompt),
                    ..
                } => prompt.payload.as_str(),
                _ => request.prompt.text(),
            };
            write_owned(io, request, &mut stamp, injection_payload(body).as_bytes())?;
            progress = DeliveryProgress::TextStaged;
            settle_after_paste(io, request, &stamp, &readiness, RENDERED_PASTE_TOTAL_BUDGET)?;
        } else {
            progress = DeliveryProgress::TextStaged;
        }
        submit(io, request, &mut stamp, &mut progress, ENTER_ACK_WINDOW)
    })();
    DeliveryOutcome {
        progress,
        stop: result.err(),
    }
}

fn output_seen_within(ms_since_output: Option<u128>, ms_since_mark: u128) -> bool {
    matches!(ms_since_output, Some(m) if m < ms_since_mark)
}

const SUBMIT_POLL: Duration = Duration::from_millis(250);

/// How long to wait for the pasted prompt to echo back in PTY output before
/// concluding this provider renders no echo and moving on.
const PASTE_ECHO_DEADLINE: Duration = Duration::from_secs(3);

/// The TUI redraw after a paste must have been quiet this long before Enter
/// is sent (mirrors `MIN_QUIET_MS`'s reasoning at a smaller scale —
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
/// Shared with [`crate::agent::prompt_delivery`]: a composer that scrolls keeps the
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

const MIN_QUIET_MS: u128 = 1_500;
const MAX_MARKER_CHARS_NORMALIZED: usize = 30;
const MIN_MARKER_CHARS_DISTINCTIVE: usize = 10;

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
    /// Lookup errors remain explicit. A caller with a resolved adapter can use
    /// `for_adapter` without consulting the database.
    pub(crate) fn prepare(node_id: i64, text: &str) -> Result<Self, String> {
        let node = crate::db::get_agent_node_by_id(node_id)
            .map_err(|error| format!("could not identify prompt target {node_id}: {error}"))?;
        let resolved = crate::preferences::resolve_harness_provider(&node.provider);
        Ok(Self::for_adapter(node_id, resolved.adapter(), text))
    }

    pub(crate) fn for_adapter(node_id: i64, adapter: &dyn AgentProvider, text: &str) -> Self {
        Self {
            node_id,
            text: paste_text_for(adapter, text),
        }
    }

    pub(crate) fn node_id(&self) -> i64 {
        self.node_id
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }
}

/// Collapse a string to just its word characters (plus `#`) so a marker can
/// be found inside TUI output where the input box wraps text at arbitrary
/// columns and pads lines with box-drawing characters. Case-insensitive.
fn normalize_for_match(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || *c == '#')
        .flat_map(char::to_lowercase)
        .collect()
}

/// Confirm the distinctive span the composer keeps in view: the head of a
/// short prefill or the tail of a draft long enough to scroll. Very short
/// markers are rejected because harness banners can contain the same words.
fn marker_hint_for_prefill(prefill: &str) -> String {
    let normalized = normalize_for_match(prefill);
    let chars = normalized.chars().count();
    if chars < MIN_MARKER_CHARS_DISTINCTIVE {
        return String::new();
    }
    if chars <= VISIBLE_PASTE_TEXT_LIMIT {
        return normalized
            .chars()
            .take(MAX_MARKER_CHARS_NORMALIZED)
            .collect();
    }
    let skip = chars - TAIL_ANCHOR_CHARS;
    normalized.chars().skip(skip).collect()
}

/// A staged prefill needs its nonempty marker and a quiet input box. A quiet
/// workspace-trust dialog alone must not authorize Enter.
fn ready_to_submit(normalized_tail: &str, normalized_marker: &str, quiet_ms: u128) -> bool {
    !normalized_marker.is_empty()
        && quiet_ms >= MIN_QUIET_MS
        && normalized_tail.contains(normalized_marker)
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
        || (!content.is_empty() && normalize_for_match(output).contains(content))
}

#[derive(Debug)]
struct RenderedPastePrompt {
    text: CountedPasteText,
    /// The sanitized body staged into the PTY, carrying the line endings the
    /// harness's composer needs. Drained by [`execute`].
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

struct PasteMatchBudget<'a> {
    deadline: Instant,
    remaining_states: usize,
    now: &'a dyn Fn() -> Instant,
}

impl<'a> PasteMatchBudget<'a> {
    fn with_clock(deadline: Instant, now: &'a dyn Fn() -> Instant) -> Self {
        Self {
            deadline,
            remaining_states: MAX_PASTE_MATCH_STATES,
            now,
        }
    }

    fn within_deadline(&self) -> bool {
        (self.now)() < self.deadline
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
            normalized.push_str(&normalize_for_match(ch.encode_utf8(&mut buffer)));
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
        budget: &mut PasteMatchBudget<'_>,
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

    fn visible_with_budget(&self, output: &str, budget: &mut PasteMatchBudget<'_>) -> bool {
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
            inline.push(normalize_for_match(&compact[previous_end..range.start]));
            markers.push(RenderedPasteMarker { chars, ordinal: 1 });
            previous_end = range.end;
        }
        if markers.is_empty() {
            return false;
        }
        inline.push(normalize_for_match(&compact[previous_end..]));
        self.text.matches_segments(&markers, &inline, budget)
    }
}

#[cfg(test)]
#[path = "prompt_delivery_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "prompt_delivery/prefill_tests.rs"]
mod prefill_tests;

#[cfg(test)]
#[path = "prompt_delivery/operation_tests.rs"]
mod operation_tests;

#[cfg(all(test, target_os = "windows"))]
#[path = "prompt_delivery/conpty_tests.rs"]
mod conpty_tests;
