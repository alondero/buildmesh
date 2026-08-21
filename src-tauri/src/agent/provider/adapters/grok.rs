//! Grok Code provider adapter — xAI's full-screen interactive coding agent,
//! installed on PATH as a single `grok` binary.
//!
//! **Interactive mode** (the default) opens a TUI that requires a PTY for
//! ANSI rendering and raw stdin input. The non-interactive `-p <prompt>` mode
//! exists but is *not* used here: the #914 prototype verified that Buildmesh's
//! PTY backend (ConPTY on Windows, native PTY on macOS/Linux) fully supports
//! Grok's TUI, so we launch in interactive mode everywhere.
//!
//! **Session resumption** uses `--resume [<id>]` / `--continue` (cwd-scoped).
//! Grok auto-assigns its own session ids (captured from PTY output by
//! `session_naming`), so `self_assigns_session_id()` is `true` and
//! `session_assign_args()` is a no-op.
//!
//! **Model override** uses `--model <model-id>` (or `-m`). Custom models are
//! configured via `[model.<name>]` blocks in `~/.grok/config.toml`.
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
        false
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Linux, Platform::Macos]
    }

    /// Grok auto-assigns session ids — captured from PTY output.
    fn self_assigns_session_id(&self) -> bool {
        true
    }

    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--resume".into(), id.into()]
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        vec!["--model".into(), model.into()]
    }

    /// No `--session-id` flag — Grok assigns its own.
    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
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
    fn self_assigns_session_id() {
        assert!(GROK.self_assigns_session_id());
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
    fn session_assign_args_empty() {
        let args = GROK.session_assign_args("any-id");
        assert!(args.is_empty(), "Grok self-assigns; session_assign_args must be empty");
    }

    #[test]
    fn no_prefill_support() {
        assert!(!GROK.supports_prefill());
    }

    // -------------------------------------------------------------------
    // Issue #1186 — capability coherence regression pins.
    //
    // Grok's CLI accepts `-m <model>` AND `--model <model>` (both forms
    // are advertised in `grok --help`); the adapter deliberately emits
    // the long form `--model`. A Buildmesh-level model override forwarded
    // via the spawn path must therefore land in the prepared recipe as
    // `--model <value>`. Mirrors the mcode precedent (issue #1179) but
    // the POSITIVE direction: where mcode's pin asserts `--model` is
    // NEVER emitted, Grok's pin asserts `--model` IS emitted when a
    // model is resolved.
    // -------------------------------------------------------------------

    /// Pin the harness-specific model-flag shape: when a model is in the
    /// resolved config, the prepared recipe MUST carry `--model` (the
    /// long form the adapter declares) followed by the model value. A
    /// future refactor that flips the adapter to the short `-m` form
    /// would still pass `capability_recipe_coherence` (which only
    /// asserts *some* model flag exists), so this sharper pin catches
    /// that drift before it reaches the wire.
    #[test]
    fn grok_interactive_recipe_carries_long_model_arg() {
        use crate::agent::capabilities::ResolvedAgentConfig;
        use crate::agent::launch::{default_prepare, HarnessLaunchInput, SessionIdModeRef};

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
        };
        let prepared = default_prepare(&GROK, input);
        let args = &prepared.recipe.base_args;

        // The long flag must appear, and the value must follow it
        // immediately (no other arg interleaved).
        let m_idx = args
            .iter()
            .position(|a| a == "--model")
            .expect("Grok prepared recipe must contain the --model flag when a model is resolved (issue #1186)");
        assert_eq!(
            args.get(m_idx + 1).map(String::as_str),
            Some("grok-3"),
            "Grok prepared recipe must put the model value immediately after --model; got args = {:?}",
            args
        );
    }

    /// Capability descriptor end-to-end pin (mirrors mcode's
    /// `capabilities_descriptor_drops_model_and_effort`). The Spawn Menu,
    /// resolver, and autopilot compatibility gate all consume this
    /// descriptor — drift here means the menu misroutes Grok.
    #[test]
    fn capabilities_descriptor_advertises_model_override() {
        let caps = GROK.capabilities();
        assert_eq!(caps.harness_id, "grok");
        assert!(caps.supports_resume);
        assert!(caps.supports_model_override);
        assert!(!caps.supports_effort_override);
        assert!(!caps.supports_prefill);
        assert!(!caps.requires_attention_hook);
        assert!(!caps.produces_readable_transcript);
        assert!(!caps.is_plain_terminal);
        assert_eq!(caps.effort_control, crate::agent::capabilities::EffortControlKind::None);
    }
}
