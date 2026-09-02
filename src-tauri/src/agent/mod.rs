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
//! - `spawn.rs` — `spawn_agent_inner` sequencing prepare / provision /
//!   launch / streams over the modules below
//! - `spawn_environment.rs` — wraps a `SpawnRecipe` for the runtime `EnvType`
//! - `workspace_trust.rs` — pre-trust the spawned worktree in the agent CLI's
//!   settings so it doesn't hit the workspace-trust dialog on first prompt

pub mod capabilities;
pub mod detection;
pub mod launch;
pub mod launch_routing;
pub mod output;
pub mod process;
pub mod provider;
pub mod provider_menu;
pub mod sandbox;
pub mod session_lifecycle;
pub mod spawn;
pub mod spawn_environment;
pub mod workspace_trust;

use std::sync::OnceLock;

/// Process-scoped hook token store (issue #1366). A single
/// module-level [`OnceLock`] backs both [`runtime_hook_token`] and
/// [`mint_runtime_hook_token`] — a previous revision kept one static
/// per function, which produced two separate allocations and made
/// `runtime_hook_token` always return `None` regardless of minting.
/// Allocating at module scope (and reading from the same `static`)
/// gives us a single source of truth.
///
/// Each Buildmesh runtime mints its own token lazily when its first
/// Grok agent spawns ([`grok::provision_attention_hooks`]); subsequent
/// Grok hook URLs in this process carry
/// `?token=$BUILDMESH_HOOK_TOKEN` and the attention route verifies
/// the presented value against the value stored here. A non-Buildmesh
/// Grok session cannot guess the token, and a different Buildmesh
/// runtime's token mismatches. Until the runtime spawns its first
/// Grok agent, this returns `None` and the route gate is permissive
/// for Claude / Codex / AGY callbacks that don't carry a token.
static RUNTIME_HOOK_TOKEN: OnceLock<String> = OnceLock::new();

/// Return the current runtime hook token, if one has been minted.
pub fn runtime_hook_token() -> Option<String> {
    RUNTIME_HOOK_TOKEN.get().cloned()
}

/// Mint the runtime hook token on demand. Idempotent: the first
/// caller fixes the token for the rest of the process; subsequent
/// callers see the same value. The token is 32 lowercase hex chars
/// (16 random bytes) — long enough to make accidental guessing
/// negligible against the loopback-only peer check.
pub fn mint_runtime_hook_token() -> String {
    RUNTIME_HOOK_TOKEN
        .get_or_init(|| {
            use rand::Rng;
            let mut rng = rand::rng();
            let bytes: [u8; 16] = std::array::from_fn(|_| rng.random());
            bytes.iter().map(|b| format!("{:02x}", b)).collect()
        })
        .clone()
}
