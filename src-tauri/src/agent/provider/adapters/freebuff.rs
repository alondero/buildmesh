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
//! **Prefill**: Freebuff takes its initial prompt as trailing positional
//! text (`[prompt]`). `prefill_args()` formats the string as a single
//! positional argument (no `--prefill` flag).

use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;

pub struct FreebuffAdapter;
pub static FREEBUFF: FreebuffAdapter = FreebuffAdapter;

/// Shell selection per platform. Windows native uses `cmd.exe /c` to wrap
/// the npm `.cmd` batch shim, while Unix platforms invoke the binary directly.
fn shell_for(platform: Platform) -> WindowsShell {
    match platform {
        Platform::Macos | Platform::Linux => WindowsShell::Direct,
        Platform::Windows => WindowsShell::Cmd,
    }
}

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
        true
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

    fn prefill_args(&self, text: &str) -> Vec<String> {
        // Freebuff prompt is the trailing positional argument.
        vec![text.into()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::capabilities::ResolvedAgentConfig;
    use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};

    #[test]
    fn id_and_ui_metadata() {
        assert_eq!(FREEBUFF.id(), "freebuff");
        let ui = FREEBUFF.ui();
        assert_eq!(ui.label, "Freebuff");
        assert_eq!(ui.color, "#f97316");
        assert_eq!(ui.icon, "F");
    }

    #[test]
    fn spawn_recipe_direct_on_macos_and_linux() {
        for platform in [Platform::Linux, Platform::Macos] {
            let recipe = FREEBUFF.spawn_recipe(platform, EnvType::Windows);
            assert_eq!(recipe.binary, "freebuff");
            assert!(recipe.base_args.is_empty());
            assert!(
                matches!(recipe.windows_shell, WindowsShell::Direct),
                "{platform:?} must use WindowsShell::Direct — got {:?}",
                recipe.windows_shell
            );
        }
    }

    #[test]
    fn spawn_recipe_cmd_on_windows_native() {
        let recipe = FREEBUFF.spawn_recipe(Platform::Windows, EnvType::Windows);
        assert_eq!(recipe.binary, "freebuff");
        assert!(recipe.base_args.is_empty());
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Cmd),
            "Windows must use WindowsShell::Cmd for the .cmd shim — got {:?}",
            recipe.windows_shell
        );
    }

    #[test]
    fn spawn_recipe_on_windows_host_with_wsl_runtime() {
        let recipe = FREEBUFF.spawn_recipe(Platform::Windows, EnvType::Wsl);
        assert_eq!(recipe.binary, "freebuff");
        // Windows platform emits WindowsShell::Cmd in adapter, runtime wrapping
        // by spawn_environment handles wsl.exe redirection.
        assert!(matches!(recipe.windows_shell, WindowsShell::Cmd));
    }

    #[test]
    fn available_on_all_three_platforms() {
        let platforms = FREEBUFF.available_on();
        assert_eq!(platforms.len(), 3);
        assert!(platforms.contains(&Platform::Windows));
        assert!(platforms.contains(&Platform::Linux));
        assert!(platforms.contains(&Platform::Macos));
    }

    #[test]
    fn does_not_self_assign_session_id() {
        // Must be false so Buildmesh generates the session UUID on fresh spawn.
        assert!(!FREEBUFF.self_assigns_session_id());
    }

    #[test]
    fn session_assign_args_format() {
        let args = FREEBUFF.session_assign_args("sess-1234");
        assert_eq!(args, vec!["--continue", "sess-1234"]);
    }

    #[test]
    fn resume_args_format() {
        let args = FREEBUFF.resume_args("sess-1234");
        assert_eq!(args, vec!["--continue", "sess-1234"]);
    }

    #[test]
    fn prefill_args_is_positional() {
        let args = FREEBUFF.prefill_args("fix the auth bug");
        assert_eq!(args, vec!["fix the auth bug"]);
    }

    #[test]
    fn prefill_args_preserves_multiline_text() {
        let multi = "first line\nsecond line\n  indented";
        let args = FREEBUFF.prefill_args(multi);
        assert_eq!(args, vec![multi]);
    }

    #[test]
    fn capabilities_descriptor_shape() {
        let caps = FREEBUFF.capabilities();
        assert_eq!(caps.harness_id, "freebuff");
        assert!(caps.supports_resume);
        assert!(caps.auto_resume_on_startup);
        assert!(caps.supports_prefill);
        assert!(caps.supports_extra_args);
        assert!(!caps.supports_model_override);
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
    fn fresh_spawn_prepare_wires_session_assign_args() {
        let config = ResolvedAgentConfig {
            model: None,
            effort: None,
            extra_args: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Windows,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::Assign("fresh-uuid-99"),
            config: &config,
            prefill: Some("start work"),
            sandbox: false,
        };
        let prepared = default_prepare(&FREEBUFF, input);
        assert_eq!(
            prepared.recipe.base_args,
            vec!["--continue", "fresh-uuid-99", "start work"],
            "fresh spawn must pass assigned session id via --continue and prefill positionally"
        );
    }

    #[test]
    fn resume_prepare_wires_resume_args() {
        let config = ResolvedAgentConfig {
            model: None,
            effort: None,
            extra_args: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::Resume("existing-uuid-100"),
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(&FREEBUFF, input);
        assert_eq!(
            prepared.recipe.base_args,
            vec!["--continue", "existing-uuid-100"],
            "resume spawn must pass resume args via --continue"
        );
    }

    #[test]
    fn produces_readable_transcript_is_false() {
        assert!(!FREEBUFF.produces_readable_transcript());
    }
}
