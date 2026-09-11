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

use std::path::PathBuf;
use std::env;

use once_cell::sync::Lazy;

use crate::process_util::command_no_window;
use crate::models::EnvType;

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
    let stdout = if output.stdout.iter().skip(1).step_by(2).any(|byte| *byte == 0) {
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

/// Guest home is a property of the installed distribution, never the Windows
/// account name or the repository's directory (which can be a mounted drive).
pub(crate) fn wsl_home() -> Option<PathBuf> {
    if !cfg!(windows) { return env::var_os("HOME").map(PathBuf::from); }
    static GUEST_HOME: Lazy<Option<PathBuf>> = Lazy::new(|| {
        let mut command = command_no_window("wsl.exe");
        command.args(["-d", &get_default_wsl_distro()?, "--cd", "~", "--exec", "sh", "-lc", "printf '__BUILDMESH_WSL_HOME__%s\\n' \"$HOME\""]);
        let output = crate::process_util::run_command_with_timeout(command, "WSL home", std::time::Duration::from_secs(10)).ok()?;
        if !output.status.success() { return None; }
        parse_wsl_home_output(&output.stdout)
    });
    GUEST_HOME.clone()
}

pub(super) fn parse_wsl_home_output(output: &[u8]) -> Option<PathBuf> {
    parse_marked_wsl_path(output, "__BUILDMESH_WSL_HOME__")
}

fn parse_marked_wsl_path(output: &[u8], marker: &str) -> Option<PathBuf> {
    let output = String::from_utf8(output.to_vec()).ok()?;
    output.lines().rev().find_map(|line| {
        let home = line.strip_prefix(marker)?.trim();
        home.starts_with('/').then(|| PathBuf::from(home))
    })
}

pub(crate) fn parse_wsl_codex_home_output(output: &[u8]) -> Option<PathBuf> {
    parse_marked_wsl_path(output, "__BUILDMESH_WSL_CODEX_HOME__")
}

/// Resolve Muse credentials in the same login environment used for spawning.
pub(crate) fn muse_auth_path() -> Option<PathBuf> {
    if cfg!(windows) {
        let mut command = command_no_window("wsl.exe");
        command.args([
            "-d", &get_default_wsl_distro()?, "--cd", "~", "--exec",
            "sh", "-lc",
            "if [ -n \"${META_API_KEY:-}\" ]; then exit 1; fi; printf '__BUILDMESH_MUSE_AUTH__%s\\n' \"${MUSE_AUTH_PATH:-${XDG_CONFIG_HOME:-$HOME/.config}/muse/auth.json}\"",
        ]);
        let output = crate::process_util::run_command_with_timeout(
            command, "WSL Muse credential location", std::time::Duration::from_secs(10),
        ).ok()?;
        if !output.status.success() { return None; }
        let guest = parse_marked_wsl_path(&output.stdout, "__BUILDMESH_MUSE_AUTH__")?;
        Some(PathBuf::from(super::to_host_path_for_runtime(&guest.to_string_lossy(), EnvType::Wsl)))
    } else {
        muse_auth_path_from_vars(|name| env::var_os(name))
    }
}

fn muse_auth_path_from_vars(get: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    if get("META_API_KEY").is_some_and(|value| !value.is_empty()) { return None; }
    get("MUSE_AUTH_PATH").filter(|v| !v.is_empty()).map(PathBuf::from)
        .or_else(|| get("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from)
            .or_else(|| get("HOME").map(|home| PathBuf::from(home).join(".config")))
            .map(|root| root.join("muse/auth.json")))
}

#[cfg(test)]
mod muse_path_tests {
    use super::*;

    #[test]
    fn muse_credential_path_precedence() {
        let resolve = |auth: &str, xdg: &str| muse_auth_path_from_vars(|name| match name {
            "MUSE_AUTH_PATH" => Some(auth.into()),
            "XDG_CONFIG_HOME" => Some(xdg.into()),
            "HOME" => Some("/home/test".into()),
            _ => None,
        }).unwrap();
        assert_eq!(resolve("/var/lib/muse/auth.json", "/opt/config"), PathBuf::from("/var/lib/muse/auth.json"));
        assert_eq!(resolve("", "/opt/config"), PathBuf::from("/opt/config/muse/auth.json"));
        assert_eq!(resolve("", ""), PathBuf::from("/home/test/.config/muse/auth.json"));
        assert_eq!(muse_auth_path_from_vars(|_| None), None);
        assert_eq!(muse_auth_path_from_vars(|name| match name {
            "META_API_KEY" => Some("test-api-key".into()),
            "MUSE_AUTH_PATH" => Some("/home/test/stale-oauth.json".into()),
            _ => None,
        }), None);
    }

    #[test]
    fn muse_guest_output_requires_marker_and_absolute_path() {
        let marker = "__BUILDMESH_MUSE_AUTH__";
        assert_eq!(parse_marked_wsl_path(b"login banner\n__BUILDMESH_MUSE_AUTH__/opt/config/muse/auth.json\n", marker), Some(PathBuf::from("/opt/config/muse/auth.json")));
        for output in [b"".as_slice(), b"/home/test/.config/muse/auth.json", b"__BUILDMESH_MUSE_AUTH__relative/path", b"__BUILDMESH_MUSE_AUTH__\xff"] {
            assert_eq!(parse_marked_wsl_path(output, marker), None);
        }
    }
}

/// Resolve the Codex state directory from the selected WSL environment. This
/// keeps a guest-side `CODEX_HOME` override visible to transcript discovery
/// without forwarding the Windows host's variable into the guest.
pub(crate) fn wsl_codex_home() -> Option<PathBuf> {
    if !cfg!(windows) {
        return Some(codex_dir());
    }
    static CODEX_HOME: Lazy<Option<PathBuf>> = Lazy::new(|| {
        let mut command = command_no_window("wsl.exe");
        command.args([
            "-d",
            &get_default_wsl_distro()?,
            "--cd",
            "~",
            "--exec",
            "sh",
            "-lc",
            "printf '__BUILDMESH_WSL_CODEX_HOME__%s\\n' \"${CODEX_HOME:-$HOME/.codex}\"",
        ]);
        let output = crate::process_util::run_command_with_timeout(
            command,
            "WSL Codex home",
            std::time::Duration::from_secs(10),
        )
        .ok()?;
        if !output.status.success() {
            return None;
        }
        parse_wsl_codex_home_output(&output.stdout)
    });
    CODEX_HOME.clone()
}

/// Runtime paths are already translated for the process. A UNC spawn path
/// belongs to a Windows process; a POSIX spawn path belongs to a guest.
pub(crate) fn runtime_for_spawn_path(path: &str) -> EnvType {
    if super::is_wsl_host() && super::is_windows_path(path) { return EnvType::WindowsInterop; }
    if cfg!(windows) && path.starts_with('/') && !path.starts_with("//") {
        EnvType::Wsl
    } else {
        EnvType::Windows
    }
}

pub(crate) fn cli_dir_for_spawn(native: PathBuf, guest_relative: &str, spawn_path: &str) -> Option<PathBuf> {
    if runtime_for_spawn_path(spawn_path) == EnvType::WindowsInterop { return super::windows_cli_home(guest_relative); }
    if runtime_for_spawn_path(spawn_path) == EnvType::Wsl {
        let guest = wsl_home()?.join(guest_relative);
        Some(PathBuf::from(super::to_host_path(&guest.to_string_lossy())))
    } else {
        Some(native)
    }
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
/// environment's native path syntax. Guest homes come from the cached WSL
/// login-shell probe because the guest username can differ from Windows.
pub(crate) fn codex_dir_for_env(env_type: EnvType, spawn_path: &str) -> Option<PathBuf> {
    match env_type {
        // CODEX_HOME belongs to the process that owns the environment. A
        // Linux Buildmesh must not reinterpret its own value as a Windows
        // harness setting, and a Windows host value must not leak into WSL.
        EnvType::WindowsInterop => super::windows_codex_home(),
        EnvType::Windows => Some(codex_dir()),
        EnvType::Wsl => {
            let _ = spawn_path;
            wsl_codex_home()
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
                PathBuf::from(home)
                    .join(".gemini")
                    .join("antigravity-cli")
            } else {
                PathBuf::from("/root/.gemini/antigravity-cli")
            }
        }
        Environment::Windows => {
            if let Ok(home) = env::var("USERPROFILE") {
                PathBuf::from(home)
                    .join(".gemini")
                    .join("antigravity-cli")
            } else if let Ok(home) = env::var("HOME") {
                PathBuf::from(home)
                    .join(".gemini")
                    .join("antigravity-cli")
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
/// using the cached guest login home (or an explicit absolute override).
/// Windows and guest usernames need not match, and a Windows-backed working
/// directory cannot identify the guest account.
pub(crate) fn agy_dir_for_env(env_type: EnvType, spawn_path: &str) -> Option<PathBuf> {
    match env_type {
        EnvType::WindowsInterop => super::windows_cli_home(".gemini/antigravity-cli"),
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
            let _ = spawn_path;
            wsl_home().map(|home| home.join(".gemini/antigravity-cli"))
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
        EnvType::WindowsInterop => super::windows_cli_home(".commandcode"),
        EnvType::Windows => Some(commandcode_dir()),
        EnvType::Wsl => {
            let _ = spawn_path;
            wsl_home().map(|home| home.join(".commandcode"))
        }
    }
}
