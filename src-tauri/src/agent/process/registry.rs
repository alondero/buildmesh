//! Registry ownership, incarnation fencing, and process-tree teardown.
//!
//! Registry mutation and teardown share one ownership protocol; keeping the claim,
//! close, reap, and join sequence here prevents callers from bypassing its fences.

use super::*;
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
    pub(super) inner: PtyRegistry<i64, AgentProcess>,
}

/// Monotonic token source for [`AgentProcess::generation`]. Starts at 1 so a
/// literal `generation: 0` is visibly "not yet inserted".
pub(super) static NEXT_PROCESS_GENERATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

/// Whether teardown should join the reader/writer threads. `WriterOnly` is
/// the natural-exit reaping path (runs on the reader, must not join itself);
/// `Both` is `kill_session`. Module-local on purpose (round-3 review
/// finding #5): agent has separate reader and writer workers, so it
/// needs an extra `Both` / `WriterOnly` distinction that build_run doesn't
/// carry. Each module owns its enum; [`crate::pty::lifecycle`] exports only
/// the shared bounded-join helper [`crate::pty::lifecycle::join_with_timeout`]
/// (no enum).
pub(super) enum JoinPolicy {
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

    pub(super) fn write_input_with_framing(
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
    pub(super) fn remove_if_current(
        &self,
        session_id: i64,
        generation: u64,
    ) -> Option<Arc<AgentProcess>> {
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

pub(super) fn sandbox_cleanup(session_id: i64) {
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
pub(super) fn teardown_incarnation(
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
