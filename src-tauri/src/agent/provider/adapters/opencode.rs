//! OpenCode provider adapter — open-source terminal coding agent installed on
//! PATH as a single `opencode` binary. The binary shape differs by host:
//!
//! - **Windows**: distributed as `opencode.cmd` (resolved via `PATHEXT`);
//!   `CreateProcess` won't run a `.cmd` directly, so the recipe wraps with
//!   `WindowsShell::Cmd` → `cmd.exe /c opencode …` (see
//!   `spawn_environment::wrap`'s `WindowsShell::Cmd` branch).
//! - **macOS / Linux**: a real executable on PATH; no wrapper needed, so
//!   `WindowsShell::Direct` is the right choice (on those platforms
//!   `spawn_environment::wrap` short-circuits to a direct spawn anyway).
//!
//! Prior to issue #827 the adapter hardcoded `WindowsShell::Cmd` for *all*
//! platforms — fine on Windows, but on Linux `cmd.exe /c opencode` doesn't
//! exist, so the provider was advertised as available yet silently failed
//! at spawn time. The adapter also omitted `Platform::Macos` from
//! `available_on()`, so the spawn menu on macOS never showed OpenCode even
//! when the binary was installed — users on macOS were told nothing.

use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe, UiMeta, WindowsShell};
use crate::models::EnvType;

pub struct OpenCodeAdapter;
pub static OPENCODE: OpenCodeAdapter = OpenCodeAdapter;

/// Per-platform shell selection. Mirrors the Codex adapter's pattern.
fn shell_for(platform: Platform) -> WindowsShell {
    match platform {
        Platform::Macos | Platform::Linux => WindowsShell::Direct,
        Platform::Windows => WindowsShell::Cmd,
    }
}

impl AgentProvider for OpenCodeAdapter {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "OpenCode".into(),
            color: "#f59e0b".into(),
            icon: "O".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "opencode",
            base_args: vec![],
            windows_shell: shell_for(platform),
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
        &[Platform::Windows, Platform::Linux, Platform::Macos]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// macOS always direct-spawns the binary via the `cfg!(target_os = "macos")`
    /// short-circuit in `spawn_environment::wrap` (the `windows_shell` field is
    /// never consulted on macOS hosts). Linux reaches the `WindowsShell::Direct`
    /// arm of the wrap function's `match`, which likewise direct-spawns. So both
    /// platforms produce a direct spawn — no shell wrapper needed because
    /// `opencode` is a real executable on PATH on both.
    ///
    /// Regression for issue #827: before the fix `spawn_recipe` ignored its
    /// `platform` arg and returned `WindowsShell::Cmd` on Linux, so a
    /// native-Linux mesh got `cmd.exe /c opencode …` — silent failure at
    /// spawn time on a platform that was advertised as supported.
    #[test]
    fn spawn_recipe_direct_on_macos() {
        let recipe = OPENCODE.spawn_recipe(Platform::Macos, EnvType::Windows);
        assert_eq!(recipe.binary, "opencode");
        assert!(recipe.base_args.is_empty());
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Direct),
            "macOS must use WindowsShell::Direct — got {:?}",
            recipe.windows_shell
        );
    }

    #[test]
    fn spawn_recipe_direct_on_linux() {
        let recipe = OPENCODE.spawn_recipe(Platform::Linux, EnvType::Windows);
        assert_eq!(recipe.binary, "opencode");
        assert!(recipe.base_args.is_empty());
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Direct),
            "Linux must use WindowsShell::Direct — got {:?}",
            recipe.windows_shell
        );
    }

    /// Windows keeps `WindowsShell::Cmd` because `opencode` is a `.cmd` batch
    /// file on Windows; `CreateProcess` won't execute it directly. Pinning
    /// this guards against the easy mistake of normalising every provider
    /// onto `Direct` and breaking the Windows path.
    #[test]
    fn spawn_recipe_cmd_on_windows() {
        let recipe = OPENCODE.spawn_recipe(Platform::Windows, EnvType::Windows);
        assert_eq!(recipe.binary, "opencode");
        assert!(
            matches!(recipe.windows_shell, WindowsShell::Cmd),
            "Windows must use WindowsShell::Cmd for the .cmd shim — got {:?}",
            recipe.windows_shell
        );
    }

    /// `available_on` must list every platform OpenCode runs on. Pre-#827
    /// macOS was missing, so the spawn-menu composer in
    /// `agent::provider_menu::available_providers` filtered OpenCode out for a
    /// macOS user even when the binary was detected at startup. Pin the exact
    /// set (not just membership) so a future "while we're here" addition
    /// forces an explicit test update — matches the equivalent assertion on
    /// `TERMINAL` (`terminal_available_on_three_platforms`).
    #[test]
    fn available_on_includes_macos() {
        let platforms = OPENCODE.available_on();
        assert_eq!(
            platforms.len(),
            3,
            "available_on should pin to exactly {{Windows, Linux, Macos}} — got {:?}",
            platforms
        );
        assert!(platforms.contains(&Platform::Macos), "OpenCode must be available on macOS (issue #827); got {:?}", platforms);
        assert!(platforms.contains(&Platform::Linux));
        assert!(platforms.contains(&Platform::Windows));
    }

    /// Pin id + UI metadata so the wire type / frontend icon mapping can't
    /// drift silently (matches the equivalent assertion on `TERMINAL`).
    #[test]
    fn id_and_ui_metadata() {
        assert_eq!(OPENCODE.id(), "opencode");
        let ui = OPENCODE.ui();
        assert_eq!(ui.label, "OpenCode");
        assert_eq!(ui.color, "#f59e0b");
        assert_eq!(ui.icon, "O");
    }
}
