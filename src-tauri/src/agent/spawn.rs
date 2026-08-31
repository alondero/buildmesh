//! Agent spawning — the `spawn_agent_inner` function and its helpers.
//!
//! Provider-specific recipe lives behind `Provider::adapter()` (see `agent/provider`).
//! OS-specific wrapping lives in `spawn_environment`.
//! This facade preserves the established `agent::spawn::*` API while the
//! implementation is divided by responsibility: intent resolution and the
//! high-level pipeline in `orchestrator`, worktree integration in `provision`,
//! PTY process/reader lifecycle in `reader`, and Tauri payloads in `wire`.

mod intent;
pub(crate) use intent::{
    ExplicitSpawnOverrides, GitHubWorkContext, ResumeCause, SpawnIntent, SpawnOutcome,
    SpawnRequest, TerminalSize,
};
mod orchestrator;
mod provision;
mod reader;
mod wire;

pub use orchestrator::SpawnOptions;
#[allow(unused_imports)]
pub(crate) use orchestrator::{
    cascade_inputs_for, decide_startup_resume, resolve_spawn_config, spawn_agent_inner,
    spawn_with_intent, ResumeSkipDecision,
};
pub(crate) use provision::resolve_base_ref_for_spawn;
pub use provision::DEFAULT_WORKTREE_MODE;
pub use reader::{
    build_spawn_command, build_spawn_command_prepared, inject_attention_hook,
    is_agent_already_running, open_pty_pair, pump_pty_output, spawn_child, SessionIdMode,
    EARLY_EXIT_WINDOW,
};
#[allow(unused_imports)]
pub(crate) use reader::{maybe_buffer_for_naming, post_exit_action, PostExitAction};
pub use wire::{
    AgentOutputPayload, AgentSpawnedPayload, MeshSyncOutcome, MeshSyncWarningPayload,
    ProviderErrorPayload,
};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_reader;
