//! Freebuff CLI provider adapter — interactive AI coding agent harness,
//! installed on PATH as a single `freebuff` binary (or via npm as
//! `freebuff.cmd` on Windows).
//!
//! **Interactive mode** (the default) opens a TUI that requires a PTY for
//! ANSI rendering and raw stdin input. Buildmesh launches in interactive
//! mode everywhere.
//!
//! **Session resumption** uses `--continue <id>`. Freebuff assigns its own
//! session IDs (self-assigns), so `self_assigns_session_id()` returns
//! `true` and `session_assign_args()` is a no-op.
//!
//! **Shell wrapping**: `freebuff` is distributed as a `.cmd` batch shim on
//! Windows (`freebuff.cmd`), which `CreateProcess` won't run directly; the
//! recipe wraps with `WindowsShell::Cmd` → `cmd.exe /c freebuff …` on
//! Windows. On macOS / Linux it is an executable on PATH so
//! `WindowsShell::Direct` is used.
//!
//! **Prefill** is the trailing positional `[prompt]` — there is no
//! `--prefill` flag. We override `prefill_args()` to return the text as a
//! single positional arg.

use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;

pub struct FreebuffAdapter;
pub static FREEBUFF: FreebuffAdapter = FreebuffAdapter;

/// Per-platform shell selection. Mirrors the MiniMax Code / DeepSeek pattern.
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

    /// Freebuff auto-assigns session ids — captured from PTY output.
    fn self_assigns_session_id(&self) -> bool {
        true
    }

    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--continue".into(), id.into()]
    }

    /// No `--session-id` flag — freebuff assigns its own.
    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        // freebuff's prompt is the trailing positional — there is no
        // `--prefill` flag. The trait default would emit `["--prefill", text]`
        // which freebuff rejects.
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
                "{:?} must use WindowsShell::Direct — got {:?}",
                platform,
                recipe.windows_shell
            );
        }
    }

    #[test]
    fn spawn_recipe_cmd_on_windows() {
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
    fn available_on_all_three_platforms() {
        let platforms = FREEBUFF.available_on();
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
    fn self_assigns_session_id() {
        assert!(FREEBUFF.self_assigns_session_id());
    }

    #[test]
    fn resume_args_format() {
        let args = FREEBUFF.resume_args("abc-123");
        assert_eq!(args, vec!["--continue", "abc-123"]);
    }

    #[test]
    fn session_assign_args_empty() {
        let args = FREEBUFF.session_assign_args("any-id");
        assert!(
            args.is_empty(),
            "Freebuff self-assigns; session_assign_args must be empty"
        );
    }

    #[test]
    fn prefill_args_is_positional() {
        // freebuff accepts `[prompt]` as a positional — there is no
        // `--prefill` flag. The trait default (`vec!["--prefill", t]`)
        // is wrong here.
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
    fn supports_resume_but_no_model_override() {
        assert!(FREEBUFF.supports_resume());
        assert!(!FREEBUFF.supports_model_override());
        assert!(!FREEBUFF.requires_attention_hook());
        assert!(FREEBUFF.supports_prefill());
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
    fn freebuff_interactive_recipe_never_carries_model_arg() {
        // Even if a caller bypassed the resolver mask and stuffed a model into
        // ResolvedAgentConfig, the prepared recipe must not include --model,
        // because the harness advertised `supports_model_override = false`.
        let config = ResolvedAgentConfig {
            model: Some("some-model".to_string()),
            effort: None,
            extra_args: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Macos,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(&FREEBUFF, input);
        assert!(
            !prepared.recipe.base_args.iter().any(|a| a == "--model"),
            "freebuff interactive recipe must never carry --model; got {:?}",
            prepared.recipe.base_args
        );
    }

    #[test]
    fn freebuff_resume_recipe_carries_continue_flag() {
        let config = ResolvedAgentConfig {
            model: None,
            effort: None,
            extra_args: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Windows,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::Resume("sess-abc"),
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(&FREEBUFF, input);
        assert!(
            prepared.recipe.base_args.contains(&"--continue".to_string()),
            "freebuff resume must include --continue, got {:?}",
            prepared.recipe.base_args
        );
        assert!(prepared.recipe.base_args.contains(&"sess-abc".to_string()));
        assert!(
            !prepared.recipe.base_args.iter().any(|a| a == "--model"),
            "freebuff resume must not carry --model"
        );
    }

    #[test]
    fn produces_readable_transcript_is_false() {
        assert!(!FREEBUFF.produces_readable_transcript());
    }
}
