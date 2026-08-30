//! MiniMax Code CLI provider adapter — MiniMax's full-screen interactive
//! coding agent, installed on PATH as a single `mcode` binary.
//!
//! **Interactive mode** (the default) opens a TUI that requires a PTY for
//! ANSI rendering and raw stdin input. Buildmesh's PTY backend (ConPTY on
//! Windows, native PTY on macOS/Linux) fully supports full-screen TUI rendering,
//! so we launch in interactive mode everywhere.
//!
//! **Session resumption** uses `--session [<id>]` or `-c` / `--continue`.
//! MiniMax Code auto-assigns its own session ids (captured from PTY output by
//! `session_naming`), so `self_assigns_session_id()` is `true` and
//! `session_assign_args()` is a no-op.
//!
//! **No model override** (issue #1179). `mcode` exposes `--model
//! <provider>/<model>` on the `exec` subcommand only; the interactive TUI
//! the harness always launches rejects it. Previously this adapter
//! advertised `supports_model_override() == true` while emitting a
//! `--model` flag the active recipe did not accept — the resolver
//! passed the value through, the spawn path appended it, and the TUI
//! surfaced an upstream rejection. The coherent choice (recorded in
//! the issue thread) is to keep the interactive TUI as the supported
//! mode and drop the override. A future `mcode exec`-based launch
//! mode (with its own lifecycle work) would re-advertise the flag.
//!
//! **Prefill** is the trailing positional `[prompt]` — there is no `--prefill`
//! flag. We override `prefill_args()` to return the text as a single positional
//! arg (the trait default `["--prefill", text]` would be rejected upstream).
//!
//! **Shell wrapping**: `mcode` is distributed as a `.cmd` batch shim on
//! Windows (`mcode.cmd`), which `CreateProcess` won't run directly; the recipe
//! wraps with `WindowsShell::Cmd` -> `cmd.exe /c mcode …` on Windows. On macOS /
//! Linux it is an executable on PATH so `WindowsShell::Direct` is used.

use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;

pub struct McodeAdapter;
pub static MCODE: McodeAdapter = McodeAdapter;

/// Per-platform shell selection. Mirrors the OpenCode / Antigravity pattern.
fn shell_for(platform: Platform) -> WindowsShell {
    match platform {
        Platform::Macos | Platform::Linux => WindowsShell::Direct,
        Platform::Windows => WindowsShell::Cmd,
    }
}

impl AgentProvider for McodeAdapter {
    fn id(&self) -> &'static str {
        "mcode"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "MiniMax Code".into(),
            color: "#6366f1".into(),
            icon: "M".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "mcode",
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

    /// `false` — the interactive TUI recipe (`mcode [--session <id>]
    /// [<prompt>]`) does not accept `--model`. The flag exists on
    /// `mcode exec`, but Buildmesh does not launch that subcommand. See
    /// the module doc for the issue #1179 product decision.
    fn supports_model_override(&self) -> bool {
        false
    }

    fn supports_extra_args(&self) -> bool {
        // Issue #1358: mcode's interactive TUI still accepts arbitrary
        // CLI flags as positional args (it's a runtime, not a
        // vocab-restricted CLI like Codex). The masking defaults are
        // conservative on `supports_model_override` and
        // `supports_effort_override` (mcode's TUI rejects them) but
        // permissive on extras.
        true
    }

    fn supports_prefill(&self) -> bool {
        // mcode accepts `[prompt]` as a trailing positional on the
        // interactive TUI (and on `exec`). Override below emits the
        // text verbatim, no `--prefill` flag.
        true
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Linux, Platform::Macos]
    }

    /// MiniMax Code auto-assigns session ids — captured from PTY output.
    fn self_assigns_session_id(&self) -> bool {
        true
    }

    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--session".into(), id.into()]
    }

    /// No `--session-id` flag — MiniMax Code assigns its own.
    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        // mcode's prompt is the trailing positional `[prompt]` on the
        // interactive TUI — there is no `--prefill` flag. The trait
        // default would emit `["--prefill", text]` which mcode rejects.
        vec![text.into()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};
    use crate::agent::capabilities::ResolvedAgentConfig;

    #[test]
    fn id_and_ui_metadata() {
        assert_eq!(MCODE.id(), "mcode");
        let ui = MCODE.ui();
        assert_eq!(ui.label, "MiniMax Code");
        assert_eq!(ui.color, "#6366f1");
        assert_eq!(ui.icon, "M");
    }

    #[test]
    fn spawn_recipe_direct_on_macos_and_linux() {
        for platform in [Platform::Linux, Platform::Macos] {
            let recipe = MCODE.spawn_recipe(platform, EnvType::Windows);
            assert_eq!(recipe.binary, "mcode");
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
        let recipe = MCODE.spawn_recipe(Platform::Windows, EnvType::Windows);
        assert_eq!(recipe.binary, "mcode");
        assert!(recipe.base_args.is_empty());
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Cmd),
            "Windows must use WindowsShell::Cmd for the .cmd shim — got {:?}",
            recipe.windows_shell
        );
    }

    #[test]
    fn available_on_all_three_platforms() {
        let platforms = MCODE.available_on();
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
        assert!(MCODE.self_assigns_session_id());
    }

    #[test]
    fn resume_args_format() {
        let args = MCODE.resume_args("abc-123");
        assert_eq!(args, vec!["--session", "abc-123"]);
    }

    #[test]
    fn session_assign_args_empty() {
        let args = MCODE.session_assign_args("any-id");
        assert!(
            args.is_empty(),
            "MiniMax Code self-assigns; session_assign_args must be empty"
        );
    }

    #[test]
    fn prefill_args_is_positional() {
        // mcode accepts `[prompt]` as a positional on the TUI — there is
        // no `--prefill` flag. The trait default (`vec!["--prefill", t]`)
        // is wrong here.
        let args = MCODE.prefill_args("fix the auth bug");
        assert_eq!(args, vec!["fix the auth bug"]);
    }

    #[test]
    fn prefill_args_preserves_multiline_text() {
        // Issue/handover prefills are often multi-line; the adapter must not
        // collapse or wrap them — they're appended verbatim.
        let multi = "first line\nsecond line\n  indented";
        let args = MCODE.prefill_args(multi);
        assert_eq!(args, vec![multi]);
    }

    #[test]
    fn supports_prefill_via_positional() {
        assert!(MCODE.supports_prefill());
    }

    /// Issue #1179: mcode does not advertise a model override. Even if a
    /// resolved value somehow reached the launch helper, the prepared
    /// recipe must never carry a `--model` flag — the interactive TUI
    /// rejects it. The capability descriptor and the recipe are
    /// required to agree.
    #[test]
    fn supports_resume_but_no_model_override_after_issue_1179() {
        assert!(MCODE.supports_resume());
        assert!(!MCODE.supports_model_override());
        assert!(!MCODE.requires_attention_hook());
    }

    /// Pin the capability descriptor end-to-end: the harness-id,
    /// `supports_model_override = false`, and the absence of effort /
    /// attention controls. Drift here means the Spawn Menu or autopilot
    /// compatibility gate will misroute mcode.
    #[test]
    fn capabilities_descriptor_drops_model_and_effort() {
        let caps = MCODE.capabilities();
        assert_eq!(caps.harness_id, "mcode");
        assert!(caps.supports_resume);
        assert!(caps.supports_prefill);
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

    /// The recipe for the default launch mode — even with a (hypothetical)
    /// resolved model in the input — must contain no `--model` flag.
    /// This is the central coherence regression the issue asked for.
    #[test]
    fn mcode_interactive_recipe_never_carries_model_arg() {
        // Defence in depth: even if a caller bypassed the resolver mask
        // and stuffed a model into ResolvedAgentConfig, the prepared
        // recipe must not include --model, because the harness
        // advertised `supports_model_override = false`.
        let config = ResolvedAgentConfig {
            model: Some("minimax/MiniMax-Text-01".to_string()),
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
        let prepared = default_prepare(&MCODE, input);
        assert!(
            !prepared.recipe.base_args.iter().any(|a| a == "--model"),
            "mcode interactive recipe must never carry --model (issue #1179); got {:?}",
            prepared.recipe.base_args
        );
    }

    /// Cross-check the resume-mode recipe: it uses the `--session`
    /// positional and the TUI never receives `--model` even when a model
    /// is in the resolved config.
    #[test]
    fn mcode_resume_recipe_carries_session_not_model() {
        let config = ResolvedAgentConfig {
            model: Some("minimax/MiniMax-Text-01".to_string()),
            effort: None,
            extra_args: None,
        };
        let input = HarnessLaunchInput {
            platform: Platform::Windows,
            runtime: EnvType::Windows,
            session: SessionIdModeRef::Resume("abc-123"),
            config: &config,
            prefill: None,
            sandbox: false,
        };
        let prepared = default_prepare(&MCODE, input);
        assert!(
            prepared.recipe.base_args.contains(&"--session".to_string()),
            "mcode resume must include --session, got {:?}",
            prepared.recipe.base_args
        );
        assert!(prepared.recipe.base_args.contains(&"abc-123".to_string()));
        assert!(
            !prepared.recipe.base_args.iter().any(|a| a == "--model"),
            "mcode resume must not carry --model"
        );
    }

    #[test]
    fn produces_readable_transcript() {
        assert!(!MCODE.produces_readable_transcript());
    }
}
