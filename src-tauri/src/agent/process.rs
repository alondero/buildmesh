//! Agent process registry — thread-safe storage for live PTY handles, plus
//! the Tauri command surface that orchestrates a live registry entry
//! (kill_agent / write_to_agent / resize_agent / send_to_agent / debug_*).
//!
//! Issue #1052 deepens `agent::process` from a low-level handle-store into
//! the deep module owning the full process-lifecycle surface (previously
//! scattered across `commands::agent`'s ~2,400-line file). `commands::agent`
//! now contains only the spawn-orchestration Tauri wrappers and the
//! prefill/wire-type helpers; `agent::provider_menu` (sibling module) owns
//! the Spawn Menu derivation that previously lived next to the lifecycle
//! commands in the same 2,400-line `commands::agent` file.

use crate::agent::session_lifecycle;
use crate::db;
use crate::pty::PtyRegistry;
use portable_pty::{Child, MasterPty};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tauri::{command, AppHandle};

/// A live agent PTY process handle.
pub struct AgentProcess {
    pub child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    /// Issue #1122: dedicated PTY writer channel. The async Tauri
    /// command `write_to_agent` enqueues bytes here with a non-blocking
    /// `try_send`; a dedicated OS thread (one per agent) owns the
    /// underlying `Box<dyn Write + Send>` and drains the channel with
    /// blocking `write_all`+`flush`. The previous `Arc<Mutex<Box<dyn
    /// Write>>>` design held the mutex during the actual write — a full
    /// ConPTY pipe could park the async runtime for the entire write
    /// duration, reintroducing the latency this PR is meant to fix.
    /// `SyncSender` is bounded so a stuck agent doesn't grow memory
    /// without limit. A full send is now reported as
    /// [`InputDisposition::Backpressured`] to the caller and the bytes are
    /// held by the client's ordered retry buffer — never dropped with a
    /// success returned (issue #1530).
    /// Wrapped in `Option` so teardown can `take()` it — dropping the
    /// last sender unblocks the writer thread's `recv()` (issue #1531).
    /// Holding a live sender while joining the writer pays the two-second
    /// fallback every time. Private: callers enqueue through
    /// [`AgentProcessRegistry::write_bytes`].
    writer_tx: Mutex<Option<std::sync::mpsc::SyncSender<Vec<u8>>>>,
    /// Byte/message accounting for `writer_tx` (issue #1530). `Arc` because
    /// the writer thread holds its own clone to release each buffer as it
    /// drains — `SyncSender` has no introspection, so this is the only way to
    /// enforce `PTY_INPUT_QUEUE_BYTE_CAPACITY` or report queue usage.
    queue: Arc<InputQueueGauge>,
    input_version: std::sync::atomic::AtomicU64,
    last_submit_ms: std::sync::atomic::AtomicI64,
    retired: AtomicBool,
    input_decoder: Mutex<TerminalInput>,
    /// The draft behind the most recent submit, retained so a prompt the
    /// harness dropped can be written again once (issue #2344).
    ///
    /// Kept beside `input_decoder` and under one lock with it, because the
    /// replay decision reads both atomically: a draft is only re-sendable
    /// while the decoder still reads as an empty prompt, and that pairing
    /// must not change between the check and the write.
    submit_replay: Mutex<SubmitReplay>,
    /// Handle to the dedicated writer thread. `kill_session` joins it
    /// with a bounded timeout so the close path can never hang the UI
    /// on a wedged writer (mirror of the `reader_handle` contract).
    pub writer_handle: Mutex<Option<JoinHandle<()>>>,
    /// PTY master. Wrapped in `Option` so `kill_session` can `take()` it
    /// out to drop the underlying pseudoconsole — on Windows ConPTY the
    /// master read pipe does not EOF on child exit, so the only way to
    /// unblock the reader thread is to close the pseudoconsole itself
    /// (drop the `MasterPty`). See issue #300.
    pub master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    pub reader_alive: Arc<AtomicBool>,
    /// Set by `kill_session` BEFORE it closes the PTY, so the reader
    /// thread's post-exit epilogue can tell a deliberate teardown (node
    /// close, stale-process kill at spawn step 2, app shutdown) apart
    /// from the child dying on its own. On a deliberate kill the reader
    /// must not write any node status — the kill initiator owns the next
    /// status. Without this, a process killed within `EARLY_EXIT_WINDOW`
    /// of its creation was misread as a failed `--resume`: the reader
    /// wrote `Error` + emitted `resume-failed`, and that stale `Error`
    /// blocked the replacing spawn's Spawning→Running promotion — the
    /// node showed "failed" while the new agent booted fine.
    ///
    /// `Arc` because the reader thread holds its own clone: `kill_session`
    /// removes the registry entry after a bounded 2s join, and a reader
    /// that outlives the join timeout could no longer reach the flag
    /// through the registry.
    pub deliberate_kill: Arc<AtomicBool>,
    /// Mesh this agent belongs to. Stored at registration time so the
    /// PTY read/write hot paths (`write_bytes`, the `pump_pty_output`
    /// closure in `agent::spawn::start_reader`) can record per-mesh
    /// activity without a DB lookup on every chunk (issue #634).
    /// `0` is the sentinel used by the test fixtures in
    /// `tests/pty_spawn.rs`; production code always populates this with
    /// a real `mesh_id` from `db::get_mesh_by_path(&node.path)`.
    pub mesh_id: i64,
    /// Job Object containing the agent's whole process tree, when assignment
    /// succeeded (Windows only; `None` on failure → `kill_session` falls back to
    /// `taskkill`). Killing the job reaches detached descendants — e.g. a dev
    /// server the agent orphaned — that the PPID tree walk would miss.
    pub job: Option<crate::process_util::JobHandle>,
    /// Handle to the reader thread so `kill_session` can `join()` it with a
    /// bounded timeout. Without this, the reader could remain wedged on
    /// `read()` after a kill, leaving PTY bytes from a dead session
    /// interleaving with new ones (issue #300).
    pub reader_handle: Mutex<Option<JoinHandle<()>>>,
    /// Original `SpawnTimer.start` clone, set at process-registration time.
    /// Used to log `first_user_input` elapsed against the same reference
    /// as every other `spawn_timing:` checkpoint. Without this, the
    /// "user typed first key" measurement would be timestamped against
    /// "process spawned" (the existing `spawned_at`) and the gap from
    /// "click Spawn" to "first key" would be off by the entire spawn
    /// pipeline duration.
    pub spawn_start: std::time::Instant,
    /// First-write gate. Set to `true` by `record_first_input_if_first`
    /// (called from `write_bytes`) after the first successful PTY write.
    /// Flipped exactly once per session via compare-exchange, so a burst
    /// of keypresses only emits one `spawn_timing:` log line. Reset only
    /// when the session is removed from the registry.
    ///
    /// Plain `AtomicBool` (not `Arc<AtomicBool>`): the field is already
    /// inside an `Arc<AgentProcess>` in the registry, so reachability
    /// through `&agent.first_user_input_logged` is one indirection. The
    /// `reader_alive: Arc<AtomicBool>` field above needs the inner Arc
    /// because it's cloned into the reader thread — this flag has no such
    /// consumer, so the inner Arc would be one needless heap alloc per
    /// session.
    pub first_user_input_logged: AtomicBool,
    /// Per-incarnation token assigned at `insert`. Natural-exit reaping
    /// and `kill_session` compare-and-remove against this so an old
    /// reader's EOF cannot delete a replacement process (issue #1531).
    /// `0` in a struct literal is unassigned; `insert` overwrites it.
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InputStamp {
    generation: u64,
    version: u64,
}

#[path = "terminal_input.rs"]
mod terminal_input;
pub use terminal_input::{
    InputActivity, InputDisposition, InputOutcome, InputQueueGauge, InputWriteError,
    PTY_INPUT_QUEUE_BYTE_CAPACITY,
};
use terminal_input::{TerminalInput, UNKNOWN_INPUT_BUFFER_LEN};

#[derive(Clone, Copy)]
enum InputFraming {
    ByteStream,
    TerminalEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputUnavailable {
    MissingProcess,
    Draft,
    UnknownInput,
    Paste,
}

impl InputStamp {
    pub(crate) fn encode(self) -> String {
        format!("{}:{}", self.generation, self.version)
    }

    pub(crate) fn decode(value: &str) -> Option<Self> {
        let (generation, version) = value.split_once(':')?;
        Some(Self {
            generation: generation.parse().ok()?,
            version: version.parse().ok()?,
        })
    }
}

/// Ceiling on the bytes retained for one unconfirmed-submit replay.
///
/// Deliberately far below [`PTY_INPUT_QUEUE_BYTE_CAPACITY`]: this buffer is
/// held for the whole confirmation window rather than drained within a write.
/// A draft that outgrows it is abandoned rather than truncated — half a prompt
/// replayed as a prompt reads, to the user, like their own text.
const SUBMIT_REPLAY_BYTE_CAPACITY: usize = 64 * 1024;

/// Why a retained draft was not written again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplaySkip {
    /// The harness does not declare that it drops early prompts.
    HarnessOptedOut,
    /// No submit is outstanding, or its draft outgrew the retention cap.
    NoRetainedDraft,
    /// The user has typed again since the submit; their text is not ours to
    /// overwrite with a replayed copy.
    PromptOccupied,
    /// This incarnation already replayed one submit.
    AlreadyReplayed,
}

/// What happened when Buildmesh re-sent an unconfirmed prompt.
///
/// A refusal keeps the reason the write path gives — a full queue and a dead
/// agent call for different responses, and reporting both as "not replayed"
/// would hide a stuck PTY behind a missing draft.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SubmitReplayOutcome {
    /// The draft reached the PTY.
    Sent,
    /// There was nothing to re-send.
    Skipped(ReplaySkip),
    /// The PTY refused the bytes.
    Refused(InputWriteError),
}

/// The bytes one accepted keystroke added to the prompt behind it.
///
/// Retained rather than the *decoded* text: the replay re-sends the exact
/// stream the user produced, so editing keys (backspace, cursor movement)
/// reproduce the same composer contents instead of a best guess at them.
#[derive(Debug, Default)]
struct SubmitReplay {
    /// Bytes typed into the prompt that has not been submitted yet.
    pending: Vec<u8>,
    /// `pending` outgrew the cap, so it no longer describes one whole prompt.
    truncated: bool,
    /// The last submitted draft, held until the harness confirms or the
    /// confirmation window closes.
    submitted: Option<Vec<u8>>,
    /// The submit terminator to re-send with the draft (`\r` or `\n`).
    terminator: u8,
    /// One replay has already been spent on this incarnation.
    replayed: bool,
}

impl SubmitReplay {
    /// Fold one accepted write into the retained prompt.
    ///
    /// `prompt_len` is the decoder's own character count *after* this write, so
    /// the retention clears itself where the composer does — on Ctrl+C and on a
    /// backspace run — rather than carrying bytes the user has already
    /// discarded into the next replay.
    fn record(&mut self, data: &[u8], activity: InputActivity, prompt_len: usize) {
        if activity.submitted {
            // One buffer can carry a prompt and its terminator together — a
            // paste, or a raw byte stream through `write_bytes` — so the text
            // ahead of the first CR/LF is still part of the prompt being
            // submitted. Splitting on the *first* terminator keeps a later
            // "text\rmore text\r" from carrying the second prompt into the
            // first one's draft.
            let split = data
                .iter()
                .position(|byte| *byte == b'\r' || *byte == b'\n')
                .unwrap_or(data.len());
            // The byte at `split` is the one that submitted *this* draft. Taking
            // `tail.last()` would pick up a later line ending in a compound
            // buffer such as "cmd1\rcmd2\n" and re-send the wrong terminator.
            self.terminator = data.get(split).copied().unwrap_or(b'\r');
            let text = &data[..split];
            self.submitted = if !self.truncated
                && self.pending.len().saturating_add(text.len()) <= SUBMIT_REPLAY_BYTE_CAPACITY
            {
                self.pending.extend_from_slice(text);
                Some(std::mem::take(&mut self.pending))
            } else {
                self.pending.clear();
                None
            };
            self.truncated = false;
            return;
        }
        // Terminal replies are the terminal talking, not the user; they are
        // not prompt content and must never be replayed back at it.
        if !activity.user_input {
            return;
        }
        if prompt_len == 0 {
            // Cleared without submitting: Ctrl+C, or backspacing to nothing.
            // Either way the *typed* text is gone and must not be replayed.
            self.pending.clear();
            self.truncated = false;
            // Ctrl+C additionally abandons the outstanding submit. Left in
            // place it would put a refused prompt back on the wire 30 seconds
            // after the user refused it: the composer is empty, so the
            // replay's "still empty" fence would pass.
            //
            // Backspacing to empty is *not* a cancel — the user is correcting a
            // typo, and the unconfirmed submit behind it is still the only
            // recoverable work — so the draft survives that.
            if data.contains(&0x03) {
                self.submitted = None;
            }
            return;
        }
        if self.pending.len().saturating_add(data.len()) > SUBMIT_REPLAY_BYTE_CAPACITY {
            self.pending.clear();
            self.truncated = true;
            return;
        }
        self.pending.extend_from_slice(data);
    }

    /// The draft to write again, or why it must not be.
    ///
    /// `prompt_empty` is the live decoder reading: it is the fence that stops
    /// a replay from landing on top of text the user has typed since.
    fn take_replay(
        &mut self,
        harness_opted_in: bool,
        prompt_empty: bool,
    ) -> Result<Vec<u8>, ReplaySkip> {
        if !harness_opted_in {
            return Err(ReplaySkip::HarnessOptedOut);
        }
        if self.replayed {
            return Err(ReplaySkip::AlreadyReplayed);
        }
        let Some(draft) = self.submitted.take() else {
            return Err(ReplaySkip::NoRetainedDraft);
        };
        if !prompt_empty {
            // Put it back: the user's next submit may be the one worth
            // rescuing, and consuming the draft here would guarantee it is not.
            self.submitted = Some(draft);
            return Err(ReplaySkip::PromptOccupied);
        }
        self.replayed = true;
        let mut bytes = draft;
        bytes.push(self.terminator);
        Ok(bytes)
    }
}

#[cfg(test)]
fn input_buffer_state_after(len: usize, bracketed_paste: bool, data: &[u8]) -> (usize, bool) {
    let mut input = TerminalInput::with_state(len, bracketed_paste);
    input.accept(data);
    (
        if input.has_pending_sequence() {
            UNKNOWN_INPUT_BUFFER_LEN
        } else {
            input.len
        },
        input.bracketed_paste,
    )
}

#[cfg(test)]
fn input_buffer_len_after(len: usize, data: &[u8]) -> usize {
    input_buffer_state_after(len, false, data).0
}

impl AgentProcess {
    /// Build a registry entry. `generation` is assigned by
    /// [`AgentProcessRegistry::insert`]; pass `0` from callers.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        child: Box<dyn Child + Send + Sync>,
        writer_tx: std::sync::mpsc::SyncSender<Vec<u8>>,
        queue: Arc<InputQueueGauge>,
        writer_handle: Option<JoinHandle<()>>,
        master: Box<dyn MasterPty + Send>,
        reader_alive: Arc<AtomicBool>,
        deliberate_kill: Arc<AtomicBool>,
        job: Option<crate::process_util::JobHandle>,
        reader_handle: Option<JoinHandle<()>>,
        spawn_start: std::time::Instant,
        mesh_id: i64,
    ) -> Self {
        Self {
            child: Arc::new(Mutex::new(child)),
            writer_tx: Mutex::new(Some(writer_tx)),
            queue,
            input_version: std::sync::atomic::AtomicU64::new(0),
            last_submit_ms: std::sync::atomic::AtomicI64::new(0),
            retired: AtomicBool::new(false),
            input_decoder: Mutex::new(TerminalInput::default()),
            submit_replay: Mutex::new(SubmitReplay::default()),
            writer_handle: Mutex::new(writer_handle),
            master: Arc::new(Mutex::new(Some(master))),
            reader_alive,
            deliberate_kill,
            mesh_id,
            job,
            reader_handle: Mutex::new(reader_handle),
            spawn_start,
            first_user_input_logged: AtomicBool::new(false),
            generation: 0,
        }
    }

    /// Non-blocking enqueue onto the dedicated writer thread.
    fn enqueue_input(
        &self,
        data: Vec<u8>,
        framing: InputFraming,
    ) -> Result<InputActivity, InputWriteError> {
        self.enqueue_input_if_current(data, None, framing)
            .map(|(_, activity)| activity)
    }

    fn input_stamp_result(&self) -> Result<InputStamp, InputUnavailable> {
        let _guard = self.writer_tx.lock().unwrap();
        let input = self.input_decoder.lock().unwrap();
        if input.bracketed_paste {
            return Err(InputUnavailable::Paste);
        }
        if input.has_pending_sequence() || input.len == UNKNOWN_INPUT_BUFFER_LEN {
            return Err(InputUnavailable::UnknownInput);
        }
        if input.len != 0 {
            return Err(InputUnavailable::Draft);
        }
        Ok(InputStamp {
            generation: self.generation,
            version: self.input_version.load(Ordering::Relaxed),
        })
    }

    /// Shared fence for observing or submitting an already staged draft.
    /// Unlike report admission, a nonempty draft is allowed: the post-write
    /// stamp owns it. Pending input sequences still make ownership ambiguous.
    /// The caller must hold writer_tx before this takes input_decoder, keeping
    /// the comparison atomic with writes and process retirement.
    fn owns_staged_draft(&self, expected: &InputStamp) -> bool {
        !self.retired.load(Ordering::SeqCst)
            && *expected
                == (InputStamp {
                    generation: self.generation,
                    version: self.input_version.load(Ordering::Relaxed),
                })
            && !self.input_decoder.lock().unwrap().has_pending_sequence()
    }

    fn enqueue_input_if_current(
        &self,
        data: Vec<u8>,
        expected: Option<InputStamp>,
        framing: InputFraming,
    ) -> Result<(Option<InputStamp>, InputActivity), InputWriteError> {
        let guard = self.writer_tx.lock().unwrap();
        self.enqueue_input_locked(&guard, data, expected, framing)
    }

    /// The body of [`Self::enqueue_input_if_current`] for callers that already
    /// hold `writer_tx`.
    ///
    /// Split out for issue #2344's replay, which has to read the prompt state
    /// and enqueue without another writer slipping a keystroke between the two
    /// — it must re-send exactly the prompt that was lost, not an approximation
    /// of it.
    fn enqueue_input_locked(
        &self,
        guard: &std::sync::MutexGuard<'_, Option<std::sync::mpsc::SyncSender<Vec<u8>>>>,
        data: Vec<u8>,
        expected: Option<InputStamp>,
        framing: InputFraming,
    ) -> Result<(Option<InputStamp>, InputActivity), InputWriteError> {
        if self.retired.load(Ordering::SeqCst) {
            return Err(InputWriteError::Closed);
        }
        if expected.is_some_and(|expected| !self.owns_staged_draft(&expected)) {
            return Ok((None, InputActivity::default()));
        }
        let stamp = InputStamp {
            generation: self.generation,
            version: self.input_version.load(Ordering::Relaxed),
        };
        let mut input = self.input_decoder.lock().unwrap();
        // Failed sends must not advance the decoder, including half a reply.
        let mut next = input.clone();
        let activity = match framing {
            InputFraming::ByteStream => next.accept(&data),
            InputFraming::TerminalEvent => next.accept_event(&data),
        };
        // Issue #1530: bound the queue by bytes as well as messages, and
        // report a refusal instead of dropping the buffer behind a success.
        let len = data.len();
        // Resolve the sender *before* reserving. `close_input` has already run
        // by this point on a retired incarnation, so it will never `reset()` the
        // gauge again — reserving first and returning `Closed` here would strand
        // `len` bytes and one message as a permanent phantom backlog on the
        // `DIAG` line until the entry is dropped (issue #1530 review).
        let Some(tx) = guard.as_ref() else {
            return Err(InputWriteError::Closed);
        };
        if !self.queue.admits(len) {
            return Err(InputWriteError::Backpressured);
        }
        // The reservation is made under `writer_tx` (held above), so a
        // concurrent enqueue cannot slip between the headroom check and the
        // reservation, and the `try_send` result rolls it back exactly.
        self.queue.reserve(len);
        // Kept only when it is prompt content. A submit always is (the decoder
        // sets `user_input` for the terminator too), and a terminal reply never
        // is — so the clone is skipped for the replies the harness itself sent.
        let retained = activity.user_input.then(|| data.clone());
        match tx.try_send(data) {
            Ok(()) => {}
            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                self.queue.release(len);
                return Err(InputWriteError::Backpressured);
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                self.queue.release(len);
                return Err(InputWriteError::Closed);
            }
        }
        let version = if activity.user_input {
            self.input_version.fetch_add(1, Ordering::Relaxed) + 1
        } else {
            stamp.version
        };
        if activity.submitted {
            self.last_submit_ms
                .store(chrono::Utc::now().timestamp_millis(), Ordering::Relaxed);
        }
        let prompt_len = next.len;
        *input = next;
        // Released before the replay lock: nothing here nests the two in the
        // opposite order to `replay_unconfirmed_submit`.
        drop(input);
        if let Some(retained) = retained {
            self.submit_replay
                .lock()
                .unwrap()
                .record(&retained, activity, prompt_len);
        }
        Ok((
            Some(InputStamp {
                generation: self.generation,
                version,
            }),
            activity,
        ))
    }

    /// Live accounting for this agent's PTY input queue, for diagnostics.
    pub fn input_queue(&self) -> (usize, u64) {
        self.queue.snapshot()
    }

    /// Stash the reader thread's `JoinHandle` on the registry entry.
    ///
    /// Split out from `register_agent` so the spawn flow can keep the
    /// original register-then-start ordering: a concurrent caller hitting
    /// `is_agent_already_running` between thread spawn and registry insert
    /// would otherwise see no entry, miss the duplicate-spawn guard, and
    /// race to spawn a second PTY/child/reader for the same session_id
    /// (code-review finding on PR for #300). The window between insert
    /// and `set_reader_handle` is benign — a `kill_session` arriving in
    /// that window sees `reader_handle = None` and skips the join, the
    /// same as a natural-exit test path.
    pub fn set_reader_handle(&self, handle: std::thread::JoinHandle<()>) {
        *self.reader_handle.lock().unwrap() = Some(handle);
    }

    /// Stash the dedicated PTY writer thread's `JoinHandle` on the
    /// registry entry. Mirrors `set_reader_handle` — the window between
    /// insert and this setter is benign because a `kill_session` arriving
    /// in that window sees `writer_handle = None` and skips the join
    /// (the thread is detached when the registry entry drops, and the
    /// channel close will terminate its loop).
    pub fn set_writer_handle(&self, handle: std::thread::JoinHandle<()>) {
        *self.writer_handle.lock().unwrap() = Some(handle);
    }

    /// Drop the PTY input sender so the writer thread's `recv()` returns.
    /// Idempotent: a second call is a no-op. Must run before joining the
    /// writer, otherwise the join waits for the two-second fallback
    /// (issue #1531).
    pub fn close_input(&self) {
        drop(self.writer_tx.lock().unwrap().take());
        // Whatever is still buffered can never be drained now, so zero the
        // accounting rather than leave a phantom backlog that the next
        // incarnation of this session would appear to inherit (issue #1530).
        self.queue.reset();
    }

    /// Write the last submitted draft to the PTY again, once (issue #2344).
    ///
    /// Called only after [`crate::agent::session_lifecycle::on_submit_unconfirmed`]
    /// has committed the node back to `Ready` — that is, when the harness
    /// observed nothing at all after the Enter. For a harness that discards
    /// input written before its TUI finished booting, those bytes never
    /// reached its composer and the prompt is simply gone; re-sending them
    /// lands in a TUI that is now running.
    ///
    /// `harness_opted_in` is the adapter's
    /// [`AgentProvider::replays_unconfirmed_submit`](crate::agent::provider::AgentProvider::replays_unconfirmed_submit).
    /// The whole method is one critical section on `writer_tx`: the prompt
    /// must still be empty and the draft must still be the one that was
    /// submitted when those are read.
    pub(crate) fn replay_unconfirmed_submit(&self, harness_opted_in: bool) -> SubmitReplayOutcome {
        let guard = self.writer_tx.lock().unwrap();
        if self.retired.load(Ordering::SeqCst) {
            return SubmitReplayOutcome::Refused(InputWriteError::Closed);
        }
        let prompt_empty = self.input_decoder.lock().unwrap().empty_prompt();
        let bytes = match self
            .submit_replay
            .lock()
            .unwrap()
            .take_replay(harness_opted_in, prompt_empty)
        {
            Ok(bytes) => bytes,
            Err(skip) => return SubmitReplayOutcome::Skipped(skip),
        };
        // The one-shot is spent either way: the bytes are on their way, or they
        // are gone. Re-arming on a refusal would let a wedged PTY collect a
        // copy of the same prompt on every later submit.
        match self.enqueue_input_locked(&guard, bytes, None, InputFraming::TerminalEvent) {
            Ok(_) => SubmitReplayOutcome::Sent,
            Err(error) => SubmitReplayOutcome::Refused(error),
        }
    }

    /// Give back the one replay this incarnation has left, after the harness
    /// *did* confirm the submit.
    ///
    /// Without this the replay would be spent for the life of the process: a
    /// node kept open for days would silently go back to dropping the first
    /// prompt of every later turn. Called on the
    /// `Ok(false)` arm of `on_submit_unconfirmed`, where the fence lost
    /// because some observation won the race — the prompt was accepted after
    /// all, so the next one deserves the same protection.
    pub(crate) fn rearm_submit_replay(&self) {
        let mut replay = self.submit_replay.lock().unwrap();
        replay.replayed = false;
        replay.submitted = None;
    }
}

/// Trait abstracting the process registry methods needed by http_server.
pub trait ProcessRegistryApi: Send + Sync {
    fn write_bytes(&self, session_id: i64, data: &[u8]) -> Result<InputOutcome, String>;
    /// One complete terminal input event; callers must not split keyboard events.
    fn write_input(&self, session_id: i64, data: &[u8]) -> Result<InputOutcome, String>;
    /// `(queued_messages, queued_bytes)` for this agent's PTY input queue.
    ///
    /// On the trait rather than reached for through the global `PROCESS_REGISTRY`
    /// so the mobile stalled-input event can be driven by an injected registry
    /// in tests, instead of a test-only global that happens to be empty
    /// (issue #1530 review).
    fn input_queue(&self, session_id: i64) -> (usize, u64);
    fn resize_pty(&self, session_id: i64, cols: u16, rows: u16) -> Result<(), String>;
}

/// Thread-safe registry for agent processes.
/// Wraps `PtyRegistry<i64, AgentProcess>` and exposes typed methods
/// for write/resize that return Result instead of Option.
pub struct AgentProcessRegistry {
    inner: PtyRegistry<i64, AgentProcess>,
}

/// Monotonic token source for [`AgentProcess::generation`]. Starts at 1 so a
/// literal `generation: 0` is visibly "not yet inserted".
static NEXT_PROCESS_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Whether teardown should join the reader/writer threads. `WriterOnly` is
/// the natural-exit reaping path (runs on the reader, must not join itself);
/// `Both` is `kill_session`. Module-local on purpose (round-3 review
/// finding #5): agent has separate reader and writer workers, so it
/// needs an extra `Both` / `WriterOnly` distinction that build_run doesn't
/// carry. Each module owns its enum; [`crate::pty::lifecycle`] exports only
/// the shared bounded-join helper [`crate::pty::lifecycle::join_with_timeout`]
/// (no enum).
enum JoinPolicy {
    Both,
    WriterOnly,
}

impl AgentProcessRegistry {
    pub fn new() -> Self {
        Self {
            inner: PtyRegistry::new(),
        }
    }

    pub fn get(&self, session_id: &i64) -> Option<Arc<AgentProcess>> {
        self.inner.get(session_id)
    }

    pub fn write_bytes(&self, session_id: i64, data: &[u8]) -> Result<InputOutcome, String> {
        self.write_input_with_framing(session_id, data, InputFraming::ByteStream)
    }

    /// Desktop IPC and mobile WebSocket preserve each complete xterm onData event.
    pub fn write_input(&self, session_id: i64, data: &[u8]) -> Result<InputOutcome, String> {
        self.write_input_with_framing(session_id, data, InputFraming::TerminalEvent)
    }

    /// Total PTY input queue depth and bytes across every live agent. Read by
    /// the diagnostics sampler — the queue bound is only meaningful if its
    /// actual usage is observable, and it must fall back to zero once the
    /// writers drain (issue #1530).
    pub fn input_queue_totals(&self) -> (usize, u64) {
        self.inner
            .iter()
            .fold((0, 0), |(messages, bytes), (_, agent)| {
                let (agent_messages, agent_bytes) = agent.input_queue();
                (messages + agent_messages, bytes + agent_bytes)
            })
    }

    fn write_input_with_framing(
        &self,
        session_id: i64,
        data: &[u8],
        framing: InputFraming,
    ) -> Result<InputOutcome, String> {
        let agent = self
            .get(&session_id)
            .ok_or_else(|| "Agent not running".to_string())?;
        // Issue #1122: non-blocking enqueue. The actual `write_all`+`flush`
        // happens on a dedicated OS thread (one per agent) that owns the
        // underlying `Box<dyn Write + Send>`. The previous design held
        // `agent.writer` mutex through the actual write — a full ConPTY
        // pipe could park the async runtime for the entire write duration,
        // reintroducing the latency this PR is meant to fix. With this
        // channel split, the Tauri command does at most one bounded
        // `try_send` per `write_to_agent` call and returns immediately.
        //
        // `try_send` (not `send`) so a stuck agent can't back-pressure
        // the async runtime. A full channel means the writer thread is
        // still draining a slow PTY — that is a *typed outcome*, never a
        // silent drop: pre-#1530 this branch logged a warning, discarded
        // the bytes, and returned `Ok(())`, so a user could be told their
        // prompt had arrived while it had vanished. The caller now gets
        // `InputDisposition::Backpressured`, keeps the exact bytes, and
        // the one ordered retry buffer at the client seam decides when to
        // try again. The bound is 64 messages *and*
        // `PTY_INPUT_QUEUE_BYTE_CAPACITY` bytes; a paste is one message of
        // whatever size the caller passed (issue #1498) and is never split.
        let outcome = match agent.enqueue_input(data.to_vec(), framing) {
            Ok(activity) => InputOutcome::accepted(activity),
            Err(InputWriteError::Backpressured) => {
                let (queued_messages, queued_bytes) = agent.input_queue();
                tracing::warn!(
                    session_id,
                    queued_messages,
                    queued_bytes,
                    "PTY input queue full; rejected {} bytes (agent is slow to consume the PTY)",
                    data.len()
                );
                // No decoder advance, no activity, no telemetry — the buffer
                // is simply not ours any more. The client still holds it.
                return Ok(InputOutcome::backpressured());
            }
            Err(InputWriteError::Closed) => {
                // The writer thread has exited (kill_session has run and
                // the registry entry is mid-removal). Signal the caller
                // the same way the old code did on a pipe failure.
                return Err("Agent not running".to_string());
            }
            Err(InputWriteError::InvalidStamp) => {
                return Err("Invalid input ownership stamp".to_string());
            }
        };
        let activity = outcome.activity;
        if !activity.user_input {
            return Ok(outcome);
        }
        // Mark THIS MESH as active so the background warm-pool worker holds
        // off its idle refills for this mesh's pool while the user is typing
        // into the terminal (issue #613 AC2; issue #634 scopes the activity
        // per-mesh so typing into mesh A's terminal doesn't prevent mesh B's
        // pool from being refilled). Recorded after an accepted enqueue, so
        // a rejected (backpressured) write never counts as activity
        // (issue #1530).
        crate::services::pool_worker::note_activity_for_mesh(agent.mesh_id);
        // Emit the `first_user_input` checkpoint exactly once per session.
        // We do this AFTER an accepted enqueue so a rejected or failed PTY
        // write does NOT claim "user input accepted". The helper is the only
        // place the flag flips and the log line fires — see its doc comment
        // for the atomic contract and the coordinator-drive caveat.
        record_first_input_if_first(
            &agent.first_user_input_logged,
            agent.spawn_start,
            session_id,
        );
        Ok(outcome)
    }

    pub(crate) fn input_stamp(&self, session_id: i64) -> Option<String> {
        self.input_stamp_result(session_id).ok()
    }

    /// Unlike report admission, a delivery watcher owns a nonempty staged draft.
    pub(crate) fn input_is_current(&self, session_id: i64, expected: &str) -> bool {
        let Some(agent) = self.get(&session_id) else {
            return false;
        };
        let Some(expected) = InputStamp::decode(expected) else {
            return false;
        };
        let _guard = agent.writer_tx.lock().unwrap();
        agent.owns_staged_draft(&expected)
    }

    pub(crate) fn input_stamp_result(&self, session_id: i64) -> Result<String, InputUnavailable> {
        self.get(&session_id)
            .ok_or(InputUnavailable::MissingProcess)?
            .input_stamp_result()
            .map(InputStamp::encode)
    }

    /// Commit a recovered lifecycle fact while input cannot change. The
    /// caller must acquire SQLite before entering this per-agent guard, so
    /// database contention holds no PTY locks. The callback only performs the
    /// conditional mutation; global registry locks never span SQLite work.
    pub(crate) fn commit_recovered_turn(
        &self,
        session_id: i64,
        expected: &str,
        completed_at_ms: i64,
        commit: impl FnOnce() -> Result<bool, String>,
    ) -> Result<bool, String> {
        let Some(expected) = InputStamp::decode(expected) else {
            return Ok(false);
        };
        let Some(agent) = self.get(&session_id) else {
            return Ok(false);
        };
        let _guard = agent.writer_tx.lock().unwrap();
        if !agent.owns_staged_draft(&expected)
            || !agent.input_decoder.lock().unwrap().empty_prompt()
            || agent.last_submit_ms.load(Ordering::Relaxed) >= completed_at_ms
            || !agent.reader_alive.load(Ordering::SeqCst)
        {
            return Ok(false);
        }
        commit()
    }

    /// Compare and enqueue under the same writer lock as ordinary keystrokes.
    /// A partial draft invalidates a continuation even before Enter is pressed.
    ///
    /// `Ok(None)` is the *guard lost* signal only — the caller still owns the
    /// draft no longer, so retrying is pointless. Backpressure is a separate
    /// [`InputWriteError::Backpressured`] rather than a third `None` meaning,
    /// because the circuit treats `None` as "someone else owns this draft,
    /// never submit it automatically" and a mere full queue must not be
    /// mistaken for that (issue #1530).
    pub(crate) fn write_bytes_if_current(
        &self,
        session_id: i64,
        data: &[u8],
        expected: &str,
    ) -> Result<Option<String>, InputWriteError> {
        let agent = self.get(&session_id).ok_or(InputWriteError::Closed)?;
        let expected = InputStamp::decode(expected).ok_or(InputWriteError::InvalidStamp)?;
        agent
            .enqueue_input_if_current(data.to_vec(), Some(expected), InputFraming::ByteStream)
            .map(|(stamp, _)| stamp.map(InputStamp::encode))
    }

    pub fn resize_pty(&self, session_id: i64, cols: u16, rows: u16) -> Result<(), String> {
        use portable_pty::PtySize;
        let agent = self
            .get(&session_id)
            .ok_or_else(|| "Agent not running".to_string())?;
        let master = agent.master.lock().unwrap();
        let Some(m) = master.as_ref() else {
            return Err("PTY master already closed".to_string());
        };
        m.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())
    }

    /// Insert `agent` and return the generation token assigned to this
    /// incarnation. A previous entry under the same session id is torn
    /// down so insert cannot leak a child/PTY/writer.
    ///
    /// Sandbox ACE grants are keyed by `session_id` (not generation). The
    /// replacement spawn registers those grants *before* `insert`, so
    /// overwriting `prev` must tear down handles/threads without calling
    /// `sandbox_cleanup` — otherwise the new child's grants are revoked.
    pub fn insert(&self, session_id: i64, mut agent: AgentProcess) -> u64 {
        let generation = NEXT_PROCESS_GENERATION.fetch_add(1, Ordering::Relaxed);
        agent.generation = generation;
        let agent = Arc::new(agent);
        let previous = loop {
            let previous = self.get(&session_id);
            // Retire under the old agent's input guard BEFORE publishing its
            // replacement. A cloned old Arc can never recover a later turn.
            // The compare-and-replace retries concurrent inserts, without
            // keeping the global map locked while waiting for an agent.
            let guard = previous.as_ref().map(|old| old.writer_tx.lock().unwrap());
            if let Some(old) = previous.as_ref() {
                old.retired.store(true, Ordering::SeqCst);
            }
            let replaced =
                self.inner
                    .replace_if_current(session_id, previous.as_ref(), agent.clone());
            drop(guard);
            if replaced {
                break previous;
            }
        };
        if let Some(prev) = previous {
            prev.deliberate_kill.store(true, Ordering::SeqCst);
            let _ = teardown_incarnation(session_id, &prev, JoinPolicy::Both, false);
        }
        generation
    }

    /// Drop the registry entry only if it is still this incarnation.
    /// A replacement spawn under the same session id keeps its entry
    /// (issue #1531).
    fn remove_if_current(&self, session_id: i64, generation: u64) -> Option<Arc<AgentProcess>> {
        let agent = self.get(&session_id)?;
        if agent.generation != generation {
            return None;
        }
        let _guard = agent.writer_tx.lock().unwrap();
        agent.retired.store(true, Ordering::SeqCst);
        self.inner
            .remove_if(&session_id, |agent| agent.generation == generation)
    }

    /// Reap a naturally-exited process incarnation. Compare-and-remove
    /// first so only one teardown owns the Arc: a replacement spawn or a
    /// concurrent `kill_session` wins the other path. Must not unregister
    /// the node-scoped output Channel.
    pub fn reap_incarnation(&self, session_id: i64, generation: u64) {
        let Some(agent) = self.remove_if_current(session_id, generation) else {
            return;
        };
        let _ = teardown_incarnation(session_id, &agent, JoinPolicy::WriterOnly, true);
    }

    pub fn contains(&self, session_id: &i64) -> bool {
        self.inner.contains(session_id)
    }

    /// Returns all session IDs currently tracked.
    pub fn session_ids(&self) -> Vec<i64> {
        self.inner.iter().map(|(id, _)| id).collect()
    }

    /// Returns the number of tracked agent processes.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Check if a session's reader is still alive.
    pub fn is_alive(&self, session_id: &i64) -> bool {
        if let Some(agent) = self.inner.get(session_id) {
            agent.reader_alive.load(Ordering::SeqCst)
        } else {
            false
        }
    }

    /// Kill the child process tree and mark the reader as dead for a session.
    ///
    /// Close order is load-bearing (issue #300 / #1531):
    ///
    /// 1. **Cancel PTY input** (`close_input`) so the writer thread's
    ///    `recv()` returns instead of waiting for every sender to drop.
    /// 2. **Drop the PTY master.** On Windows ConPTY the master read
    ///    pipe does not EOF when the child exits — conhost holds it open
    ///    until the pseudoconsole itself closes (the `MasterPty` drops).
    ///    `take()`-ing the `Option` here drops the inner `Box<dyn MasterPty>`,
    ///    which closes the pseudoconsole and EOFs the reader thread's
    ///    `read()`. Skipping this step wedges the reader indefinitely
    ///    (the pre-fix bug).
    /// 3. **Kill and reap the process tree** (Job Object → `taskkill`
    ///    fallback → `child.kill` + `try_wait`) so descendants release
    ///    their CWD and the worktree can be removed.
    /// 4. **Join worker threads** with a bounded timeout. The bounded
    ///    join protects the close path from any future regression that
    ///    re-wedges a worker — we never want `kill_session` to hang the
    ///    UI thread.
    ///
    /// The registry entry is removed *before* the joins so `is_alive`
    /// cannot report a corpse, and so a racing `reap_incarnation` cannot
    /// teardown the same Arc. Natural-exit reaping uses compare-and-remove
    /// against the generation token so it cannot delete a replacement.
    ///
    /// Must not touch the node-scoped PTY output Channel. Fresh spawn
    /// calls this before the child exists (step 2 of `spawn_agent_inner`);
    /// unregistering here drops the terminal's subscription and the new
    /// reader buffers bytes the viewport never sees.
    /// Cleanup callers must name the incarnation they observed, so a delayed
    /// retirement cannot tear down a replacement process for the same node.
    pub(crate) fn kill_session_if_generation(&self, session_id: i64, generation: u64) -> bool {
        let Some(agent) = self.remove_if_current(session_id, generation) else {
            return false;
        };
        agent.deliberate_kill.store(true, Ordering::SeqCst);
        let _ = teardown_incarnation(session_id, &agent, JoinPolicy::Both, true);
        true
    }

    pub fn kill_session(&self, session_id: i64) {
        while let Some(current) = self.get(&session_id) {
            let Some(agent) = self.remove_if_current(session_id, current.generation) else {
                continue;
            };
            // Flag the teardown as deliberate BEFORE closing anything,
            // so the reader thread — EOFed by the master drop below —
            // is guaranteed to observe the flag when its epilogue runs.
            // See the `deliberate_kill` field doc for why the reader
            // must not apply the early-exit Error heuristic here.
            agent.deliberate_kill.store(true, Ordering::SeqCst);
            let _ = teardown_incarnation(session_id, &agent, JoinPolicy::Both, true);
            return;
        }

        // No live process (fresh-spawn step 2, already reaped). Still
        // revoke sandbox grants so a failed spawn cannot leak ACEs.
        sandbox_cleanup(session_id);
    }
}

fn sandbox_cleanup(session_id: i64) {
    // Sandbox cleanup (issue #498/#528): revoke the node's restricted-token
    // worktree ACE grant. No-op for unsandboxed sessions. Runs after the
    // process tree is dead so nothing is still using the granted directory.
    #[cfg(target_os = "windows")]
    crate::sandbox::spawn::cleanup_restricted(session_id);
    #[cfg(not(target_os = "windows"))]
    let _ = session_id;
}

/// Per-thread join telemetry for a process incarnation teardown. A detached
/// thread is an expected outcome, not an error: the master is already
/// closed by then, so a reader that does not exit promptly can only be
/// genuinely wedged, and a missing handle means there was nothing to join.
///
/// Production callers discard this; it is read only under `cfg(test)`. The
/// teardown path must still build and return it (that is the instrumentation
/// seam), so the lint is allowed here with that reason rather than the
/// production return type removed (issue #2450).
#[allow(dead_code)]
pub(crate) struct TeardownOutcome {
    /// `Some(true)` when the reader thread was joined, `Some(false)` when
    /// it had to be detached, `None` when there was no reader to join
    /// (`JoinPolicy::WriterOnly` detaches by design; a missing handle
    /// means the reader was never registered).
    pub reader_joined: Option<bool>,
    /// Same contract for the writer; `true` when there was no writer
    /// handle. An idle writer is joined only when `close_input` runs
    /// before the join — see [`AgentProcess::close_input`].
    pub writer_joined: bool,
}

/// Shared teardown for a process incarnation (issue #1531). `kill_session`
/// joins both worker threads; natural-exit reaping runs on the reader and
/// therefore only joins the writer. The caller must already have removed
/// `agent` from the registry so only one path owns this Arc.
///
/// `cleanup_sandbox` is false when `insert` replaces a previous entry: the
/// replacement's restricted-token grants are already registered under the
/// same `session_id`, and revoking them would strand the new child.
fn teardown_incarnation(
    session_id: i64,
    agent: &AgentProcess,
    join: JoinPolicy,
    cleanup_sandbox: bool,
) -> TeardownOutcome {
    // 1. Cancel input. Dropping the sender unblocks `recv()` so the
    //    writer join does not pay the two-second fallback.
    agent.close_input();

    // 2. Drop the master. `Option::take` removes the `Box<dyn MasterPty>`
    //    from the mutex; the binding falls out of scope and drops it,
    //    closing the pseudoconsole. Closing the master also unblocks a
    //    writer stuck in `write_all` on a full pipe.
    if let Ok(mut master_guard) = agent.master.lock() {
        master_guard.take();
    }

    // 3. Kill the process tree. Job Object first is authoritative
    //    (reaches detached descendants `taskkill /T` can't); the
    //    `kill_process_tree` is a fallback for the rare spawn case
    //    where job assignment failed; `child.kill` + `try_wait` reap
    //    the shell handle portable-pty owns.
    if let Some(job) = &agent.job {
        job.terminate();
    }
    {
        let mut child = agent.child.lock().unwrap();
        if let Some(pid) = child.process_id() {
            crate::process_util::kill_process_tree(pid);
        }
        child.kill().ok();
        let _ = child.try_wait();
    }

    // 4. Mark the reader dead (it's the reader itself that flips this
    //    on a clean path; flipping here is a belt-and-braces against a
    //    reader that gets stuck after master close).
    agent.reader_alive.store(false, Ordering::SeqCst);

    let reader_joined = match join {
        JoinPolicy::Both => agent.reader_handle.lock().unwrap().take().map(|handle| {
            // A detached reader is an expected outcome here (the master
            // is already closed, so the reader can only be genuinely
            // wedged).
            crate::pty::lifecycle::join_with_timeout(handle, std::time::Duration::from_secs(2))
        }),
        JoinPolicy::WriterOnly => {
            // This thread *is* the reader. Drop the handle without
            // joining — `JoinHandle::drop` detaches.
            drop(agent.reader_handle.lock().unwrap().take());
            None
        }
    };

    let writer_joined = match agent.writer_handle.lock().unwrap().take() {
        Some(handle) => {
            crate::pty::lifecycle::join_with_timeout(handle, std::time::Duration::from_secs(2))
        }
        None => true,
    };

    if cleanup_sandbox {
        sandbox_cleanup(session_id);
    }
    TeardownOutcome {
        reader_joined,
        writer_joined,
    }
}

impl Default for AgentProcessRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Emit the `spawn_timing: first_user_input` checkpoint exactly once per
/// session. Called by `AgentProcessRegistry::write_bytes` after a
/// successful PTY write — covers every PTY-writer path that funnels
/// through `write_bytes`:
///
/// 1. Desktop xterm `onData` → `write_to_agent` (`commands/agent.rs`)
/// 2. Frontend paste / submit → `send_to_agent` → `write_to_agent`
/// 3. Mobile WebSocket → `forward_mobile_input` (`http/ws.rs:149`)
/// 4. Coordinator drive → `RegistryTarget::write_prompt` (`coordinator/drive.rs:141`,
///    NOT user input — see caveat below)
///
/// Atomicity: compare-exchange from `false → true` flips the flag on
/// the first successful call and returns `true`; subsequent calls see
/// `true → true` (CAS fails) and return `false`. `Relaxed` ordering is
/// sufficient because no other state depends on this flag — it's a pure
/// "log this exactly once" gate.
///
/// Caveat: path (4) is the coordinator's `RegistryTarget::write_prompt`
/// (#319 AgentDriver feature). A coordinator-driven prompt on a node the
/// user hasn't yet typed into would log a misleading `first_user_input`.
/// This is the trade-off for hooking at the single chokepoint that
/// covers desktop + mobile + paste in one place — coordinator-first is
/// rare in practice (the coordinator feature is opt-in and typically
/// operates on already-interactive nodes).
///
/// Extracted as a standalone helper so it can be unit-tested without
/// standing up a real PTY: tests just pass an `AtomicBool` and an
/// `Instant` directly.
pub(crate) fn record_first_input_if_first(
    flag: &AtomicBool,
    spawn_start: std::time::Instant,
    session_id: i64,
) -> bool {
    if flag
        .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        tracing::info!(
            "spawn_timing: session={} checkpoint=first_user_input elapsed={}ms",
            session_id,
            spawn_start.elapsed().as_millis()
        );
        true
    } else {
        false
    }
}

impl ProcessRegistryApi for AgentProcessRegistry {
    fn write_bytes(&self, session_id: i64, data: &[u8]) -> Result<InputOutcome, String> {
        AgentProcessRegistry::write_bytes(self, session_id, data)
    }
    fn write_input(&self, session_id: i64, data: &[u8]) -> Result<InputOutcome, String> {
        AgentProcessRegistry::write_input(self, session_id, data)
    }
    fn input_queue(&self, session_id: i64) -> (usize, u64) {
        self.get(&session_id)
            .map(|agent| agent.input_queue())
            .unwrap_or_default()
    }
    fn resize_pty(&self, session_id: i64, cols: u16, rows: u16) -> Result<(), String> {
        AgentProcessRegistry::resize_pty(self, session_id, cols, rows)
    }
}

/// Global singleton agent process registry.
pub static PROCESS_REGISTRY: once_cell::sync::Lazy<Arc<AgentProcessRegistry>> =
    once_cell::sync::Lazy::new(|| Arc::new(AgentProcessRegistry::new()));

/// Low-frequency poller that drops the PTY master as soon as the child
/// process has exited.
///
/// On Windows ConPTY (10.0.28120, June 2026 update) the master read pipe
/// no longer EOFs when the child exits — conhost holds it open until the
/// pseudoconsole itself is closed (the `MasterPty` is dropped). The PTY
/// reader thread is therefore blocked on `read()` long after the agent
/// CLI has gone, which means `reader_alive` stays `true`, the node
/// status never flips to `Idle`, and `is_agent_already_running` blocks
/// respawning the node (issue #287).
///
/// This helper spawns a thread that polls `child.try_wait()` every
/// `POLL_INTERVAL` and, on first detection of child exit, takes the
/// master out of its `Option`, dropping the `Box<dyn MasterPty>`. The
/// pseudoconsole closes, the reader's `read()` returns EOF, the reader
/// sets `reader_alive = false`, and the post-pump path updates DB
/// status to `Idle`.
///
/// **Why `try_wait` and not `wait`:** `AgentProcess.child` is behind a
/// `Mutex` that `kill_session` also locks. A blocked `wait()` would hold
/// the mutex indefinitely and deadlock the close path. `try_wait` is
/// non-blocking; if the lock is contended (kill in progress) we skip
/// the tick and try again next round. The watcher makes no attempt to
/// kill the child itself — it just detects the natural exit.
///
/// Returns the `JoinHandle` so a test can wait for the watcher to
/// finish. Production code can ignore it; the thread self-terminates
/// after the first detected exit.
pub fn watch_child_exit(
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
        loop {
            std::thread::sleep(POLL_INTERVAL);

            // try_lock, NOT lock: never block the kill path. If
            // kill_session is holding the child mutex, skip this
            // tick and try again next round. A poisoned mutex means
            // another lock-holder panicked; a silent skip would
            // leave the reader wedged, so propagate loudly
            // (consistent with kill_session's panic on poison).
            let mut child_guard = match child.try_lock() {
                Ok(g) => g,
                Err(std::sync::TryLockError::Poisoned(p)) => {
                    panic!("watch_child_exit: child mutex poisoned: {}", p);
                }
                Err(std::sync::TryLockError::WouldBlock) => continue,
            };
            let exited = match child_guard.try_wait() {
                Ok(Some(_status)) => true,
                Ok(None) => false,
                Err(e) => {
                    tracing::warn!("watch_child_exit: try_wait error: {}", e);
                    false
                }
            };
            // Release the child lock before touching the master —
            // mirrors kill_session's order and avoids holding both
            // mutexes simultaneously.
            drop(child_guard);

            if !exited {
                continue;
            }

            // Child has exited. Drop the master to EOF the reader.
            // try_lock on the master too — symmetric with the child
            // lock above. If kill_session is mid-step-1 (which also
            // holds master briefly) we yield; kill_session will
            // close the master for us, and the next watcher tick
            // (or this watcher's exit) makes the EOF happen. A
            // poisoned master mutex is a kill_session / resize_pty
            // panic we cannot recover from; propagate loudly.
            match master.try_lock() {
                Ok(mut master_guard) => {
                    master_guard.take();
                }
                Err(std::sync::TryLockError::Poisoned(p)) => {
                    panic!("watch_child_exit: master mutex poisoned: {}", p);
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    // kill_session is mid-flight. It'll take the
                    // master for us; nothing for us to do here.
                }
            }
            return;
        }
    })
}

/// Reusable PTY-write test doubles, in the shape of
/// [`crate::agent::session_lifecycle::testing`]. They live in their own
/// `cfg(test)` module rather than inside `mod tests` for two reasons: the
/// capture seam swaps [`AgentProcess::writer_tx`], a private field, and
/// `circuit::delivery`'s injected-registry tests need the same fixture.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Install a live stand-in into a caller-selected registry.
    pub(crate) fn insert_standin(registry: &AgentProcessRegistry, session_id: i64) {
        super::tests::insert_trivial_agent(registry, session_id);
    }

    /// A fresh registry holding one live stand-in agent for `session_id`, plus
    /// the receiver for every chunk written to that agent's PTY. The
    /// injected-registry sibling of `http::ws`'s `MockRegistry`, for callers
    /// that need the real `AgentProcessRegistry` surface
    /// (`write_bytes_if_current` is not on `ProcessRegistryApi`).
    pub(crate) fn capturing_registry(
        session_id: i64,
    ) -> (
        Arc<AgentProcessRegistry>,
        std::sync::mpsc::Receiver<Vec<u8>>,
    ) {
        let registry = Arc::new(AgentProcessRegistry::new());
        super::tests::insert_trivial_agent(&registry, session_id);
        // Swap the draining writer for a channel the caller owns, so every byte
        // the path under test enqueues is observable instead of leaving the
        // process for a real PTY.
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        *registry.get(&session_id).unwrap().writer_tx.lock().unwrap() = Some(tx);
        (registry, rx)
    }

    /// A PTY writer that is deliberately wedged: it takes buffers off the
    /// channel and then parks, standing in for an agent that has stopped
    /// reading its input. This is the condition issue #1530 is about — a real
    /// full queue, not a simulated `TrySendError`.
    pub(crate) struct BlockedWriter {
        /// Buffers the writer has taken off the channel, including one it is
        /// currently parked on. Distinct from `written` because the gate is
        /// closed until a test opens it — a test waiting for the writer to
        /// *park* needs to observe the take, not the completed write.
        taken: Arc<AtomicUsize>,
        written: Arc<Mutex<Vec<Vec<u8>>>>,
        open: Arc<(Mutex<bool>, std::sync::Condvar)>,
    }

    impl BlockedWriter {
        /// Unpark the writer and wait for it to finish `expected` buffers.
        /// Returns what actually reached the PTY, in order.
        pub(crate) fn release_and_collect(&self, expected: usize) -> Vec<Vec<u8>> {
            {
                let (lock, cv) = &*self.open;
                *lock.lock().unwrap() = true;
                cv.notify_all();
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let written = self.written.lock().unwrap().len();
                if written >= expected {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "writer never finished {expected} buffers (got {written})"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            self.written.lock().unwrap().clone()
        }

        /// How many buffers the writer has taken off the channel so far.
        pub(crate) fn taken(&self) -> usize {
            self.taken.load(Ordering::SeqCst)
        }
    }

    /// A registry whose PTY input queue fills because the writer is blocked.
    ///
    /// Built on the real [`AgentProcess`] and the real enqueue path — only the
    /// channel consumer is swapped — so the byte ceiling, the message ceiling
    /// and the gauge are all the production ones (issue #1530). Lives here
    /// rather than in `mod tests` so a transport-level test (`http::routes::
    /// nodes`) can force a genuine refusal without duplicating the fixture.
    pub(crate) fn blocked_writer_registry(
        session_id: i64,
        capacity: usize,
    ) -> (Arc<AgentProcessRegistry>, Arc<BlockedWriter>) {
        let registry = Arc::new(AgentProcessRegistry::new());
        super::tests::insert_trivial_agent(&registry, session_id);
        let agent = registry.get(&session_id).unwrap();
        let taken = Arc::new(AtomicUsize::new(0));
        let written = Arc::new(Mutex::new(Vec::new()));
        let open = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(capacity);
        *agent.writer_tx.lock().unwrap() = Some(tx);
        let drain_taken = Arc::clone(&taken);
        let drain_written = Arc::clone(&written);
        let drain_open = Arc::clone(&open);
        let drain_queue = Arc::clone(&agent.queue);
        std::thread::spawn(move || {
            while let Ok(bytes) = rx.recv() {
                drain_queue.release(bytes.len());
                // Count the take *before* parking, so a test can observe that
                // the writer is holding a buffer.
                drain_taken.fetch_add(1, Ordering::SeqCst);
                let (lock, cv) = &*drain_open;
                let mut open = lock.lock().unwrap();
                while !*open {
                    open = cv.wait(open).unwrap();
                }
                drop(open);
                drain_written.lock().unwrap().push(bytes);
            }
        });
        (
            registry,
            Arc::new(BlockedWriter {
                taken,
                written,
                open,
            }),
        )
    }
}

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;

// ---------------------------------------------------------------------------
// Tauri command surface — process lifecycle home
//
// `commands::agent::kill_agent / resize_agent / write_to_agent / send_to_agent /
//  is_agent_running / debug_list_agents / debug_crash_snapshot` moved here as
// part of issue #1052 (extract process-lifecycle home) and are now
// `agent::process::kill_agent` etc. The deep module owns
// the full surface so a Tauri command-registration update in `lib.rs` is the
// only seam changes the move requires. `commands::agent::spawn_agent`,
// `create_issue_node`, `create_pr_node`, `spawn_issue_agent`,
// `spawn_handover_agent`, `auto_resume_agent_nodes`
// stay in `commands::agent` — those are spawn orchestration, the legitimate
// role of the commands layer.
//
// Sync cores are `pub(crate)` so production call sites in other modules
// (`services/agent_node.rs:580`, `agent/spawn.rs:1314`) and the test handle in
// `commands/test.rs:367` can reach them via `agent::process::kill_agent`
// directly — the dependency arrow now points at the deep module rather than
// the transport-named commands layer.
// ---------------------------------------------------------------------------

/// Kill all running agent processes. Used during graceful shutdown.
/// Renamed from `kill_all_agents` for naming consistency with the existing
/// `kill_session` (registry-level primitive) — the rename lands in the same
/// PR so callers and the lib.rs shutdown handler update at once.
pub fn kill_all_sessions() {
    for id in PROCESS_REGISTRY.session_ids() {
        PROCESS_REGISTRY.kill_session(id);
        crate::http_server::clear_scrollback(id);
        tracing::info!("kill_all_sessions: killed agent for session {}", id);
    }
}

#[command]
pub async fn resize_agent(session_id: i64, rows: u16, cols: u16) -> Result<(), String> {
    PROCESS_REGISTRY.resize_pty(session_id, cols, rows)
}

#[command]
pub async fn write_to_agent(
    app: AppHandle,
    session_id: i64,
    data: String,
) -> Result<InputOutcome, String> {
    // Issue #1122 (progressive text entry latency).
    //
    // The previous implementation offloaded the entire body — PTY write
    // + disarm + DB read + DB write — onto `spawn_blocking` for every
    // keystroke. The blocking pool is shared with git/diff/GitHub probes;
    // at peak, a single queued probe could park 10-50ms of keystroke
    // latency, and the symptom surfaced as the whole TUI feeling sluggish
    // after a long session.
    //
    // Split the body into a fast path and a slow path instead:
    //
    // 1. Fast path: PTY write + autoclear disarm. Both are short,
    //    non-blocking (Mutex<Box<dyn Write>> + in-memory map lookup) and
    //    never hold a lock across an `.await`, so they go on the async
    //    runtime. Every keystroke — letters, arrows, backspace, paste —
    //    takes this path.
    // 2. Slow path: only when the streaming decoder observes a submission
    //    outside bracketed paste. Terminal replies and pasted newlines cannot
    //    clear attention. Then we ask the DB whether the node is a plain
    //    shell (no LLM attention to clear) and, if not, record the
    //    attention-cleared transition. The DB work goes through
    //    `spawn_blocking` because it's IO-bound and the convention
    //    (added by #761) keeps `reqwest`/rusqlite off the async worker
    //    pool.
    //
    // The PTY write happens *first* even on the slow path — a failed
    // write must not claim "user input accepted", otherwise the
    // post-exit detector would see no signal for the dead child and the
    // status flip would land on a session that never received the byte.
    //
    // The typed outcome is returned to the caller rather than flattened to
    // `Ok(())` (issue #1530): a `Backpressured` result is the client's cue to
    // hold the exact bytes in its ordered retry buffer instead of believing
    // the prompt was delivered.
    let outcome = PROCESS_REGISTRY.write_input(session_id, data.as_bytes())?;
    // Any accepted keystroke means the user is engaged with this node —
    // the stale-mark hypothesis behind auto-clear (issue #878) no longer
    // holds, and the keystroke's own echo must not count toward the
    // resume burst. A refused write engaged nobody, so it must not disarm.
    if outcome.activity.user_input {
        crate::attention_autoclear::disarm(session_id);
    }
    if outcome.is_accepted() {
        watch_for_interrupt(session_id, data.as_bytes());
    }
    if !outcome.is_accepted() || !outcome.activity.submitted {
        return Ok(outcome);
    }
    let published = crate::commands::run_blocking("write_to_agent_signal", move || {
        write_to_agent_signal_blocking(session_id)
    })
    .await?;
    if let Some(payload) = published {
        let sink = session_lifecycle::AppSessionLifecycleSink { app: &app };
        publish_submitted_input(&sink, payload);
    }
    Ok(outcome)
}

/// Publish a resume envelope that was already committed on the blocking pool.
///
/// The legacy `attention-cleared` event does not carry a status. Desktop
/// ignores it for the badge, so a node whose list snapshot was still
/// `spawning` stays on "Starting…" after the user sends a prompt unless this
/// also publishes `agent-lifecycle`.
pub(crate) fn publish_submitted_input(
    sink: &dyn crate::agent::session_lifecycle::SessionLifecycleSink,
    payload: crate::agent::session_lifecycle::LifecycleChangedPayload,
) {
    let session_id = payload.session_id;
    sink.emit_lifecycle_changed(payload);
    sink.emit_attention_cleared(session_id);
}

/// Commit a submitted keystroke's resume on the blocking pool.
///
/// Plain-shell providers have no LLM attention state to clear — a shell's
/// Enter is just shell input, and flipping status to `Running` would render
/// a spurious "Running" badge for a shell sitting at a prompt (issue #535).
/// Returns the stored envelope so the async caller can publish it. `None`
/// means the keystroke was shell input, or the row rejected the transition.
/// A commit error is logged and treated as "nothing to publish": the PTY
/// bytes were already accepted.
pub(crate) fn write_to_agent_signal_blocking(
    session_id: i64,
) -> Result<Option<crate::agent::session_lifecycle::LifecycleChangedPayload>, String> {
    if should_skip_attention_signals(session_id) {
        return Ok(None);
    }
    match commit_submitted_input(&crate::agent::session_lifecycle::DbOnlySink, session_id) {
        Ok(payload) => Ok(payload),
        Err(error) => {
            tracing::warn!(session_id, %error, "submitted input could not commit a lifecycle");
            Ok(None)
        }
    }
}

/// Commit a submitted keystroke as resumed work — the desktop and mobile
/// input paths share it — and, when the node was idling at its prompt under
/// a harness that reports a prompt starting a turn, check after
/// [`SUBMIT_CONFIRM_WINDOW`](crate::agent::session_lifecycle::SUBMIT_CONFIRM_WINDOW)
/// that the harness confirmed it. An unconfirmed Enter started no turn.
pub(crate) fn commit_submitted_input(
    sink: &dyn crate::agent::session_lifecycle::SessionLifecycleSink,
    session_id: i64,
) -> Result<Option<crate::agent::session_lifecycle::LifecycleChangedPayload>, String> {
    use crate::agent::session_lifecycle as lifecycle;
    // One row read answers both questions about this node's harness, and both
    // collapse to plain values so the confirmation thread below carries no DB
    // handle across its 30-second sleep.
    let (awaits_confirmation, replays_submit) = match db::get_agent_node_by_id(session_id) {
        Ok(node) => (
            lifecycle::submit_needs_confirmation(
                node.status,
                harness_confirms_prompt_submission(&node.provider),
            ),
            harness_replays_unconfirmed_submit(&node.provider),
        ),
        Err(_) => (false, false),
    };
    let committed = lifecycle::commit_user_resumed(sink, session_id)?;
    if let Some(submitted) = committed.clone().filter(|_| awaits_confirmation) {
        std::thread::spawn(move || {
            std::thread::sleep(lifecycle::SUBMIT_CONFIRM_WINDOW);
            let reverted = match crate::http::state::app_handle() {
                Some(app) => lifecycle::on_submit_unconfirmed(
                    &lifecycle::AppSessionLifecycleSink { app },
                    &submitted,
                ),
                None => lifecycle::on_submit_unconfirmed(&lifecycle::DbOnlySink, &submitted),
            };
            match reverted {
                Ok(true) => {
                    tracing::info!(
                        session_id,
                        "submitted input was not confirmed by the harness; node is back at its prompt"
                    );
                    replay_dropped_submit(session_id, replays_submit);
                }
                Ok(false) => {
                    // Some observation won the race, so the prompt was accepted
                    // after all. Hand the next submit its own replay rather than
                    // leaving this incarnation with none (issue #2344).
                    if replays_submit {
                        if let Some(agent) = PROCESS_REGISTRY.get(&session_id) {
                            agent.rearm_submit_replay();
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(session_id, %error, "unconfirmed-submit revert failed")
                }
            }
        });
    }
    Ok(committed)
}

/// Re-send the prompt behind a submit the harness never acknowledged
/// (issue #2344).
///
/// Runs only after the revert committed, so the node is sitting at an empty
/// prompt and the TUI — for OpenCode, long since attached its input reader —
/// can accept what the first send lost. Failure is logged and dropped: the
/// bytes are gone, the user is back at their prompt, and the status has
/// already been corrected.
fn replay_dropped_submit(session_id: i64, harness_opted_in: bool) {
    let Some(agent) = PROCESS_REGISTRY.get(&session_id) else {
        tracing::info!(
            session_id,
            "agent left the registry before its prompt could be re-sent"
        );
        return;
    };
    match agent.replay_unconfirmed_submit(harness_opted_in) {
        SubmitReplayOutcome::Sent => {
            tracing::info!(
                session_id,
                "re-sent a prompt the harness had dropped before its TUI was reading"
            );
            // A re-send is submitted work, not a silent repair. Publish it the
            // same way the first submit was published, so the UI leaves Ready
            // at the moment the TUI starts running instead of sitting on an
            // idle-looking node until OpenCode's own hook arrives. Re-arming
            // the confirmation window also means a replay that goes nowhere is
            // caught: the next revert returns the node to Ready, and the
            // already-spent one-shot keeps it from replaying again.
            if let Err(error) = match crate::http::state::app_handle() {
                Some(app) => commit_submitted_input(
                    &crate::agent::session_lifecycle::AppSessionLifecycleSink { app },
                    session_id,
                ),
                None => {
                    commit_submitted_input(&crate::agent::session_lifecycle::DbOnlySink, session_id)
                }
            } {
                tracing::warn!(
                    session_id,
                    %error,
                    "re-sent prompt left the node Ready: resumed work could not be published"
                );
            }
        }
        SubmitReplayOutcome::Skipped(skip) => {
            tracing::info!(session_id, ?skip, "no prompt to re-send")
        }
        SubmitReplayOutcome::Refused(error) => {
            tracing::warn!(session_id, %error, "the PTY refused the re-sent prompt")
        }
    }
}

/// The bytes a terminal sends for the interrupt keys: Esc, and Ctrl+C.
fn is_interrupt_key(data: &[u8]) -> bool {
    matches!(data, [0x1b] | [0x03])
}

/// What one look at an interrupted node found.
#[derive(Clone, Copy, Debug)]
enum InterruptProbe {
    /// The harness recorded the interruption.
    Recorded,
    /// Not recorded yet; look again.
    NotYet,
    /// The node left the observed `Running` revision by another path.
    Moved,
}

/// Look up to `attempts` times, `poll` apart, until the interruption is
/// recorded (`true`) or the node moves on (`false`).
fn await_interrupt_record(
    attempts: u32,
    poll: std::time::Duration,
    mut probe: impl FnMut() -> InterruptProbe,
) -> bool {
    for _ in 0..attempts {
        std::thread::sleep(poll);
        match probe() {
            InterruptProbe::Recorded => return true,
            InterruptProbe::Moved => return false,
            InterruptProbe::NotYet => {}
        }
    }
    false
}

/// An interrupt key reached a node. Claude Code ends an interrupted turn
/// without any hook, so after the harness has had time to record the
/// interruption, a node still `Running` whose transcript shows it is settled
/// back to `Ready`. Anything else — no transcript evidence, a newer
/// observation, a harness that reports interrupts natively — leaves it alone.
pub(crate) fn watch_for_interrupt(session_id: i64, data: &[u8]) {
    if !is_interrupt_key(data) {
        return;
    }
    let pressed_at = std::time::SystemTime::now();
    std::thread::spawn(move || {
        use crate::agent::session_lifecycle as lifecycle;
        let Ok((crate::models::SessionStatus::Running, observed)) =
            db::agent_node::agent_status_revision(session_id)
        else {
            return;
        };
        let recorded = await_interrupt_record(
            lifecycle::INTERRUPT_ATTEMPTS,
            lifecycle::INTERRUPT_POLL,
            || {
                match db::agent_node::agent_status_revision(session_id) {
                    Ok((crate::models::SessionStatus::Running, current)) if current == observed => {
                    }
                    _ => return InterruptProbe::Moved,
                }
                match db::get_agent_node_by_id(session_id) {
                    Ok(node)
                        if crate::coordinator::enrichment::interrupted_turn(&node, pressed_at) =>
                    {
                        InterruptProbe::Recorded
                    }
                    Ok(_) => InterruptProbe::NotYet,
                    Err(_) => InterruptProbe::Moved,
                }
            },
        );
        if !recorded {
            return;
        }
        let settled = match crate::http::state::app_handle() {
            Some(app) => lifecycle::on_turn_interrupted(
                &lifecycle::AppSessionLifecycleSink { app },
                session_id,
                &observed,
            ),
            None => lifecycle::on_turn_interrupted(&lifecycle::DbOnlySink, session_id, &observed),
        };
        match settled {
            Ok(true) => tracing::info!(
                session_id,
                "interrupted turn settled; node is back at its prompt"
            ),
            Ok(false) => {}
            Err(error) => tracing::warn!(session_id, %error, "interrupted-turn settle failed"),
        }
    });
}

/// Whether the node's harness reports a prompt that starts a turn.
pub(super) fn harness_confirms_prompt_submission(provider: &str) -> bool {
    crate::preferences::resolve_harness_provider(provider)
        .adapter()
        .confirms_prompt_submission()
}

/// Whether the node's harness re-sends a prompt it never acknowledged
/// (issue #2344).
pub(super) fn harness_replays_unconfirmed_submit(provider: &str) -> bool {
    crate::preferences::resolve_harness_provider(provider)
        .adapter()
        .replays_unconfirmed_submit()
}

/// Sync core for [`write_to_agent`] — **legacy combined entry point**
/// retained for the existing test suite and for any future mobile HTTP
/// route that wants to mirror the identical behaviour in one call. New
/// IPC callers should use [`write_to_agent`] directly (which splits the
/// fast PTY write from the slow DB work) — issue #1122.
///
/// PTY write happens first; a failed write must NOT claim "user input
/// accepted" — the reader thread's exit-detector would see no signal for
/// the dead child, and the status flip would land on a session that
/// never received the byte.
#[cfg(test)]
pub(crate) fn write_to_agent_blocking(session_id: i64, data: String) -> Result<bool, String> {
    let outcome = PROCESS_REGISTRY.write_input(session_id, data.as_bytes())?;
    if outcome.activity.user_input {
        crate::attention_autoclear::disarm(session_id);
    }
    let should_signal = outcome.is_accepted()
        && outcome.activity.submitted
        && !should_skip_attention_signals(session_id);
    if should_signal {
        session_lifecycle::on_attention_cleared(&session_lifecycle::DbOnlySink, session_id).ok();
    }
    Ok(should_signal)
}

/// Returns true if a newline in `write_to_agent` should NOT flip the
/// node to `Running` and should NOT emit `attention-cleared`. A plain
/// terminal's "Enter" is just shell input — the node has no LLM
/// attention state to clear, and flipping status would render a
/// spurious cyan "Running" badge for a shell sitting at a prompt.
pub(super) fn should_skip_attention_signals(session_id: i64) -> bool {
    db::get_agent_node_by_id(session_id)
        .ok()
        .map(|n| provider_is_plain_terminal(&n.provider))
        .unwrap_or(false)
}

/// Whether the stored harness id resolves to a plain shell. Resolving through
/// the harness profile (not just the legacy enum) ensures a node spawned via
/// the **Terminal profile** still skips LLM attention signals (issue #535).
pub(super) fn provider_is_plain_terminal(provider: &str) -> bool {
    crate::preferences::resolve_harness_provider(provider)
        .adapter()
        .is_plain_terminal()
}

#[command]
pub async fn send_to_agent(
    app: AppHandle,
    session_id: i64,
    input: String,
) -> Result<InputOutcome, String> {
    write_to_agent(app, session_id, format!("{}\n", input)).await
}

/// Reject a handover that carries nothing. Split from [`handover_to_agent`]
/// because the command takes an `AppHandle` only the real app can construct,
/// and this is the half with a user-facing rule in it.
fn validate_handover_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("Nothing to hand over — select text in the source terminal first".to_string());
    }
    Ok(())
}

fn prepare_existing_node_handover(
    target_node_id: i64,
    text: &str,
    app_data_dir: Option<&std::path::Path>,
) -> Result<crate::agent::handover::PreparedHandover, String> {
    validate_handover_text(text)?;
    let node = db::get_agent_node_by_id(target_node_id).map_err(|error| error.to_string())?;
    let provider = crate::agent::provider::resolve_node_provider(&node);
    crate::agent::handover::prepare(&node, provider, text, app_data_dir)
}

fn deliver_prepared_handover(
    prepared: crate::agent::handover::PreparedHandover,
    deliver: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), String> {
    match deliver(prepared.prompt()) {
        Ok(()) => Ok(()),
        Err(error) => {
            prepared.discard();
            Err(error)
        }
    }
}

/// Hand the text selected in one node's terminal over to another **running**
/// node: stage it in the target's PTY input box and submit it. The
/// existing-node sibling of `commands::agent::spawn_handover_agent`, and the
/// backing command for the terminal context menu's "Handover to node" rows.
///
/// Delegates to [`crate::circuit::delivery::write_prompt_to_pty`] instead of
/// writing the bytes here, because the submit is the hard part: a multi-line
/// buffer has to arrive as ONE bracketed paste and the Enter has to land as its
/// own write at an idle input box, or an ink TUI swallows the keystroke into
/// the paste and the prompt sits staged forever (issue #874). That primitive
/// also settles before the Enter when the target is one the turn evaluator does
/// *not* buffer — the ordinary case here, since the agent a human hands work to
/// is one the user spawned rather than one Autopilot owns.
#[command]
pub async fn handover_to_agent(
    app: AppHandle,
    target_node_id: i64,
    text: String,
) -> Result<(), String> {
    let app_data_dir = crate::preferences::app_data_dir();
    crate::commands::run_blocking("handover_to_agent", move || {
        let prepared =
            prepare_existing_node_handover(target_node_id, &text, app_data_dir.as_deref())?;
        deliver_prepared_handover(prepared, |prompt| {
            crate::circuit::delivery::write_prompt_to_pty(target_node_id, prompt, &app)
        })
    })
    .await
}

#[command]
pub async fn kill_agent(session_id: i64) -> Result<(), String> {
    // Offload to the blocking pool: `kill_session` shells out to
    // `taskkill /F /T` on Windows (a synchronous `.output()` wait) and then
    // joins the PTY reader thread with a bounded 2s timeout — up to several
    // seconds parked on a Tauri async worker per call. This command runs on
    // every node close AND at step 2 of every spawn (`spawn_agent_inner`
    // kills stale processes first), so leaving it inline is exactly the
    // pool-starvation class from the Command Threading convention.
    crate::commands::run_blocking("kill_agent", move || kill_agent_blocking(session_id)).await
}

/// Sync core for [`kill_agent`] — see the `*_blocking` + `run_blocking`
/// convention in `commands/mod.rs`.
pub(crate) fn kill_agent_blocking(session_id: i64) -> Result<(), String> {
    crate::session_naming::reset_buffers(session_id);
    crate::agent::provider::notify_process_terminated(session_id);
    PROCESS_REGISTRY.kill_session(session_id);
    crate::http_server::clear_scrollback(session_id);
    // Routes through SessionLifecycle (issue #132). `DbOnlySink` because
    // the blocking core has no `AppHandle` — kill is silent on the
    // attention channel anyway (no `attention-cleared` emit here).
    session_lifecycle::on_idle(&session_lifecycle::DbOnlySink, session_id)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[command]
pub async fn is_agent_running(session_id: i64) -> bool {
    PROCESS_REGISTRY.is_alive(&session_id)
}

// ---------------------------------------------------------------------------
// Debug
// ---------------------------------------------------------------------------
//
// The two commands below are dev-only (issue #2153): they expose
// process-registry state and have no product UI caller. The `cfg` mirrors the
// handler entry in `lib.rs` — a shipped build drops both, and gating only one
// side leaves a dead `#[command]` (release dead-code warning) or a handler
// that cannot resolve (compile error).

#[derive(Serialize)]
pub struct AgentDebugState {
    pub session_id: i64,
    pub is_alive: bool,
}

#[cfg(any(debug_assertions, feature = "dev-tools"))]
#[command]
pub async fn debug_list_agents() -> Vec<AgentDebugState> {
    PROCESS_REGISTRY
        .session_ids()
        .into_iter()
        .map(|id| AgentDebugState {
            session_id: id,
            is_alive: PROCESS_REGISTRY.is_alive(&id),
        })
        .collect()
}

/// Snapshot of all relevant state at the time of a crash, for post-mortem diagnosis.
/// Call this via invoke('debug_crash_snapshot') immediately after a crash to get
/// a consistent view of what the backend was doing.
#[derive(Serialize)]
pub struct CrashSnapshot {
    pub process_registry_ids: Vec<i64>,
    pub session_count: usize,
    pub renamed_sessions: usize,
    pub buffers_size_bytes: usize,
    pub turn_counters_entries: usize,
}

#[cfg(any(debug_assertions, feature = "dev-tools"))]
#[command]
pub async fn debug_crash_snapshot() -> CrashSnapshot {
    let process_ids = PROCESS_REGISTRY.session_ids();
    let session_count = db::list_agent_nodes().map(|s| s.len()).unwrap_or(0);
    let buffers_size = crate::session_naming::buffers_size_bytes();

    CrashSnapshot {
        process_registry_ids: process_ids,
        session_count,
        renamed_sessions: 0,
        buffers_size_bytes: buffers_size,
        turn_counters_entries: 0,
    }
}

#[cfg(test)]
mod handover_command_tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn existing_handover_validates_then_stages_and_discards_a_rejected_file() {
        let database = crate::db::test_support::isolated();
        let root = tempfile::tempdir().unwrap();
        crate::preferences::init_for_tests(root.path().to_path_buf());
        let mesh =
            crate::db::create_mesh("existing handover", "C:/existing-handover-fixture").unwrap();
        let node = crate::db::create_agent_node(
            mesh.id,
            "existing-handover",
            &mesh.path,
            "main",
            crate::models::EnvType::Windows,
            "launch/saved-reviewer",
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
        let configuration: crate::preferences::spawn_configurations::SpawnConfiguration =
            serde_json::from_value(serde_json::json!({
                "id": "launch/saved-reviewer", "name": "Reviewer",
                "spawn_option_id": "launch/saved-reviewer",
                "model": null, "effort": null, "extra_args": null,
                "resolved": {
                    "configuration_id": "launch/saved-reviewer",
                    "configuration_name": "Reviewer",
                    "spawn_option_id": "launch/saved-reviewer",
                    "harness": { "id": "codex", "name": "Codex", "harness": "codex" },
                    "route": null, "model": null, "effort": null, "extra_args": null
                }
            }))
            .unwrap();
        crate::db::set_node_launch_snapshot(node.id, &configuration).unwrap();

        let db_handle = database.handle();
        let invalid = crate::commands::run_blocking("validate_existing_handover_test", move || {
            let _adopted = crate::db::test_support::adopt(&db_handle);
            prepare_existing_node_handover(9_999_999, "  ", None).map(|_| ())
        })
        .await
        .unwrap_err();
        assert!(invalid.contains("Nothing to hand over"), "{invalid}");

        let db_handle = database.handle();
        let app_data_dir = root.path().to_path_buf();
        let text = "review all findings\n".repeat(100);
        let expected_text = text.clone();
        let prepared = crate::commands::run_blocking("prepare_existing_handover_test", move || {
            let _adopted = crate::db::test_support::adopt(&db_handle);
            prepare_existing_node_handover(node.id, &text, Some(&app_data_dir))
        })
        .await
        .unwrap();
        assert!(prepared
            .prompt()
            .contains("Read and follow the complete task brief"));
        assert!(!prepared.prompt().contains(['\r', '\n']));
        let files: Vec<_> = std::fs::read_dir(
            root.path()
                .join("handovers")
                .join(format!("node-{}", node.id)),
        )
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
        assert_eq!(files.len(), 1);
        assert_eq!(std::fs::read(&files[0]).unwrap(), expected_text.as_bytes());

        let error = deliver_prepared_handover(prepared, |_| {
            Err("node has no live agent process".to_string())
        })
        .unwrap_err();
        assert_eq!(error, "node has no live agent process");
        assert!(
            !files[0].exists(),
            "a rejected pointer must not leave its file"
        );
        crate::preferences::reset_for_tests();
    }
}
