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
    assert!(!TERMINAL.descriptor().supports_resume);
    assert!(!TERMINAL.descriptor().auto_resume_on_startup);
    assert!(!TERMINAL.descriptor().requires_attention_hook());
    assert!(!TERMINAL.descriptor().supports_model_override);
    assert!(!TERMINAL.descriptor().supports_prefill);
    assert!(TERMINAL.descriptor().is_plain_terminal);
}

/// Terminal is available on every host platform we ship to.
#[test]
fn terminal_available_on_three_platforms() {
    let platforms = TERMINAL.descriptor().available_on;
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
