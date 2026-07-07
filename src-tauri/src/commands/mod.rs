//! Tauri command modules

/// Offload a blocking command body onto the blocking thread pool so a stalled
/// network / git call can't park a Tauri async worker.
///
/// The GitHub probes (`reqwest::blocking`) and git shell-outs (`git fetch`)
/// have no business occupying the bounded tokio worker pool: enough of them
/// stuck at once starves it and every other async command (agent keystrokes,
/// further probes) stops being polled — the overnight-freeze failure mode.
/// Each such command is split into a plain-sync core (`*_blocking`, still
/// callable directly by the mobile HTTP routes, which run their own accept
/// loop) plus a thin `#[command] async fn` wrapper that threads the core
/// through here. Mirrors the `spawn_blocking` usage in `usage.rs` /
/// `agent_node.rs`.
pub(crate) async fn run_blocking<T, F>(label: &'static str, f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("{label} task failed: {e}"))?
}

pub mod agent;
pub mod clipboard;
#[cfg(test)]
pub mod agent_tests;
pub mod agent_node;
pub mod agent_node_discovery;
pub mod ai_context;
pub mod attention;
pub mod build_run;
pub mod coordinator;
pub mod devices;
pub mod diff;
pub mod file_tree;
pub mod file_watcher;
pub mod frontend_log;
pub mod github;
pub mod git;
#[cfg(test)]
pub mod git_tests;
pub mod mesh;
pub mod mesh_properties;
pub mod network;
pub mod preferences;
pub mod pr;
pub mod project_detect;
pub mod prune;
pub mod remote;
pub mod scratchpad;
pub mod test;
pub mod usage;
