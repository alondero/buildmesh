//! Agent spawning and process management.
//!
//! Organized into focused modules:
//! - `process.rs` — `AgentProcess` and `AgentProcessRegistry` (PTY handle storage)
//! - `spawn.rs` — `spawn_agent_inner` and provider-aware command building

pub mod process;
pub mod spawn;