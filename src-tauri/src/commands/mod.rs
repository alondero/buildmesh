//! Tauri command modules

/// Offload a blocking command body onto the blocking thread pool so a stalled
/// network / git / SQLite / disk call can't park a Tauri async worker.
///
/// The GitHub probes (`reqwest::blocking`), git shell-outs (`git fetch`),
/// rusqlite transactions, and `preferences.json` reads/writes have no
/// business occupying the bounded tokio worker pool: enough of them stuck
/// at once starves it and every other async command (agent keystrokes,
/// WebSocket streaming, further probes) stops being polled — the
/// overnight-freeze failure mode. Each such command is split into a
/// plain-sync core (`*_blocking`) plus an async caller (Tauri command or
/// mobile HTTP route) that threads the core through here. Mirrors the
/// `spawn_blocking` usage in `usage.rs` / `agent_node.rs`.
///
/// Issue #1380: an async `#[command]` must not call `db::*`, `std::fs::*`,
/// or `preferences::load`/`save` on the tokio worker — wrap those through
/// here (gated by `tests/unit/async-command-blocking.test.ts`).
pub(crate) use crate::blocking::run_blocking;

pub mod agent;
pub mod app;
pub mod clipboard;
#[cfg(test)]
pub mod agent_tests;
pub mod agent_node;
pub mod agent_node_discovery;
pub mod ai_context;
pub mod ai_context_gitignore;
pub mod attention;
pub mod build_run;
pub mod circuit;
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
pub mod opencode_oauth;
pub mod preferences;
pub mod pr;
pub mod project_detect;
pub mod prune;
pub mod remote;
pub mod scratchpad;
pub mod test;
pub mod usage;
