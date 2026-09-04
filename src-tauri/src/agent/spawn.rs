//! Agent spawning — the `spawn_agent_inner` function and its helpers.
//!
//! Provider-specific recipe lives behind `Provider::adapter()` (see `agent/provider`).
//! OS-specific wrapping lives in `spawn_environment`.
//! This facade preserves the established `agent::spawn::*` API while the
//! implementation is divided by responsibility: intent resolution and
//! phase coordination in `orchestrator` (prepare returns workspace
//! params + launch params; provision takes only git/disk inputs;
//! launch takes the provisioned workspace plus launch params;
//! streams register the process). Command construction lives in
//! `command`, process sandboxing/hooks in `process`, PTY reader
//! lifecycle in `reader`, Tauri payloads in `wire`.

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
pub(crate) use orchestrator::spawn_with_intent;
pub(crate) use prepare::resolve_base_ref_for_spawn;
pub use prepare::DEFAULT_WORKTREE_MODE;
pub use process::{
    inject_attention_hook, inject_opencode_attention_plugin, is_agent_already_running,
    spawn_child, OPENCODE_ATTENTION_PLUGIN,
};
#[cfg(test)]
pub(crate) use reader::maybe_buffer_for_naming;
pub use reader::{open_pty_pair, pump_pty_output, SessionIdMode, EARLY_EXIT_WINDOW};
pub use wire::{
    AgentOutputPayload, AgentSpawnedPayload, MeshSyncOutcome, MeshSyncWarningPayload,
    ProviderErrorPayload,
};

#[cfg(test)]
mod command_tests;
#[cfg(test)]
mod launch_tests;
#[cfg(test)]
mod orchestrator_tests;
#[cfg(test)]
mod prepare_tests;
#[cfg(test)]
mod process_tests;
#[cfg(test)]
mod provision_tests;
#[cfg(test)]
mod reader_tests;
#[cfg(test)]
mod streams_tests;
