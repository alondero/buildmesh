//! DeepSeek Harness CLI provider adapter — DeepSeek's Cordis-powered
//! interactive coding agent harness (`@deepseek-ai/dsh`), installed on PATH as
//! a single `dsh` binary.
//!
//! **Interactive mode** (the default) opens a terminal TUI/web interface that
//! requires a PTY for ANSI rendering and raw stdin input. Buildmesh launches
//! in interactive mode everywhere.
//!
//! **Session resumption** uses `--session-id <id>` (issue #1124 research).
//! Passing `--session-id <id>` resumes an existing session if present under
//! `~/.dsh/sessions/` or creates one with that ID on fresh spawn.
//!
//! **Model override** uses `--model <model>`.
//!
//! **Shell wrapping**: `dsh` is distributed as a Node.js CLI with a `.cmd`
//! batch shim on Windows (`dsh.cmd`), which `CreateProcess` won't run directly;
//! the recipe wraps with `WindowsShell::Cmd` -> `cmd.exe /c dsh …` on Windows.
//! On macOS / Linux it is an executable on PATH so `WindowsShell::Direct` is used.

use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;

pub struct DshAdapter;
pub static DSH: DshAdapter = DshAdapter;

/// Per-platform shell selection. Mirrors the OpenCode / Mcode pattern.
fn shell_for(platform: Platform) -> WindowsShell {
    match platform {
        Platform::Macos | Platform::Linux => WindowsShell::Direct,
        Platform::Windows => WindowsShell::Cmd,
    }
}

impl AgentProvider for DshAdapter {
    fn id(&self) -> &'static str {
        "dsh"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "DeepSeek Harness".into(),
            color: "#1E88E5".into(),
            icon: "D".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "dsh",
            base_args: vec![],
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
        vec!["--session-id".into(), id.into()]
    }

    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--session-id".into(), id.into()]
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        vec!["--model".into(), model.into()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_and_ui_metadata() {
        assert_eq!(DSH.id(), "dsh");
        let ui = DSH.ui();
        assert_eq!(ui.label, "DeepSeek Harness");
        assert_eq!(ui.color, "#1E88E5");
        assert_eq!(ui.icon, "D");
    }

    #[test]
    fn spawn_recipe_direct_on_macos_and_linux() {
        for platform in [Platform::Linux, Platform::Macos] {
            let recipe = DSH.spawn_recipe(platform, EnvType::Windows);
            assert_eq!(recipe.binary, "dsh");
            assert!(recipe.base_args.is_empty());
            assert!(
                matches!(recipe.windows_shell, WindowsShell::Direct),
                "{:?} must use WindowsShell::Direct — got {:?}",
                platform,
                recipe.windows_shell
            );
        }
    }

    #[test]
    fn spawn_recipe_cmd_on_windows() {
        let recipe = DSH.spawn_recipe(Platform::Windows, EnvType::Windows);
        assert_eq!(recipe.binary, "dsh");
        assert!(recipe.base_args.is_empty());
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Cmd),
            "Windows must use WindowsShell::Cmd for the .cmd shim — got {:?}",
            recipe.windows_shell
        );
    }

    #[test]
    fn available_on_all_three_platforms() {
        let platforms = DSH.available_on();
        assert_eq!(
            platforms.len(),
            3,
            "available_on should pin to exactly {{Windows, Linux, Macos}} — got {:?}",
            platforms
        );
        assert!(platforms.contains(&Platform::Windows));
        assert!(platforms.contains(&Platform::Linux));
        assert!(platforms.contains(&Platform::Macos));
    }

    #[test]
    fn does_not_self_assign_session_id() {
        assert!(!DSH.self_assigns_session_id());
    }

    #[test]
    fn session_assign_and_resume_args_format() {
        let assign_args = DSH.session_assign_args("sess-123");
        assert_eq!(assign_args, vec!["--session-id", "sess-123"]);

        let resume_args = DSH.resume_args("sess-123");
        assert_eq!(resume_args, vec!["--session-id", "sess-123"]);
    }

    #[test]
    fn model_args_format() {
        let args = DSH.model_args("deepseek-chat");
        assert_eq!(args, vec!["--model", "deepseek-chat"]);
    }

    #[test]
    fn no_prefill_support() {
        assert!(!DSH.supports_prefill());
    }

    #[test]
    fn supports_resume_and_model_override_but_no_attention_hook() {
        assert!(DSH.supports_resume());
        assert!(DSH.supports_model_override());
        assert!(!DSH.requires_attention_hook());
    }

    #[test]
    fn produces_readable_transcript() {
        assert!(!DSH.produces_readable_transcript());
    }
}
