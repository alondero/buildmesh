//! Command Code CLI provider adapter — neuro-symbolic coding agent CLI
//! installed on PATH as `cmdc` on Windows (a `.cmd` batch shim) and `cmd`
//! on macOS / Linux / WSL (wayfinder #1394, ticket #1397).
//!
//! **Platform binary & shell wrapping**:
//! - **Windows**: distributed as `cmdc.cmd` (or `cmdc.exe`); `CreateProcess`
//!   won't run a `.cmd` directly, so the recipe wraps with `WindowsShell::Cmd`
//!   → `cmd.exe /c cmdc …` (see `spawn_environment::wrap`'s
//!   `WindowsShell::Cmd` branch).
//! - **macOS / Linux / WSL**: real executable `cmd` on PATH; no wrapper needed,
//!   so `WindowsShell::Direct` is used.
//!
//! **Session lifecycle**:
//! Command Code auto-assigns session IDs (`self_assigns_session_id() -> true`),
//! so `session_assign_args()` is empty. Resumption uses `--session <id>`.
//!
//! **Model & prefill**:
//! Accepts `--model <name>` for model overrides and positional prompt
//! text for prefill queries.

use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;

pub struct CommandCodeAdapter;
pub static COMMANDCODE: CommandCodeAdapter = CommandCodeAdapter;

/// Per-platform shell selection. Mirrors the OpenCode / MiniMax pattern.
fn shell_for(platform: Platform) -> WindowsShell {
    match platform {
        Platform::Macos | Platform::Linux => WindowsShell::Direct,
        Platform::Windows => WindowsShell::Cmd,
    }
}

/// Binary name selection per platform and runtime environment.
/// On Windows native, Command Code installs as `cmdc` to avoid colliding with
/// `System32\cmd.exe`. In WSL or Unix, it installs as `cmd`.
fn binary_for(platform: Platform, env_type: EnvType) -> &'static str {
    match (platform, env_type) {
        (_, EnvType::Wsl) => "cmd",
        (Platform::Windows, _) => "cmdc",
        (Platform::Macos | Platform::Linux, _) => "cmd",
    }
}

impl AgentProvider for CommandCodeAdapter {
    fn id(&self) -> &'static str {
        "commandcode"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Command Code".into(),
            color: "#8C4EDD".into(),
            icon: "C".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform, env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: binary_for(platform, env_type),
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

    fn supports_extra_args(&self) -> bool {
        true
    }

    fn supports_prefill(&self) -> bool {
        true
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Macos, Platform::Linux]
    }

    fn self_assigns_session_id(&self) -> bool {
        true
    }

    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--session".into(), id.into()]
    }

    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        vec!["--model".into(), model.into()]
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        vec![text.into()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::capabilities::ResolvedAgentConfig;
    use crate::agent::launch::{
        assert_flag_followed_by_value, default_prepare, HarnessLaunchInput, SessionIdModeRef,
    };

    #[test]
    fn id_and_ui_metadata() {
        assert_eq!(COMMANDCODE.id(), "commandcode");
        let ui = COMMANDCODE.ui();
        assert_eq!(ui.label, "Command Code");
        assert_eq!(ui.color, "#8C4EDD");
        assert_eq!(ui.icon, "C");
    }

    #[test]
    fn spawn_recipe_cmd_on_windows_native() {
        let recipe = COMMANDCODE.spawn_recipe(Platform::Windows, EnvType::Windows);
        assert_eq!(recipe.binary, "cmdc");
        assert!(recipe.base_args.is_empty());
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Cmd),
            "Windows native must use WindowsShell::Cmd for the .cmd shim — got {:?}",
            recipe.windows_shell
        );
    }

    #[test]
    fn spawn_recipe_direct_on_macos() {
        let recipe = COMMANDCODE.spawn_recipe(Platform::Macos, EnvType::Windows);
        assert_eq!(recipe.binary, "cmd");
        assert!(recipe.base_args.is_empty());
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Direct),
            "macOS must use WindowsShell::Direct — got {:?}",
            recipe.windows_shell
        );
    }

    #[test]
    fn spawn_recipe_direct_on_linux() {
        let recipe = COMMANDCODE.spawn_recipe(Platform::Linux, EnvType::Windows);
        assert_eq!(recipe.binary, "cmd");
        assert!(recipe.base_args.is_empty());
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Direct),
            "Linux must use WindowsShell::Direct — got {:?}",
            recipe.windows_shell
        );
    }

    #[test]
    fn spawn_recipe_wsl_uses_cmd_binary() {
        let recipe = COMMANDCODE.spawn_recipe(Platform::Windows, EnvType::Wsl);
        assert_eq!(recipe.binary, "cmd");
        assert!(recipe.base_args.is_empty());
    }

    #[test]
    fn available_on_includes_all_three_platforms() {
        let platforms = COMMANDCODE.available_on();
        assert_eq!(platforms.len(), 3);
        assert!(platforms.contains(&Platform::Windows));
        assert!(platforms.contains(&Platform::Linux));
        assert!(platforms.contains(&Platform::Macos));
    }

    #[test]
    fn self_assigns_session_id() {
        assert!(COMMANDCODE.self_assigns_session_id());
        assert!(
            COMMANDCODE.captures_session_id_from_pty(),
            "Command Code captures session id from PTY output"
        );
    }

    #[test]
    fn resume_args_format() {
        let args = COMMANDCODE.resume_args("ses_cmdc_12345");
        assert_eq!(args, vec!["--session", "ses_cmdc_12345"]);
    }

    #[test]
    fn session_assign_args_empty() {
        let args = COMMANDCODE.session_assign_args("any-id");
        assert!(args.is_empty());
    }

    #[test]
    fn model_args_use_model_flag() {
        let args = COMMANDCODE.model_args("taste-1");
        assert_eq!(args, vec!["--model", "taste-1"]);
    }

    #[test]
    fn prefill_args_positional() {
        let args = COMMANDCODE.prefill_args("implement the feature");
        assert_eq!(args, vec!["implement the feature"]);
    }

    #[test]
    fn supports_expected_capabilities() {
        assert!(COMMANDCODE.supports_resume());
        assert!(COMMANDCODE.auto_resume_on_startup());
        assert!(COMMANDCODE.supports_model_override());
        assert!(COMMANDCODE.supports_extra_args());
        assert!(COMMANDCODE.supports_prefill());
        assert!(!COMMANDCODE.requires_attention_hook());
        assert!(!COMMANDCODE.produces_readable_transcript());
    }

    #[test]
    fn capabilities_descriptor_matches() {
        let caps = COMMANDCODE.capabilities();
        assert_eq!(caps.harness_id, "commandcode");
        assert!(caps.supports_resume);
        assert!(caps.auto_resume_on_startup);
        assert!(caps.supports_model_override);
        assert!(caps.supports_extra_args);
        assert!(caps.supports_prefill);
        assert!(!caps.supports_effort_override);
        assert!(!caps.requires_attention_hook);
        assert!(!caps.produces_readable_transcript);
        assert!(!caps.is_plain_terminal);
        assert_eq!(
            caps.effort_control,
            crate::agent::capabilities::EffortControlKind::None
        );
    }

    #[test]
    fn resume_recipe_carries_session_flag() {
        let config = ResolvedAgentConfig::default();
        let input = HarnessLaunchInput {
            platform: Platform::Windows,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::Resume("ses_12345"),
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(&COMMANDCODE, input);
        assert_eq!(
            prepared.recipe.base_args,
            vec!["--session".to_string(), "ses_12345".to_string()]
        );
    }

    #[test]
    fn fresh_recipe_forwards_model_and_prompt_without_session_id() {
        let config = ResolvedAgentConfig {
            model: Some("taste-1".to_string()),
            effort: None,
            extra_args: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: Some("refactor auth flow"),
            sandbox: false,
        };
        let prepared = default_prepare(&COMMANDCODE, input);
        let args = &prepared.recipe.base_args;
        assert_flag_followed_by_value(args, "--model", "taste-1");
        assert!(args.contains(&"refactor auth flow".to_string()));
        assert!(!args.iter().any(|a| a == "--session" || a == "--session-id"));
    }
}
