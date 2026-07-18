//! Agent spawning and process management.
//!
//! Organized into focused modules:
//! - `detection.rs` — startup scan of `PATH`/config dirs for installed harnesses
//! - `process.rs` — `AgentProcess` and `AgentProcessRegistry` (PTY handle storage)
//! - `provider/` — `AgentProvider` trait and per-provider adapters
//! - `sandbox.rs` — macOS Seatbelt profile generation + `sandbox-exec` wrapping
//! - `session_lifecycle.rs` — single owner of state transitions (issue #132)
//! - `spawn.rs` — `spawn_agent_inner` orchestrating the above
//! - `spawn_environment.rs` — wraps a `SpawnRecipe` for the runtime `EnvType`
//! - `workspace_trust.rs` — pre-trust the spawned worktree in the agent CLI's
//!   settings so it doesn't hit the workspace-trust dialog on first prompt

pub mod detection;
pub mod process;
pub mod provider;
pub mod sandbox;
pub mod session_lifecycle;
pub mod spawn;
pub mod spawn_environment;
pub mod workspace_trust;