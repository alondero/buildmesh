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

use crate::agent::session_lifecycle::{self, SessionLifecycleSink as _};
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
    /// without limit; full sends are dropped with a warn-level log.
    pub writer_tx: std::sync::mpsc::SyncSender<Vec<u8>>,
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
}

impl AgentProcess {
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
}

/// Trait abstracting the process registry methods needed by http_server.
pub trait ProcessRegistryApi: Send + Sync {
    fn write_bytes(&self, session_id: i64, data: &[u8]) -> Result<(), String>;
    fn resize_pty(&self, session_id: i64, cols: u16, rows: u16) -> Result<(), String>;
}

/// Thread-safe registry for agent processes.
/// Wraps `PtyRegistry<i64, AgentProcess>` and exposes typed methods
/// for write/resize that return Result instead of Option.
pub struct AgentProcessRegistry {
    inner: PtyRegistry<i64, AgentProcess>,
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

    pub fn write_bytes(&self, session_id: i64, data: &[u8]) -> Result<(), String> {
        let agent = self.get(&session_id).ok_or_else(|| "Agent not running".to_string())?;
        // Issue #1122: non-blocking enqueue. The actual `write_all`+`flush`
        // happens on a dedicated OS thread (one per agent) that owns the
        // underlying `Box<dyn Write + Send>`. The previous design held
        // `agent.writer` mutex through the actual write — a full ConPTY
        // pipe could park the async runtime for the entire write duration,
        // reintroducing the latency this PR is meant to fix. With this
        // channel split, the Tauri command does at most one bounded
        // `try_send` per keystroke and returns immediately.
        //
        // `try_send` (not `send`) so a stuck agent can't back-pressure
        // the async runtime. A full channel means the writer thread is
        // still draining a slow PTY; we drop the new bytes with a warn
        // (the user can re-type). Bound is 64 entries × ~tens of bytes
        // — a few KB of in-flight data, well within the PTY pipe buffer.
        match agent.writer_tx.try_send(data.to_vec()) {
            Ok(()) => {}
            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                tracing::warn!(
                    session_id,
                    "PTY writer channel full; dropping {} bytes (agent is slow to consume the PTY)",
                    data.len()
                );
                // Drop the bytes and still return Ok — the user can re-type;
                // failing the call would surface as a confusing "Agent not
                // running" toast and silently lose keystrokes.
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                // The writer thread has exited (kill_session has run and
                // the registry entry is mid-removal). Signal the caller
                // the same way the old code did on a pipe failure.
                return Err("Agent not running".to_string());
            }
        }
        // Mark THIS MESH as active so the background warm-pool worker holds
        // off its idle refills for this mesh's pool while the user is typing
        // into the terminal (issue #613 AC2; issue #634 scopes the activity
        // per-mesh so typing into mesh A's terminal doesn't prevent mesh B's
        // pool from being refilled). Recorded after a successful enqueue so
        // a dropped (full-channel) write doesn't count as activity.
        crate::services::pool_worker::note_activity_for_mesh(agent.mesh_id);
        // Emit the `first_user_input` checkpoint exactly once per session.
        // We do this AFTER a successful enqueue so a failed PTY write
        // (broken pipe, etc.) does NOT claim "user input accepted". The
        // helper is the only place the flag flips and the log line fires —
        // see its doc comment for the atomic contract and the
        // coordinator-drive caveat.
        record_first_input_if_first(&agent.first_user_input_logged, agent.spawn_start, session_id);
        Ok(())
    }

    pub fn resize_pty(&self, session_id: i64, cols: u16, rows: u16) -> Result<(), String> {
        use portable_pty::PtySize;
        let agent = self.get(&session_id).ok_or_else(|| "Agent not running".to_string())?;
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

    pub fn insert(&self, session_id: i64, agent: AgentProcess) {
        self.inner.insert(session_id, Arc::new(agent));
    }

    pub fn remove(&self, session_id: &i64) -> Option<Arc<AgentProcess>> {
        self.inner.remove(session_id)
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
    /// Close order is load-bearing (issue #300):
    ///
    /// 1. **Drop the PTY master first.** On Windows ConPTY the master read
    ///    pipe does not EOF when the child exits — conhost holds it open
    ///    until the pseudoconsole itself closes (the `MasterPty` drops).
    ///    `take()`-ing the `Option` here drops the inner `Box<dyn MasterPty>`,
    ///    which closes the pseudoconsole and EOFs the reader thread's
    ///    `read()`. Skipping this step wedges the reader indefinitely
    ///    (the pre-fix bug).
    /// 2. **Kill the process tree** (Job Object → `taskkill` fallback →
    ///    `child.kill`) so descendants release their CWD and the worktree
    ///    can be removed.
    /// 3. **Mark the reader dead** and **join the reader thread** with a
    ///    bounded timeout. The bounded join protects the close path from
    ///    any future regression that re-wedges the reader — we never want
    ///    `kill_session` to hang the UI thread.
    ///
    /// Must not touch the node-scoped PTY output Channel. Fresh spawn
    /// calls this before the child exists (step 2 of `spawn_agent_inner`);
    /// unregistering here drops the terminal's subscription and the new
    /// reader buffers bytes the viewport never sees.
    pub fn kill_session(&self, session_id: i64) {
        if let Some(agent) = self.inner.get(&session_id) {
            // 0. Flag the teardown as deliberate BEFORE closing anything,
            //    so the reader thread — EOFed by the master drop below —
            //    is guaranteed to observe the flag when its epilogue runs.
            //    See the `deliberate_kill` field doc for why the reader
            //    must not apply the early-exit Error heuristic here.
            agent.deliberate_kill.store(true, Ordering::SeqCst);

            // 1. Drop the master. `Option::take` removes the `Box<dyn MasterPty>`
            //    from the mutex; the binding falls out of scope and drops
            //    it, closing the pseudoconsole. We don't hold the mutex
            //    across the rest of the function so `resize_pty` callers on
            //    a racing agent don't block.
            if let Ok(mut master_guard) = agent.master.lock() {
                master_guard.take();
            }

            // 2. Kill the process tree. Job Object first is authoritative
            //    (reaches detached descendants `taskkill /T` can't); the
            //    `kill_process_tree` is a fallback for the rare spawn
            //    case where job assignment failed; `child.kill` reaps the
            //    shell handle portable-pty owns.
            if let Some(job) = &agent.job {
                job.terminate();
            }
            {
                let mut child = agent.child.lock().unwrap();
                if let Some(pid) = child.process_id() {
                    crate::process_util::kill_process_tree(pid);
                }
                child.kill().ok();
            }

            // 3. Mark the reader dead (it's the reader itself that flips
            //    this on a clean path; flipping here is a belt-and-braces
            //    against a reader that gets stuck after master close).
            agent.reader_alive.store(false, Ordering::SeqCst);

            // 4. Take the reader handle and join with a bounded timeout.
            //    On timeout the watchdog thread is detached (the inner
            //    `JoinHandle` is dropped when the closure ends, per
            //    `JoinHandle::drop` docs), so we never leak a process or
            //    wedge the close path.
            if let Some(handle) = agent.reader_handle.lock().unwrap().take() {
                join_with_timeout(handle, std::time::Duration::from_secs(2));
            }

            // 5. Issue #1122: drop the dedicated writer thread. The channel
            //    sender is in `agent.writer_tx`; when the registry entry is
            //    dropped (after `kill_session` returns), the sender drops,
            //    the channel closes, and the writer thread's `recv()` returns
            //    Err — its loop exits. We additionally drop the handle here
            //    so the close-path symmetry mirrors the reader join; if the
            //    writer thread is mid-write on a dying PTY, the bounded join
            //    protects the UI from a wedged worker.
            if let Some(handle) = agent.writer_handle.lock().unwrap().take() {
                join_with_timeout(handle, std::time::Duration::from_secs(2));
            }
        }

        // Sandbox cleanup (issue #498/#528): revoke the node's restricted-token
        // worktree ACE grant. No-op for unsandboxed sessions. Runs after the
        // process tree is dead so nothing is still using the granted directory.
        #[cfg(target_os = "windows")]
        crate::sandbox::spawn::cleanup_restricted(session_id);
    }
}

/// Join a thread, detaching it (via the watchdog) if it hasn't returned in
/// `timeout`. Used by `kill_session` so the close path can never hang the UI
/// on a stuck reader thread.
///
/// We can't `JoinHandle::join_timeout` (the method doesn't exist on stable),
/// so we run the join on a watchdog thread and wait on a oneshot channel.
/// The watchdog outlives the timeout only if the reader is genuinely stuck;
/// the thread itself is cheap (it's idle in `join`) and exits as soon as the
/// reader does. The inner `JoinHandle` is dropped at the end of the
/// watchdog's closure, which detaches the reader per `JoinHandle::drop` docs.
fn join_with_timeout(handle: JoinHandle<()>, timeout: std::time::Duration) {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let _watchdog = std::thread::spawn(move || {
        let _ = handle.join();
        // If the receiver is gone (we timed out), the send errors silently;
        // the `JoinHandle` is still dropped on closure exit, detaching the
        // reader thread.
        let _ = tx.send(());
    });
    let _ = rx.recv_timeout(timeout);
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
    fn write_bytes(&self, session_id: i64, data: &[u8]) -> Result<(), String> {
        AgentProcessRegistry::write_bytes(self, session_id, data)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::provider::{SpawnRecipe, WindowsShell};
    use crate::agent::spawn::{open_pty_pair, spawn_child};
    use crate::agent::spawn_environment;
    use crate::models::EnvType;

    #[test]
    fn insert_and_get() {
        let registry = AgentProcessRegistry::new();
        assert!(registry.is_empty());
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

        assert!(logged, "first call must return true (the log line was emitted)");
        assert!(flag.load(Ordering::SeqCst), "first call must flip the flag to true");
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
        let (writer_tx, writer_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(1);
        let writer_thread = std::thread::spawn(move || {
            let mut writer = writer;
            while let Ok(bytes) = writer_rx.recv() {
                let _ = writer.write_all(&bytes);
                let _ = writer.flush();
            }
        });
        registry.insert(
            -915_4002,
            AgentProcess {
                child: Arc::new(Mutex::new(child)),
                writer_tx,
                writer_handle: Mutex::new(Some(writer_thread)),
                master: Arc::new(Mutex::new(Some(pair.master))),
                reader_alive: Arc::new(AtomicBool::new(true)),
                deliberate_kill: deliberate_kill.clone(),
                job: None,
                reader_handle: Mutex::new(None),
                spawn_start: std::time::Instant::now(),
                first_user_input_logged: AtomicBool::new(false),
                mesh_id: 0,
            },
        );

        registry.kill_session(-915_4002);

        assert!(
            deliberate_kill.load(Ordering::SeqCst),
            "kill_session must set deliberate_kill before closing the PTY, \
             so the reader epilogue skips the early-exit Error heuristic"
        );
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

        // The watcher polls every 500ms. With a 0-byte command the
        // child exits well inside the first poll, so the master
        // should be dropped well within 2s.
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
// `spawn_handover_agent`, `auto_resume_agent_nodes`, `list_autopilot_runs`
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
        PROCESS_REGISTRY.remove(&id);
        crate::http_server::clear_scrollback(id);
        tracing::info!("kill_all_sessions: killed agent for session {}", id);
    }
}

#[command]
pub async fn resize_agent(session_id: i64, rows: u16, cols: u16) -> Result<(), String> {
    PROCESS_REGISTRY.resize_pty(session_id, cols, rows)
}

#[command]
pub async fn write_to_agent(app: AppHandle, session_id: i64, data: String) -> Result<(), String> {
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
    // 2. Slow path: only when the data contains a newline / carriage
    //    return (the user pressed Enter, pasted a multi-line buffer, or
    //    sent a Ctrl-M). Then we ask the DB whether the node is a plain
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
    let contains_newline = data.bytes().any(|b| b == b'\n' || b == b'\r');
    PROCESS_REGISTRY.write_bytes(session_id, data.as_bytes())?;
    // Any accepted keystroke means the user is engaged with this node —
    // the stale-mark hypothesis behind auto-clear (issue #878) no longer
    // holds, and the keystroke's own echo must not count toward the
    // resume burst.
    crate::attention_autoclear::disarm(session_id);
    if !contains_newline {
        return Ok(());
    }
    let should_signal = crate::commands::run_blocking(
        "write_to_agent_signal",
        move || write_to_agent_signal_blocking(session_id),
    )
    .await?;
    if should_signal {
        // Route through the SessionLifecycle sink so all `attention-cleared`
        // emits pass through one owner — matches the invariant in
        // `session_lifecycle.rs` that no caller emits lifecycle events
        // directly. (`on_attention_cleared` would also write `Running`
        // status, which `write_to_agent` intentionally doesn't — user
        // input doesn't by itself mark the node as no longer awaiting.)
        let sink = session_lifecycle::AppSessionLifecycleSink { app: &app };
        sink.emit_attention_cleared(session_id);
    }
    Ok(())
}

/// Slow-path DB work for [`write_to_agent`]. The PTY write and autoclear
/// disarm already ran on the async runtime by the time this is called;
/// only the attention-cleared transition is left, and that requires a DB
/// read (to skip plain-shell providers) and a DB write (to flip status
/// out of `AwaitingInput`). Returns `Ok(true)` when the caller should
/// emit `attention-cleared`.
///
/// The read is here because plain-shell providers have no LLM attention
/// state to clear — a shell's Enter is just shell input, and flipping
/// status to `Running` would render a spurious "Running" badge for a
/// shell sitting at a prompt (issue #535).
pub(crate) fn write_to_agent_signal_blocking(session_id: i64) -> Result<bool, String> {
    if should_skip_attention_signals(session_id) {
        return Ok(false);
    }
    // Status write routes through SessionLifecycle (issue #132). The
    // sink here is `DbOnlySink` because the blocking core has no
    // `AppHandle`; the corresponding `attention-cleared` emit is the
    // caller's responsibility (the `should_signal` flag tells the
    // caller to emit, preserving the original behaviour where the
    // emit lived in the async wrapper).
    session_lifecycle::on_attention_cleared(&session_lifecycle::DbOnlySink, session_id)
        .ok();
    Ok(true)
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
pub(crate) fn write_to_agent_blocking(
    session_id: i64,
    data: String,
) -> Result<bool, String> {
    PROCESS_REGISTRY.write_bytes(session_id, data.as_bytes())?;
    crate::attention_autoclear::disarm(session_id);
    let should_signal = data.bytes().any(|b| b == b'\n' || b == b'\r')
        && !should_skip_attention_signals(session_id);
    if should_signal {
        session_lifecycle::on_attention_cleared(
            &session_lifecycle::DbOnlySink,
            session_id,
        )
        .ok();
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
pub async fn send_to_agent(app: AppHandle, session_id: i64, input: String) -> Result<(), String> {
    write_to_agent(app, session_id, format!("{}\n", input)).await
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
    PROCESS_REGISTRY.kill_session(session_id);
    PROCESS_REGISTRY.remove(&session_id);
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
