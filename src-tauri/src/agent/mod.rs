//! Agent spawning and process management.
//!
//! Organized into focused modules:
//! - `process.rs` — `AgentProcess` and `AgentProcessRegistry` (PTY handle storage)
//! - `provider/` — `AgentProvider` trait and per-provider adapters
//! - `spawn_environment.rs` — wraps a `SpawnRecipe` for the runtime `EnvType`
//! - `spawn.rs` — `spawn_agent_inner` orchestrating the above

pub mod process;
pub mod provider;
pub mod spawn;
pub mod spawn_environment;