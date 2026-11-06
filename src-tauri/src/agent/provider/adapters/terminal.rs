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
    fn descriptor(&self) -> &'static crate::agent::capabilities::HarnessDescriptor {
        static DESCRIPTOR: std::sync::LazyLock<crate::agent::capabilities::HarnessDescriptor> =
            std::sync::LazyLock::new(|| crate::agent::capabilities::HarnessDescriptor {
                supports_resume: false,
                auto_resume_on_startup: false,
                supports_model_override: false,
                supports_extra_args: {
                    // Issue #1358: Terminal opens the user's interactive shell with
                    // no LLM-driven recipe — splicing synthetic flags into a
                    // user's interactive command line would be a footgun. The
                    // resolver drops the layer-1 `extra_args` value at the mask.
                    false
                },
                supports_prefill: false,
                available_on: { &[Platform::Windows, Platform::Macos, Platform::Linux] },
                is_plain_terminal: true,
                ..Default::default()
            });
        &DESCRIPTOR
    }

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
#[path = "terminal_tests.rs"]
mod tests;
