//! Freebuff CLI provider adapter — interactive AI coding agent harness,
//! installed on PATH as a single `freebuff` binary (or via npm as
//! `freebuff.cmd` on Windows).
//!
//! **Provenance (issue #1437)**: Freebuff is an AI coding agent CLI built
//! on Codebuff (`https://github.com/manicode/codebuff`). Upstream configuration
//! and runtime credentials reside under `~/.config/manicode/`.
//!
//! **Interactive mode**: Opens a terminal TUI that requires a PTY for ANSI
//! rendering and raw stdin input. Buildmesh launches in interactive mode.
//!
//! **Session resumption**: Resumption is handled via `--continue <id>`.
//! Rather than attempting to scrape unpredictable PTY output, Buildmesh
//! assigns a UUID on fresh spawn (`session_assign_args` -> `["--continue", id]`)
//! and persists `cli_session_id` in SQLite. On resume, `resume_args` forwards
//! `["--continue", id]`, guaranteeing end-to-end resumption across restarts.
//!
//! **Shell wrapping**: Distributed as an npm `.cmd` batch shim on Windows
//! (`freebuff.cmd`), which `CreateProcess` will not execute directly without
//! a shell wrapper. The recipe specifies `WindowsShell::Cmd` on Windows native
//! (`cmd.exe /c freebuff …`). On macOS and Linux it executes directly via
//! `WindowsShell::Direct`.
//!
//! **Initial prompt**: Freebuff has no startup prefill, so it launches
//! plain. An issue, pull-request or prompt spawn has its prompt typed into
//! the TUI once it has booted (`agent::spawn::startup_prompt`); circuits use
//! their own post-spawn PTY injection.

use crate::agent::provider::{shell_for, AgentProvider, Platform, SpawnRecipe, UiMeta};
// The recipe tests assert the shell each platform produces; nothing outside
// `cfg(test)` names the type now that `shell_for` is shared.
#[cfg(test)]
use crate::agent::provider::WindowsShell;
use crate::models::EnvType;

pub struct FreebuffAdapter;
pub static FREEBUFF: FreebuffAdapter = FreebuffAdapter;

impl AgentProvider for FreebuffAdapter {
    fn id(&self) -> &'static str {
        "freebuff"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Freebuff".into(),
            color: "#f97316".into(),
            icon: "F".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "freebuff",
            base_args: vec![],
            trailing_args: Vec::new(),
            windows_shell: shell_for(platform),
        }
    }

    fn supports_resume(&self) -> bool {
        true
    }

    fn auto_resume_on_startup(&self) -> bool {
        true
    }

    fn requires_attention_hook(&self) -> bool {
        false
    }

    fn produces_readable_transcript(&self) -> bool {
        false
    }

    fn supports_model_override(&self) -> bool {
        false
    }

    fn supports_extra_args(&self) -> bool {
        true
    }

    fn supports_prefill(&self) -> bool {
        false
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Linux, Platform::Macos]
    }

    /// Buildmesh assigns the session UUID at spawn so `cli_session_id` is
    /// reliably stored in the database for later resumption.
    fn self_assigns_session_id(&self) -> bool {
        false
    }

    /// Fresh spawn assigns the session ID via `--continue <id>`.
    fn session_assign_args(&self, id: &str) -> Vec<String> {
        vec!["--continue".into(), id.into()]
    }

    /// Resume invocation continues the existing session via `--continue <id>`.
    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--continue".into(), id.into()]
    }
}

#[cfg(test)]
#[path = "freebuff_tests.rs"]
mod tests;
