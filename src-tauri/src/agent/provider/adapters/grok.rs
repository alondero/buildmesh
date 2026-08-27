//! Grok Code provider adapter — xAI's full-screen interactive coding agent,
//! installed on PATH as a single `grok` binary.
//!
//! **Interactive mode** (the default) opens a TUI that requires a PTY for
//! ANSI rendering and raw stdin input. The non-interactive `-p <prompt>` mode
//! exists but is *not* used here: the #914 prototype verified that Buildmesh's
//! PTY backend (ConPTY on Windows, native PTY on macOS/Linux) fully supports
//! Grok's TUI, so we launch in interactive mode everywhere.
//!
//! **Session IDs** follow ADR-0024. Fresh spawns mint a UUID and pass
//! `--session-id <uuid>` (Grok 1.0.5: create-only; errors if the ID already
//! exists under the cwd). Resume uses `--resume <id>`. `--continue` exists
//! but is unused — auto-resume always passes the stored id explicitly.
//!
//! **Prefill** is the trailing positional `[PROMPT]` on the interactive TUI
//! (`grok "fix the bug"`). There is no `--prefill` flag; `-p`/`--single` is
//! headless (print and exit) and is not used here.
//!
//! **Model override** uses `-m <model-id>` / `--model <model-id>` (`grok
//! --help` advertises the long form; the adapter emits it). Grok Code
//! accepts Buildmesh-level model overrides passed via the spawn path —
//! the `--model <model>` flag is forwarded to the Grok CLI, which then
//! runs that model for the invocation (overriding the harness's
//! `[model.<name>]` default in `~/.grok/config.toml` for that one
//! session). Custom models are configured via `[model.<name>]` blocks
//! in `~/.grok/config.toml`; Buildmesh does not manage those.
//!
//! **Shell wrapping**: `grok` is a native binary on all platforms (not a
//! `.cmd` shim), so `WindowsShell::Direct` is correct everywhere — matching
//! the AGY adapter pattern.

use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;

pub struct GrokAdapter;
pub static GROK: GrokAdapter = GrokAdapter;

impl AgentProvider for GrokAdapter {
    fn id(&self) -> &'static str {
        "grok"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Grok Code".into(),
            // Official xAI brand colour from
            // https://x.ai/legal/brand-guidelines (Feb 14, 2025).
            // Paired with the white Grok Logomark at src/assets/providers/grok.svg
            // for a high-contrast (WCAG AAA) mobile avatar chip.
            color: "#0A0A0A".into(),
            icon: "X".into(),
        }
    }

    fn spawn_recipe(&self, _platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "grok",
            base_args: vec![],
            windows_shell: WindowsShell::Direct,
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

    fn supports_model_override(&self) -> bool {
        true
    }

    fn supports_prefill(&self) -> bool {
        // Interactive TUI accepts a trailing positional [PROMPT] as the
        // first turn (`grok "fix the bug"`). There is no `--prefill` flag;
        // override `prefill_args` below. Headless `-p`/`--single` is a
        // different mode (print and exit) and is not used here.
        true
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Linux, Platform::Macos]
    }

    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--resume".into(), id.into()]
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        vec!["--model".into(), model.into()]
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        // Trailing positional [PROMPT] on the interactive TUI. The trait
        // default would emit `["--prefill", text]`, which grok rejects.
        vec![text.into()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_and_ui_metadata() {
        assert_eq!(GROK.id(), "grok");
        let ui = GROK.ui();
        assert_eq!(ui.label, "Grok Code");
        assert_eq!(ui.color, "#0A0A0A");
        assert_eq!(ui.icon, "X");
    }

    #[test]
    fn spawn_recipe_direct_on_all_platforms() {
        for platform in [Platform::Windows, Platform::Linux, Platform::Macos] {
            let recipe = GROK.spawn_recipe(platform, EnvType::Windows);
            assert_eq!(recipe.binary, "grok");
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
    fn available_on_all_three_platforms() {
        let platforms = GROK.available_on();
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
    fn assigns_session_id_via_cli_flag() {
        // Grok 1.0.5 accepts `-s/--session-id <UUID>` to create a new
        // session (create-only; resume is `--resume`). ADR-0024: we mint
        // the UUID and pass it, rather than scraping PTY output.
        assert!(!GROK.self_assigns_session_id());
        assert_eq!(
            GROK.session_assign_args("550e8400-e29b-41d4-a716-446655440000"),
            vec!["--session-id", "550e8400-e29b-41d4-a716-446655440000"]
        );
    }

    #[test]
    fn resume_args_format() {
        let args = GROK.resume_args("abc-123");
        assert_eq!(args, vec!["--resume", "abc-123"]);
    }

    #[test]
    fn model_args_format() {
        let args = GROK.model_args("grok-3");
        assert_eq!(args, vec!["--model", "grok-3"]);
    }

    #[test]
    fn supports_prefill_via_positional() {
        // Interactive TUI takes a trailing [PROMPT] as the first turn
        // (`grok "fix the bug"`). There is no `--prefill` flag; emitting
        // the trait default would be rejected upstream.
        assert!(GROK.supports_prefill());
        assert_eq!(GROK.prefill_args("fix the auth bug"), vec!["fix the auth bug"]);
    }

    #[test]
    fn prefill_args_preserves_multiline_text() {
        let multi = "first line\nsecond line\n  indented";
        assert_eq!(GROK.prefill_args(multi), vec![multi]);
    }

    /// Interactive TUI prefill is the trailing positional, never
    /// `--prefill`. The table-driven coherence check accepts either
    /// shape; this pin forbids the Claude-shaped flag.
    #[test]
    fn grok_interactive_recipe_carries_positional_prefill() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};

        let config = ResolvedAgentConfig::default();
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: Some("fix the auth bug in handler.rs"),
            sandbox: false,
        };
        let prepared = default_prepare(&GROK, input);
        let args = &prepared.recipe.base_args;
        assert_eq!(
            args.last().map(String::as_str),
            Some("fix the auth bug in handler.rs"),
            "Grok prefill must be the trailing positional; got {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "--prefill"),
            "Grok has no --prefill flag; got {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "-p" || a == "--single"),
            "Grok prefill must stay on the interactive TUI, not headless -p; got {args:?}"
        );
    }

    /// Fresh spawn (Assign) must pass `--session-id <uuid>` so the
    /// orchestrator owns the ID before launch. `--resume` must not appear.
    #[test]
    fn grok_assign_recipe_carries_session_id_flag() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{assert_flag_followed_by_value, default_prepare, HarnessLaunchInput, SessionIdModeRef};

        let config = ResolvedAgentConfig::default();
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::Assign(id),
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(&GROK, input);
        let args = &prepared.recipe.base_args;
        assert_flag_followed_by_value(args, "--session-id", id);
        assert!(
            !args.iter().any(|a| a == "--resume"),
            "Assign must not emit --resume; got {args:?}"
        );
    }

    /// Resume keeps `--resume <id>` (not `-s`) and still appends a
    /// positional prefill as the trailing arg.
    #[test]
    fn grok_resume_recipe_keeps_resume_flag_and_positional_prefill() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{assert_flag_followed_by_value, default_prepare, HarnessLaunchInput, SessionIdModeRef};

        let config = ResolvedAgentConfig {
            model: Some("grok-3".to_string()),
            effort: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::Resume("01a0400a-6ac5-7d90-a1a6-b5397ff81d62"),
            config: &config,
            prefill: Some("continue from the last turn"),
            sandbox: false,
        };
        let prepared = default_prepare(&GROK, input);
        let args = &prepared.recipe.base_args;
        assert_flag_followed_by_value(args, "--resume", "01a0400a-6ac5-7d90-a1a6-b5397ff81d62");
        assert_flag_followed_by_value(args, "--model", "grok-3");
        assert_eq!(
            args.last().map(String::as_str),
            Some("continue from the last turn")
        );
        assert!(
            !args.iter().any(|a| a == "--session-id"),
            "Resume must not also assign; got {args:?}"
        );
    }

    /// Issue #1186: pin the harness-specific model-flag shape. The
    /// table-driven `capability_recipe_coherence` only asserts *some*
    /// model flag exists in the recipe — a silent `-m` ↔ `--model`
    /// flip on the adapter would pass. This pin catches the drift
    /// before it reaches the wire.
    #[test]
    fn grok_interactive_recipe_carries_long_model_arg() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{assert_flag_followed_by_value, default_prepare, HarnessLaunchInput, SessionIdModeRef};

        let config = ResolvedAgentConfig {
            model: Some("grok-3".to_string()),
            effort: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Linux,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::None,
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(&GROK, input);
        assert_flag_followed_by_value(&prepared.recipe.base_args, "--model", "grok-3");
    }

    /// Issue #1179 (mirror): end-to-end descriptor pin. The Spawn Menu,
    /// resolver, and autopilot compatibility gate all consume this
    /// descriptor — drift here means the menu misroutes Grok.
    #[test]
    fn capabilities_descriptor_advertises_model_override() {
        let caps = GROK.capabilities();
        assert_eq!(caps.harness_id, "grok");
        assert!(caps.supports_resume);
        assert!(caps.supports_model_override);
        assert!(!caps.supports_effort_override);
        assert!(caps.supports_prefill);
        assert!(!caps.requires_attention_hook);
        assert!(!caps.produces_readable_transcript);
        assert!(!caps.is_plain_terminal);
        assert_eq!(caps.effort_control, crate::agent::capabilities::EffortControlKind::None);
    }
}
