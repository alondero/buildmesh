//! Cursor CLI provider adapter — Cursor's interactive coding agent installed
//! on PATH as `cursor-agent`.
//!
//! Cursor's default interactive mode runs inside Buildmesh's PTY. The same
//! recipe works across Windows ConPTY and native macOS/Linux PTYs; Windows
//! uses `cmd.exe` because Cursor's installer exposes a `.cmd` shim.
//!
//! Cursor assigns session UUIDs itself and persists workspace-scoped JSONL
//! transcripts. Fresh spawns therefore omit a session-id argument, while
//! archived sessions resume with `--resume <id>`.

use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;

pub struct CursorAdapter;
pub static CURSOR: CursorAdapter = CursorAdapter;

fn shell_for(platform: Platform) -> WindowsShell {
    match platform {
        Platform::Macos | Platform::Linux => WindowsShell::Direct,
        Platform::Windows => WindowsShell::Cmd,
    }
}

impl AgentProvider for CursorAdapter {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Cursor Agent".into(),
            // Cursor's current brand palette uses a warm near-black surface;
            // the frontend renders the official cube mark in its light
            // variant through the brand registry.
            color: "#1B1913".into(),
            icon: "C".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "cursor-agent",
            // Cursor documents `--force` as allowing commands unless denied.
            // `--trust` is a print-mode option, not an interactive-TUI flag.
            base_args: vec!["--force".into()],
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
        // Cursor writes compatible JSONL, but the shared reader and archive
        // discovery do not yet know its ~/.cursor/projects/ layout.
        false
    }

    fn supports_model_override(&self) -> bool {
        true
    }

    fn supports_prefill(&self) -> bool {
        true
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Linux, Platform::Macos]
    }

    fn self_assigns_session_id(&self) -> bool {
        true
    }

    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    fn resume_args(&self, id: &str) -> Vec<String> {
        vec!["--resume".into(), id.into()]
    }

    fn model_args(&self, model: &str) -> Vec<String> {
        vec!["--model".into(), model.into()]
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        // Cursor accepts the initial prompt as a positional argument in
        // interactive mode; `--prefill` is not a Cursor CLI flag.
        vec![text.into()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_and_ui_metadata() {
        assert_eq!(CURSOR.id(), "cursor");
        let ui = CURSOR.ui();
        assert_eq!(ui.label, "Cursor Agent");
        assert_eq!(ui.color, "#1B1913");
        assert_eq!(ui.icon, "C");
    }

    #[test]
    fn spawn_recipe_uses_cmd_only_on_windows() {
        let windows = CURSOR.spawn_recipe(Platform::Windows, EnvType::Windows);
        assert_eq!(windows.binary, "cursor-agent");
        assert_eq!(windows.base_args, vec!["--force"]);
        assert!(matches!(windows.windows_shell, WindowsShell::Cmd));

        for platform in [Platform::Linux, Platform::Macos] {
            let recipe = CURSOR.spawn_recipe(platform, EnvType::Windows);
            assert_eq!(recipe.binary, "cursor-agent");
            assert_eq!(recipe.base_args, vec!["--force"]);
            assert!(
                matches!(recipe.windows_shell, WindowsShell::Direct),
                "{platform:?} must use WindowsShell::Direct"
            );
        }
    }

    #[test]
    fn available_on_all_three_platforms() {
        let platforms = CURSOR.available_on();
        assert_eq!(platforms.len(), 3);
        assert!(platforms.contains(&Platform::Windows));
        assert!(platforms.contains(&Platform::Linux));
        assert!(platforms.contains(&Platform::Macos));
    }

    #[test]
    fn self_assigns_session_id_and_resumes_by_id() {
        assert!(CURSOR.self_assigns_session_id());
        assert!(CURSOR.session_assign_args("any-id").is_empty());
        assert_eq!(CURSOR.resume_args("abc-123"), vec!["--resume", "abc-123"]);
    }

    #[test]
    fn supports_model_override_and_positional_prefill() {
        assert!(CURSOR.supports_model_override());
        assert_eq!(
            CURSOR.model_args("claude-3-7-sonnet"),
            vec!["--model", "claude-3-7-sonnet"]
        );
        assert!(CURSOR.supports_prefill());
        assert_eq!(
            CURSOR.prefill_args("inspect the repo"),
            vec!["inspect the repo"]
        );
    }

    #[test]
    fn defers_transcript_support_until_reader_wiring() {
        // Cursor writes compatible JSONL, but the shared reader and archive
        // discovery do not yet know its ~/.cursor/projects/ layout.
        assert!(!CURSOR.produces_readable_transcript());
        assert!(!CURSOR.requires_attention_hook());
    }
}
