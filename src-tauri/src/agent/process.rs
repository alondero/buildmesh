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

#[cfg(test)]
fn input_buffer_state_after(len: usize, bracketed_paste: bool, data: &[u8]) -> (usize, bool) {
    let mut input = TerminalInput::with_state(len, bracketed_paste);
    input.accept(data);
    (if input.has_pending_sequence() { UNKNOWN_INPUT_BUFFER_LEN } else { input.len }, input.bracketed_paste)
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
    fn enqueue_input(&self, data: Vec<u8>, framing: InputFraming) -> Result<InputActivity, InputWriteError> {
        self.enqueue_input_if_current(data, None, framing).map(|(_, activity)| activity)
    }

    fn input_stamp_result(&self) -> Result<InputStamp, InputUnavailable> {
        let _guard = self.writer_tx.lock().unwrap();
        let input = self.input_decoder.lock().unwrap();
        if input.bracketed_paste { return Err(InputUnavailable::Paste); }
        if input.has_pending_sequence() || input.len == UNKNOWN_INPUT_BUFFER_LEN {
            return Err(InputUnavailable::UnknownInput);
        }
        if input.len != 0 { return Err(InputUnavailable::Draft); }
        Ok(InputStamp { generation: self.generation, version: self.input_version.load(Ordering::Relaxed) })
    }

    /// Shared fence for observing or submitting an already staged draft.
    /// Unlike report admission, a nonempty draft is allowed: the post-write
    /// stamp owns it. Pending input sequences still make ownership ambiguous.
    /// The caller must hold writer_tx before this takes input_decoder, keeping
    /// the comparison atomic with writes and process retirement.
    fn owns_staged_draft(&self, expected: &InputStamp) -> bool {
        !self.retired.load(Ordering::SeqCst)
            && *expected == (InputStamp { generation: self.generation, version: self.input_version.load(Ordering::Relaxed) })
            && !self.input_decoder.lock().unwrap().has_pending_sequence()
    }

    fn enqueue_input_if_current(&self, data: Vec<u8>, expected: Option<InputStamp>, framing: InputFraming) -> Result<(Option<InputStamp>, InputActivity), InputWriteError> {
        let guard = self.writer_tx.lock().unwrap();
        if self.retired.load(Ordering::SeqCst) { return Err(InputWriteError::Closed); }
        if expected.is_some_and(|expected| !self.owns_staged_draft(&expected)) {
            return Ok((None, InputActivity::default()));
        }
        let stamp = InputStamp { generation: self.generation, version: self.input_version.load(Ordering::Relaxed) };
        let mut input = self.input_decoder.lock().unwrap();
        // Failed sends must not advance the decoder, including half a reply.
        let mut next = input.clone();
        let activity = match framing {
            InputFraming::ByteStream => next.accept(&data),
            InputFraming::TerminalEvent => next.accept_event(&data),
        };
        // Issue #1530: bound the queue by bytes as well as messages, and
        // report a refusal instead of dropping the buffer behind a success.
        // The reservation is made under `writer_tx` (held above), so a
        // concurrent enqueue cannot slip between the headroom check and the
        // reservation, and the `try_send` result rolls it back exactly.
        let len = data.len();
        if !self.queue.admits(len) { return Err(InputWriteError::Backpressured); }
        self.queue.reserve(len);
        let sent = match guard.as_ref() {
            Some(tx) => tx.try_send(data),
            None => return Err(InputWriteError::Closed),
        };
        match sent {
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
        } else { stamp.version };
        if activity.submitted { self.last_submit_ms.store(chrono::Utc::now().timestamp_millis(), Ordering::Relaxed); }
        *input = next;
        Ok((Some(InputStamp { generation: self.generation, version }), activity))
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
}

/// Trait abstracting the process registry methods needed by http_server.
pub trait ProcessRegistryApi: Send + Sync {
    fn write_bytes(&self, session_id: i64, data: &[u8]) -> Result<InputOutcome, String>;
    /// One complete terminal input event; callers must not split keyboard events.
    fn write_input(&self, session_id: i64, data: &[u8]) -> Result<InputOutcome, String>;
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
        self.inner.iter().fold((0, 0), |(messages, bytes), (_, agent)| {
            let (agent_messages, agent_bytes) = agent.input_queue();
            (messages + agent_messages, bytes + agent_bytes)
        })
    }

    fn write_input_with_framing(&self, session_id: i64, data: &[u8], framing: InputFraming) -> Result<InputOutcome, String> {
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
        if !activity.user_input { return Ok(outcome); }
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
        let Some(agent) = self.get(&session_id) else { return false; };
        let Some(expected) = InputStamp::decode(expected) else { return false; };
        let _guard = agent.writer_tx.lock().unwrap();
        agent.owns_staged_draft(&expected)
    }

    pub(crate) fn input_stamp_result(&self, session_id: i64) -> Result<String, InputUnavailable> {
        self.get(&session_id).ok_or(InputUnavailable::MissingProcess)?
            .input_stamp_result().map(InputStamp::encode)
    }

    /// Commit a recovered lifecycle fact while input cannot change. The
    /// caller must acquire SQLite before entering this per-agent guard, so
    /// database contention holds no PTY locks. The callback only performs the
    /// conditional mutation; global registry locks never span SQLite work.
    pub(crate) fn commit_recovered_turn(
        &self, session_id: i64, expected: &str, completed_at_ms: i64,
        commit: impl FnOnce() -> Result<bool, String>,
    ) -> Result<bool, String> {
        let Some(expected) = InputStamp::decode(expected) else { return Ok(false); };
        let Some(agent) = self.get(&session_id) else { return Ok(false); };
        let _guard = agent.writer_tx.lock().unwrap();
        if !agent.owns_staged_draft(&expected)
            || !agent.input_decoder.lock().unwrap().empty_prompt()
            || agent.last_submit_ms.load(Ordering::Relaxed) >= completed_at_ms
            || !agent.reader_alive.load(Ordering::SeqCst)
        { return Ok(false); }
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
    pub(crate) fn write_bytes_if_current(&self, session_id: i64, data: &[u8], expected: &str) -> Result<Option<String>, InputWriteError> {
        let agent = self.get(&session_id).ok_or(InputWriteError::Closed)?;
        let expected = InputStamp::decode(expected).ok_or(InputWriteError::InvalidStamp)?;
        agent.enqueue_input_if_current(data.to_vec(), Some(expected), InputFraming::ByteStream)
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
            if let Some(old) = previous.as_ref() { old.retired.store(true, Ordering::SeqCst); }
            let replaced = self.inner.replace_if_current(session_id, previous.as_ref(), agent.clone());
            drop(guard);
            if replaced { break previous; }
        };
        if let Some(prev) = previous {
            prev.deliberate_kill.store(true, Ordering::SeqCst);
            teardown_incarnation(session_id, &prev, JoinPolicy::Both, false);
        }
        generation
    }

    /// Drop the registry entry only if it is still this incarnation.
    /// A replacement spawn under the same session id keeps its entry
    /// (issue #1531).
    fn remove_if_current(&self, session_id: i64, generation: u64) -> Option<Arc<AgentProcess>> {
        let agent = self.get(&session_id)?;
        if agent.generation != generation { return None; }
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
        teardown_incarnation(session_id, &agent, JoinPolicy::WriterOnly, true);
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
        let Some(agent) = self.remove_if_current(session_id, generation) else { return false; };
        agent.deliberate_kill.store(true, Ordering::SeqCst);
        teardown_incarnation(session_id, &agent, JoinPolicy::Both, true);
        true
    }

    pub fn kill_session(&self, session_id: i64) {
        while let Some(current) = self.get(&session_id) {
            let Some(agent) = self.remove_if_current(session_id, current.generation) else { continue; };
            // Flag the teardown as deliberate BEFORE closing anything,
            // so the reader thread — EOFed by the master drop below —
            // is guaranteed to observe the flag when its epilogue runs.
            // See the `deliberate_kill` field doc for why the reader
            // must not apply the early-exit Error heuristic here.
            agent.deliberate_kill.store(true, Ordering::SeqCst);
            teardown_incarnation(session_id, &agent, JoinPolicy::Both, true);
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
) {
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

    match join {
        JoinPolicy::Both => {
            if let Some(handle) = agent.reader_handle.lock().unwrap().take() {
                crate::pty::lifecycle::join_with_timeout(
                    handle,
                    std::time::Duration::from_secs(2),
                );
            }
        }
        JoinPolicy::WriterOnly => {
            // This thread *is* the reader. Drop the handle without
            // joining — `JoinHandle::drop` detaches.
            drop(agent.reader_handle.lock().unwrap().take());
        }
    }

    if let Some(handle) = agent.writer_handle.lock().unwrap().take() {
        crate::pty::lifecycle::join_with_timeout(handle, std::time::Duration::from_secs(2));
    }

    if cleanup_sandbox {
        sandbox_cleanup(session_id);
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

    /// A fresh registry holding one live stand-in agent for `session_id`, plus
    /// the receiver for every chunk written to that agent's PTY. The
    /// injected-registry sibling of `http::ws`'s `MockRegistry`, for callers
    /// that need the real `AgentProcessRegistry` surface
    /// (`write_bytes_if_current` is not on `ProcessRegistryApi`).
    pub(crate) fn capturing_registry(
        session_id: i64,
    ) -> (Arc<AgentProcessRegistry>, std::sync::mpsc::Receiver<Vec<u8>>) {
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
            Arc::new(BlockedWriter { taken, written, open }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::provider::{SpawnRecipe, WindowsShell};
    use crate::agent::spawn::{open_pty_pair, spawn_child};
    use crate::agent::spawn_environment;
    use crate::models::EnvType;
    use std::io::Write;

    /// The activity from a write the queue **accepted**.
    ///
    /// `write_input` answers `Ok` for an accepted write and also for a refused
    /// one (a refusal carries default activity), so pinning only the activity
    /// would let "the queue dropped it" read as "it decoded to nothing" — the
    /// exact ambiguity issue #1530 exists to remove. Every decoding assertion
    /// goes through here so the disposition is checked too.
    fn accepted_activity(registry: &AgentProcessRegistry, id: i64, data: &[u8]) -> InputActivity {
        let outcome = registry.write_input(id, data).unwrap();
        assert_eq!(
            outcome.disposition,
            InputDisposition::Accepted,
            "expected the queue to accept {data:?}"
        );
        outcome.activity
    }

    #[test]
    fn terminal_protocol_preserves_registry_report_fence_across_packets() {
        let registry = AgentProcessRegistry::new();
        let id = -930_020;
        insert_trivial_agent(&registry, id);
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        let agent = registry.get(&id).unwrap();
        *agent.writer_tx.lock().unwrap() = Some(tx);
        let stamp = registry.input_stamp(id).unwrap();
        for packet in [b"\x1b[I".as_slice(), b"\x1b[O", b"\x1b[12;34R", b"\x1b[?1;2c", b"\x1b[<0;24;12M"] {
            for byte in packet {
                assert_eq!(accepted_activity(&registry, id, &[*byte]), InputActivity::default());
                assert_eq!(rx.recv().unwrap(), vec![*byte]);
            }
            assert_eq!(registry.input_stamp(id).as_ref(), Some(&stamp));
            assert!(registry.commit_recovered_turn(id, &stamp, 1, || Ok(true)).unwrap());
        }
        assert_eq!(agent.last_submit_ms.load(Ordering::Relaxed), 0);
        assert!(!agent.first_user_input_logged.load(Ordering::Relaxed));
        registry.write_input(id, b"\x1b[").unwrap();
        rx.recv().unwrap();
        assert_eq!(registry.input_stamp_result(id), Err(InputUnavailable::UnknownInput));
        assert!(!registry.input_is_current(id, &stamp));
        assert!(!registry.commit_recovered_turn(id, &stamp, 1, || panic!("incomplete input is ambiguous")).unwrap());
        assert!(registry.write_bytes_if_current(id, b"continue", &stamp).unwrap().is_none());
        assert!(rx.try_recv().is_err());
        registry.write_input(id, b"A").unwrap();
        rx.recv().unwrap();
        assert_eq!(registry.input_stamp_result(id), Err(InputUnavailable::UnknownInput));
        assert_ne!(agent.input_version.load(Ordering::Relaxed), InputStamp::decode(&stamp).unwrap().version);
        registry.kill_session(id);
    }

    #[test]
    fn registry_recovers_input_fence_after_escape_then_submit_or_clear() {
        let id = -930_023;
        let (registry, rx) = testing::capturing_registry(id);
        let agent = registry.get(&id).unwrap();
        for boundary in [b'\r', 0x03] {
            let previous = registry.input_stamp(id).unwrap();
            registry.write_input(id, b"\x1b").unwrap();
            assert_eq!(rx.recv().unwrap(), b"\x1b");
            assert_eq!(registry.input_stamp_result(id), Err(InputUnavailable::UnknownInput));
            assert!(!registry.commit_recovered_turn(id, &previous, i64::MAX,
                || panic!("pending Escape must block recovery")).unwrap());
            assert!(registry.write_bytes_if_current(id, b"continue", &previous).unwrap().is_none());
            assert!(rx.try_recv().is_err());

            let last_submit = agent.last_submit_ms.load(Ordering::Relaxed);
            assert_eq!(accepted_activity(&registry, id, &[boundary]), InputActivity {
                user_input: true, submitted: boundary == b'\r',
            });
            assert_eq!(rx.recv().unwrap(), vec![boundary]);
            let current = registry.input_stamp(id).expect("boundary makes reports bindable again");
            assert_ne!(current, previous);
            assert!(!registry.commit_recovered_turn(id, &previous, i64::MAX,
                || panic!("old report cannot cross new input")).unwrap());
            assert!(registry.commit_recovered_turn(id, &current, i64::MAX, || Ok(true)).unwrap());
            if boundary == b'\r' {
                assert!(agent.last_submit_ms.load(Ordering::Relaxed) > 0);
            } else {
                assert_eq!(agent.last_submit_ms.load(Ordering::Relaxed), last_submit);
            }
        }
        assert!(!accepted_activity(&registry, id, b"\x1b\r").submitted);
        assert_eq!(rx.recv().unwrap(), b"\x1b\r");
        assert_eq!(registry.input_stamp_result(id), Err(InputUnavailable::UnknownInput));
        registry.write_input(id, b"\x03").unwrap();
        rx.recv().unwrap();

        registry.write_bytes(id, b"\x1b").unwrap();
        rx.recv().unwrap();
        registry.write_bytes(id, b"\r").unwrap();
        rx.recv().unwrap();
        assert_eq!(registry.input_stamp_result(id), Err(InputUnavailable::UnknownInput),
            "raw byte fragments must not acquire terminal event semantics");
        registry.kill_session(id);
    }

    #[test]
    fn dropped_clear_event_does_not_recover_an_uncertain_prompt() {
        let id = -930_024;
        let registry = AgentProcessRegistry::new();
        insert_trivial_agent(&registry, id);
        // Capacity 1 on purpose, so the `\x03` below genuinely cannot be
        // queued. That was always the intent of this fixture, but pre-#1530 the
        // loss was invisible: the write came back `Ok` with default activity.
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        *registry.get(&id).unwrap().writer_tx.lock().unwrap() = Some(tx);
        let original = registry.input_stamp(id).unwrap();
        registry.write_input(id, b"\x1b").unwrap();
        // Issue #1530: the refused clear is *reported*, not swallowed. The
        // assertion used to be `activity == default`, which a silent drop and a
        // genuine no-op decoder answer identically — exactly the ambiguity that
        // let a dropped prompt read as delivered.
        assert_eq!(
            registry.write_input(id, b"\x03").unwrap().disposition,
            InputDisposition::Backpressured,
            "a full input queue must be reported, not dropped behind a success"
        );
        assert_eq!(registry.input_stamp_result(id), Err(InputUnavailable::UnknownInput));
        assert_eq!(rx.recv().unwrap(), b"\x1b");
        assert!(rx.try_recv().is_err(), "the refused clear must never have been queued");
        assert!(!registry.commit_recovered_turn(id, &original, i64::MAX,
            || panic!("dropped clear cannot authorize recovery")).unwrap());
        assert_eq!(accepted_activity(&registry, id, b"\x03"), InputActivity { user_input: true, submitted: false });
        assert_eq!(rx.recv().unwrap(), b"\x03");
        assert!(registry.input_stamp(id).is_some());
        registry.kill_session(id);
    }

    #[test]
    fn registry_distinguishes_drafts_paste_and_actual_submission() {
        let registry = AgentProcessRegistry::new();
        let id = -930_021;
        insert_trivial_agent(&registry, id);
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        let agent = registry.get(&id).unwrap();
        *agent.writer_tx.lock().unwrap() = Some(tx);
        let original = registry.input_stamp(id).unwrap();
        assert_eq!(accepted_activity(&registry, id, b"draft"), InputActivity { user_input: true, submitted: false });
        rx.recv().unwrap();
        assert_eq!(registry.input_stamp_result(id), Err(InputUnavailable::Draft));
        registry.write_input(id, b"\x03").unwrap();
        rx.recv().unwrap();
        for packet in [b"\x1b[20".as_slice(), b"0~", b"line\n", b"\x1b[201", b"~"] {
            assert!(!accepted_activity(&registry, id, packet).submitted);
            rx.recv().unwrap();
        }
        assert_eq!(agent.last_submit_ms.load(Ordering::Relaxed), 0);
        assert_eq!(registry.input_stamp_result(id), Err(InputUnavailable::Draft));
        assert!(!registry.commit_recovered_turn(id, &original, i64::MAX, || panic!("new draft invalidates old report")).unwrap());
        assert_eq!(accepted_activity(&registry, id, b"\r"), InputActivity { user_input: true, submitted: true });
        rx.recv().unwrap();
        assert!(agent.last_submit_ms.load(Ordering::Relaxed) > 0);
        assert_ne!(registry.input_stamp(id).unwrap(), original);
        registry.kill_session(id);
    }

    /// Raw registry input must leave the staged-draft stream unclaimable while
    /// an escape sequence is open.
    ///
    /// The claim in this test's name is about the *stream*, and that is what it
    /// now asserts. The old assertion — that the stamp is byte-identical to a
    /// pre-write capture — passed only because the cap-1 fixture dropped these
    /// writes: a dropped write leaves the prompt pristine, which looked the
    /// same as "the decoder considered the bytes and stayed uncertain". Send
    /// them for real and the decoder reports `UnknownInput`, because an
    /// unterminated `\x1b[A` makes the prompt uncertain. That is the stricter
    /// and more useful guarantee: the circuit cannot append to, or submit, a
    /// prompt whose boundaries it cannot prove.
    #[test]
    fn dropped_registry_input_does_not_advance_stream_or_activity() {
        let registry = AgentProcessRegistry::new();
        let id = -930_022;
        insert_trivial_agent(&registry, id);
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        *registry.get(&id).unwrap().writer_tx.lock().unwrap() = Some(tx);
        registry.write_bytes(id, b"\x1b[").unwrap();
        rx.recv().unwrap();
        registry.write_bytes(id, b"A").unwrap();
        rx.recv().unwrap();
        registry.write_bytes(id, b"I").unwrap();
        rx.recv().unwrap();
        assert_eq!(
            registry.input_stamp_result(id),
            Err(InputUnavailable::UnknownInput),
            "an unterminated escape sequence must leave the prompt uncertain, not stamped"
        );
        assert!(registry.input_stamp(id).is_none(),
            "no usable stamp exists while the sequence is open");
        registry.kill_session(id);
    }

    #[test]
    fn circuit_retirement_cannot_stop_a_replacement_process_generation() {
        let registry = AgentProcessRegistry::new();
        let id = -930_099;
        insert_trivial_agent(&registry, id);
        let old_generation = registry.get(&id).unwrap().generation;
        assert!(registry.kill_session_if_generation(id, old_generation));
        // Stop succeeded but its durable acknowledgement failed. A replacement
        // must survive the original cleanup request being retried.
        insert_trivial_agent(&registry, id);
        let replacement = registry.get(&id).unwrap().generation;
        assert_ne!(old_generation, replacement);
        assert!(!registry.kill_session_if_generation(id, old_generation));
        assert_eq!(registry.get(&id).unwrap().generation, replacement);
        assert!(registry.kill_session_if_generation(id, replacement));
    }

    #[test]
    fn recovered_turn_database_work_does_not_lock_other_sessions() {
        let registry = Arc::new(AgentProcessRegistry::new());
        let id = -930_004;
        insert_trivial_agent(&registry, id);
        let other = -930_005;
        insert_trivial_agent(&registry, other);
        let (other_tx, other_rx) = std::sync::mpsc::sync_channel(8);
        *registry.get(&other).unwrap().writer_tx.lock().unwrap() = Some(other_tx);
        let input = registry.input_stamp(id).unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (lookup_tx, lookup_rx) = std::sync::mpsc::channel();
        let recovery_registry = registry.clone();
        let recovery = std::thread::spawn(move || recovery_registry.commit_recovered_turn(id, &input, i64::MAX, || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(true)
        }).unwrap());
        entered_rx.recv().unwrap();
        let lookup_registry = registry.clone();
        let lookup = std::thread::spawn(move || {
            let found = lookup_registry.get(&other).is_some();
            let written = lookup_registry.write_bytes(other, b"still responsive\r").is_ok();
            lookup_tx.send(found && written).unwrap();
        });
        let responsive = lookup_rx.recv_timeout(std::time::Duration::from_secs(1));
        // Release even on failure so the regression reports a failure rather
        // than leaving blocked worker threads behind.
        release_tx.send(()).unwrap();
        assert!(recovery.join().unwrap());
        lookup.join().unwrap();
        registry.kill_session(id);
        registry.kill_session(other);
        assert!(responsive.unwrap(), "another session must remain accessible while SQLite is busy");
        assert_eq!(other_rx.recv().unwrap(), b"still responsive\r");
    }

    #[test]
    fn recovered_turn_commits_only_for_the_observed_process_input_and_lifecycle() {
        let registry = AgentProcessRegistry::new();
        let id = -930_003;
        insert_trivial_agent(&registry, id);
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        *registry.get(&id).unwrap().writer_tx.lock().unwrap() = Some(tx);
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE agent_nodes (id INTEGER PRIMARY KEY, status TEXT,
            session_started_at INTEGER, status_changed_at TEXT);
            INSERT INTO agent_nodes VALUES (-930003, 'running', 1, '2026-09-13T18:25:20Z');").unwrap();
        let lifecycle = "1:2026-09-13T18:25:20Z";
        let input = registry.input_stamp(id).unwrap();
        let agent = registry.get(&id).unwrap();
        let completed_at = chrono::Utc::now().timestamp_millis() - 1000;
        let commit = || crate::db::complete_agent_turn_if_current_inner(&conn, id, lifecycle).map_err(|e| e.to_string());
        assert!(registry.commit_recovered_turn(id, &input, completed_at, || {
            assert!(agent.writer_tx.try_lock().is_err(), "input stays fenced through the DB commit");
            commit()
        }).unwrap());
        assert_eq!(conn.query_row("SELECT status FROM agent_nodes", [], |r| r.get::<_, String>(0)).unwrap(), "ready");

        // A lifecycle change after observation must win even when the new
        // state is also Running. Comparing only status would accept it.
        conn.execute_batch("UPDATE agent_nodes SET status='running', status_changed_at='2026-09-13T18:30:00Z'").unwrap();
        assert!(!registry.commit_recovered_turn(id, &input, completed_at, commit).unwrap());
        assert_eq!(conn.query_row("SELECT status FROM agent_nodes", [], |r| r.get::<_, String>(0)).unwrap(), "running");

        registry.write_bytes(id, b"draft").unwrap();
        assert_eq!(rx.recv().unwrap(), b"draft");
        assert!(registry.input_stamp(id).is_none());
        assert!(!registry.commit_recovered_turn(id, &input, completed_at, || panic!("draft must prevent mutation")).unwrap());
        registry.write_bytes(id, b"\r").unwrap();
        assert_eq!(rx.recv().unwrap(), b"\r");
        let after_submit = registry.input_stamp(id).unwrap();
        assert!(!registry.commit_recovered_turn(id, &after_submit, completed_at,
            || panic!("even a prompt submitted before observation supersedes old native completion")).unwrap());
        assert!(!registry.commit_recovered_turn(id, &input, i64::MAX,
            || panic!("a changed input version must prevent mutation")).unwrap());
        insert_trivial_agent(&registry, id);
        assert!(!registry.commit_recovered_turn(id, &after_submit, i64::MAX,
            || panic!("a replacement process cannot consume its predecessor's completion")).unwrap());
        registry.kill_session(id);
        assert!(!registry.commit_recovered_turn(id, &after_submit, i64::MAX,
            || panic!("a dead process cannot publish completion")).unwrap());
    }

    #[test]
    fn circuit_continuation_cannot_append_to_or_submit_a_user_draft() {
        let registry = AgentProcessRegistry::new();
        let id = -930_001;
        insert_trivial_agent(&registry, id);
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        *registry.get(&id).unwrap().writer_tx.lock().unwrap() = Some(tx);
        let observed = registry.input_stamp(id).unwrap();
        registry.write_bytes(id, b"unfinished draft").unwrap();
        assert_eq!(rx.recv().unwrap(), b"unfinished draft");
        assert!(registry.input_stamp(id).is_none(), "a draft already present before classification is ineligible");
        assert!(registry.write_bytes_if_current(id, b"continue", &observed).unwrap().is_none());
        assert!(rx.try_recv().is_err());
        registry.write_bytes(id, b"\r").unwrap();
        assert_eq!(rx.recv().unwrap(), b"\r");
        let observed = registry.input_stamp(id).unwrap();
        let staged = registry.write_bytes_if_current(id, b"staged prompt", &observed).unwrap().unwrap();
        assert_eq!(rx.recv().unwrap(), b"staged prompt");
        registry.write_bytes(id, b"more user input").unwrap();
        assert_eq!(rx.recv().unwrap(), b"more user input");
        assert!(registry.write_bytes_if_current(id, b"\r", &staged).unwrap().is_none(), "Enter must not submit newer input");
        assert!(rx.try_recv().is_err());
        registry.kill_session(id);
    }

    #[test]
    fn input_stamp_ignores_navigation_and_tracks_backspace_to_empty() {
        assert_eq!(input_buffer_len_after(0, b"\x1b[A\x1b[I"), UNKNOWN_INPUT_BUFFER_LEN);
        assert_eq!(input_buffer_len_after(0, b"draft"), 5);
        assert_eq!(input_buffer_len_after(5, b"\x1b[D\x7f\x7f\x7f\x7f\x7f"), UNKNOWN_INPUT_BUFFER_LEN);
        assert_eq!(input_buffer_len_after(5, b"\x7f\x7f\x7f\x7f\x7f"), 0);
        assert_eq!(input_buffer_len_after(5, b"\x03"), 0);
        assert_eq!(input_buffer_len_after(0, b"\x1b[200~line\n two\x1b[201~"), 8);
        assert_eq!(input_buffer_len_after(UNKNOWN_INPUT_BUFFER_LEN, b"\x1b[D\x7f"), UNKNOWN_INPUT_BUFFER_LEN);
        assert_eq!(input_buffer_len_after(UNKNOWN_INPUT_BUFFER_LEN, b"\x15"), UNKNOWN_INPUT_BUFFER_LEN);
        assert_eq!(input_buffer_len_after(0, b"\x10"), UNKNOWN_INPUT_BUFFER_LEN);
    }

    #[test]
    fn input_buffer_state_preserves_bracketed_paste_across_packets() {
        assert_eq!(input_buffer_state_after(0, false, b"\x1b[200~draft"), (5, true));
        assert_eq!(input_buffer_state_after(5, true, b"\n"), (5, true));
        assert_eq!(input_buffer_state_after(5, true, b"\x1b[201~"), (5, false));
    }

    /// `pub(crate)` so [`super::testing::capturing_registry`] can reuse the
    /// fixture rather than duplicating the spawn setup.
    pub(crate) fn insert_trivial_agent(
        registry: &AgentProcessRegistry,
        session_id: i64,
    ) -> (u64, Arc<AtomicBool>) {
        let recipe = SpawnRecipe {
            binary: if cfg!(windows) { "cmd.exe" } else { "/bin/sh" },
            base_args: if cfg!(windows) {
                vec!["/c".into(), "exit".into(), "0".into()]
            } else {
                vec!["-c".into(), "exit 0".into()]
            },
            trailing_args: Vec::new(),
            windows_shell: WindowsShell::Direct,
        };
        let cwd = std::env::current_dir().unwrap();
        let cmd = spawn_environment::wrap(
            recipe,
            EnvType::Windows,
            None,
            None,
            &cwd.to_string_lossy(),
            session_id,
            false,
        );
        let pair = open_pty_pair(24, 80).expect("open pty pair");
        let child = spawn_child(&pair, cmd).expect("spawn child");
        let writer = pair.master.take_writer().expect("take writer");
        let writer_exited = Arc::new(AtomicBool::new(false));
        let writer_exited_thread = writer_exited.clone();
        // Capacity matches production (`PTY_WRITER_CHANNEL_CAPACITY`) on purpose.
        // A capacity of 1 made these decoding fixtures depend on the writer
        // thread winning a scheduling race, and the pre-#1530 silent drop hid
        // the flakiness by reporting a full queue as a successful write. These
        // tests are about input decoding, not about queue saturation — the
        // saturation path has its own fixtures.
        let (writer_tx, writer_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(crate::agent::spawn::PTY_WRITER_CHANNEL_CAPACITY);
        let queue = Arc::new(InputQueueGauge::default());
        let writer_queue = Arc::clone(&queue);
        let writer_thread = std::thread::spawn(move || {
            let mut writer = writer;
            while let Ok(bytes) = writer_rx.recv() {
                writer_queue.release(bytes.len());
                let _ = writer.write_all(&bytes);
                let _ = writer.flush();
            }
            writer_exited_thread.store(true, Ordering::SeqCst);
        });
        let generation = registry.insert(
            session_id,
            AgentProcess::new(
                child,
                writer_tx,
                queue,
                Some(writer_thread),
                pair.master,
                Arc::new(AtomicBool::new(true)),
                Arc::new(AtomicBool::new(false)),
                None,
                None,
                std::time::Instant::now(),
                0,
            ),
        );
        (generation, writer_exited)
    }

    use testing::blocked_writer_registry;

    /// Fill the queue and confirm the next write is *reported*, not swallowed.
    ///
    /// This is the issue's headline regression: pre-fix the same sequence
    /// dropped the bytes and returned `Ok`, so a caller recorded a prompt as
    /// delivered while the agent never saw it.
    #[test]
    fn a_saturated_queue_reports_backpressure_and_leaves_telemetry_untouched() {
        let id = -930_030;
        // Capacity 1: the writer takes the first buffer and parks, so the second
        // fills the single slot and the third is refused.
        let (registry, writer) = blocked_writer_registry(id, 1);
        let agent = registry.get(&id).unwrap();
        assert_eq!(registry.input_queue_totals(), (0, 0), "baseline is an empty queue");

        // Fill the queue with *empty* buffers. They occupy a message slot each
        // without producing any input activity, which keeps the first-user-input
        // checkpoint unset — so the assertion after the refusal proves the
        // refused write is what left telemetry untouched, rather than an earlier
        // accepted keystroke having already set it.
        assert!(registry.write_bytes(id, b"").unwrap().is_accepted());
        wait_for(|| writer.taken() == 1, "writer to park on the first buffer");
        assert!(registry.write_bytes(id, b"").unwrap().is_accepted());
        assert_eq!(agent.input_queue(), (1, 0), "one slot still buffered, zero bytes");
        assert!(
            !agent.first_user_input_logged.load(Ordering::SeqCst),
            "an empty buffer is not user input, so the checkpoint is still unset"
        );

        let stamp_before = registry
            .input_stamp_result(id)
            .expect("an empty prompt with no pending sequence has a stamp");

        let refused = registry.write_input(id, b"lost prompt").unwrap();
        assert_eq!(
            refused.disposition,
            InputDisposition::Backpressured,
            "a full queue must be reported, never reported as success"
        );
        assert_eq!(refused.activity, InputActivity::default());

        // The refusal must leave every downstream signal untouched: no decoder
        // advance, no first-user-input checkpoint, no activity.
        assert!(!agent.first_user_input_logged.load(Ordering::SeqCst),
            "a refused write must not claim the user typed");
        assert_eq!(
            registry.input_stamp_result(id),
            Ok(stamp_before),
            "the refused bytes must not have staged a draft: had they been applied, \
             the 11-byte prompt would report Err(Draft) instead"
        );

        // And it must not have quietly joined the queue either.
        assert_eq!(agent.input_queue(), (1, 0), "a refusal must not be accounted as queued");
        assert!(!writer.release_and_collect(2).iter().any(|chunk| chunk == b"lost prompt"),
            "the refused buffer must never reach the PTY");
        registry.kill_session(id);
    }

    /// Drain, retry, and land every byte exactly once and in order.
    #[test]
    fn a_drained_queue_accepts_the_retry_with_exact_bytes_and_no_duplicate() {
        let id = -930_031;
        // Capacity 1: the writer parks holding `alpha`, `beta` fills the only
        // free slot, and `gamma` is the refusal under test.
        let (registry, writer) = blocked_writer_registry(id, 1);
        let agent = registry.get(&id).unwrap();

        assert!(registry.write_input(id, b"alpha").unwrap().is_accepted());
        wait_for(|| writer.taken() == 1, "writer to park");
        assert!(registry.write_input(id, b"beta").unwrap().is_accepted());
        let refused = registry.write_input(id, b"gamma").unwrap();
        assert_eq!(refused.disposition, InputDisposition::Backpressured);
        // The first byte of a partially-drained "gamma" would prove a split.
        assert_eq!(agent.input_queue(), (1, 4));

        // Drain, then retry the identical bytes. A refusal never queued them,
        // so re-sending cannot duplicate.
        writer.release_and_collect(2);
        wait_for(|| agent.input_queue() == (0, 0), "queue to return to baseline");
        let retry = registry.write_input(id, b"gamma").unwrap();
        assert!(retry.is_accepted(), "a drained queue must accept the retry");

        let delivered = writer.release_and_collect(3);
        assert_eq!(delivered, vec![b"alpha".to_vec(), b"beta".to_vec(), b"gamma".to_vec()],
            "exact bytes, original order, no duplicate");
        assert_eq!(agent.input_queue(), (0, 0), "byte usage returns to baseline after draining");
        registry.kill_session(id);
    }

    /// The byte ceiling must refuse a write that the message ceiling would let
    /// through, and — critically — must not make an oversized paste
    /// undeliverable (issue #1498: a real 17,508-byte paste is one entry).
    #[test]
    fn the_byte_ceiling_refuses_a_backlog_a_paste_can_still_be_sent_through() {
        let id = -930_032;
        let (registry, writer) = blocked_writer_registry(id, 64);
        let agent = registry.get(&id).unwrap();
        // 32 KiB a message: 64 of these is 2 MiB, well past the 1 MiB byte
        // ceiling, so it is the *byte* bound that stops the loop below — not
        // the 64-message bound. That gap is exactly the pairing this test
        // exists to prove is load-bearing.
        let chunk = vec![b'y'; 32 * 1024];

        let mut accepted = 0;
        while registry.write_input(id, &chunk).unwrap().is_accepted() {
            accepted += 1;
            let (_messages, bytes) = agent.input_queue();
            assert!(
                bytes <= PTY_INPUT_QUEUE_BYTE_CAPACITY as u64,
                "the byte ceiling must hold, saw {bytes}"
            );
            assert!(accepted <= 64, "the fixture should never need the message bound");
        }
        assert!(accepted > 1, "the fixture must build a real backlog, got {accepted}");
        assert!(
            accepted < 64,
            "the byte ceiling should bite before the 64-message bound; it stopped after {accepted}"
        );
        wait_for(|| writer.taken() >= 1, "writer to park");

        // A half-cap paste is refused on top of that backlog, even though the
        // message bound still has room.
        let oversize = vec![b'x'; PTY_INPUT_QUEUE_BYTE_CAPACITY / 2 + 1];
        let refusal = registry.write_input(id, &oversize).unwrap();
        assert_eq!(refusal.disposition, InputDisposition::Backpressured,
            "a backlog under the message bound but over the byte bound must still be refused");
        let (_messages, bytes) = agent.input_queue();
        assert!(bytes <= PTY_INPUT_QUEUE_BYTE_CAPACITY as u64,
            "the byte ceiling must hold: {bytes} > {PTY_INPUT_QUEUE_BYTE_CAPACITY}");

        // With the queue drained, a buffer larger than the whole cap is still
        // admitted whole. Rejecting it would mean no paste of any size could be
        // sent, and splitting it would break issue #1498.
        writer.release_and_collect(accepted);
        wait_for(|| agent.input_queue() == (0, 0), "queue to drain");
        assert!(registry.write_input(id, &oversize).unwrap().is_accepted(),
            "an oversized paste must be admitted whole once the queue is empty");
        assert_eq!(writer.release_and_collect(accepted + 1).last().unwrap().len(), oversize.len());
        registry.kill_session(id);
    }

    /// A #1498-sized bracketed paste stays one accepted logical write even
    /// while the byte ceiling is in force.
    #[test]
    fn a_large_bracket_paste_stays_one_accepted_write() {
        let id = -930_033;
        let (registry, writer) = blocked_writer_registry(id, 64);
        let mut paste = b"\x1b[200~".to_vec();
        paste.extend(std::iter::repeat_n(b'p', 17_508 - 12));
        paste.extend_from_slice(b"\x1b[201~");
        assert_eq!(paste.len(), 17_508);

        assert!(registry.write_input(id, &paste).unwrap().is_accepted());
        let delivered = writer.release_and_collect(1);
        assert_eq!(delivered.len(), 1, "a paste must never be split into several writes");
        assert_eq!(delivered[0], paste, "the paste must arrive byte-for-byte");
        registry.kill_session(id);
    }

    /// Poll until `condition` holds, so a test never depends on how fast a
    /// writer thread gets scheduled.
    fn wait_for(mut condition: impl FnMut() -> bool, what: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !condition() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    #[test]
    fn insert_and_get() {
        let registry = AgentProcessRegistry::new();
        assert!(registry.is_empty());
    }

    /// The user-facing half of `handover_to_agent`: an empty selection must be
    /// refused at the IPC boundary rather than staging nothing into a target's
    /// input box (where it would look like a submitted empty prompt). The
    /// command itself needs a real `AppHandle`, so the rule is tested here.
    #[test]
    fn handover_rejects_text_with_nothing_in_it() {
        for text in ["", " ", "\n", "\t \r\n"] {
            let err = validate_handover_text(text)
                .expect_err("whitespace-only is not a handover");
            assert!(err.contains("Nothing to hand over"), "{err}");
        }
        // The selection is handed over as-is: surrounding whitespace is part of
        // what the user selected, so it is not trimmed off the payload.
        for text in ["fix the race\r", "\n  fix the race\n"] {
            assert!(validate_handover_text(text).is_ok(), "{text:?} carries text");
        }
    }

    #[test]
    fn write_bytes_errors_on_missing() {
        let registry = AgentProcessRegistry::new();
        let result = registry.write_bytes(999, b"test");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not running"));
    }

    #[test]
    fn resize_pty_errors_on_missing() {
        let registry = AgentProcessRegistry::new();
        let result = registry.resize_pty(999, 80, 24);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not running"));
    }

    #[test]
    fn is_alive_false_for_missing() {
        let registry = AgentProcessRegistry::new();
        assert!(!registry.is_alive(&999));
    }

    /// Fresh spawn calls `kill_session` before it starts the process. The
    /// terminal has already subscribed by then, so the no-process cleanup
    /// must preserve that node-scoped output subscription for the process
    /// that is about to start.
    #[test]
    fn kill_session_without_process_preserves_output_subscription() {
        use tauri::ipc::{Channel, InvokeResponseBody};

        let session_id = -915_4010;
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_by_channel = received.clone();
        let channel = Channel::new(move |body| {
            if let InvokeResponseBody::Raw(bytes) = body {
                received_by_channel.lock().unwrap().extend(bytes);
            }
            Ok(())
        });
        crate::agent::output::register(session_id, channel);

        let registry = AgentProcessRegistry::new();
        registry.kill_session(session_id);
        crate::agent::output::ensure(session_id).send_owned(b"visible".to_vec());

        assert_eq!(&*received.lock().unwrap(), b"visible");
        crate::agent::output::unregister(session_id);
    }

    // -----------------------------------------------------------------------
    // first_user_input timing-gate tests (spawn-latency investigation)
    //
    // `record_first_input_if_first` is the pure helper that `write_bytes`
    // calls after a successful PTY write. It does the compare-exchange,
    // emits the `spawn_timing:` log line on the false→true transition,
    // and returns whether it logged. Tests pin the one-shot contract.
    // -----------------------------------------------------------------------

    #[test]
    fn record_first_input_flips_flag_on_first_call() {
        let flag = AtomicBool::new(false);
        let spawn_start = std::time::Instant::now();

        let logged = record_first_input_if_first(&flag, spawn_start, 42);

        assert!(
            logged,
            "first call must return true (the log line was emitted)"
        );
        assert!(
            flag.load(Ordering::SeqCst),
            "first call must flip the flag to true"
        );
    }

    #[test]
    fn record_first_input_skips_on_subsequent_calls() {
        let flag = AtomicBool::new(false);
        let spawn_start = std::time::Instant::now();

        // First call: the log line is emitted.
        assert!(record_first_input_if_first(&flag, spawn_start, 42));

        // Second call: CAS sees true→false, returns Err, helper returns false.
        // No log line is emitted — the `spawn_timing: first_user_input`
        // checkpoint fires exactly once per session, no matter how many
        // keypresses follow.
        assert!(!record_first_input_if_first(&flag, spawn_start, 42));
        assert!(!record_first_input_if_first(&flag, spawn_start, 42));

        // Flag is still true (we never reset it).
        assert!(flag.load(Ordering::SeqCst));
    }

    #[test]
    fn record_first_input_has_no_data_dependency() {
        // The helper takes no `data` argument — its contract is purely
        // "flip the flag once on the first invocation, never again".
        // The empty-write behavior (a zero-byte focus event still
        // tripping the gate) lives at the `write_bytes` call site: the
        // helper runs unconditionally after a successful write+flush.
        // This test pins the helper's data-independence so a future
        // refactor that adds a `data.is_empty()` guard to the helper
        // would surface as a test failure here, not as a silently-lost
        // checkpoint log in production.
        let flag = AtomicBool::new(false);
        let spawn_start = std::time::Instant::now();

        assert!(record_first_input_if_first(&flag, spawn_start, 42));
        assert!(flag.load(Ordering::SeqCst));
    }

    /// A `kill_session` teardown must be observable as deliberate by the
    /// reader thread (via the shared `deliberate_kill` flag) — this is
    /// what stops the reader's 3s early-exit heuristic from misreading a
    /// stale-process kill (spawn step 2), node close, or app shutdown as
    /// a failed `--resume` and stamping the node `Error` ("failed to
    /// start") while a replacing spawn is booting fine.
    #[test]
    fn kill_session_sets_deliberate_kill_flag() {
        let recipe = SpawnRecipe {
            binary: if cfg!(windows) { "cmd.exe" } else { "/bin/sh" },
            base_args: if cfg!(windows) {
                vec!["/c".into(), "exit".into(), "0".into()]
            } else {
                vec!["-c".into(), "exit 0".into()]
            },
            trailing_args: Vec::new(),
            windows_shell: WindowsShell::Direct,
        };
        let cwd = std::env::current_dir().unwrap();
        let cmd = spawn_environment::wrap(
            recipe,
            EnvType::Windows,
            None,
            None,
            &cwd.to_string_lossy(),
            -915_4002,
            false,
        );

        let pair = open_pty_pair(24, 80).expect("open pty pair");
        let child = spawn_child(&pair, cmd).expect("spawn child");
        let writer = pair.master.take_writer().expect("take writer");

        let deliberate_kill = Arc::new(AtomicBool::new(false));
        let registry = AgentProcessRegistry::new();
        // Issue #1122: stand up the dedicated writer thread + channel
        // before inserting, mirroring the production `register_agent`
        // ordering. The test only asserts `deliberate_kill` propagation,
        // not the writer thread behaviour, but the registry now requires
        // the channel field to be present.
        // Capacity matches production (`PTY_WRITER_CHANNEL_CAPACITY`) on purpose.
        // A capacity of 1 made these decoding fixtures depend on the writer
        // thread winning a scheduling race, and the pre-#1530 silent drop hid
        // the flakiness by reporting a full queue as a successful write. These
        // tests are about input decoding, not about queue saturation — the
        // saturation path has its own fixtures.
        let (writer_tx, writer_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(crate::agent::spawn::PTY_WRITER_CHANNEL_CAPACITY);
        let queue = Arc::new(InputQueueGauge::default());
        let writer_queue = Arc::clone(&queue);
        let writer_thread = std::thread::spawn(move || {
            let mut writer = writer;
            while let Ok(bytes) = writer_rx.recv() {
                writer_queue.release(bytes.len());
                let _ = writer.write_all(&bytes);
                let _ = writer.flush();
            }
        });
        registry.insert(
            -915_4002,
            AgentProcess::new(
                child,
                writer_tx,
                queue,
                Some(writer_thread),
                pair.master,
                Arc::new(AtomicBool::new(true)),
                deliberate_kill.clone(),
                None,
                None,
                std::time::Instant::now(),
                0,
            ),
        );

        registry.kill_session(-915_4002);

        assert!(
            deliberate_kill.load(Ordering::SeqCst),
            "kill_session must set deliberate_kill before closing the PTY, \
             so the reader epilogue skips the early-exit Error heuristic"
        );
    }

    /// Issue #1531: the writer thread blocks in `recv()` until every sender
    /// is dropped. `kill_session` used to join that thread while still
    /// holding `writer_tx` on the live `Arc<AgentProcess>`, so the join
    /// always paid the two-second fallback. Closing the input channel
    /// before the join must let an idle writer exit promptly.
    #[test]
    fn kill_session_unblocks_idle_writer_promptly() {
        let recipe = SpawnRecipe {
            binary: if cfg!(windows) { "cmd.exe" } else { "/bin/sh" },
            base_args: if cfg!(windows) {
                vec!["/c".into(), "exit".into(), "0".into()]
            } else {
                vec!["-c".into(), "exit 0".into()]
            },
            trailing_args: Vec::new(),
            windows_shell: WindowsShell::Direct,
        };
        let cwd = std::env::current_dir().unwrap();
        let session_id = -915_1531;
        let cmd = spawn_environment::wrap(
            recipe,
            EnvType::Windows,
            None,
            None,
            &cwd.to_string_lossy(),
            session_id,
            false,
        );

        let pair = open_pty_pair(24, 80).expect("open pty pair");
        let child = spawn_child(&pair, cmd).expect("spawn child");
        let writer = pair.master.take_writer().expect("take writer");

        let writer_exited = Arc::new(AtomicBool::new(false));
        let writer_exited_thread = writer_exited.clone();
        // Capacity matches production (`PTY_WRITER_CHANNEL_CAPACITY`) on purpose.
        // A capacity of 1 made these decoding fixtures depend on the writer
        // thread winning a scheduling race, and the pre-#1530 silent drop hid
        // the flakiness by reporting a full queue as a successful write. These
        // tests are about input decoding, not about queue saturation — the
        // saturation path has its own fixtures.
        let (writer_tx, writer_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(crate::agent::spawn::PTY_WRITER_CHANNEL_CAPACITY);
        let queue = Arc::new(InputQueueGauge::default());
        let writer_queue = Arc::clone(&queue);
        let writer_thread = std::thread::spawn(move || {
            let mut writer = writer;
            while let Ok(bytes) = writer_rx.recv() {
                writer_queue.release(bytes.len());
                let _ = writer.write_all(&bytes);
                let _ = writer.flush();
            }
            writer_exited_thread.store(true, Ordering::SeqCst);
        });

        let registry = AgentProcessRegistry::new();
        registry.insert(
            session_id,
            AgentProcess::new(
                child,
                writer_tx,
                queue,
                Some(writer_thread),
                pair.master,
                Arc::new(AtomicBool::new(true)),
                Arc::new(AtomicBool::new(false)),
                None,
                None,
                std::time::Instant::now(),
                0,
            ),
        );

        let started = std::time::Instant::now();
        registry.kill_session(session_id);
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(1500),
            "kill_session must close the writer channel before joining; \
             paid the fallback budget instead ({elapsed:?})"
        );
        assert!(
            writer_exited.load(Ordering::SeqCst),
            "idle writer thread must exit once kill_session drops the sender"
        );
        assert!(
            !registry.contains(&session_id),
            "kill_session must reap the current incarnation from the registry"
        );
    }

    #[test]
    fn insert_assigns_distinct_generation_tokens() {
        let registry = AgentProcessRegistry::new();
        let (g1, _) = insert_trivial_agent(&registry, -915_1532);
        let (g2, _) = insert_trivial_agent(&registry, -915_1533);
        assert_ne!(g1, 0, "insert must overwrite the unassigned 0 token");
        assert_ne!(g2, 0);
        assert_ne!(g1, g2);
        registry.kill_session(-915_1532);
        registry.kill_session(-915_1533);
    }

    #[test]
    fn stale_generation_cannot_remove_replacement() {
        let registry = AgentProcessRegistry::new();
        let session_id = -915_1534;
        let (gen1, _) = insert_trivial_agent(&registry, session_id);
        registry.kill_session(session_id);
        let (gen2, _) = insert_trivial_agent(&registry, session_id);
        assert_ne!(gen1, gen2);

        registry.reap_incarnation(session_id, gen1);
        assert!(
            registry.contains(&session_id),
            "an old incarnation must not reap the replacement"
        );
        assert_eq!(registry.get(&session_id).unwrap().generation, gen2);

        registry.kill_session(session_id);
        assert!(!registry.contains(&session_id));
    }

    #[test]
    fn reap_incarnation_cleans_up_natural_exit() {
        let registry = AgentProcessRegistry::new();
        let session_id = -915_1535;
        let (generation, writer_exited) = insert_trivial_agent(&registry, session_id);

        registry.reap_incarnation(session_id, generation);

        assert!(
            !registry.contains(&session_id),
            "natural exit must drop the registry entry"
        );
        assert!(
            writer_exited.load(Ordering::SeqCst),
            "natural exit must close the writer channel so the thread exits"
        );
    }

    #[test]
    fn reap_incarnation_does_not_reap_a_replacement() {
        let registry = AgentProcessRegistry::new();
        let session_id = -915_1536;
        let (gen1, _) = insert_trivial_agent(&registry, session_id);
        registry.kill_session(session_id);
        let (gen2, replacement_writer_exited) = insert_trivial_agent(&registry, session_id);

        registry.reap_incarnation(session_id, gen1);

        assert!(
            registry.contains(&session_id),
            "old reader EOF must not remove the replacement process"
        );
        assert_eq!(registry.get(&session_id).unwrap().generation, gen2);
        assert!(
            !replacement_writer_exited.load(Ordering::SeqCst),
            "old EOF must not close the replacement's input channel"
        );

        registry.kill_session(session_id);
    }

    #[test]
    fn reap_incarnation_is_a_noop_after_kill_session() {
        let registry = AgentProcessRegistry::new();
        let session_id = -915_1537;
        let (generation, _) = insert_trivial_agent(&registry, session_id);
        registry.kill_session(session_id);
        assert!(!registry.contains(&session_id));

        registry.reap_incarnation(session_id, generation);
        assert!(
            !registry.contains(&session_id),
            "kill_session already claimed exclusive teardown"
        );
    }

    /// Restricted-token grants are keyed by session id. Phase 3 registers
    /// the *replacement*'s grants before `insert` tears down `prev`; that
    /// overwrite teardown must not call `sandbox_cleanup` or it revokes
    /// the live child's ACEs.
    #[cfg(target_os = "windows")]
    #[test]
    fn insert_replacement_does_not_revoke_new_sandbox_grants() {
        use crate::sandbox::spawn::{
            cleanup_restricted, has_restricted_cleanup, insert_restricted_cleanup_for_test,
        };

        let registry = AgentProcessRegistry::new();
        let session_id = -915_1538;
        let (_gen1, _) = insert_trivial_agent(&registry, session_id);

        // Simulate phase 3 of the replacement spawn: grants already live
        // under this session id before register_agent → insert.
        insert_restricted_cleanup_for_test(session_id);
        assert!(has_restricted_cleanup(session_id));

        let (_gen2, _) = insert_trivial_agent(&registry, session_id);
        assert!(
            has_restricted_cleanup(session_id),
            "overwriting prev must not revoke the replacement's session-scoped sandbox grants"
        );

        registry.kill_session(session_id);
        assert!(
            !has_restricted_cleanup(session_id),
            "kill_session must still revoke grants for the final incarnation"
        );
        // Belt-and-braces if an earlier assert failed mid-test.
        cleanup_restricted(session_id);
    }

    /// Regression guard for issue #287: when the agent CLI exits
    /// naturally, the watcher must drop the master within ~1s of the
    /// exit so the reader thread can EOF and `reader_alive` can flip
    /// to `false`. Without this, a Windows 28120 ConPTY build leaves
    /// the node stuck "running" until the app restarts.
    ///
    /// Platform-agnostic by design: we drive a real portable-pty child
    /// (so the types match production exactly) but only assert the
    /// watcher's observable contract — that it takes the master once
    /// the child has exited.
    #[test]
    fn watcher_drops_master_after_child_exit() {
        // Trivial command: exit immediately, no output. The watcher
        // is what we're testing, not the child's output, so a 0-byte
        // recipe keeps the test focused.
        let recipe = SpawnRecipe {
            binary: if cfg!(windows) { "cmd.exe" } else { "/bin/sh" },
            base_args: if cfg!(windows) {
                vec!["/c".into(), "exit".into(), "0".into()]
            } else {
                vec!["-c".into(), "exit 0".into()]
            },
            trailing_args: Vec::new(),
            windows_shell: WindowsShell::Direct,
        };
        let cwd = std::env::current_dir().unwrap();
        let cmd = spawn_environment::wrap(
            recipe,
            EnvType::Windows,
            None,
            None,
            &cwd.to_string_lossy(),
            -915_4001,
            false,
        );

        let pair = open_pty_pair(24, 80).expect("open pty pair");
        let child = spawn_child(&pair, cmd).expect("spawn child");

        let child_arc: Arc<Mutex<Box<dyn Child + Send + Sync>>> = Arc::new(Mutex::new(child));
        let master_arc: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>> =
            Arc::new(Mutex::new(Some(pair.master)));

        let watcher = watch_child_exit(child_arc.clone(), master_arc.clone());

        // ConPTY startup may negotiate terminal capabilities before launching
        // the child. Measure the teardown deadline from observed child exit,
        // not from spawn (this test deliberately has no terminal responder).
        let exit_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while child_arc.lock().unwrap().try_wait().unwrap().is_none() {
            assert!(std::time::Instant::now() < exit_deadline, "probe child did not exit");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // The watcher polls every 500ms, so retain the original 2s bound.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while master_arc.lock().unwrap().is_some() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            master_arc.lock().unwrap().is_none(),
            "watch_child_exit should drop the master within 2s of child exit (issue #287: \
             without this, a Windows 28120 ConPTY build leaves nodes stuck 'running')"
        );

        // The watcher self-terminates after taking the master; this
        // join is best-effort and just keeps the test process clean.
        let _ = watcher.join();
    }

    // -----------------------------------------------------------------------
    // provider_is_plain_terminal attention-signal tests (issue #535 / #550)
    //
    // These pin the per-provider attention-cleared gating without driving
    // `write_to_agent`'s full PTY path. A plain terminal's Enter is just
    // shell input — flipping status to `Running` would render a spurious
    // cyan badge for a shell sitting at a prompt, so we skip the
    // `attention-cleared` emit (and the `Running` status flip) for
    // Terminal-typed nodes.
    // -----------------------------------------------------------------------

    #[test]
    fn plain_terminal_provider_skips_attention_signals() {
        // The "terminal" harness id resolves to the plain-shell executor.
        assert!(provider_is_plain_terminal("terminal"));
    }

    #[test]
    fn llm_providers_do_emit_attention_signals() {
        // Every non-terminal harness id resolves to an LLM executor, so none skip
        // attention signals. "minimax"/"kimi" are legacy ids that now resolve to
        // the Anthropic executor (still an LLM, still not plain-terminal).
        for id in ["anthropic", "minimax", "kimi", "agy", "opencode", "codex"] {
            assert!(
                !provider_is_plain_terminal(id),
                "LLM provider {id:?} should not skip attention signals"
            );
        }
    }

    /// Submitting a prompt must publish the stored `work_resumed` envelope.
    /// The legacy attention-clear event has no status, so a desktop that
    /// still shows "Starting…" never leaves it.
    #[test]
    fn submitted_input_publishes_the_running_lifecycle() {
        let blocking = crate::agent::session_lifecycle::testing::RecordingSink::new();
        let payload = crate::agent::session_lifecycle::commit_user_resumed(&blocking, 7)
            .unwrap()
            .expect("a live row accepts the resume");
        let desktop = crate::agent::session_lifecycle::testing::RecordingSink::new();
        super::publish_submitted_input(&desktop, payload);
        let events = desktop.lifecycle_changed();
        assert_eq!(events.len(), 1, "submitting input must publish agent-lifecycle, not only the legacy clear");
        assert_eq!(events[0].session_id, 7);
        assert_eq!(events[0].status, crate::models::SessionStatus::Running);
        assert_eq!(events[0].kind, crate::agent::session_lifecycle::LifecycleKind::WorkResumed);
        assert_eq!(*desktop.attention_cleared(), vec![7]);
    }

    /// Regression test for the sync core: an unregistered session must short-
    /// circuit on the PTY write before the DB read. Without the `?` ordering
    /// the unit-test DB-not-initialised panic surfaces as a test failure.
    /// The full PTY-write + DB-update path needs a registered agent
    /// (real `portable_pty` handles) and is covered by integration tests.
    #[test]
    fn write_to_agent_blocking_unknown_session_short_circuits_before_db() {
        let result = write_to_agent_blocking(999_999_999, "x".to_string());
        let err = result.expect_err("unknown session must surface an error");
        assert!(
            err.contains("Agent not running"),
            "expected 'Agent not running' error, got {err:?}"
        );
    }
}

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
pub async fn write_to_agent(app: AppHandle, session_id: i64, data: String) -> Result<InputOutcome, String> {
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
    if outcome.activity.user_input { crate::attention_autoclear::disarm(session_id); }
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
    match crate::agent::session_lifecycle::commit_user_resumed(
        &crate::agent::session_lifecycle::DbOnlySink,
        session_id,
    ) {
        Ok(payload) => Ok(payload),
        Err(error) => {
            tracing::warn!(session_id, %error, "submitted input could not commit a lifecycle");
            Ok(None)
        }
    }
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
    if outcome.activity.user_input { crate::attention_autoclear::disarm(session_id); }
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
pub async fn send_to_agent(app: AppHandle, session_id: i64, input: String) -> Result<InputOutcome, String> {
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
    validate_handover_text(&text)?;
    // The registry — not the DB status — is the liveness gate
    // (`write_prompt_to_pty` rejects a target with no live process before
    // writing): a node still reading `pending`/`spawning` can already accept
    // input, and an archived row could not.
    crate::circuit::delivery::write_prompt_to_pty(target_node_id, &text, &app)
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

#[derive(Serialize)]
pub struct AgentDebugState {
    pub session_id: i64,
    pub is_alive: bool,
}

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
