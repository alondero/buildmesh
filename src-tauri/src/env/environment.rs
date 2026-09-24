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

use std::collections::HashMap;
use std::path::PathBuf;
use std::env;
use std::sync::Mutex;

use once_cell::sync::Lazy;

use crate::process_util::command_no_window;
use crate::models::EnvType;

// ── WSL distro lookup ──────────────────────────────────────────────────────

/// The default WSL distro name (e.g., "Ubuntu"), cached after first detection.
/// A miss is cached only after a few tries: the full test suite starts many
/// `wsl.exe` processes at once, and the first listing can fail while the
/// service is busy. Caching that miss made every later guest-path check
/// fail for the rest of the process.
static DETECTED_DISTRO: Lazy<Option<String>> =
    Lazy::new(|| probe_until_some(3, detect_default_wsl_distro));

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
    // A memoized miss from distro detection cannot become Some later.
    // Entering the retry loop would only sleep 500ms to re-read None.
    get_default_wsl_distro()?;
    static GUEST_HOME: Lazy<Option<PathBuf>> = Lazy::new(|| probe_until_some(3, probe_wsl_home_once));
    GUEST_HOME.clone()
}

/// One guest-home probe. `None` covers a missing distro, a non-zero exit,
/// a timeout, and an unparsable banner. Callers retry before caching.
fn probe_wsl_home_once() -> Option<PathBuf> {
    let mut command = command_no_window("wsl.exe");
    command.args([
        "-d",
        &get_default_wsl_distro()?,
        "--cd",
        "~",
        "--exec",
        "sh",
        "-lc",
        "printf '__BUILDMESH_WSL_HOME__%s\\n' \"$HOME\"",
    ]);
    let output = crate::process_util::run_command_with_timeout(
        command,
        "WSL home",
        std::time::Duration::from_secs(10),
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_wsl_home_output(&output.stdout)
}

/// Run `probe` up to `attempts` times, pausing briefly between misses so a
/// busy `wsl.exe` can answer. A success is returned immediately. The last
/// miss is what a `Lazy` caches, so a machine with no WSL still settles.
fn probe_until_some<T>(attempts: u32, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let mut last = None;
    for attempt in 0..attempts {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        last = probe();
        if last.is_some() {
            return last;
        }
    }
    last
}

#[cfg(test)]
mod wsl_probe_retry_tests {
    use super::probe_until_some;
    use std::cell::Cell;

    #[test]
    fn first_success_does_not_retry() {
        let calls = Cell::new(0);
        let got = probe_until_some(3, || {
            calls.set(calls.get() + 1);
            Some(7)
        });
        assert_eq!(got, Some(7));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn exhausted_retries_call_the_probe_once_per_attempt() {
        let calls = Cell::new(0);
        let got: Option<i32> = probe_until_some(3, || {
            calls.set(calls.get() + 1);
            None
        });
        assert_eq!(got, None);
        assert_eq!(calls.get(), 3);
    }
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
        static MUSE_AUTH_PATH: Lazy<Option<PathBuf>> = Lazy::new(|| {
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
        });
        MUSE_AUTH_PATH.clone()
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

/// A `Hash`able tag for [`EnvType`] (the enum itself doesn't derive `Hash`).
/// Used as half of the [`SPAWN_ENV_MEMO`] fingerprint.
fn env_type_tag(env_type: EnvType) -> &'static str {
    match env_type {
        EnvType::Windows => "windows",
        EnvType::Wsl => "wsl",
        EnvType::WindowsInterop => "windows-interop",
    }
}

/// The `(runtime tag, distro)` fingerprint a spawn-env memo entry is keyed on.
type SpawnEnvFingerprint = (&'static str, Option<String>);

/// The `(provider env, mesh env fingerprint)` key the spawn-env memo is keyed
/// on: the resolved runtime plus the target distro. A change to either is a
/// different key, so a settings change can never produce a stale hit.
fn spawn_env_memo_key(runtime: EnvType, distro: Option<&str>) -> SpawnEnvFingerprint {
    (env_type_tag(runtime), distro.map(str::to_string))
}

/// Process-lifetime memo for spawn-time env probes that are keyed on the
/// resolved runtime (so they cannot use the one-shot [`Lazy`] their siblings
/// use). Without this, every spawn re-runs the underlying subprocess.
/// Values are the *host-form* paths the callers consume; only **successful**
/// probes are stored (see [`memoized_spawn_env`]), so there is no negative
/// entry to represent here.
static SPAWN_ENV_MEMO: Lazy<Mutex<HashMap<SpawnEnvFingerprint, PathBuf>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Test/diagnostic hook: drop every memoised spawn-env probe.
#[cfg(test)]
pub(crate) fn clear_spawn_env_memo() {
    SPAWN_ENV_MEMO
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

/// Memoise `compute` under `key`. The lock is released across `compute` — the
/// probe spawns a subprocess, and holding the mutex through it would serialise
/// concurrent spawns on the probe (the exact cost this avoids).
///
/// **Only successful probes are memoised.** A `None` (WSL not up yet,
/// `wsl.exe` missing, a transient failure) is returned but *not* stored, so the
/// next spawn retries it. Latching a negative result would disable the harness
/// for the whole process lifetime, with no recovery short of an app restart.
fn memoized_spawn_env(
    key: SpawnEnvFingerprint,
    compute: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(hit) = SPAWN_ENV_MEMO
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
    {
        return Some(hit.clone());
    }
    let value = compute();
    if let Some(path) = value.as_ref() {
        SPAWN_ENV_MEMO
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, path.clone());
    }
    value
}

/// Resolve Kimi Code's config in the environment that will execute the CLI.
/// Guest login overrides belong to the guest, never to the Windows host.
///
/// Memoised per `(runtime, distro)` — see [`SPAWN_ENV_MEMO`] — because the
/// underlying probe is a `wsl.exe` / `powershell.exe` subprocess on the Windows
/// side and is otherwise re-run on *every* spawn (issue #1752).
pub(crate) fn kimi_home_for_spawn(spawn_path: &str, distro: Option<&str>) -> Option<PathBuf> {
    let runtime = runtime_for_spawn_path(spawn_path);
    memoized_spawn_env(spawn_env_memo_key(runtime, distro), || {
        kimi_home_probe(runtime, distro)
    })
}

/// The uncached probe [`kimi_home_for_spawn`] memoises. Split out so the memo
/// wrapper owns the fingerprint while this keeps the platform-specific probe
/// (and its exact stdout contract) unchanged.
fn kimi_home_probe(runtime: EnvType, distro: Option<&str>) -> Option<PathBuf> {
    let path = if runtime == EnvType::Wsl {
        let distro = distro.map(str::to_string).or_else(get_default_wsl_distro)?;
        let mut command = command_no_window("wsl.exe");
        command.args(["-d", &distro, "--cd", "~", "--exec", "sh", "-lc",
            "printf '__BUILDMESH_KIMI_HOME__%s\\n' \"${KIMI_CODE_HOME:-$HOME/.kimi-code}\""]);
        let output = crate::process_util::run_command_with_timeout(command, "WSL Kimi home", std::time::Duration::from_secs(10)).ok()?;
        if !output.status.success() { return None; }
        parse_marked_wsl_path(&output.stdout, "__BUILDMESH_KIMI_HOME__")?
    } else if runtime == EnvType::WindowsInterop {
        let command = super::powershell_command("[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); $path = if ([string]::IsNullOrWhiteSpace($env:KIMI_CODE_HOME)) { Join-Path $env:USERPROFILE '.kimi-code' } else { $env:KIMI_CODE_HOME }; [Console]::Write($path)");
        let output = crate::process_util::run_command_with_timeout(command, "Windows Kimi home", std::time::Duration::from_secs(10)).ok()?;
        if !output.status.success() { return None; }
        let path = String::from_utf8(output.stdout).ok()?;
        if !super::is_windows_path(path.trim()) { return None; }
        PathBuf::from(path.trim())
    } else {
        kimi_home_from_vars(cfg!(windows), |key| env::var(key).ok())?
    };
    Some(PathBuf::from(super::to_host_path(&path.to_string_lossy())))
}

fn kimi_home_from_vars(windows: bool, get: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(path) = get("KIMI_CODE_HOME").filter(|path| !path.trim().is_empty()) {
        return Some(PathBuf::from(path));
    }
    let home = if windows { get("USERPROFILE").or_else(|| get("HOME")) } else { get("HOME") }?;
    Some(PathBuf::from(home).join(".kimi-code"))
}

#[cfg(test)]
mod kimi_tests {
    use super::*;

    #[test]
    fn kimi_home_uses_runtime_override_and_native_home() {
        let vars = |key: &str| match key {
            "USERPROFILE" => Some("C:/Users/windows".into()),
            "HOME" => Some("/home/linux".into()),
            _ => None,
        };
        assert_eq!(kimi_home_from_vars(true, vars), Some(PathBuf::from("C:/Users/windows/.kimi-code")));
        assert_eq!(kimi_home_from_vars(false, vars), Some(PathBuf::from("/home/linux/.kimi-code")));
        assert_eq!(kimi_home_from_vars(false, |key| if key == "KIMI_CODE_HOME" { Some("/custom/kimi".into()) } else { vars(key) }), Some(PathBuf::from("/custom/kimi")));
        assert_eq!(kimi_home_from_vars(false, |_| None), None);
        assert_eq!(parse_marked_wsl_path(b"banner\n__BUILDMESH_KIMI_HOME__/custom/kimi\n", "__BUILDMESH_KIMI_HOME__"), Some(PathBuf::from("/custom/kimi")));
    }

    /// Issue #1752: the spawn-env probe must run **once per fingerprint**, not
    /// once per spawn. `probe` counts invocations so a regression that dropped
    /// the memo (re-introducing the per-spawn subprocess) trips here. The
    /// fingerprint is `(runtime, distro)`: a changed distro is a different key
    /// and must re-probe; a memoised `None` (failed probe) must NOT re-probe.
    #[test]
    fn spawn_env_memo_probes_once_per_fingerprint() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        clear_spawn_env_memo();
        let calls = AtomicUsize::new(0);
        let probe = |value: &str| {
            calls.fetch_add(1, Ordering::SeqCst);
            Some(PathBuf::from(value))
        };

        let ubuntu = spawn_env_memo_key(EnvType::Wsl, Some("Ubuntu"));
        assert_eq!(
            memoized_spawn_env(ubuntu.clone(), || probe("/home/u/.kimi-code")),
            Some(PathBuf::from("/home/u/.kimi-code"))
        );
        assert_eq!(
            memoized_spawn_env(ubuntu, || probe("/must-not-run")),
            Some(PathBuf::from("/home/u/.kimi-code")),
            "a repeated fingerprint must return the cached value, not re-probe"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "second call must hit the memo");

        // A different distro is a different fingerprint → re-probe.
        let debian = spawn_env_memo_key(EnvType::Wsl, Some("Debian"));
        assert_eq!(
            memoized_spawn_env(debian, || probe("/home/u/.kimi-code")),
            Some(PathBuf::from("/home/u/.kimi-code"))
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2, "a changed distro must re-probe");

        // A failed probe (`None`) must NOT be latched: a transient failure (WSL
        // not up yet during startup, a flaky subprocess) has to be retryable on
        // the next spawn, not memoised away for the process lifetime.
        let no_distro = spawn_env_memo_key(EnvType::Wsl, None);
        assert_eq!(
            memoized_spawn_env(no_distro.clone(), || {
                calls.fetch_add(1, Ordering::SeqCst);
                None
            }),
            None
        );
        assert_eq!(
            memoized_spawn_env(no_distro, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Some(PathBuf::from("/recovered"))
            }),
            Some(PathBuf::from("/recovered")),
            "a failed probe must be retried rather than served from a latched None"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            4,
            "the failed probe must run twice (once failing, once recovering) — \
             a negative result is not cached"
        );

        clear_spawn_env_memo();
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

/// The MiniMax Code data directory: `$MINIMAX_DATA_DIR` else
/// `$MAVIS_DATA_DIR` else `<home>/.minimax`. Sessions land under
/// `<dataDir>/v2/sessions/` (one dated directory per session carrying
/// `manifest.json` + `messages.jsonl`). The `~/.minimax-code` install
/// directory is a separate choice and never holds sessions. Mirrors the
/// CLI's own resolution (verified against the shipped `@minimax-ai/code`
/// 0.4.12 data-dir chunk).
///
/// Note: under WSL the spawn-aware lookup (`cli_dir_for_spawn`) resolves
/// the guest `$HOME/.minimax` and ignores a host-side `$MINIMAX_DATA_DIR`
/// override — same convention as the other harness home helpers.
pub fn minimax_data_dir() -> PathBuf {
    let override_dir = env::var("MINIMAX_DATA_DIR")
        .or_else(|_| env::var("MAVIS_DATA_DIR"))
        .map(|dir| dir.trim().to_string())
        .ok()
        .filter(|dir| !dir.is_empty());
    if let Some(dir) = override_dir {
        return PathBuf::from(dir);
    }
    match current_env() {
        Environment::Wsl => env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/root"))
            .join(".minimax"),
        Environment::Windows => env::var("USERPROFILE")
            .or_else(|_| env::var("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let user = env::var("USERNAME").unwrap_or_else(|_| "Public".to_string());
                PathBuf::from(format!("C:\\Users\\{user}"))
            })
            .join(".minimax"),
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

// ── Cline CLI home ──────────────────────────────────────────────────────────
//
// Issue #1773 wired the Cline CLI as a first-class agent harness. Issue
// #1774 captures the session id it self-assigns
// (`<epochms>_<5 chars>` legacy, `session_<epochms>_<6 chars>` current)
// from `~/.cline/data/db/sessions.db`. The capture helper below resolves
// that home directory and SQLite store from the *spawn* environment
// (matches `codex_dir_for_env` / `agy_dir_for_env`) so a Windows
// Buildmesh driving a WSL Cline still reads the guest-side store.
//
// `--data-dir` is an explicit Cline runtime override for parallel runs;
// `CLINE_DATA_DIR` is the matching env var. Issue #1769 research pinned
// `--help` as the source of truth, so we honour both with the same
// precedence Cline does: explicit flag > env > default `~/.cline/data`.
// Round 1 review note: `CLINE_DATA_DIR` IS the data directory (it does
// not point at `~/.cline`); the DB sits at `$CLINE_DATA_DIR/db/sessions.db`,
// not `$CLINE_DATA_DIR/data/db/sessions.db`. The split below keeps the
// two shapes separate.
///
/// WSL guests run on the Linux userland, so the path returned by
/// [`cline_db_path_for_env`] is a guest POSIX path. The Windows-side
/// SQLite `Connection::open` call in
/// `services::cline_session::try_capture_from_db_path` must convert
/// that to a `\\wsl$\…` UNC path via [`cline_db_path_for_host`] (the
/// `host_path` module is the only place a UNC string is built — see
/// CLAUDE.md rule 21 / the `HostPath` sub-module).

/// Honour the Cline `--data-dir` / `CLINE_DATA_DIR` override for the
/// resolved environment. `get` lets tests inject a fake env without
/// mutating process state. The `env_type` parameter is reserved for
/// future per-env override shapes (e.g. a WSL-specific lookup) — for
/// now the same env var is honoured on every runtime.
///
/// Round 2 review: returns `None` when `CLINE_DATA_DIR` is unset,
/// empty, or whitespace-only. `is_some()` on a process env var is
/// *not* the same predicate (empty strings still report `Some`), so
/// the suffix decision in [`cline_db_path_with_resolver`] routes
/// through this helper, never through `std::env::var_os`.
fn cline_data_dir_override_for_env<F: Fn(&str) -> Option<std::ffi::OsString>>(
    _env_type: EnvType,
    get: F,
) -> Option<PathBuf> {
    let raw = get("CLINE_DATA_DIR")?;
    let raw = raw.to_string_lossy().trim().to_string();
    if raw.is_empty() {
        return None;
    }
    Some(PathBuf::from(raw))
}

/// Absolute path to Cline's authoritative session store, with an
/// injectable env resolver. The resolver closes over `std::env` in
/// production and over an in-memory map in tests; tests therefore
/// never touch process state and stay race-free under cargo's default
/// multi-threaded runner.
///
/// Suffix decision: the override path *is* the data directory, so the
/// DB sits at `<override>/db/sessions.db`. The bare-home derivation
/// points at `~/.cline`, so the DB sits at `<home>/data/db/sessions.db`.
/// Round 2 review: the suffix decision keys off whether the override
/// actually resolved (post-trim, post-non-empty check), not whether the
/// env var is set — `CLINE_DATA_DIR=""` reports `Some("")` via
/// `var_os`, but the override returns `None` here, so the bare-home
/// path picks the `data/db/sessions.db` suffix (the bug the empty-string
/// guard prevents).
pub(crate) fn cline_db_path_with_resolver<
    F: Fn(&str) -> Option<std::ffi::OsString>,
>(
    env_type: EnvType,
    spawn_path: &str,
    get: F,
) -> Option<PathBuf> {
    if let Some(override_dir) = cline_data_dir_override_for_env(env_type, &get) {
        return Some(override_dir.join("db").join("sessions.db"));
    }
    // Default derivation: cline home + data/db/sessions.db.
    // The native arm goes through the same injectable `get` as the override above,
    // so the whole helper is hermetic: a test that injects an environment must
    // not pick up the process CLINE_DIR through cline_dir()'s live-env wrapper.
    let dir = match env_type {
        EnvType::WindowsInterop => super::windows_cli_home(".cline")?,
        EnvType::Windows => {
            let _ = spawn_path;
            cline_dir_with_resolver(current_env(), &get)
        }
        EnvType::Wsl => {
            let _ = spawn_path;
            wsl_home()?.join(".cline")
        }
    };
    Some(dir.join("data").join("db").join("sessions.db"))
}

/// Absolute path to Cline's authoritative session store:
/// `<data dir>/db/sessions.db` when `CLINE_DATA_DIR` resolves to a
/// non-empty path, otherwise `<cline home>/data/db/sessions.db` (issue
/// #1769 research). Returns `None` when the home directory is unknown
/// (no `$HOME` / `$USERPROFILE`).
///
/// The returned path is in the *spawn* environment's native syntax. For
/// a Windows-side reader driving a WSL Cline, the caller must route
/// through [`cline_db_path_for_host`] to convert the guest POSIX path
/// to a Windows-host UNC path.
pub(crate) fn cline_db_path_for_env(env_type: EnvType, spawn_path: &str) -> Option<PathBuf> {
    cline_db_path_with_resolver(env_type, spawn_path, |k| env::var_os(k))
}

/// Windows-host form of the Cline session store. For Windows and
/// WindowsInterop spawn paths this is a no-op (the spawn path already
/// lives on the host); for `EnvType::Wsl` the guest POSIX path is
/// converted to the matching `\\wsl$\…` UNC path that a Rust
/// `Connection::open` can read. The conversion runs through
/// `host_path::to_host_path`, the single module allowed to build UNC
/// strings (CLAUDE.md rule 21).
pub(crate) fn cline_db_path_for_host(env_type: EnvType, spawn_path: &str) -> Option<PathBuf> {
    let db_path = cline_db_path_for_env(env_type, spawn_path)?;
    let db_str = db_path.to_string_lossy();
    Some(PathBuf::from(super::to_host_path(&db_str)))
}

/// Cline session-id capture root for the **current** (running) Buildmesh
/// environment. Used by tests and by helpers that do not have a `EnvType`
/// in scope. Mirrors the structure of [`codex_dir`] / [`agy_dir`] —
/// no `CLINE_DATA_DIR` honoured at this level (the spawn-aware helpers
/// above are the only ones that need to reason about guest vs host).
pub fn cline_dir() -> PathBuf {
    cline_dir_with_resolver(current_env(), |key| env::var_os(key))
}

/// Cline's own home precedence (`resolveClineDir()` in
/// `@cline/shared/storage`, verified in the shipped 3.0.62 bundle): an
/// explicit `CLINE_DIR` wins, otherwise `<home>/.cline`. `get` lets tests
/// inject an environment without mutating process state.
///
/// Honouring `CLINE_DIR` keeps Buildmesh's hook directory and session store
/// under the same root Cline actually uses: without it, a user who set the
/// override would get a hook written to `~/.cline/hooks` that Cline never
/// searches, and a capture poller reading the wrong store. A blank or
/// whitespace-only value collapses to unset (the same normalisation the
/// `CLINE_DATA_DIR` helper applies). The spawn-aware WSL/Interop arms do
/// **not** honour it - that override lives in the guest environment, which
/// this process cannot read.
pub(crate) fn cline_dir_with_resolver<F: Fn(&str) -> Option<std::ffi::OsString>>(
    environment: Environment,
    get: F,
) -> PathBuf {
    if let Some(dir) = get("CLINE_DIR").and_then(|value| {
        let trimmed = value.to_string_lossy().trim().to_string();
        (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
    }) {
        return dir;
    }
    match environment {
        Environment::Wsl => get("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/root"))
            .join(".cline"),
        Environment::Windows => get("USERPROFILE")
            .or_else(|| get("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let user = get("USERNAME")
                    .map(|value| value.to_string_lossy().to_string())
                    .unwrap_or_else(|| "Public".to_string());
                PathBuf::from(format!("C:\\Users\\{user}"))
            })
            .join(".cline"),
    }
}
