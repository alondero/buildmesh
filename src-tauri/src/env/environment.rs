//! Runtime environment detection — Windows vs WSL.
//!
//! This module owns the *what environment are we in?* question and nothing
//! else. Distro / login-shell / Windows-username lookups live here so that
//! `host_path` can stay silent on detection (the path-conversion layer
//! accepts the already-detected [`Environment`] as input).
//!
//! The `claude_dir` / `codex_dir` helpers also live here because they key
//! off `current_env()` plus `HOME` / `USERPROFILE` / `USERNAME` — every input
//! is in this module's vocabulary. Path conversion (the `\\wsl$\`-shaped
//! strings, the `/mnt/c/` rewrites) belongs to [`super::host_path`].
//!
//! ## Layering rule
//!
//! No module outside `host_path` may build `\\wsl$\` or `/mnt/` paths. The
//! CLAUDE.md hard rule is *structurally* enforced by this module's surface:
//! there are no `to_host_path`-shaped functions here, only detection results.

use std::env;
use std::path::PathBuf;

use once_cell::sync::Lazy;

use crate::models::EnvType;
use crate::process_util::command_no_window;

// ── WSL distro lookup ──────────────────────────────────────────────────────

/// The default WSL distro name (e.g., "Ubuntu"), cached after first detection
static DETECTED_DISTRO: Lazy<Option<String>> = Lazy::new(detect_default_wsl_distro);

/// Get the default WSL distro name by parsing `wsl.exe -l -v` output.
/// Returns the distro marked as (default) or the first one if none marked.
pub(crate) fn detect_default_wsl_distro() -> Option<String> {
    let output = command_no_window("wsl.exe")
        .args(["-l", "-v"])
        .output()
        .ok()?;
    let stdout = if output
        .stdout
        .iter()
        .skip(1)
        .step_by(2)
        .any(|byte| *byte == 0)
    {
        let units = output
            .stdout
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    parse_wsl_distro_list(&stdout)
}

pub(super) fn parse_wsl_distro_list(stdout: &str) -> Option<String> {
    let rows = stdout
        .lines()
        .skip(1)
        .map(|line| line.trim_matches('\0').trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    rows.iter()
        .find_map(|line| {
            line.strip_prefix('*')
                .map(str::trim_start)
                .and_then(|line| line.split_whitespace().next())
                .map(str::to_string)
        })
        .or_else(|| {
            rows.first()
                .and_then(|line| line.split_whitespace().next())
                .map(str::to_string)
        })
}

/// Get the cached default WSL distro name
pub(crate) fn get_default_wsl_distro() -> Option<String> {
    DETECTED_DISTRO.clone()
}

// ── WSL login-shell lookup ─────────────────────────────────────────────────

/// Parse the login shell (field 7) out of a `getent passwd <user>` line.
///
/// `getent passwd` formats a line as `name:pw:uid:gid:gecos:home:shell` —
/// colons in any field are not escaped by glibc's NSS, so plain `split(':')`
/// is correct in practice (a GECOS field containing `:` is a malformed
/// entry by spec). Returns `None` for the no-login shells
/// (`/usr/sbin/nologin`, `/bin/false`) and any line with fewer than 7
/// fields, so the cached lookup can fall through to a plain `sh` default
/// rather than launching a shell that exits immediately.
///
/// `pub(crate)` so the `mod tests` block in `env/mod.rs` can keep its
/// existing assertions on the parsing rules without moving the tests into
/// this module (they live in `mod.rs` for layout reasons — see the file
/// header there).
pub(crate) fn parse_login_shell_from_passwd(line: &str) -> Option<String> {
    let shell = line.split(':').nth(6)?.trim();
    if shell.is_empty() || shell == "/usr/sbin/nologin" || shell == "/bin/false" {
        return None;
    }
    Some(shell.to_string())
}

/// Resolve the WSL user's login shell by running `getent passwd $(whoami)`
/// inside the default distro. Returns `None` if WSL is unavailable, the
/// passwd entry can't be read, or the entry points at a no-login shell —
/// the caller is expected to fall back to a POSIX-`sh` default in that case.
///
/// The returned `&'static str` is leaked from a one-shot `String`; the leak
/// happens at most once per Buildmesh session (the result is cached in
/// [`DETECTED_WSL_LOGIN_SHELL`]). The same one-shot leak pattern is used
/// for the tracing `_guard` in `lib.rs`.
fn get_default_wsl_login_shell_impl() -> Option<&'static str> {
    let distro = get_default_wsl_distro().unwrap_or_else(|| "Ubuntu".to_string());
    let output = command_no_window("wsl.exe")
        .args(["-d", &distro, "--", "sh", "-c", "getent passwd $(whoami)"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().next()?;
    parse_login_shell_from_passwd(line).map(|s| Box::leak(s.into_boxed_str()) as &'static str)
}

/// The user's WSL login shell (e.g., `/usr/bin/zsh`), cached after first
/// detection. `None` when WSL isn't available or the login shell isn't
/// usable as an interactive terminal.
static DETECTED_WSL_LOGIN_SHELL: Lazy<Option<&'static str>> =
    Lazy::new(get_default_wsl_login_shell_impl);

/// Get the cached WSL login shell, if any. `SpawnRecipe::binary` needs
/// `&'static str`, so the cached value is leaked once at first detection
/// (see [`get_default_wsl_login_shell_impl`]).
pub fn wsl_login_shell() -> Option<&'static str> {
    *DETECTED_WSL_LOGIN_SHELL
}

// ── Windows username ───────────────────────────────────────────────────────

/// The Windows username, cached after first lookup
#[allow(dead_code)]
static WINDOWS_USERNAME: Lazy<Option<String>> = Lazy::new(get_windows_username_impl);

/// Get the Windows username (used for path construction)
#[allow(dead_code)]
fn get_windows_username_impl() -> Option<String> {
    env::var("USERNAME").ok()
}

/// Get the cached Windows username
#[allow(dead_code)]
fn get_windows_username() -> Option<String> {
    WINDOWS_USERNAME.clone()
}

// ── Environment enum + detection ───────────────────────────────────────────

/// The detected runtime environment for this process
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    /// Running on native Windows (Git Bash/MSYS2)
    Windows,
    /// Running inside WSL (Windows Subsystem for Linux)
    Wsl,
}

impl Environment {
    /// Detect the current environment by checking for WSL signature
    pub fn detect() -> Self {
        if cfg!(target_os = "windows") {
            // On Windows, check if /proc/version contains "microsoft" (WSL signature)
            if let Ok(versions) = std::fs::read_to_string("/proc/version") {
                if versions.to_lowercase().contains("microsoft") {
                    return Environment::Wsl;
                }
            }
            // Check via wsl.exe detection
            if let Ok(output) = command_no_window("wsl.exe")
                .args(["--detect-nested"])
                .output()
            {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.trim() == "1" {
                    return Environment::Wsl;
                }
            }
            Environment::Windows
        } else {
            // Non-Windows (Linux/WSL)
            if let Ok(versions) = std::fs::read_to_string("/proc/version") {
                if versions.to_lowercase().contains("microsoft") {
                    Environment::Wsl
                } else {
                    Environment::Windows // treat native Linux as "Windows" for our purposes
                }
            } else {
                Environment::Windows
            }
        }
    }

    /// Returns true if we're running inside WSL
    pub fn is_wsl(&self) -> bool {
        matches!(self, Environment::Wsl)
    }
}

static CURRENT_ENV: Lazy<Environment> = Lazy::new(Environment::detect);

/// Get the current environment (cached)
pub fn current_env() -> Environment {
    *CURRENT_ENV
}

// ── Agent CLI home directories (depend on current_env) ─────────────────────

/// Get the .claude directory for session storage in the correct environment
pub fn claude_dir() -> PathBuf {
    match current_env() {
        Environment::Wsl => {
            // Buildmesh running inside a Unix/WSL userland: the agent CLI writes
            // its config to `$HOME/.claude`, the standard Claude Code location.
            // Resolve it dynamically instead of hardcoding a specific user.
            if let Ok(home) = env::var("HOME") {
                PathBuf::from(home).join(".claude")
            } else {
                PathBuf::from("/root/.claude")
            }
        }
        Environment::Windows => {
            if let Ok(home) = env::var("USERPROFILE") {
                PathBuf::from(home).join(".claude")
            } else if let Ok(home) = env::var("HOME") {
                PathBuf::from(home).join(".claude")
            } else {
                // USERPROFILE and HOME both unset — effectively impossible on a
                // real Windows session. Derive from the account name rather than
                // a hardcoded user.
                let user = env::var("USERNAME").unwrap_or_else(|_| "Public".to_string());
                PathBuf::from(format!("C:\\Users\\{user}\\.claude"))
            }
        }
    }
}

/// The Cursor CLI home directory, mirroring [`claude_dir`]. Cursor stores
/// workspace-scoped agent transcripts below `<cursor home>/projects/`.
pub fn cursor_dir() -> PathBuf {
    match current_env() {
        Environment::Wsl => {
            if let Ok(home) = env::var("HOME") {
                PathBuf::from(home).join(".cursor")
            } else {
                PathBuf::from("/root/.cursor")
            }
        }
        Environment::Windows => {
            if let Ok(home) = env::var("USERPROFILE") {
                PathBuf::from(home).join(".cursor")
            } else if let Ok(home) = env::var("HOME") {
                PathBuf::from(home).join(".cursor")
            } else {
                let user = env::var("USERNAME").unwrap_or_else(|_| "Public".to_string());
                PathBuf::from(format!("C:\\Users\\{user}\\.cursor"))
            }
        }
    }
}

/// The Codex CLI home directory, mirroring [`claude_dir`]. Codex honours a
/// `CODEX_HOME` override for its *entire* state directory (sessions, auth,
/// config — issue #885), so that takes precedence; otherwise `~/.codex` in the
/// current environment. Rollout transcripts live under
/// `<codex home>/sessions/YYYY/MM/DD/`.
pub fn codex_dir() -> PathBuf {
    if let Ok(home) = env::var("CODEX_HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(home);
        }
    }
    match current_env() {
        Environment::Wsl => {
            if let Ok(home) = env::var("HOME") {
                PathBuf::from(home).join(".codex")
            } else {
                PathBuf::from("/root/.codex")
            }
        }
        Environment::Windows => {
            if let Ok(home) = env::var("USERPROFILE") {
                PathBuf::from(home).join(".codex")
            } else if let Ok(home) = env::var("HOME") {
                PathBuf::from(home).join(".codex")
            } else {
                let user = env::var("USERNAME").unwrap_or_else(|_| "Public".to_string());
                PathBuf::from(format!("C:\\Users\\{user}\\.codex"))
            }
        }
    }
}

/// The Codex CLI home for an agent environment, expressed in that
/// environment's native path syntax. This deliberately derives the WSL home
/// from the host username instead of launching a WSL shell.
pub(crate) fn codex_dir_for_env(env_type: EnvType, spawn_path: &str) -> Option<PathBuf> {
    match env_type {
        EnvType::Windows => Some(codex_dir()),
        EnvType::Wsl => {
            if let Ok(home) = env::var("CODEX_HOME") {
                if home.starts_with('/') {
                    return Some(PathBuf::from(home));
                }
            }
            if let Some(user) = spawn_path
                .strip_prefix("/home/")
                .and_then(|path| path.split('/').next())
            {
                return Some(PathBuf::from(format!("/home/{user}/.codex")));
            }
            let user = env::var("USERNAME")
                .ok()
                .or_else(|| env::var("USER").ok())?;
            Some(PathBuf::from(format!("/home/{user}/.codex")))
        }
    }
}

/// The Antigravity CLI home directory, mirroring [`claude_dir`]. AGY stores
/// its config under `<home>/.gemini/antigravity-cli/` and its session
/// transcripts under `<home>/.gemini/antigravity-cli/brain/<conversation-id>/
/// .system_generated/logs/transcript.jsonl` (issue #1283). Unlike Claude
/// Code / Cursor this is *globally* keyed — not project-scoped — so the
/// transcript reader and the AGY session scanner walk the brain dir directly.
///
/// Honours a `GEMINI_HOME` / `ANTIGRAVITY_HOME` override (checked in that
/// order) so a user who relocates the gemini tree keeps their transcripts
/// reachable; falls through to `~/.gemini/antigravity-cli` under the current
/// environment.
pub fn agy_dir() -> PathBuf {
    if let Ok(home) = env::var("GEMINI_HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(home).join("antigravity-cli");
        }
    }
    if let Ok(home) = env::var("ANTIGRAVITY_HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(home);
        }
    }
    match current_env() {
        Environment::Wsl => {
            if let Ok(home) = env::var("HOME") {
                PathBuf::from(home).join(".gemini").join("antigravity-cli")
            } else {
                PathBuf::from("/root/.gemini/antigravity-cli")
            }
        }
        Environment::Windows => {
            if let Ok(home) = env::var("USERPROFILE") {
                PathBuf::from(home).join(".gemini").join("antigravity-cli")
            } else if let Ok(home) = env::var("HOME") {
                PathBuf::from(home).join(".gemini").join("antigravity-cli")
            } else {
                let user = env::var("USERNAME").unwrap_or_else(|_| "Public".to_string());
                PathBuf::from(format!("C:\\Users\\{user}\\.gemini\\antigravity-cli"))
            }
        }
    }
}

/// The AGY "brain" directory that holds one subdirectory per conversation.
/// Sessions live at `<brain>/<conversation-id>/.system_generated/logs/
/// transcript.jsonl` (issue #1283) — globally keyed, so this is the single
/// shared root every AGY session scanner and the transcript reader consult.
pub fn agy_brain_dir() -> PathBuf {
    agy_dir().join("brain")
}

/// The Antigravity CLI home for an agent environment, expressed in that
/// environment's native path syntax. Shaped like [`codex_dir_for_env`] but
/// without its `USERNAME` fallback: the WSL home is derived solely from the
/// spawn path's `/home/<user>` prefix (plus absolute-path overrides), since
/// a Windows account name is not necessarily the distro username (issue
/// #1499 review — the capture poller needs the brain dir for the node's own
/// environment, not the host's `current_env()`, and never a guessed one).
pub(crate) fn agy_dir_for_env(env_type: EnvType, spawn_path: &str) -> Option<PathBuf> {
    match env_type {
        EnvType::Windows => Some(agy_dir()),
        EnvType::Wsl => {
            if let Ok(home) = env::var("GEMINI_HOME") {
                if !home.trim().is_empty() && home.starts_with('/') {
                    return Some(PathBuf::from(home).join("antigravity-cli"));
                }
            }
            if let Ok(home) = env::var("ANTIGRAVITY_HOME") {
                if !home.trim().is_empty() && home.starts_with('/') {
                    return Some(PathBuf::from(home));
                }
            }
            if let Some(user) = spawn_path
                .strip_prefix("/home/")
                .and_then(|path| path.split('/').next())
            {
                if !user.is_empty() {
                    return Some(PathBuf::from(format!(
                        "/home/{user}/.gemini/antigravity-cli"
                    )));
                }
            }
            // No `USERNAME`/`USER` fallback: on a Windows host `USERNAME` is
            // the Windows account name (possibly containing spaces), which is
            // not necessarily the WSL distro username. Guessing produces a
            // wrong user's brain directory; returning `None` skips capture
            // cleanly (the `Stop`-hook path still rescues the session).
            None
        }
    }
}

/// The Grok Code CLI home directory — `<home>/.grok/`. Sessions land under
/// `<grok>/sessions/<session_id>/chat_history.jsonl` (issue #1281) and
/// updates stream to `<grok>/sessions/<session_id>/updates.jsonl`. Mirrors
/// the structure of [`agy_dir`] (no `GROK_HOME` override yet — pinned by
/// `env::tests::grok_dir_uses_the_current_environment_home`).
pub fn grok_dir() -> PathBuf {
    match current_env() {
        Environment::Wsl => env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/root"))
            .join(".grok"),
        Environment::Windows => env::var("USERPROFILE")
            .or_else(|_| env::var("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let user = env::var("USERNAME").unwrap_or_else(|_| "Public".to_string());
                PathBuf::from(format!("C:\\Users\\{user}"))
            })
            .join(".grok"),
    }
}

/// The Command Code CLI home directory — `<home>/.commandcode/`. Sessions land
/// under `<home>/.commandcode/projects/<encoded-cwd>/` (issue #1500).
pub fn commandcode_dir() -> PathBuf {
    match current_env() {
        Environment::Wsl => env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/root"))
            .join(".commandcode"),
        Environment::Windows => env::var("USERPROFILE")
            .or_else(|_| env::var("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let user = env::var("USERNAME").unwrap_or_else(|_| "Public".to_string());
                PathBuf::from(format!("C:\\Users\\{user}"))
            })
            .join(".commandcode"),
    }
}

/// The Command Code CLI home for an agent environment.
pub(crate) fn commandcode_dir_for_env(env_type: EnvType, spawn_path: &str) -> Option<PathBuf> {
    match env_type {
        EnvType::Windows => Some(commandcode_dir()),
        EnvType::Wsl => {
            if let Some(user) = spawn_path
                .strip_prefix("/home/")
                .and_then(|path| path.split('/').next())
            {
                return Some(PathBuf::from(format!("/home/{user}/.commandcode")));
            }
            let user = env::var("USERNAME")
                .ok()
                .or_else(|| env::var("USER").ok())?;
            Some(PathBuf::from(format!("/home/{user}/.commandcode")))
        }
    }
}
