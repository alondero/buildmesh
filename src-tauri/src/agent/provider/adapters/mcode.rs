//! MiniMax Code CLI provider adapter — MiniMax's full-screen interactive
//! coding agent, installed on PATH as a single `mcode` binary.
//!
//! **Interactive mode** (the default) opens a TUI that requires a PTY for
//! ANSI rendering and raw stdin input. Buildmesh's PTY backend (ConPTY on
//! Windows, native PTY on macOS/Linux) fully supports full-screen TUI rendering,
//! so we launch in interactive mode everywhere.
//!
//! **Session resumption** uses `--resume [<id>]` or `-c` / `--continue`.
//! MiniMax Code auto-assigns its own session ids (captured from PTY output by
//! `session_naming`), so `self_assigns_session_id()` is `true` and
//! `session_assign_args()` is a no-op.
//!
//! **Model override** uses `-m <model-id>` / `--model <model-id>`.
//!
//! **Shell wrapping**: `mcode` is a native binary / bundled runtime on all
//! platforms (not a `.cmd` shim), so `WindowsShell::Direct` is correct everywhere —
//! matching the AGY, Grok, and Kimi adapter patterns.

use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;

pub struct McodeAdapter;
pub static MCODE: McodeAdapter = McodeAdapter;

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

    fn spawn_recipe(&self, _platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "mcode",
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

    /// MiniMax Code auto-assigns session ids — captured from PTY output.
    fn self_assigns_session_id(&self) -> bool {
        true
    }

    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--session".into(), id.into()]
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        vec!["-m".into(), model.into()]
    }

    /// No `--session-id` flag — MiniMax Code assigns its own.
    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_and_ui_metadata() {
        assert_eq!(MCODE.id(), "mcode");
        let ui = MCODE.ui();
        assert_eq!(ui.label, "MiniMax Code");
        assert_eq!(ui.color, "#6366f1");
        assert_eq!(ui.icon, "M");
    }

    #[test]
    fn spawn_recipe_direct_on_all_platforms() {
        for platform in [Platform::Windows, Platform::Linux, Platform::Macos] {
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
    fn model_args_format() {
        let args = MCODE.model_args("MiniMax-Text-01");
        assert_eq!(args, vec!["-m", "MiniMax-Text-01"]);
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
    fn no_prefill_support() {
        assert!(!MCODE.supports_prefill());
    }

    #[test]
    fn supports_resume_and_model_override_but_no_attention_hook() {
        assert!(MCODE.supports_resume());
        assert!(MCODE.supports_model_override());
        assert!(!MCODE.requires_attention_hook());
    }

    #[test]
    fn produces_readable_transcript() {
        assert!(!MCODE.produces_readable_transcript());
    }
}
