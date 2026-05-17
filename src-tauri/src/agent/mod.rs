//! Agent spawning and process management.
//!
//! Organized into focused modules:
//! - `process.rs` — `AgentProcess` struct and `AgentProcessRegistry`
//! - `spawn.rs` — `spawn_agent_inner` async function

pub mod process;
pub mod spawn;

pub use process::{AgentProcess, AgentProcessRegistry, PROCESS_REGISTRY};
pub use spawn::{
    spawn_agent_inner, parse_provider, build_spawn_command, inject_attention_hook,
};