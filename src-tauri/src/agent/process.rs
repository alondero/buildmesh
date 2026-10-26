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

mod input;
pub(crate) use input::*;
mod registry;
pub use registry::*;

mod commands;
pub use commands::*;

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

#[path = "terminal_input.rs"]
mod terminal_input;
pub use terminal_input::{
    InputActivity, InputDisposition, InputOutcome, InputQueueGauge, InputWriteError,
    PTY_INPUT_QUEUE_BYTE_CAPACITY,
};
use terminal_input::{TerminalInput, UNKNOWN_INPUT_BUFFER_LEN};

/// Every field [`AgentProcess::new`] needs for a registry entry, in the order
/// the spawn path supplies them.
pub struct AgentProcessParams {
    pub child: Box<dyn Child + Send + Sync>,
    pub writer_tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    pub queue: Arc<InputQueueGauge>,
    pub writer_handle: Option<JoinHandle<()>>,
    pub master: Box<dyn MasterPty + Send>,
    pub reader_alive: Arc<AtomicBool>,
    pub deliberate_kill: Arc<AtomicBool>,
    pub job: Option<crate::process_util::JobHandle>,
    pub reader_handle: Option<JoinHandle<()>>,
    pub spawn_start: std::time::Instant,
    pub mesh_id: i64,
}

impl AgentProcess {
    /// Build a registry entry. `generation` is assigned by
    /// [`AgentProcessRegistry::insert`]; pass `0` from callers.
    pub fn new(params: AgentProcessParams) -> Self {
        let AgentProcessParams {
            child,
            writer_tx,
            queue,
            writer_handle,
            master,
            reader_alive,
            deliberate_kill,
            job,
            reader_handle,
            spawn_start,
            mesh_id,
        } = params;
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
        let node = crate::db::create_agent_node(crate::db::NewAgentNode {
            mesh_id: mesh.id,
            name: "existing-handover",
            path: &mesh.path,
            branch: "main",
            env: crate::models::EnvType::Windows,
            provider: "launch/saved-reviewer",
            worktree_name: None,
            source_issue: None,
            source_pr: None,
            source_pr_pinned_sha: None,
            use_worktree: false,
            head_repo_owner: None,
            head_repo_clone_url: None,
            worktree_path: None,
            spawn_configuration: None,
        })
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

#[cfg(test)] // allow-test-in-production: retain the existing fixture and test paths during responsibility extraction
fn input_buffer_len_after(len: usize, data: &[u8]) -> usize {
    input_buffer_state_after(len, false, data).0
}

#[cfg(test)] // allow-test-in-production: retain the existing fixture and test paths during responsibility extraction
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


