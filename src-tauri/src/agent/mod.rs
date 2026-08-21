//! Agent spawning and process management.
//!
//! Organized into focused modules:
//! - `detection.rs` — startup scan of `PATH`/config dirs for installed harnesses
//! - `process.rs` — process-lifecycle home: `AgentProcess`/`AgentProcessRegistry`
//!   storage plus the Tauri commands (`kill_agent`, `write_to_agent`,
//!   `resize_agent`, `send_to_agent`, `is_agent_running`, `debug_*`) that
//!   orchestrate a live registry entry.
//! - `provider/` — `AgentProvider` trait and per-provider adapters
//! - `provider_menu.rs` — Spawn Menu composition (`compose_provider_menu`,
//!   `order_providers`, `order_proxied_children`, `available_providers`,
//!   `list_providers` Tauri command)
//! - `sandbox.rs` — macOS Seatbelt profile generation + `sandbox-exec` wrapping
//! - `session_lifecycle.rs` — single owner of state transitions (issue #132)
//! - `spawn.rs` — `spawn_agent_inner` orchestrating the above
//! - `spawn_environment.rs` — wraps a `SpawnRecipe` for the runtime `EnvType`
//! - `workspace_trust.rs` — pre-trust the spawned worktree in the agent CLI's
//!   settings so it doesn't hit the workspace-trust dialog on first prompt

pub mod capabilities;
pub mod detection;
pub mod launch;
pub mod launch_routing;
pub mod process;
pub mod provider;
pub mod provider_menu;
pub mod sandbox;
pub mod session_lifecycle;
pub mod spawn;
pub mod spawn_environment;
pub mod workspace_trust;
