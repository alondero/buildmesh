//! Agent spawning — the `spawn_agent_inner` function and its helpers.
//!
//! Provider-specific recipe lives behind `Provider::adapter()` (see `agent/provider`).
//! OS-specific wrapping lives in `spawn_environment`.
//! This facade preserves the established `agent::spawn::*` API while the
//! implementation is divided by responsibility: intent resolution and the
//! four-phase pipeline in `orchestrator` (prepare context, provision
//! workspace, launch PTY/process, register/start streams), command
//! construction in `command`, process sandboxing/hooks in `process`,
//! PTY reader lifecycle in `reader`, and Tauri payloads in `wire`.

mod command;
mod intent;
mod process;
pub(crate) use intent::{
    format_issue_prefill_with_url, ExplicitSpawnOverrides, GitHubWorkContext, ResumeCause,
    SpawnIntent, SpawnOutcome, SpawnRequest, TerminalSize, WorktreePolicy,
};
mod launch;
mod orchestrator;
mod prepare;
mod provision;
mod reader;
mod streams;
mod wire;

pub use command::{build_spawn_command, build_spawn_command_prepared};
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use command::{cascade_inputs_for, resolve_spawn_config};
pub(crate) use orchestrator::spawn_with_intent;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use orchestrator::{decide_startup_resume, spawn_agent_inner, ResumeSkipDecision};
pub(crate) use prepare::resolve_base_ref_for_spawn;
pub use prepare::DEFAULT_WORKTREE_MODE;
pub use process::{inject_attention_hook, is_agent_already_running, spawn_child};
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use reader::{maybe_buffer_for_naming, post_exit_action, PostExitAction};
pub use reader::{open_pty_pair, pump_pty_output, SessionIdMode, EARLY_EXIT_WINDOW};
pub use wire::{
    AgentOutputPayload, AgentSpawnedPayload, MeshSyncOutcome, MeshSyncWarningPayload,
    ProviderErrorPayload,
};

#[cfg(test)]
mod command_tests;
#[cfg(test)]
mod orchestrator_tests;
#[cfg(test)]
mod process_tests;
#[cfg(test)]
mod provision_tests;
#[cfg(test)]
mod reader_tests;
