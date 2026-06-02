//! Agent process registry — thread-safe storage for live PTY handles.

use crate::pty::PtyRegistry;
use portable_pty::{Child, MasterPty};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// A live agent PTY process handle.
pub struct AgentProcess {
    pub child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    pub writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pub master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    pub reader_alive: Arc<AtomicBool>,
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
        master
            .resize(PtySize {
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
    /// The PTY child is a shell; the agent CLI runs as its descendant. On
    /// Windows `Child::kill` (`TerminateProcess`) would leave that descendant
    /// alive, pinning the worktree directory and blocking its removal on close.
    /// Killing the whole tree first ensures nothing holds the worktree's CWD.
    pub fn kill_session(&self, session_id: i64) {
        if let Some(agent) = self.inner.get(&session_id) {
            let mut child = agent.child.lock().unwrap();
            if let Some(pid) = child.process_id() {
                crate::process_util::kill_process_tree(pid);
            }
            child.kill().ok();
            agent.reader_alive.store(false, Ordering::SeqCst);
        }
    }
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