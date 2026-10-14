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
use crate::models::EnvType;

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
            // Single-printable-char contract (issue #328): the mobile badge
            // renders `icon` as the chip glyph directly from the live
            // `listProviders()` payload, so a multi-char string would overflow
            // the 34×34 chip. Sister adapters (`A`, `G`, `O`, `X`) all
            // honour the same convention.
            icon: "T".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform, env_type: EnvType) -> SpawnRecipe {
        // WSL login-shell lookup (issue #548) — for a WSL mesh, prefer the
        // user's `getent passwd` login shell (zsh, fish, bash) over the
        // system `sh` (dash on Ubuntu). Falls back to `sh` when the lookup
        // is unavailable. The lookup itself lives in `crate::env`.
        let wsl_shell = if env_type == EnvType::Wsl {
            crate::env::wsl_login_shell()
        } else {
            None
        };
        terminal_recipe(platform, env_type, wsl_shell)
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

    fn supports_extra_args(&self) -> bool {
        // Issue #1358: Terminal opens the user's interactive shell with
        // no LLM-driven recipe — splicing synthetic flags into a
        // user's interactive command line would be a footgun. The
        // resolver drops the layer-1 `extra_args` value at the mask.
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

/// Pure helper for the Terminal adapter's spawn recipe. Pulled out of
/// `spawn_recipe` so it can be unit-tested without depending on the cached
/// `wsl_login_shell()` state — on a Windows+WSL dev machine the cache holds
/// `Some("/usr/bin/zsh")` and would make the "Linux returns sh" tests
/// environment-dependent.
///
/// Decision table:
/// - `Platform::Windows` → `powershell.exe` (any `env_type`)
/// - `Platform::Linux` + `EnvType::Wsl` + `Some(shell)` → shell (e.g. `/usr/bin/zsh`)
/// - `Platform::Linux` + `EnvType::Wsl` + `None` → `sh` fallback
/// - `Platform::Linux` + `EnvType::Windows` → `sh` (native Linux mesh)
/// - `Platform::Macos` + `EnvType::Windows` → `sh` (the only realistic
///   macOS input — `env_for_path` on macOS always returns
///   `EnvType::Windows`, so the WSL branch is unreachable in practice)
fn terminal_recipe(
    platform: Platform,
    env_type: EnvType,
    wsl_shell: Option<&'static str>,
) -> SpawnRecipe {
    if platform == Platform::Windows {
        return SpawnRecipe {
            binary: "powershell.exe",
            base_args: vec![],
            trailing_args: Vec::new(),
            windows_shell: WindowsShell::Direct,
        };
    }
    // macOS or Linux. For WSL meshes, prefer the user's login shell from
    // `getent passwd` (see `crate::env::wsl_login_shell`); otherwise `sh`.
    let binary = if env_type == EnvType::Wsl {
        wsl_shell.unwrap_or("sh")
    } else {
        "sh"
    };
    SpawnRecipe {
        binary,
        base_args: vec![],
        trailing_args: Vec::new(),
        windows_shell: WindowsShell::Direct,
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
        let recipe = terminal_recipe(Platform::Windows, EnvType::Windows, None);
        assert_eq!(recipe.binary, "powershell.exe");
        assert!(
            recipe.base_args.is_empty(),
            "terminal must spawn PowerShell with no extra args"
        );
        assert!(matches!(recipe.windows_shell, WindowsShell::Direct));
    }

    /// macOS uses `sh` so it's available on every macOS install without
    /// depending on the user's login shell (zsh, fish, etc.).
    #[test]
    fn terminal_spawn_recipe_sh_direct_on_macos() {
        let recipe = terminal_recipe(Platform::Macos, EnvType::Windows, None);
        assert_eq!(recipe.binary, "sh");
        assert!(recipe.base_args.is_empty());
        assert!(matches!(recipe.windows_shell, WindowsShell::Direct));
    }

    /// Native Linux (env_type is `Windows` because the path doesn't start
    /// with `/home/` or `/mnt/`) uses `sh` — universally available and
    /// matches the choice the existing Build/Run "Terminal in worktree"
    /// option makes. WSL meshes are a separate case (`terminal_recipe`
    /// with `EnvType::Wsl`) and live in their own tests below.
    #[test]
    fn terminal_spawn_recipe_sh_direct_on_linux() {
        let recipe = terminal_recipe(Platform::Linux, EnvType::Windows, None);
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

    /// Pin the id and UI metadata. `icon: "T"` follows the single-printable-char
    /// contract every adapter honours (issue #328) — the mobile NodeRow badge
    /// renders `icon` directly as the chip glyph, so a multi-char string
    /// would overflow the chip.
    #[test]
    fn terminal_id_and_ui_metadata() {
        assert_eq!(TERMINAL.id(), "terminal");
        let ui = TERMINAL.ui();
        assert_eq!(ui.label, "Terminal");
        assert_eq!(ui.color, "#9ca3af");
        assert_eq!(ui.icon, "T");
    }

    // ----- WSL login-shell path (issue #548) -----
    //
    // The pure helper `terminal_recipe` is the surface tested here, not
    // `TERMINAL.spawn_recipe` — the latter wires `env::wsl_login_shell()`
    // into the helper, and the cache lives in a `Lazy` that's hard to
    // reset from tests without a dedicated seam. The pure helper tests
    // pin the decision table; the cache→helper wiring is a one-liner
    // reviewed alongside the helper.

    /// The headline case: an ohmyzsh user spawns a Terminal node on a WSL
    /// mesh and the recipe picks up zsh as the binary instead of `sh`
    /// (dash on Ubuntu). `Platform::Linux` is what `spawn.rs` routes a
    /// WSL mesh to; `EnvType::Wsl` is the path-derived env type.
    ///
    /// Note: launching zsh via `wsl.exe --cd <path> -- /usr/bin/zsh` is
    /// non-interactive — zsh won't source `~/.zshrc` and the user will
    /// see a plain `$` prompt, not their ohmyzsh prompt. Adding `-i` or
    /// `-l` here would source rc files; deliberately deferred per issue
    /// #548 v1 to keep the diff zero-behaviour-change for users who
    /// don't chsh (today's Terminal node launches `sh` non-interactively).
    #[test]
    fn terminal_recipe_wsl_zsh_login_shell_uses_zsh() {
        let recipe = terminal_recipe(Platform::Linux, EnvType::Wsl, Some("/usr/bin/zsh"));
        assert_eq!(recipe.binary, "/usr/bin/zsh");
        assert!(
            recipe.base_args.is_empty(),
            "no -i/-l in v1 — see issue #548 for the follow-up"
        );
        assert!(matches!(recipe.windows_shell, WindowsShell::Direct));
    }

    /// Same as zsh but with bash — pins that the helper passes the path
    /// through verbatim rather than special-casing zsh.
    #[test]
    fn terminal_recipe_wsl_bash_login_shell_uses_bash() {
        let recipe = terminal_recipe(Platform::Linux, EnvType::Wsl, Some("/bin/bash"));
        assert_eq!(recipe.binary, "/bin/bash");
    }

    /// Same with fish — covers a non-bash non-zsh login shell so the
    /// helper isn't accidentally hardcoded to the two most common shells.
    #[test]
    fn terminal_recipe_wsl_fish_login_shell_uses_fish() {
        let recipe = terminal_recipe(Platform::Linux, EnvType::Wsl, Some("/usr/bin/fish"));
        assert_eq!(recipe.binary, "/usr/bin/fish");
    }

    /// When the cached lookup returns `None` — WSL not installed, the
    /// `wsl.exe` call failed, or the passwd entry points at `nologin` /
    /// `/bin/false` — fall back to the existing `sh` default rather
    /// than panicking or spawning a shell that exits immediately.
    #[test]
    fn terminal_recipe_wsl_no_login_shell_falls_back_to_sh() {
        let recipe = terminal_recipe(Platform::Linux, EnvType::Wsl, None);
        assert_eq!(recipe.binary, "sh");
        assert!(matches!(recipe.windows_shell, WindowsShell::Direct));
    }
}
