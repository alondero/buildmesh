//! Perch CLI provider adapter — interactive coding agent installed on PATH
//! as `perch` (npm package `perchai-cli`; a `.cmd` shim on Windows).
//!
//! **Interactive mode**: `perch` opens a terminal UI. It does not take a
//! positional prompt. `perch run` is a one-shot that exits, so it is not the
//! durable node. Prefill stays off until an interactive submit is proven.
//!
//! **Session resumption**: `--thread <id>` on both a fresh spawn and a
//! resume. Buildmesh mints the id. Perch's own fallback (`cli-cwd-…`) is
//! only used when no thread is passed, which would collide across nodes in
//! one directory.
//!
//! **Permission** (CLI 2.4.116): unattended passes `--permission
//! take_the_wheel`. Prompt mode passes no flag, so the CLI keeps the saved
//! human session. The recipe itself stays bare.
//!
//! **Model and effort**: `--model` and `--effort`. Effort is the closed set
//! `off|low|medium|high|xhigh|max|ultra`. A model value is one argument,
//! including a roost tier such as `standard 1m`.
//!
//! **Shell wrapping**: the Windows npm shim needs `WindowsShell::Cmd`.
//! macOS and Linux spawn `perch` directly.

use crate::agent::capabilities::{
    EffortControlKind, PermissionModeOption, PERCH_EFFORT_ALLOWED, PERMISSION_MODE_UNATTENDED,
};
use crate::agent::provider::{shell_for, AgentProvider, Platform, SpawnRecipe, UiMeta};
use crate::models::EnvType;

pub struct PerchAdapter;
pub static PERCH: PerchAdapter = PerchAdapter;

impl AgentProvider for PerchAdapter {
    fn id(&self) -> &'static str {
        "perch"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Perch".into(),
            color: "#c2410c".into(),
            icon: "P".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "perch",
            base_args: Vec::new(),
            trailing_args: Vec::new(),
            windows_shell: shell_for(platform),
        }
    }

    fn permission_modes(&self) -> Vec<PermissionModeOption> {
        vec![
            PermissionModeOption::unattended(
                "--permission take_the_wheel",
                "Prompts off — Perch acts without asking (required for unattended runs).",
            ),
            PermissionModeOption::prompt(
                "Prompts on (no flag)",
                "Perch asks for confirmation the way a human-launched session does.",
            ),
        ]
    }

    fn permission_args(&self, mode_id: &str) -> Vec<String> {
        if mode_id == PERMISSION_MODE_UNATTENDED {
            vec!["--permission".into(), "take_the_wheel".into()]
        } else {
            Vec::new()
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
        true
    }

    fn validate_model_override(&self, model: &str) -> Result<(), String> {
        let trimmed = model.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('-')
            || trimmed.contains('\n')
            || trimmed.contains('\r')
        {
            return Err("Perch model overrides must be a model name, not a flag.".into());
        }
        Ok(())
    }

    fn effort_control(&self) -> EffortControlKind {
        EffortControlKind::Closed {
            allowed: PERCH_EFFORT_ALLOWED.iter().map(|s| s.to_string()).collect(),
        }
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

    fn self_assigns_session_id(&self) -> bool {
        false
    }

    fn session_assign_args(&self, id: &str) -> Vec<String> {
        vec!["--thread".into(), id.into()]
    }

    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--thread".into(), id.into()]
    }
}

#[cfg(test)]
#[path = "perch_tests.rs"]
mod tests;
