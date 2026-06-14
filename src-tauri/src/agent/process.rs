//! Agent process registry — thread-safe storage for live PTY handles.

use crate::pty::PtyRegistry;
use portable_pty::{Child, MasterPty};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// A live agent PTY process handle.
pub struct AgentProcess {
    pub child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    pub writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// PTY master. Wrapped in `Option` so `kill_session` can `take()` it
    /// out to drop the underlying pseudoconsole — on Windows ConPTY the
    /// master read pipe does not EOF on child exit, so the only way to
    /// unblock the reader thread is to close the pseudoconsole itself
    /// (drop the `MasterPty`). See issue #300.
    pub master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    pub reader_alive: Arc<AtomicBool>,
    /// Job Object containing the agent's whole process tree, when assignment
    /// succeeded (Windows only; `None` on failure → `kill_session` falls back to
    /// `taskkill`). Killing the job reaches detached descendants — e.g. a dev
    /// server the agent orphaned — that the PPID tree walk would miss.
    pub job: Option<crate::process_util::JobHandle>,
    /// Handle to the reader thread so `kill_session` can `join()` it with a
    /// bounded timeout. Without this, the reader could remain wedged on
    /// `read()` after a kill, leaving `agent-output` events from a dead
    /// session interleaving with new ones (issue #300).
    pub reader_handle: Mutex<Option<JoinHandle<()>>>,
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
        let mut writer = agent.writer.lock().unwrap();
        writer.write_all(data).map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())
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
    pub fn kill_session(&self, session_id: i64) {
        if let Some(agent) = self.inner.get(&session_id) {
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
        }
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

#[cfg(test)]
mod tests {
    use super::*;

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
}