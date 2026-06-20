//! Plain-terminal provider — opens the OS-preferred shell (PowerShell on
//! Windows, `sh` on macOS/Linux) at the spawn path with no LLM agent
//! loop. WSL meshes are handled by `spawn_environment::wrap`, which
//! matches `env_type == EnvType::Wsl` first and prepends `wsl.exe --cd
//! <path> --` to whatever binary/args the recipe provides.
//!
//! `WindowsShell::Direct` is the right choice on Windows: we ARE the
//! shell, so there is no intermediate wrapper to mangle ANSI. (Codex
//! uses `WindowsShell::PowerShell` because its binary needs ANSI
//! propagation through ConPTY; we don't have that problem.)
//! Empty `base_args` keeps the spawned command literal —
//! `["powershell.exe"]` on Windows or `["sh"]` on macOS/Linux — and
//! matches the shape `build_shell_command` uses for the Build/Run
//! "Terminal in worktree" option, so user expectations align.

use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};

pub struct TerminalAdapter;
pub static TERMINAL: TerminalAdapter = TerminalAdapter;

impl AgentProvider for TerminalAdapter {
    fn id(&self) -> &'static str {
        "terminal"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "Terminal".into(),
            color: "#9ca3af".into(),
            icon: "terminal-prompt".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform) -> SpawnRecipe {
        match platform {
            Platform::Windows => SpawnRecipe {
                binary: "powershell.exe",
                base_args: vec![],
                windows_shell: WindowsShell::Direct,
            },
            Platform::Macos | Platform::Linux => SpawnRecipe {
                binary: "sh",
                base_args: vec![],
                windows_shell: WindowsShell::Direct,
            },
        }
    }

    fn supports_resume(&self) -> bool {
        false
    }

    fn auto_resume_on_startup(&self) -> bool {
        false
    }

    fn requires_attention_hook(&self) -> bool {
        false
    }

    fn supports_model_override(&self) -> bool {
        false
    }

    fn supports_prefill(&self) -> bool {
        false
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Windows, Platform::Macos, Platform::Linux]
    }

    fn is_plain_terminal(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the Windows recipe. `WindowsShell::Direct` matters: future
    /// refactors of `spawn_environment::wrap` shouldn't be tempted to
    /// "improve" this into a `PowerShell` wrapping, which would change
    /// the argv-quoting semantics (and the `-NoLogo` flag) and break
    /// the symmetry with the Build/Run "Terminal in worktree" path.
    #[test]
    fn terminal_spawn_recipe_powershell_direct_on_windows() {
        let recipe = TERMINAL.spawn_recipe(Platform::Windows);
        assert_eq!(recipe.binary, "powershell.exe");
        assert!(recipe.base_args.is_empty(), "terminal must spawn PowerShell with no extra args");
        assert!(matches!(recipe.windows_shell, WindowsShell::Direct));
    }

    /// macOS uses `sh` so it's available on every macOS install without
    /// depending on the user's login shell (zsh, fish, etc.).
    #[test]
    fn terminal_spawn_recipe_sh_direct_on_macos() {
        let recipe = TERMINAL.spawn_recipe(Platform::Macos);
        assert_eq!(recipe.binary, "sh");
        assert!(recipe.base_args.is_empty());
        assert!(matches!(recipe.windows_shell, WindowsShell::Direct));
    }

    /// Same as macOS — `sh` is universally available on Linux and
    /// matches the choice the existing Build/Run "Terminal in worktree"
    /// option makes.
    #[test]
    fn terminal_spawn_recipe_sh_direct_on_linux() {
        let recipe = TERMINAL.spawn_recipe(Platform::Linux);
        assert_eq!(recipe.binary, "sh");
        assert!(recipe.base_args.is_empty());
        assert!(matches!(recipe.windows_shell, WindowsShell::Direct));
    }

    /// All capability flags are `false` for a plain terminal — it has
    /// no LLM agent loop, no resume, no prefill, no model override, and
    /// no attention hook (the `.claude/settings.local.json` injection is
    /// for the Claude Code Notification hook, which a plain shell
    /// doesn't need). `is_plain_terminal` is the only `true` flag.
    #[test]
    fn terminal_capabilities_all_false_except_is_plain_terminal() {
        assert!(!TERMINAL.supports_resume());
        assert!(!TERMINAL.auto_resume_on_startup());
        assert!(!TERMINAL.requires_attention_hook());
        assert!(!TERMINAL.supports_model_override());
        assert!(!TERMINAL.supports_prefill());
        assert!(TERMINAL.is_plain_terminal());
    }

    /// Terminal is available on every host platform we ship to.
    #[test]
    fn terminal_available_on_three_platforms() {
        let platforms = TERMINAL.available_on();
        assert_eq!(platforms.len(), 3);
        assert!(platforms.contains(&Platform::Windows));
        assert!(platforms.contains(&Platform::Macos));
        assert!(platforms.contains(&Platform::Linux));
    }

    /// Pin the id and UI metadata. The `icon: "terminal-prompt"` string
    /// is not currently rendered by the frontend (which keys off the
    /// provider id via `INLINE_ICONS`), but pinning the contract here
    /// makes a future renderer that does key on the icon char safe.
    #[test]
    fn terminal_id_and_ui_metadata() {
        assert_eq!(TERMINAL.id(), "terminal");
        let ui = TERMINAL.ui();
        assert_eq!(ui.label, "Terminal");
        assert_eq!(ui.color, "#9ca3af");
        assert_eq!(ui.icon, "terminal-prompt");
    }
}
