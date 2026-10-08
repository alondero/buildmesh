use crate::agent::capabilities::{EffortControlKind, CODEX_EFFORT_ALLOWED, CODEX_EFFORT_KEY};
use crate::agent::provider::{
    AgentProvider, LaunchRuntime, Platform, SpawnRecipe, UiMeta, WindowsShell,
};
use crate::env::ResolvedPath;
use crate::models::EnvType;
use base64::Engine;
use once_cell::sync::Lazy;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use toml_edit::{value, DocumentMut, Item, Table};

pub struct CodexAdapter;
pub static CODEX: CodexAdapter = CodexAdapter;

/// Native `request_user_input` hooks were verified against Codex 0.154.0.
/// Older binaries may accept the config but omit the callbacks Buildmesh uses
/// to track an outstanding human question.
const CODEX_MIN_HOOK_VERSION: &str = "0.154.0";

fn shell_for(platform: Platform) -> WindowsShell {
    match platform {
        Platform::Macos | Platform::Linux => WindowsShell::Direct,
        Platform::Windows => WindowsShell::PowerShell,
    }
}

fn base_flags() -> Vec<String> {
    let sandbox = if std::env::var_os(CODEX_HOOK_RELAY_ENV).is_some() {
        // Controlled hook-delivery runs may exercise real callbacks, but
        // must not give the model a path to alter the fixture or workspace.
        "read-only"
    } else {
        "danger-full-access"
    };
    base_flags_with_sandbox(sandbox)
}

fn base_flags_with_sandbox(sandbox: &str) -> Vec<String> {
    vec![
        "--ask-for-approval".into(),
        "never".into(),
        "--sandbox".into(),
        sandbox.into(),
        // Do not force `--no-alt-screen`: Codex's fullscreen transcript avoids
        // rebuilding terminal scrollback after width changes.
        // Run the project-local `.codex/hooks.json` hooks without Codex's
        // interactive hook-review prompt (issue #884) — a headless spawn
        // must never block on a trust prompt. The adapter also provisions the
        // runtime's project trust entry before launch.
        "--dangerously-bypass-hook-trust".into(),
    ]
}

/// Codex already launches hook commands through `cmd.exe /C` (Windows) or
/// `$SHELL -lc` (Unix), then `env_clear()`s down to a Core inherit snapshot.
/// Nested `cmd.exe /c "%BUILDMESH_PORT%"` therefore never expands and never
/// sees stdin. Bake the loopback callback URL and let Codex's own shell run
/// curl. Discard the HTTP response body and print `{}`: Codex Stop requires
/// JSON on stdout. Attention callbacks are best-effort notifications, so a
/// stopped server or stale node must not fail the Codex hook itself.
fn attention_hook_unix_command(url: &str) -> String {
    format!(
        "curl -fsS --connect-timeout 2 --max-time 10 -o /dev/null -X POST --data-binary @- {url} 2>/dev/null || true; printf '{{}}'"
    )
}

/// A Windows host driving a *WSL* Codex runs a Linux binary, so `command`
/// reaches `$SHELL -lc` — not cmd.exe. `curl.exe` leaves WSL to reach the
/// Windows loopback listener the node's Buildmesh owns; a Linux curl would
/// resolve `localhost` inside the distro and find nothing.
fn attention_hook_wsl_shell_command(url: &str) -> String {
    format!(
        "if command -v curl.exe >/dev/null 2>&1; then curl.exe -fsS --connect-timeout 2 --max-time 10 -o NUL -X POST --data-binary @- {url} 2>/dev/null || true; else curl -fsS --connect-timeout 2 --max-time 10 -o /dev/null -X POST --data-binary @- {url} 2>/dev/null || true; fi; printf '{{}}'"
    )
}

fn attention_hook_windows_fallback_command(url: &str) -> String {
    let url = crate::env::powershell_literal(url);
    let script = format!(
        "$ErrorActionPreference = 'Stop'; $body = [Console]::In.ReadToEnd(); try {{ Invoke-WebRequest -UseBasicParsing -Method Post -Uri {url} -ContentType 'application/json' -Body $body -TimeoutSec 10 | Out-Null }} catch {{ }}; [Console]::Out.WriteLine('{{}}'); exit 0"
    );
    format!(
        "powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {}",
        crate::env::encode_powershell(&script)
    )
}

const CODEX_HOOK_RELAY_ENV: &str = "BUILDMESH_CODEX_HOOK_RELAY_URL";

fn attention_hook_url(node_id: i64, app_port: u16, relay_url: Option<&str>) -> String {
    let app_path = format!("/api/attention/{node_id}");
    if let Some(origin) = relay_url.and_then(valid_loopback_relay_origin) {
        return format!("{origin}{app_path}?forward_port={app_port}");
    }
    format!("http://localhost:{app_port}{app_path}")
}

fn valid_loopback_relay_origin(value: &str) -> Option<&str> {
    let origin = value.trim().trim_end_matches('/');
    let authority = origin.strip_prefix("http://")?;
    let (host, port) = authority.rsplit_once(':')?;
    if host != "127.0.0.1" || port.parse::<u16>().ok().filter(|port| *port != 0).is_none() {
        return None;
    }
    Some(origin)
}

/// The callback a Windows Codex binary runs. Encoded PowerShell carries the
/// URL, the stdin read and the `{}` stdout contract without shell-specific
/// quoting, so it survives Codex's own `cmd.exe /C` and a PowerShell host
/// alike.
fn attention_hook_windows_command(url: &str) -> String {
    crate::env::windows_attention_command_with_json_output(Some(url))
        .unwrap_or_else(|| attention_hook_windows_fallback_command(url))
}

/// The `command` Codex runs when it does not prefer `commandWindows`.
///
/// Codex 0.131.0+ resolves a command hook with
/// `command_windows.unwrap_or(command)` under `cfg!(windows)`
/// (`codex-rs/hooks/src/engine/discovery.rs`), so only a Windows binary reads
/// the override. An older binary ignores the unknown key and hands `command`
/// to `cmd.exe /C`, which fails on a POSIX script with `-v was unexpected at
/// this time.` — so `command` has to be written for the shell that will really
/// parse it, which is the *target* Codex binary, not the Buildmesh host.
fn attention_hook_default_command(url: &str, env_type: EnvType) -> String {
    if env_type == EnvType::Wsl && cfg!(windows) {
        return attention_hook_wsl_shell_command(url);
    }
    // Every other runtime whose Codex is a Windows binary — spawned natively,
    // or reached through WSL interop — parses `command` with cmd.exe.
    let native = cfg!(windows) && env_type == EnvType::Windows;
    if native || env_type == EnvType::WindowsInterop {
        return attention_hook_windows_command(url);
    }
    attention_hook_unix_command(url)
}

fn attention_hook_handler(node_id: i64, env_type: EnvType) -> serde_json::Value {
    let port = crate::http_server::current_http_port();
    let relay_url = std::env::var(CODEX_HOOK_RELAY_ENV).ok();
    let url = attention_hook_url(node_id, port, relay_url.as_deref());
    serde_json::json!({
        "type": "command",
        "command": attention_hook_default_command(&url, env_type),
        "commandWindows": attention_hook_windows_command(&url),
        "statusMessage": BUILDMESH_HOOK_STATUS_MESSAGE,
    })
}

const BUILDMESH_HOOK_STATUS_MESSAGE: &str = "Buildmesh attention callback";

pub const PROXY_CREDENTIAL_ENV: &str = "BUILDMESH_CODEX_PROVIDER_KEY";
pub const MIN_PROXY_CODEX_VERSION: (u32, u32, u32) = (0, 144, 0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexInstall {
    pub executable: String,
    pub version: String,
    pub runtime_identity: String,
    pub codex_home: String,
    pub wsl_distro: Option<String>,
}

static PROFILE_WRITE_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
static ATTENTION_CONFIG_WRITE_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
static CLI_CAPABILITY_CACHE: Lazy<Mutex<HashSet<String>>> =
    Lazy::new(|| Mutex::new(HashSet::new()));
// Bound stale executable/version metadata after an out-of-process CLI update.
const CODEX_INSTALL_CACHE_TTL: Duration = Duration::from_secs(45);

struct CachedCodexInstall {
    resolved_at: Instant,
    result: Result<CodexInstall, String>,
}

struct CodexInstallCacheSlot {
    cell: Arc<OnceLock<CachedCodexInstall>>,
    refreshing: bool,
}

#[derive(Default)]
struct CodexInstallCache {
    entries: Mutex<HashMap<&'static str, CodexInstallCacheSlot>>,
}

impl CodexInstallCache {
    fn discover(
        &self,
        env_type: EnvType,
        probe: impl FnOnce() -> Result<CodexInstall, String>,
    ) -> Result<CodexInstall, String> {
        self.discover_at(env_type, Instant::now(), probe)
    }

    fn discover_at(
        &self,
        env_type: EnvType,
        now: Instant,
        probe: impl FnOnce() -> Result<CodexInstall, String>,
    ) -> Result<CodexInstall, String> {
        let runtime = runtime_identity(env_type);
        let cell = {
            let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
            match entries.get(runtime) {
                Some(slot)
                    if slot
                        .cell
                        .get()
                        .is_none_or(|cached| is_entry_fresh(cached, now)) =>
                {
                    Arc::clone(&slot.cell)
                }
                _ => {
                    let cell = Arc::new(OnceLock::new());
                    entries.insert(
                        runtime,
                        CodexInstallCacheSlot {
                            cell: Arc::clone(&cell),
                            refreshing: false,
                        },
                    );
                    cell
                }
            }
        };

        let result = resolve_codex_install_cell(&cell, probe);
        if result.is_err() {
            self.discard_failed_entry(runtime, &cell);
        }
        result
    }

    fn discover_fresh(
        &self,
        env_type: EnvType,
        probe: impl FnOnce() -> Result<CodexInstall, String>,
    ) -> Result<CodexInstall, String> {
        let runtime = runtime_identity(env_type);
        let cell = {
            let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
            match entries.get_mut(runtime) {
                Some(slot) if slot.refreshing => Arc::clone(&slot.cell),
                _ => {
                    let cell = Arc::new(OnceLock::new());
                    entries.insert(
                        runtime,
                        CodexInstallCacheSlot {
                            cell: Arc::clone(&cell),
                            refreshing: true,
                        },
                    );
                    cell
                }
            }
        };

        let result = resolve_codex_install_cell(&cell, probe);
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        if entries
            .get(runtime)
            .is_some_and(|slot| Arc::ptr_eq(&slot.cell, &cell))
        {
            if result.is_err() {
                entries.remove(runtime);
            } else if let Some(slot) = entries.get_mut(runtime) {
                slot.refreshing = false;
            }
        }
        result
    }

    /// Whether the next [`CodexInstallCache::discover`] for this runtime
    /// would reuse a cached install instead of probing (issue #1948).
    /// Read-only: never inserts, never probes. An in-flight probe started
    /// by another thread reports `false` — joining it still waits on
    /// process work, so for timing attribution it is cold, not cached.
    /// Same freshness predicate as `discover_at`, so the two never disagree
    /// on what counts as cached.
    fn is_fresh(&self, env_type: EnvType) -> bool {
        self.is_fresh_at(env_type, Instant::now())
    }

    fn is_fresh_at(&self, env_type: EnvType, now: Instant) -> bool {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        entries.get(runtime_identity(env_type)).is_some_and(|slot| {
            slot.cell
                .get()
                .is_some_and(|cached| is_entry_fresh(cached, now))
        })
    }

    fn discard_failed_entry(
        &self,
        runtime: &'static str,
        cell: &Arc<OnceLock<CachedCodexInstall>>,
    ) {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        if entries
            .get(runtime)
            .is_some_and(|slot| Arc::ptr_eq(&slot.cell, cell))
        {
            entries.remove(runtime);
        }
    }
}

/// Freshness predicate shared by [`CodexInstallCache::discover_at`] (cache
/// reuse) and [`CodexInstallCache::is_fresh_at`] (cold-vs-warm reporting) so
/// the two can never disagree on what counts as cached (issue #1948).
/// `now` is read before the cache lock, so a concurrent resolution can be newer
/// than it; that entry's age is zero, not unknown.
fn is_entry_fresh(cached: &CachedCodexInstall, now: Instant) -> bool {
    now.saturating_duration_since(cached.resolved_at) < CODEX_INSTALL_CACHE_TTL
}

fn resolve_codex_install_cell(
    cell: &OnceLock<CachedCodexInstall>,
    probe: impl FnOnce() -> Result<CodexInstall, String>,
) -> Result<CodexInstall, String> {
    cell.get_or_init(|| {
        let result = probe();
        CachedCodexInstall {
            resolved_at: Instant::now(),
            result,
        }
    })
    .result
    .clone()
}

/// Keep Settings reads fast without hiding a Codex CLI update for long: Codex
/// can update independently while Buildmesh stays open. Per-runtime OnceLocks
/// share one in-flight probe, while this map mutex never spans process work.
static CODEX_INSTALL_CACHE: Lazy<CodexInstallCache> = Lazy::new(CodexInstallCache::default);

pub fn runtime_identity(env_type: EnvType) -> &'static str {
    match env_type {
        EnvType::Wsl => "wsl",
        EnvType::WindowsInterop => "windows-interop",
        EnvType::Windows if cfg!(target_os = "windows") => "native-windows",
        EnvType::Windows if cfg!(target_os = "macos") => "native-macos",
        EnvType::Windows => "native-linux",
    }
}

fn wsl_runtime_identity(distro: &str, codex_home: &str) -> String {
    format!("wsl:{distro}:{codex_home}")
}

/// Log- and UI-safe runtime identity for diagnostics (issue #1937): the WSL
/// identity is `wsl:{distro}:{codex_home}`, and the home path can carry the
/// guest username — report `wsl:{distro}` so support logs and error surfaces
/// never include filesystem paths. Native identities pass through unchanged.
/// Built from the `wsl_distro` field (never by parsing the identity string),
/// so the home path is structurally unreachable. The stored
/// `runtime_identity` (DB keys, signatures) is untouched.
pub fn log_safe_runtime_identity(install: &CodexInstall) -> String {
    if let Some(distro) = install.wsl_distro.as_deref() {
        format!("wsl:{distro}")
    } else {
        install.runtime_identity.clone()
    }
}

pub fn stable_profile_name(harness_id: &str, provider_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(harness_id.as_bytes());
    digest.update([0]);
    digest.update(provider_id.as_bytes());
    format!("buildmesh_{}", &hex::encode(digest.finalize())[..16])
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).expect("serializing a Rust string cannot fail")
}

pub fn render_proxy_profile(
    profile_name: &str,
    provider_display_name: &str,
    base_url: &str,
) -> String {
    let profile = toml_string(profile_name);
    format!(
        "model_provider = {profile}\n\n[model_providers.{profile_name}]\nname = {}\nbase_url = {}\nwire_api = \"responses\"\nenv_key = \"{PROXY_CREDENTIAL_ENV}\"\nrequires_openai_auth = false\n",
        toml_string(&format!("Buildmesh: {provider_display_name}")),
        toml_string(base_url),
    )
}

fn native_codex_home_from(
    codex_home: Option<std::ffi::OsString>,
    user_home: Option<std::ffi::OsString>,
) -> Result<std::path::PathBuf, String> {
    codex_home
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| user_home.map(|p| std::path::PathBuf::from(p).join(".codex")))
        .ok_or_else(|| "could not resolve the runtime Codex home".to_string())
}

fn native_codex_home() -> Result<std::path::PathBuf, String> {
    let user_home_key = if cfg!(target_os = "windows") {
        "USERPROFILE"
    } else {
        "HOME"
    };
    native_codex_home_from(
        std::env::var_os("CODEX_HOME"),
        std::env::var_os(user_home_key),
    )
}

fn is_owned_legacy_profile(path: &Path, content: &str) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(profile_name) = file_name.strip_suffix(".config.toml") else {
        return false;
    };
    let Some(hash) = profile_name.strip_prefix("bm") else {
        return false;
    };
    if hash.len() != 16
        || !hash
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    {
        return false;
    }

    let mut lines = content.lines();
    let mut first = lines.next();
    if first.is_some_and(|line| line.starts_with("model = \"") && line.ends_with('"')) {
        first = lines.next();
    }
    first == Some(&format!("model_provider = \"{profile_name}\""))
        && lines.next() == Some("")
        && lines.next() == Some(&format!("[model_providers.{profile_name}]"))
        && lines.next() == Some(&format!("name = \"Buildmesh proxy {profile_name}\""))
        && lines
            .next()
            .is_some_and(|line| line.starts_with("base_url = \"") && line.ends_with('"'))
        && lines.next() == Some("env_key = \"OPENAI_API_KEY\"")
        && lines.next() == Some("requires_openai_auth = true")
        && lines.next().is_none()
}

fn cleanup_owned_legacy_profiles(home: &Path) {
    let Ok(entries) = std::fs::read_dir(home) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if is_owned_legacy_profile(&path, &content) {
            if let Err(error) = std::fs::remove_file(&path) {
                tracing::warn!(
                    "failed to remove owned legacy Codex profile {:?}: {error}",
                    path
                );
            }
        }
    }
}

fn materialize_native_profile_at(
    home: &Path,
    profile_name: &str,
    content: &str,
) -> Result<(), String> {
    let _guard = PROFILE_WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    std::fs::create_dir_all(home)
        .map_err(|e| format!("failed to create Codex home {}: {e}", home.display()))?;
    let target = home.join(format!("{profile_name}.config.toml"));
    if std::fs::read_to_string(&target).ok().as_deref() == Some(content) {
        cleanup_owned_legacy_profiles(home);
        return Ok(());
    }
    let mut temp = tempfile::NamedTempFile::new_in(home)
        .map_err(|e| format!("failed to create temporary Codex profile: {e}"))?;
    use std::io::Write;
    temp.write_all(content.as_bytes())
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|e| format!("failed to write temporary Codex profile: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("failed to restrict Codex profile permissions: {e}"))?;
    }
    temp.persist(&target).map_err(|e| {
        format!(
            "failed to atomically replace {}: {}",
            target.display(),
            e.error
        )
    })?;
    cleanup_owned_legacy_profiles(home);
    Ok(())
}

const WSL_PROFILE_SCRIPT: &str = r#"set -eu
d="${BUILDMESH_CODEX_PROFILE_HOME:?}"
mkdir -p "$d"
chmod 700 "$d" 2>/dev/null || true
target="$d/${BUILDMESH_CODEX_PROFILE_NAME:?}.config.toml"
tmp="$d/.${BUILDMESH_CODEX_PROFILE_NAME}.$$.tmp"
printf %s "${BUILDMESH_CODEX_PROFILE_CONTENT:?}" | base64 -d > "$tmp"
chmod 600 "$tmp"
if [ -f "$target" ] && cmp -s "$tmp" "$target"; then rm -f "$tmp"; else mv -f "$tmp" "$target"; fi
for legacy in "$d"/bm*.config.toml; do
  [ -f "$legacy" ] || continue
  file=${legacy##*/}; profile=${file%.config.toml}; hash=${profile#bm}
  [ ${#hash} -eq 16 ] || continue
  case "$hash" in *[!0-9a-f]*) continue ;; esac
  if awk -v p="$profile" '
    NR == 1 && /^model = ".*"$/ { next }
    { n++; line[n] = $0 }
    END {
      ok = n == 7 &&
        line[1] == "model_provider = \"" p "\"" &&
        line[2] == "" &&
        line[3] == "[model_providers." p "]" &&
        line[4] == "name = \"Buildmesh proxy " p "\"" &&
        line[5] ~ /^base_url = ".*"$/ &&
        line[6] == "env_key = \"OPENAI_API_KEY\"" &&
        line[7] == "requires_openai_auth = true"
      exit !ok
    }' "$legacy"; then rm -f "$legacy"; fi
done"#;
const WSL_CODEX_HOME_SCRIPT: &str =
    "printf '__BUILDMESH_WSL_CODEX_HOME__%s\\n' \"${CODEX_HOME:-$HOME/.codex}\"";

fn materialize_wsl_profile(
    distro: &str,
    codex_home: &str,
    profile_name: &str,
    content: &str,
) -> Result<(), String> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(content);
    let mut command = crate::process_util::command_no_window("wsl.exe");
    command.args(["-d", distro, "--exec", "sh", "-c", WSL_PROFILE_SCRIPT]);
    const PROFILE_HOME_ENV: &str = "BUILDMESH_CODEX_PROFILE_HOME";
    const PROFILE_NAME_ENV: &str = "BUILDMESH_CODEX_PROFILE_NAME";
    const PROFILE_CONTENT_ENV: &str = "BUILDMESH_CODEX_PROFILE_CONTENT";
    let mut wslenv = std::env::var("WSLENV").unwrap_or_default();
    for name in [PROFILE_HOME_ENV, PROFILE_NAME_ENV, PROFILE_CONTENT_ENV] {
        if !wslenv
            .split(':')
            .any(|part| part.split('/').next() == Some(name))
        {
            if !wslenv.is_empty() {
                wslenv.push(':');
            }
            wslenv.push_str(name);
        }
    }
    command
        .env(PROFILE_HOME_ENV, codex_home)
        .env(PROFILE_NAME_ENV, profile_name)
        .env(PROFILE_CONTENT_ENV, encoded)
        .env("WSLENV", wslenv);
    let status = command
        .status()
        .map_err(|e| format!("failed to materialize WSL Codex profile: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "WSL Codex profile materialization exited with {status}"
        ))
    }
}

const WSL_FILE_END_MARKER: &str = "__BUILDMESH_CODEX_FILE_END__";

const WSL_READ_FILES_SCRIPT: &str = r#"set -eu
for p in "$@"; do
  if [ -f "$p" ]; then base64 "$p"; fi
  printf '%s\n' '__BUILDMESH_CODEX_FILE_END__'
done
"#;

const WSL_ATOMIC_WRITE_FILES_SCRIPT: &str = r#"set -eu
i=0
while [ "$#" -gt 0 ]; do
  p="$1"; shift
  case "$p" in
    */*) d="${p%/*}" ;;
    *) d="." ;;
  esac
  [ -n "$d" ] || d="."
  if [ ! -d "$d" ]; then
    mkdir -p "$d"
    chmod 700 "$d" 2>/dev/null || true
  fi
  tmp="$d/.buildmesh-codex.$$.${i}.tmp"
  IFS= read -r encoded || exit 1
  printf %s "$encoded" | base64 -d > "$tmp"
  chmod 600 "$tmp"
  if [ -f "$p" ] && cmp -s "$tmp" "$p"; then rm -f "$tmp"; else mv -f "$tmp" "$p"; fi
  i=$((i + 1))
done
"#;

fn wsl_command_with_values(
    distro: &str,
    script: &str,
    values: &[(&str, &str)],
    args: &[&str],
) -> std::process::Command {
    let mut command = crate::process_util::command_no_window("wsl.exe");
    command.args(["-d", distro, "--exec", "sh", "-c", script, "--"]);
    command.args(args);
    let mut wslenv = std::env::var("WSLENV").unwrap_or_default();
    for (name, value) in values {
        command.env(name, value);
        crate::agent::spawn_environment::append_to_wslenv(&mut wslenv, name, "");
    }
    command.env("WSLENV", wslenv);
    command
}

fn wsl_read_files(distro: &str, paths: &[&Path]) -> Result<Vec<String>, String> {
    let path_strings: Vec<String> = paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    let path_args: Vec<&str> = path_strings.iter().map(String::as_str).collect();
    let command = wsl_command_with_values(distro, WSL_READ_FILES_SCRIPT, &[], &path_args);
    let output = crate::process_util::run_command_with_timeout(
        command,
        "WSL Codex file read",
        CODEX_LOOKUP_TIMEOUT,
    )?;
    if !output.status.success() {
        return Err(format!("WSL Codex file read exited with {}", output.status));
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| format!("WSL Codex file read was not UTF-8: {e}"))?;
    let mut encoded_files = Vec::with_capacity(paths.len());
    let mut current = String::new();
    for line in stdout.split('\n') {
        let line = line.trim_end_matches('\r');
        if line == WSL_FILE_END_MARKER {
            encoded_files.push(std::mem::take(&mut current));
        } else {
            current.push_str(line);
        }
    }
    if !current.is_empty() || encoded_files.len() != paths.len() {
        return Err(format!(
            "WSL Codex file read returned {} files for {} requested",
            encoded_files.len(),
            paths.len()
        ));
    }
    encoded_files
        .into_iter()
        .enumerate()
        .map(|(index, encoded)| {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|e| format!("WSL Codex file {index} was not valid base64: {e}"))?;
            String::from_utf8(bytes)
                .map_err(|e| format!("WSL Codex file {index} is not UTF-8: {e}"))
        })
        .collect()
}

fn wsl_write_files(distro: &str, files: &[(&Path, &str)]) -> Result<(), String> {
    if files.is_empty() {
        return Ok(());
    }
    let mut args = Vec::with_capacity(files.len());
    let mut payload = String::new();
    for (path, content) in files {
        let encoded = base64::engine::general_purpose::STANDARD.encode(content);
        args.push(path.to_string_lossy().into_owned());
        payload.push_str(&encoded);
        payload.push('\n');
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut command =
        wsl_command_with_values(distro, WSL_ATOMIC_WRITE_FILES_SCRIPT, &[], &arg_refs);
    command.stdin(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| format!("failed to write WSL Codex files: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        if let Err(error) = stdin.write_all(payload.as_bytes()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("failed to send WSL Codex file payload: {error}"));
        }
    }
    let status = child
        .wait()
        .map_err(|e| format!("failed to wait for WSL Codex file write: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("WSL Codex file write exited with {status}"))
    }
}

fn read_runtime_files(paths: &[&Path], distro: Option<&str>) -> Result<Vec<String>, String> {
    match distro {
        Some(distro) => wsl_read_files(distro, paths),
        None => paths
            .iter()
            .map(|path| match std::fs::read_to_string(path) {
                Ok(content) => Ok(content),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
                Err(error) => Err(format!(
                    "failed to read Codex file {}: {error}",
                    path.display()
                )),
            })
            .collect(),
    }
}

fn write_runtime_files(files: &[(&Path, &str)], distro: Option<&str>) -> Result<(), String> {
    match distro {
        Some(distro) => wsl_write_files(distro, files),
        None => files.iter().try_for_each(|(path, content)| {
            write_atomic(path, content)
                .map_err(|e| format!("failed to write Codex file {}: {e}", path.display()))
        }),
    }
}

/// Resolve the Codex home used by the runtime that will execute a spawn.
/// Proxy preflight supplies the exact home/distro used by the child. Native
/// launches without a prepared install resolve the ordinary environment once.
/// WSL paths remain guest paths here so all WSL file operations use the guest
/// CLI rather than host-side UNC writes.
fn runtime_codex_home(
    env_type: EnvType,
    runtime: &LaunchRuntime,
) -> Result<(PathBuf, Option<String>), String> {
    if env_type == EnvType::WindowsInterop {
        let home = runtime
            .harness_home
            .as_deref()
            .map(|home| PathBuf::from(crate::env::to_host_path(home)))
            .or_else(|| crate::env::codex_dir_for_env(env_type, ""))
            .ok_or_else(|| "Windows Codex home is unavailable".to_string())?;
        return Ok((home, None));
    }
    if env_type == EnvType::Windows {
        return Ok((
            runtime
                .harness_home
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or(native_codex_home()?),
            None,
        ));
    }

    let distro = runtime
        .wsl_distro
        .clone()
        .or_else(crate::env::get_default_wsl_distro)
        .ok_or_else(|| "could not resolve the WSL distribution for Codex trust".to_string())?;
    if let Some(home) = runtime
        .harness_home
        .as_deref()
        .filter(|home| !home.is_empty())
    {
        return Ok((PathBuf::from(home), Some(distro)));
    }

    let mut command = crate::process_util::command_no_window("wsl.exe");
    command.args(["-d", &distro, "--exec", "sh", "-lc", WSL_CODEX_HOME_SCRIPT]);
    let output = crate::process_util::run_command_with_timeout(
        command,
        "WSL Codex home resolution for trust",
        CODEX_LOOKUP_TIMEOUT,
    )?;
    let Some(home) = crate::env::parse_wsl_codex_home_output(&output.stdout) else {
        return Err("WSL Codex home identity is unavailable for trust".to_string());
    };
    if !output.status.success() {
        return Err("WSL Codex home identity is unavailable for trust".to_string());
    }
    Ok((home, Some(distro)))
}

fn runtime_wsl_distro(
    env_type: EnvType,
    runtime: &LaunchRuntime,
) -> Result<Option<String>, String> {
    if matches!(env_type, EnvType::Windows | EnvType::WindowsInterop) {
        return Ok(None);
    }
    runtime
        .wsl_distro
        .clone()
        .or_else(crate::env::get_default_wsl_distro)
        .map(Some)
        .ok_or_else(|| "could not resolve the WSL distribution for Codex hooks".to_string())
}

fn codex_trust_config_path(
    env_type: EnvType,
    runtime: &LaunchRuntime,
) -> Result<(PathBuf, Option<String>), String> {
    let (home, distro) = runtime_codex_home(env_type, runtime)?;
    Ok((home.join("config.toml"), distro))
}

fn trust_project_path(resolved: &ResolvedPath) -> String {
    let path = if matches!(resolved.env_type, EnvType::Wsl | EnvType::WindowsInterop) {
        &resolved.spawn_path
    } else {
        &resolved.host_path
    };
    if cfg!(target_os = "windows") && resolved.env_type == EnvType::Windows {
        path.replace('/', "\\")
    } else {
        path.clone()
    }
}

/// Add the exact project path to Codex's global project trust map. This is
/// separate from `--dangerously-bypass-hook-trust`: that flag bypasses review
/// of a hook definition, while Codex still ignores the whole project layer
/// when the project itself is untrusted.
fn ensure_codex_project_trusted(
    resolved: &ResolvedPath,
    runtime: &LaunchRuntime,
) -> Result<(), String> {
    // Multiple agents can start together from different linked worktrees. A
    // read/merge/write without a process-wide lock could atomically replace a
    // sibling's newly added project entry even though each individual write
    // is safe.
    let _guard = ATTENTION_CONFIG_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (config_path, distro) = codex_trust_config_path(resolved.env_type, runtime)?;
    let project_path = trust_project_path(resolved);
    let existing = read_runtime_files(&[&config_path], distro.as_deref())
        .map_err(|error| {
            format!(
                "failed to read Codex trust config {}: {error}",
                config_path.display()
            )
        })?
        .into_iter()
        .next()
        .expect("one Codex trust file was requested");
    let updated = ensure_project_trust_content(&existing, &project_path, resolved.env_type)?;
    if updated == existing {
        return Ok(());
    }
    write_runtime_files(&[(&config_path, &updated)], distro.as_deref()).map_err(|e| {
        format!(
            "failed to write Codex trust config {}: {e}",
            config_path.display()
        )
    })
}

/// Parse and edit Codex's TOML document with `toml_edit`. This keeps comments,
/// quote style, and unrelated tables intact while treating equivalent TOML
/// key spellings as the same project on Windows.
fn ensure_project_trust_content(
    existing: &str,
    project_path: &str,
    env_type: EnvType,
) -> Result<String, String> {
    let mut document = existing
        .parse::<DocumentMut>()
        .map_err(|error| format!("failed to parse Codex trust config: {error}"))?;
    let projects = document
        .as_table_mut()
        .entry("projects")
        .or_insert(Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or_else(|| "Codex trust config 'projects' value must be a table".to_string())?;
    let existing_key = projects
        .iter()
        .find(|(key, _)| project_keys_match(key, project_path, env_type))
        .map(|(key, _)| key.to_string());
    let project = if let Some(key) = existing_key {
        projects
            .get_mut(&key)
            .ok_or_else(|| "Codex trust project disappeared during merge".to_string())?
    } else {
        projects.insert(project_path, Item::Table(Table::new()));
        projects
            .get_mut(project_path)
            .ok_or_else(|| "Codex trust project was not inserted".to_string())?
    };
    let project = project
        .as_table_like_mut()
        .ok_or_else(|| "Codex trust project entry must be a table".to_string())?;
    if let Some(trust_level) = project.get_mut("trust_level") {
        if let Some(value) = trust_level.as_value_mut() {
            let decor = value.decor().clone();
            let mut replacement = toml_edit::Value::from("trusted");
            *replacement.decor_mut() = decor;
            *value = replacement;
        } else {
            project.insert("trust_level", value("trusted"));
        }
    } else {
        project.insert("trust_level", value("trusted"));
    }
    Ok(document.to_string())
}

fn project_keys_match(existing: &str, candidate: &str, env_type: EnvType) -> bool {
    if matches!(env_type, EnvType::Windows | EnvType::WindowsInterop) {
        let normalize = |path: &str| path.replace('/', "\\");
        let existing = normalize(existing);
        let candidate = normalize(candidate);
        existing
            .trim_end_matches('\\')
            .eq_ignore_ascii_case(candidate.trim_end_matches('\\'))
    } else {
        existing.trim_end_matches('/') == candidate.trim_end_matches('/')
    }
}

pub fn materialize_proxy_profile(
    env_type: EnvType,
    install: &CodexInstall,
    profile_name: &str,
    provider_display_name: &str,
    base_url: &str,
) -> Result<(), String> {
    let content = render_proxy_profile(profile_name, provider_display_name, base_url);
    match env_type {
        EnvType::Wsl => {
            // WSL and native profile writes are serialized through the same
            // process lock; each target is then replaced atomically.
            let _guard = PROFILE_WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let distro = install
                .wsl_distro
                .as_deref()
                .ok_or_else(|| "verified WSL distribution identity is missing".to_string())?;
            materialize_wsl_profile(distro, &install.codex_home, profile_name, &content)
        }
        EnvType::Windows | EnvType::WindowsInterop => materialize_native_profile_at(
            Path::new(&crate::env::to_host_path(&install.codex_home)),
            profile_name,
            &content,
        ),
    }
}

fn parse_version(output: &str) -> Option<(u32, u32, u32, String)> {
    let token = output
        .split_whitespace()
        .find(|part| part.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    let normalized = token.split('-').next()?;
    let mut parts = normalized.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch, normalized.to_string()))
}

fn validate_proxy_cli_help(fresh_help: &str, resume_help: &str) -> Result<(), String> {
    for (invocation, help) in [("fresh", fresh_help), ("resume", resume_help)] {
        let missing = ["--profile", "--model"]
            .into_iter()
            .filter(|flag| !help.contains(flag))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(format!(
                "Codex {invocation} invocation does not support required proxy flags: {}",
                missing.join(", ")
            ));
        }
    }
    Ok(())
}

/// Wall-clock bound for one Codex capability probe (`--version`, `--help`).
///
/// These are short, local, non-interactive reads that finish in well under a
/// second, but a bare `.output()` waits forever for a child that never exits:
/// a wedged `wsl.exe` (paused VM, busy LxssManager) or a `.cmd` shim blocked on
/// a console handle returns no output and no exit code. That pinned a
/// blocking-pool thread for the life of the process, so one stuck probe left
/// the Settings → Providers tab stuck on "loading" with no way to recover
/// (this command runs 4 discovery chains per tab open — see
/// `provider_menu::available_providers`). 20s matches the bound the
/// WindowsInterop branch already used and is far above the real cost.
const CODEX_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Bound for the executable-location and `CODEX_HOME` lookups below. These are
/// plain host/guest reads with no PowerShell in the path, so they get a tighter
/// bound than [`CODEX_PROBE_TIMEOUT`]; the first `wsl.exe` call after a cold
/// VM start can take seconds, and 10s leaves ample headroom.
const CODEX_LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn codex_output(
    env_type: EnvType,
    wsl_distro: Option<&str>,
    args: &[&str],
) -> Result<std::process::Output, String> {
    let (mut command, op_name) = if env_type == EnvType::WindowsInterop {
        let args = args
            .iter()
            .map(|arg| crate::env::powershell_literal(arg))
            .collect::<Vec<_>>()
            .join(" ");
        let command =
            crate::env::powershell_command(&format!("& codex {args}; exit $LASTEXITCODE"));
        return crate::process_util::run_command_with_timeout(
            command,
            "Windows Codex probe",
            CODEX_PROBE_TIMEOUT,
        );
    } else if env_type == EnvType::Wsl {
        let mut command = crate::process_util::command_no_window("wsl.exe");
        command.args([
            "-d",
            wsl_distro.ok_or_else(|| "WSL distribution identity is unavailable".to_string())?,
            "--exec",
            "codex",
        ]);
        (command, "WSL Codex probe")
    } else if cfg!(target_os = "windows") {
        // npm installs Codex as a `.cmd` shim. `std::process::Command` cannot
        // execute batch files directly on Windows, so capability probes use
        // the same non-interactive cmd relay as other shim-backed providers.
        let mut command = crate::process_util::command_no_window("cmd.exe");
        command.args(["/d", "/c", "codex"]);
        (command, "Windows Codex probe")
    } else {
        (
            crate::process_util::command_no_window("codex"),
            "Codex probe",
        )
    };
    command.args(args);
    // `run_command_with_timeout` names the operation and the failure in its
    // own error strings ("WSL Codex probe timed out after 20s"), which is more
    // actionable than the old blanket "Codex executable is unavailable" —
    // the timeout case is not an availability problem.
    crate::process_util::run_command_with_timeout(command, op_name, CODEX_PROBE_TIMEOUT)
}

fn successful_help(
    env_type: EnvType,
    wsl_distro: Option<&str>,
    args: &[&str],
    label: &str,
) -> Result<String, String> {
    let output = codex_output(env_type, wsl_distro, args)?;
    if !output.status.success() {
        return Err(format!("Codex {label} capability check failed"));
    }
    Ok(format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

pub fn discover_supported_install(env_type: EnvType) -> Result<CodexInstall, String> {
    CODEX_INSTALL_CACHE.discover(env_type, || discover_supported_install_uncached(env_type))
}

/// Re-resolve Codex for an explicit pairing verification. Verifying still
/// checks the live endpoint, and it must also observe a CLI update immediately
/// instead of trusting the short-lived Settings cache.
pub fn discover_supported_install_fresh(env_type: EnvType) -> Result<CodexInstall, String> {
    CODEX_INSTALL_CACHE.discover_fresh(env_type, || discover_supported_install_uncached(env_type))
}

/// Whether [`discover_supported_install`] for this runtime currently serves
/// from the short-lived install cache instead of spawning probes (issue
/// #1948). Secret-free by construction (a bool per runtime) — the
/// cold-vs-warm bit in the provider-menu timing log. Read-only: never
/// inserts, never probes.
///
/// This mirrors the TTL reuse of [`discover_supported_install`] only:
/// [`discover_supported_install_fresh`] keys off the in-flight `refreshing`
/// flag and re-probes every completed entry, so reading this bit next to a
/// `_fresh` call would misreport cold discovery as warm.
pub fn codex_install_cached(env_type: EnvType) -> bool {
    CODEX_INSTALL_CACHE.is_fresh(env_type)
}

/// Distinct wrappers for probe results.
///
/// Left as bare `String`s, all three identity probes (and both help probes)
/// share one signature, so passing them to a joiner out of order type-checks
/// while silently changing which error a broken install surfaces. With these
/// wrappers any permutation is a type error, so the precedence the tests pin
/// cannot regress at a call site (issue #1934).
#[derive(Debug)]
struct CodexVersion(String);
#[derive(Debug)]
struct CodexExecutable(String);
#[derive(Debug)]
struct CodexHome(String);
#[derive(Debug)]
struct FreshHelp(String);
#[derive(Debug)]
struct ResumeHelp(String);

/// Join a probe thread, re-raising a panic with its original payload.
///
/// `join().unwrap()` replaces the payload with `called 'Result::unwrap()' on an
/// 'Err' value: Any { .. }`, which would reach `run_blocking` without the
/// message naming the probe that failed. `resume_unwind` propagates the
/// original payload, which is what the serial chain did.
pub(crate) fn join_probe<T>(probe: std::thread::ScopedJoinHandle<'_, T>) -> T {
    match probe.join() {
        Ok(value) => value,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

/// Issue the three independent Codex identity probes for one runtime
/// concurrently and collect them in discovery order.
///
/// They read the same CLI but do not depend on each other: the executable
/// location and `CODEX_HOME` each have their own process spawn, and only the
/// capability key further down needs the version. Chaining them made one WSL
/// discovery pay three `wsl.exe` starts in sequence, so the wall clock was
/// their sum rather than their max.
///
/// **Error precedence stays the serial order.** A missing `codex` fails all
/// three probes at once, and the user must still see `Codex version check
/// failed` — the root cause — rather than the location/home failures that only
/// follow from it. Every result is joined before any is surfaced, then they are
/// reported earliest-step-first. The distinct result types above make that order
/// a compile-time property of this signature, not a convention.
fn probe_codex_identity_concurrently(
    env_type: EnvType,
    wsl_distro: Option<&str>,
    version: impl FnOnce(EnvType, Option<&str>) -> Result<CodexVersion, String> + Send,
    executable: impl FnOnce(EnvType, Option<&str>) -> Result<CodexExecutable, String> + Send,
    home: impl FnOnce(EnvType, Option<&str>) -> Result<CodexHome, String> + Send,
) -> Result<(CodexVersion, CodexExecutable, CodexHome), String> {
    std::thread::scope(|scope| {
        let version = scope.spawn(|| version(env_type, wsl_distro));
        let executable = scope.spawn(|| executable(env_type, wsl_distro));
        let home = scope.spawn(|| home(env_type, wsl_distro));
        let version = join_probe(version);
        let executable = join_probe(executable);
        let home = join_probe(home);
        Ok((version?, executable?, home?))
    })
}

/// `codex --version`, parsed and range-checked against the proxied-CLI floor.
fn probe_codex_version(
    env_type: EnvType,
    wsl_distro: Option<&str>,
) -> Result<CodexVersion, String> {
    let output = codex_output(env_type, wsl_distro, &["--version"])?;
    if !output.status.success() {
        return Err("Codex version check failed".into());
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let (major, minor, patch, version) = parse_version(&raw)
        .ok_or_else(|| format!("could not parse Codex version from {:?}", raw.trim()))?;
    if (major, minor, patch) < MIN_PROXY_CODEX_VERSION {
        return Err(format!(
            "proxied Codex requires codex-cli >= 0.144.0; found {version}"
        ));
    }
    Ok(CodexVersion(version))
}

/// Step 2: resolve the absolute path of this runtime's Codex executable.
fn probe_codex_executable(
    env_type: EnvType,
    wsl_distro: Option<&str>,
) -> Result<CodexExecutable, String> {
    let executable = if env_type == EnvType::WindowsInterop {
        let command = crate::env::powershell_command("[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); (Get-Command codex -CommandType Application -ErrorAction Stop).Source");
        let output = crate::process_util::run_command_with_timeout(
            command,
            "Windows Codex location",
            CODEX_LOOKUP_TIMEOUT,
        )?;
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else if env_type == EnvType::Wsl {
        let mut locate = crate::process_util::command_no_window("wsl.exe");
        locate.args([
            "-d",
            wsl_distro.expect("WSL distribution was resolved"),
            "--exec",
            "sh",
            "-lc",
            "printf '__BUILDMESH_WSL_CODEX_EXECUTABLE__%s\\n' \"$(command -v codex)\"",
        ]);
        let out = crate::process_util::run_command_with_timeout(
            locate,
            "WSL Codex executable location",
            CODEX_LOOKUP_TIMEOUT,
        )?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .rev()
            .find_map(|line| line.strip_prefix("__BUILDMESH_WSL_CODEX_EXECUTABLE__"))
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .unwrap_or_default()
            .to_string()
    } else {
        let locator = if cfg!(target_os = "windows") {
            "where.exe"
        } else {
            "which"
        };
        let mut locate = crate::process_util::command_no_window(locator);
        locate.arg("codex");
        let out = crate::process_util::run_command_with_timeout(
            locate,
            "Codex executable location",
            CODEX_LOOKUP_TIMEOUT,
        )?;
        let candidates = String::from_utf8_lossy(&out.stdout);
        if cfg!(target_os = "windows") {
            candidates
                .lines()
                .map(str::trim)
                .find(|path| {
                    [".exe", ".cmd", ".bat", ".com"]
                        .iter()
                        .any(|extension| path.to_ascii_lowercase().ends_with(extension))
                })
                .unwrap_or_default()
                .to_string()
        } else {
            candidates
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string()
        }
    };
    if executable.is_empty() {
        return Err("Codex executable identity is unavailable".into());
    }
    Ok(CodexExecutable(executable))
}

/// Step 3: resolve this runtime's `CODEX_HOME`.
fn probe_codex_home(env_type: EnvType, wsl_distro: Option<&str>) -> Result<CodexHome, String> {
    if env_type == EnvType::WindowsInterop {
        let home = crate::env::codex_dir_for_env(env_type, "")
            .ok_or_else(|| "Windows Codex home unavailable".to_string())?;
        return Ok(CodexHome(crate::env::windows_path_from_wsl(
            &home.to_string_lossy(),
        )));
    }
    if let Some(distro) = wsl_distro {
        let mut command = crate::process_util::command_no_window("wsl.exe");
        command.args(["-d", distro, "--exec", "sh", "-lc", WSL_CODEX_HOME_SCRIPT]);
        let output = crate::process_util::run_command_with_timeout(
            command,
            "WSL Codex home resolution",
            CODEX_LOOKUP_TIMEOUT,
        )?;
        let Some(home) = crate::env::parse_wsl_codex_home_output(&output.stdout) else {
            return Err("WSL Codex home identity is unavailable".into());
        };
        if !output.status.success() {
            return Err("WSL Codex home identity is unavailable".into());
        }
        return Ok(CodexHome(home.to_string_lossy().into_owned()));
    }
    Ok(CodexHome(
        native_codex_home()?.to_string_lossy().into_owned(),
    ))
}

/// Run the two proxy-capability `--help` probes concurrently.
///
/// `fresh` and `resume` are independent reads of the same CLI, and each one is
/// a separate process spawn, so the pair was paying double for one answer. They
/// run after the identity trio because the capability key needs the parsed
/// version, so they overlap each other but not the trio. Error precedence stays
/// in probe order, which is also the order [`validate_proxy_cli_help`] names a
/// missing flag in.
fn probe_proxy_cli_help_concurrently(
    env_type: EnvType,
    wsl_distro: Option<&str>,
    fresh: impl FnOnce(EnvType, Option<&str>) -> Result<FreshHelp, String> + Send,
    resume: impl FnOnce(EnvType, Option<&str>) -> Result<ResumeHelp, String> + Send,
) -> Result<(FreshHelp, ResumeHelp), String> {
    std::thread::scope(|scope| {
        let fresh = scope.spawn(|| fresh(env_type, wsl_distro));
        let resume = scope.spawn(|| resume(env_type, wsl_distro));
        let fresh = join_probe(fresh);
        let resume = join_probe(resume);
        Ok((fresh?, resume?))
    })
}

fn discover_supported_install_uncached(env_type: EnvType) -> Result<CodexInstall, String> {
    let wsl_distro = if env_type == EnvType::Wsl {
        Some(
            crate::env::detect_default_wsl_distro()
                .ok_or_else(|| "default WSL distribution is unavailable".to_string())?,
        )
    } else {
        None
    };
    let (CodexVersion(version), CodexExecutable(executable), CodexHome(codex_home)) =
        probe_codex_identity_concurrently(
            env_type,
            wsl_distro.as_deref(),
            probe_codex_version,
            probe_codex_executable,
            probe_codex_home,
        )?;
    let runtime = if let Some(distro) = wsl_distro.as_deref() {
        wsl_runtime_identity(distro, &codex_home)
    } else {
        runtime_identity(env_type).to_string()
    };
    let capability_key = format!("{}\0{}\0{}", runtime, executable, version);
    let capabilities_are_cached = CLI_CAPABILITY_CACHE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(&capability_key);
    if !capabilities_are_cached {
        let (FreshHelp(fresh_help), ResumeHelp(resume_help)) = probe_proxy_cli_help_concurrently(
            env_type,
            wsl_distro.as_deref(),
            |env_type, wsl_distro| {
                successful_help(env_type, wsl_distro, &["--help"], "fresh").map(FreshHelp)
            },
            |env_type, wsl_distro| {
                successful_help(env_type, wsl_distro, &["resume", "--help"], "resume")
                    .map(ResumeHelp)
            },
        )?;
        validate_proxy_cli_help(&fresh_help, &resume_help)?;
        // The cache-miss path stays idempotent: two runtimes discovering the
        // same `runtime\0executable\0version` concurrently may both probe and
        // both insert, which is harmless. No single-flight assumption is built
        // on top of it.
        CLI_CAPABILITY_CACHE
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(capability_key);
    }
    Ok(CodexInstall {
        executable,
        version,
        runtime_identity: runtime,
        codex_home,
        wsl_distro,
    })
}

/// Ensure `<project>/.codex/config.toml` enables the hooks feature
/// (`[features] hooks = true` — `codex_hooks` is the legacy alias, issue
/// #884). `toml_edit` preserves comments and formatting while the parsed
/// document prevents duplicate keys or bracket-like comments from confusing
/// the merge.
fn ensure_hooks_feature_content(existing: &str) -> Result<String, String> {
    let mut document = existing
        .parse::<DocumentMut>()
        .map_err(|error| format!("failed to parse Codex project config: {error}"))?;
    let features = document
        .as_table_mut()
        .entry("features")
        .or_insert(Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or_else(|| "Codex project config 'features' value must be a table".to_string())?;
    if features
        .get("hooks")
        .and_then(Item::as_value)
        .and_then(|item| item.as_bool())
        == Some(true)
        || features
            .get("codex_hooks")
            .and_then(Item::as_value)
            .and_then(|item| item.as_bool())
            == Some(true)
    {
        // Either spelling already enables the feature. Keep the user's
        // spelling, formatting, and comments untouched.
        return Ok(document.to_string());
    }

    let key = if features.get("hooks").is_some() {
        "hooks"
    } else if features.get("codex_hooks").is_some() {
        "codex_hooks"
    } else {
        features.insert("hooks", value(true));
        return Ok(document.to_string());
    };

    if let Some(existing_value) = features.get_mut(key).and_then(Item::as_value_mut) {
        let decor = existing_value.decor().clone();
        let mut replacement = toml_edit::Value::from(true);
        *replacement.decor_mut() = decor;
        *existing_value = replacement;
    } else {
        features.insert("hooks", value(true));
    }
    Ok(document.to_string())
}

/// Ensure `<project>/.codex/hooks.json` carries the seven attention webhooks.
/// Codex's matcher/event schema nests hook entries one
/// level deeper than Claude Code's (each event maps to matcher groups, each
/// carrying a `hooks` array — issue #884). `PreToolUse` is matched to the
/// native question tool; `PostToolUse` is catch-all so approved permissions
/// can correlate by tool name. The helper is idempotent and preserves any
/// unrelated top-level keys the user added.
/// Return updated hooks JSON, or `None` when the existing document already
/// contains the current Buildmesh handlers. The caller owns the runtime-aware
/// read/write so WSL can batch guest file operations.
fn ensure_hooks_json_content(
    existing: &str,
    hook: &serde_json::Value,
) -> Result<Option<String>, String> {
    let mut settings: serde_json::Value = if existing.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(existing).map_err(|e| format!("failed to parse hooks.json: {e}"))?
    };
    let Some(settings_object) = settings.as_object_mut() else {
        return Err("hooks.json top level must be an object".to_string());
    };
    let hooks = settings_object
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let Some(events) = hooks.as_object_mut() else {
        return Err("hooks.json 'hooks' value must be an object".to_string());
    };

    let mut changed = false;
    for event in [
        "SessionStart",
        "Stop",
        "PermissionRequest",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "Interrupt",
    ] {
        let groups = events.entry(event).or_insert_with(|| serde_json::json!([]));
        let Some(groups) = groups.as_array_mut() else {
            return Err(format!("hooks.json event '{event}' must be an array"));
        };

        let mut found = false;
        for group in groups.iter_mut() {
            let old_post_matcher = event == "PostToolUse"
                && group.get("matcher").and_then(|value| value.as_str())
                    == Some("^request_user_input$");
            let Some(handlers) = group.get_mut("hooks").and_then(|v| v.as_array_mut()) else {
                continue;
            };
            if let Some(index) = handlers.iter().position(is_buildmesh_hook_handler) {
                if handlers[index] != *hook {
                    handlers[index] = hook.clone();
                    changed = true;
                }
                if old_post_matcher && handlers.iter().all(is_buildmesh_hook_handler) {
                    if let Some(group) = group.as_object_mut() {
                        group.remove("matcher");
                        changed = true;
                    }
                }
                found = true;
                break;
            }
        }
        if !found {
            let mut group = serde_json::json!({ "hooks": [hook.clone()] });
            if event == "PreToolUse" {
                group["matcher"] = serde_json::json!("^request_user_input$");
            }
            groups.push(group);
            changed = true;
        }
    }
    if !changed {
        return Ok(None);
    }
    let content = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("serialize hooks.json failed: {e}"))?;
    Ok(Some(content))
}

fn is_buildmesh_hook_handler(handler: &serde_json::Value) -> bool {
    handler.get("statusMessage").and_then(|v| v.as_str()) == Some(BUILDMESH_HOOK_STATUS_MESSAGE)
        || handler
            .get("command")
            .and_then(|v| v.as_str())
            .is_some_and(|command| {
                command.contains("BUILDMESH_PORT")
                    && command.contains("BUILDMESH_SESSION_ID")
                    && command.contains("/api/attention/")
            })
}

fn write_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(content.as_bytes())?;
    temp.as_file().sync_all()?;
    temp.persist(path).map(|_| ()).map_err(|error| error.error)
}

fn ensure_codex_project_files(
    resolved: &ResolvedPath,
    runtime: &LaunchRuntime,
    node_id: i64,
) -> Result<(), String> {
    let _guard = ATTENTION_CONFIG_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let distro = runtime_wsl_distro(resolved.env_type, runtime)?;
    let project_dir = if distro.is_some() {
        PathBuf::from(&resolved.spawn_path).join(".codex")
    } else {
        PathBuf::from(&resolved.host_path).join(".codex")
    };
    if distro.is_none() {
        std::fs::create_dir_all(&project_dir)
            .map_err(|e| format!("failed to create .codex dir: {e}"))?;
    }

    // Last-writer-wins: hooks.json bakes this node_id into the callback URL
    // because Codex's hook runner env_clear()s BUILDMESH_*. Two Codex nodes
    // sharing one worktree directory will redirect Node A's subsequent
    // attention webhooks to whoever spawned last. Buildmesh worktrees are
    // 1:1 with nodes today; do not relax that without a per-node hook path.
    //
    // Read and validate both files before writing either one. A malformed
    // hooks.json must not leave a half-applied project configuration behind.
    let config_path = project_dir.join("config.toml");
    let hooks_path = project_dir.join("hooks.json");
    let existing = read_runtime_files(&[&config_path, &hooks_path], distro.as_deref())?;
    let config_existing = &existing[0];
    let config_updated = ensure_hooks_feature_content(config_existing)?;
    let hooks_existing = &existing[1];
    let hooks_updated = ensure_hooks_json_content(
        hooks_existing,
        &attention_hook_handler(node_id, resolved.env_type),
    )?;

    let mut writes: Vec<(&Path, &str)> = Vec::new();
    if config_updated != *config_existing {
        writes.push((&config_path, &config_updated));
    }
    if let Some(hooks_updated) = hooks_updated.as_ref() {
        writes.push((&hooks_path, hooks_updated));
    }
    write_runtime_files(&writes, distro.as_deref())
}

impl AgentProvider for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn ui(&self) -> UiMeta {
        UiMeta {
            label: "OpenAI Codex".into(),
            color: "#10a37f".into(),
            icon: "X".into(),
        }
    }

    fn spawn_recipe(&self, platform: Platform, _env_type: EnvType) -> SpawnRecipe {
        SpawnRecipe {
            binary: "codex",
            base_args: base_flags(),
            trailing_args: Vec::new(),
            windows_shell: shell_for(platform),
        }
    }

    /// Codex draws mid-size pastes inline and can collapse just the suffix
    /// of a larger Windows paste burst. Its marker counts that suffix, not
    /// the complete prompt, so readiness must also match the inline prefix.
    fn paste_gate_policy(&self) -> crate::agent::provider::PasteGatePolicy {
        crate::agent::provider::PasteGatePolicy::RenderedWithSplitMarker
    }

    fn background_recipe(
        &self,
        platform: Platform,
    ) -> Option<crate::agent::background::BackgroundRecipe> {
        use crate::agent::{
            background::BackgroundRecipe,
            capabilities::{BackgroundPromptInput, BackgroundResultOutput},
        };
        let mut spawn = self.spawn_recipe(platform, EnvType::Windows);
        spawn.base_args = [
            "--ask-for-approval",
            "never",
            "exec",
            "--ignore-user-config",
            "--ephemeral",
            "--skip-git-repo-check",
            "--sandbox",
            "read-only",
            "--color",
            "never",
            "-c",
            "features.shell_tool=false",
            "-c",
            "features.multi_agent=false",
        ]
        .map(str::to_owned)
        .to_vec();
        spawn.trailing_args = vec!["-".into()];
        let mut recipe = BackgroundRecipe::new(
            spawn,
            BackgroundPromptInput::Stdin,
            BackgroundResultOutput::LastMessageFile,
        );
        recipe.env_remove = vec!["OPENAI_API_KEY".into(), "OPENAI_BASE_URL".into()];
        Some(recipe)
    }

    fn spawn_recipe_for_resume(&self, platform: Platform, session_id: &str) -> Option<SpawnRecipe> {
        // `codex resume [OPTIONS] [SESSION_ID] [PROMPT]`. Options after the
        // UUID are the prompt, so a restart would "resume" into a garbage turn.
        // Keep the id in trailing_args; default_prepare and Codex proxy
        // `--profile`/`--model` append options to base_args only.
        let mut args = vec!["resume".into()];
        args.extend(base_flags());
        Some(SpawnRecipe {
            binary: "codex",
            base_args: args,
            trailing_args: vec![session_id.into()],
            windows_shell: shell_for(platform),
        })
    }

    fn supports_resume(&self) -> bool {
        true
    }

    fn auto_resume_on_startup(&self) -> bool {
        true
    }

    fn requires_attention_hook(&self) -> bool {
        true
    }

    fn attention_capability(&self) -> crate::agent::capabilities::AttentionCapability {
        use crate::agent::capabilities::{AttentionCapability, AttentionLaunchMode};
        use crate::agent::session_lifecycle::LifecycleKind;
        AttentionCapability::Hook {
            events: vec![
                LifecycleKind::TurnCompleted,
                LifecycleKind::InputRequired,
                LifecycleKind::QuestionRequested,
                LifecycleKind::PermissionRequested,
                LifecycleKind::BackgroundRunning,
            ],
            launch_mode: AttentionLaunchMode::PermissionAsk,
            trust: Some("codex project trust (#1379)".into()),
            min_version: Some(CODEX_MIN_HOOK_VERSION.into()),
        }
    }

    /// Codex's global project trust is a launch prerequisite, not an attention
    /// hook side effect. Proxy preflight supplies the exact runtime home/distro
    /// so this edits the same trust store the child will read.
    fn ensure_workspace_trusted(
        &self,
        resolved: &ResolvedPath,
        runtime: &LaunchRuntime,
    ) -> Result<(), String> {
        ensure_codex_project_trusted(resolved, runtime)
    }

    fn provision_attention_hooks(
        &self,
        resolved: &ResolvedPath,
        runtime: &LaunchRuntime,
        node_id: i64,
    ) -> Result<(), String> {
        ensure_codex_project_files(resolved, runtime, node_id)
    }

    fn wsl_passthrough_env(&self) -> &'static [&'static str] {
        // The guest resolves its own CODEX_HOME. Forwarding the host process
        // value through WSLENV would make Windows state override guest state.
        &[]
    }

    /// Codex writes rollout transcripts under `~/.codex/sessions/` that
    /// `services::transcript_reader` parses via `TranscriptFormat::Codex`
    /// (issue #887).
    fn produces_readable_transcript(&self) -> bool {
        true
    }

    fn supports_model_override(&self) -> bool {
        true
    }

    fn supports_extra_args(&self) -> bool {
        true
    }

    fn supports_prefill(&self) -> bool {
        true
    }

    fn prefill_requires_pty(&self, text: &str) -> bool {
        // Codex accepts a short positional prompt, but multiline review text
        // has been observed to reach its clap parser as separate arguments
        // (notably diff lines beginning with `+`). A single-line prompt that
        // begins with a CLI flag is also unsafe as a positional argument. The
        // PTY path preserves the prompt as one pasted turn and is the safe
        // automated-launch mode.
        let trimmed = text.trim_start();
        text.contains('\n')
            || text.contains('\r')
            || trimmed.starts_with('-')
            || trimmed.starts_with('+')
    }

    fn ready_for_initial_prompt(&self, tail: &str) -> bool {
        // Codex paints its composer (the `›` chevron plus a one-line
        // placeholder) in the TUI's first frame, and that painted input box is
        // the only startup evidence that stdin is being read — the state a
        // pasted prompt needs. Its earlier readiness banner
        // (`model: <name> /model to change`) is gone: Codex 0.158 renders the
        // model only as a bare `loading` line on boot and moves the resolved
        // model into the bottom status footer, so keying on that banner (or on
        // "not loading") never matched and the initial prompt was dropped after
        // the 300s wait. Launch still passes `--dangerously-bypass-hook-trust`,
        // so no review dialog can hold the composer back.
        const COMPOSER_PLACEHOLDERS: [&str; 2] =
            ["Ask Codex to do anything", "Ask a follow-up question"];
        COMPOSER_PLACEHOLDERS
            .iter()
            .any(|placeholder| tail.contains(placeholder))
    }

    fn available_on(&self) -> &'static [Platform] {
        &[Platform::Macos, Platform::Windows, Platform::Linux]
    }

    fn self_assigns_session_id(&self) -> bool {
        true
    }

    /// Recent Codex TUIs no longer reliably print the session UUID on the
    /// PTY. Its rollout's `session_meta` record is the durable fallback, so
    /// capture it shortly after every fresh spawn rather than leaving a node
    /// impossible to resume after a Buildmesh restart.
    fn after_fresh_spawn(
        &self,
        node_id: i64,
        spawn_path: &str,
        env_type: EnvType,
        _app: &tauri::AppHandle,
    ) {
        crate::services::codex_session::start_capture_poller(
            node_id,
            spawn_path.to_string(),
            env_type,
        );
    }

    fn recover_suspended_session_id(
        &self,
        spawn_path: &str,
        env_type: EnvType,
        anchor_ms: i64,
        recorded_start: bool,
    ) -> Option<String> {
        crate::services::codex_session::find_historic_id_for_directory(
            env_type,
            spawn_path,
            anchor_ms,
            recorded_start,
        )
    }

    fn session_assign_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    fn resume_args(&self, _id: &str) -> Vec<String> {
        vec![]
    }

    fn effort_args(&self, effort: &str) -> Vec<String> {
        // Codex has no dedicated --effort flag, but exposes the same setting
        // as a stable per-invocation config override. Rust's debug string
        // representation supplies the quoted/escaped TOML string value.
        vec!["-c".into(), format!("model_reasoning_effort={effort:?}")]
    }

    fn prefill_args(&self, text: &str) -> Vec<String> {
        vec![text.into()]
    }

    /// Codex's reasoning-effort knob is the inline per-invocation config
    /// override `-c model_reasoning_effort="…"` (issue #1143 research),
    /// distinct from Claude Code's closed-vocab flag. The vocabulary
    /// list lives in `agent::capabilities::CODEX_EFFORT_ALLOWED` and is
    /// consumed by both this method and the resolver; the key is
    /// surfaced to the frontend for knob labelling.
    fn effort_control(&self) -> EffortControlKind {
        EffortControlKind::InlineConfig {
            key: CODEX_EFFORT_KEY.to_string(),
            allowed: CODEX_EFFORT_ALLOWED.iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Regression guard: no read probe in this module may spawn a child
    /// with a bare `.output()`.
    ///
    /// Settings can request the Codex install for Windows and the foreign
    /// runtime through both `available_providers` and pairing statuses. The
    /// cache collapses repeated requests, but every actual probe still needs
    /// a timeout: an unbounded child blocks forever if it never exits — a
    /// wedged `wsl.exe` on a paused VM, a `.cmd`
    /// shim stuck on a console handle. That pinned a blocking-pool thread
    /// for the life of the process with no way for the user to recover, so
    /// the tab sat on "loading" indefinitely.
    ///
    /// This is a source-shape assertion rather than a live hung-probe test
    /// because the failure needs a genuinely un-exitable child in a
    /// specific runtime, which can't be fabricated portably. `run_command_with_timeout`
    /// itself is covered in `process_util` (kill-on-deadline, early-exit,
    /// spawn-failure); what's pinned here is that this module keeps routing
    /// through it. Mirrors `process_util::git_command_disables_interactive_prompts`,
    /// which guards the same class of regression for the git shell-out.
    #[test]
    fn no_codex_read_probe_spawns_without_a_timeout() {
        let source = include_str!("codex.rs");
        // Everything before the test module is production code; the test
        // module legitimately spawns real children with plain `.output()`.
        let production = source
            .split_once("#[cfg(test)]\nmod tests {")
            .expect("codex.rs must keep its #[cfg(test)] mod tests block")
            .0;
        // Strip comment lines first: this file's own doc comments name
        // `.output()` when explaining the bug, and prose about the hazard
        // must not read as an instance of it.
        let code: String = production
            .lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                !trimmed.starts_with("//")
                    && !trimmed.starts_with('*')
                    && !trimmed.starts_with("/*")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains(".output()"),
            "every Codex probe must use process_util::run_command_with_timeout — \
             a bare `.output()` can block forever and strands the Settings → \
             Providers tab on a spinner with no recovery"
        );
    }

    fn test_install(version: &str) -> CodexInstall {
        CodexInstall {
            executable: "codex-test".into(),
            version: version.into(),
            runtime_identity: "test-runtime".into(),
            codex_home: "/tmp/codex-test".into(),
            wsl_distro: None,
        }
    }

    fn wait_for_probe_release(release: &Arc<(Mutex<bool>, std::sync::Condvar)>) {
        let (lock, condition) = &**release;
        let released = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _released = condition
            .wait_while(released, |released| !*released)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }

    fn release_probes(release: &Arc<(Mutex<bool>, std::sync::Condvar)>) {
        let (lock, condition) = &**release;
        *lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        condition.notify_all();
    }

    #[test]
    fn install_resolution_is_single_flight_and_refreshes_after_ttl() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::mpsc;
        use std::thread;

        let cache = Arc::new(CodexInstallCache::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let (probe_started_tx, probe_started_rx) = mpsc::channel();
        let first_cache = Arc::clone(&cache);
        let first_calls = Arc::clone(&calls);
        let first_release = Arc::clone(&release);
        let first = thread::spawn(move || {
            first_cache.discover(EnvType::Wsl, || {
                first_calls.fetch_add(1, Ordering::SeqCst);
                probe_started_tx.send(()).unwrap();
                wait_for_probe_release(&first_release);
                Ok(test_install("0.144.0"))
            })
        });
        probe_started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first caller starts the runtime probe");

        let (second_calling_tx, second_calling_rx) = mpsc::sync_channel(0);
        let second_cache = Arc::clone(&cache);
        let second_calls = Arc::clone(&calls);
        let second = thread::spawn(move || {
            second_calling_tx.send(()).unwrap();
            second_cache.discover(EnvType::Wsl, || {
                second_calls.fetch_add(1, Ordering::SeqCst);
                Ok(test_install("0.145.0"))
            })
        });
        second_calling_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("second caller reaches the same runtime cache");
        release_probes(&release);

        assert_eq!(first.join().unwrap().unwrap().version, "0.144.0");
        assert_eq!(second.join().unwrap().unwrap().version, "0.144.0");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let resolved_at = cache
            .entries
            .lock()
            .unwrap()
            .get(runtime_identity(EnvType::Wsl))
            .unwrap()
            .cell
            .get()
            .unwrap()
            .resolved_at;
        let just_before_expiry = resolved_at + CODEX_INSTALL_CACHE_TTL - Duration::from_nanos(1);
        let still_cached = cache
            .discover_at(EnvType::Wsl, just_before_expiry, || {
                panic!("a fresh install result must be reused until its TTL expires")
            })
            .unwrap();
        assert_eq!(still_cached.version, "0.144.0");

        let refreshed = cache
            .discover_at(EnvType::Wsl, resolved_at + CODEX_INSTALL_CACHE_TTL, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(test_install("0.145.0"))
            })
            .unwrap();
        assert_eq!(refreshed.version, "0.145.0");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_caller_whose_clock_read_precedes_the_resolution_reuses_it() {
        // `discover` reads the clock before it takes the cache lock, so a
        // concurrent caller's resolution can land with a `resolved_at` later
        // than this caller's `now`. That entry is brand new, not stale;
        // treating it as stale replaced the slot and ran a duplicate probe.
        let cache = CodexInstallCache::default();
        cache
            .discover(EnvType::Wsl, || Ok(test_install("0.144.0")))
            .unwrap();
        let resolved_at = cache
            .entries
            .lock()
            .unwrap()
            .get(runtime_identity(EnvType::Wsl))
            .unwrap()
            .cell
            .get()
            .unwrap()
            .resolved_at;

        // Milliseconds, not nanoseconds: under load the gap between the clock
        // read and the lock is that wide, and Windows' `Instant` rounds a
        // sub-tick difference to zero, which would hide the bug.
        let earlier_clock_read = resolved_at - Duration::from_millis(1);
        let reused = cache
            .discover_at(EnvType::Wsl, earlier_clock_read, || {
                panic!("a resolution newer than the caller's clock read must be reused")
            })
            .unwrap();
        assert_eq!(reused.version, "0.144.0");
    }

    #[test]
    fn separate_runtime_probes_can_run_at_the_same_time() {
        use std::sync::mpsc;
        use std::thread;

        let cache = Arc::new(CodexInstallCache::default());
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let (probe_started_tx, probe_started_rx) = mpsc::channel();
        let mut probes = Vec::new();
        for (env_type, version) in [
            (EnvType::Windows, "0.144.0"),
            (EnvType::Wsl, "0.145.0"),
            (EnvType::WindowsInterop, "0.146.0"),
        ] {
            let cache = Arc::clone(&cache);
            let release = Arc::clone(&release);
            let probe_started_tx = probe_started_tx.clone();
            probes.push(thread::spawn(move || {
                cache.discover(env_type, || {
                    probe_started_tx.send(env_type).unwrap();
                    wait_for_probe_release(&release);
                    Ok(test_install(version))
                })
            }));
        }
        drop(probe_started_tx);
        let started = (0..3)
            .filter_map(|_| probe_started_rx.recv_timeout(Duration::from_secs(2)).ok())
            .collect::<Vec<_>>();
        release_probes(&release);
        for probe in probes {
            probe.join().unwrap().unwrap();
        }
        assert_eq!(
            started.len(),
            3,
            "each runtime must start before either probe completes"
        );
    }

    #[test]
    fn fresh_install_resolution_replaces_the_cached_version() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let cache = CodexInstallCache::default();
        let calls = AtomicUsize::new(0);
        let initial = cache
            .discover(EnvType::Windows, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(test_install("0.144.0"))
            })
            .unwrap();
        assert_eq!(initial.version, "0.144.0");

        let verified = cache
            .discover_fresh(EnvType::Windows, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(test_install("0.145.0"))
            })
            .unwrap();
        assert_eq!(verified.version, "0.145.0");

        let next_read = cache
            .discover(EnvType::Windows, || {
                panic!("fresh verification should have replaced the cached install")
            })
            .unwrap();
        assert_eq!(next_read.version, "0.145.0");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failed_install_resolution_is_retried_on_the_next_read() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let cache = CodexInstallCache::default();
        let calls = AtomicUsize::new(0);
        let failed = cache.discover(EnvType::Windows, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Err("Codex is unavailable".into())
        });
        assert_eq!(failed.unwrap_err(), "Codex is unavailable");

        let recovered = cache
            .discover(EnvType::Windows, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(test_install("0.145.0"))
            })
            .unwrap();
        assert_eq!(recovered.version, "0.145.0");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// Issue #1948: the cold-vs-warm bit in the provider-menu timing log
    /// must agree with what `discover` would do — report cached only when a
    /// fresh entry exists, without probing. A panicking probe proves the
    /// warm path performs no process work.
    #[test]
    fn install_cache_freshness_reports_cold_and_warm_without_probing() {
        let cache = CodexInstallCache::default();
        assert!(!cache.is_fresh(EnvType::Windows), "an empty cache is cold");

        cache
            .discover(EnvType::Windows, || Ok(test_install("0.144.0")))
            .unwrap();
        assert!(
            cache.is_fresh(EnvType::Windows),
            "a resolved install is warm"
        );
        assert!(!cache.is_fresh(EnvType::Wsl), "freshness is per-runtime");

        // The warm read must reuse the entry: a probe here would panic.
        let warm = cache
            .discover(EnvType::Windows, || {
                panic!("a fresh install result must be reused without probing")
            })
            .unwrap();
        assert_eq!(warm.version, "0.144.0");

        let resolved_at = cache
            .entries
            .lock()
            .unwrap()
            .get(runtime_identity(EnvType::Windows))
            .unwrap()
            .cell
            .get()
            .unwrap()
            .resolved_at;
        assert!(cache.is_fresh_at(
            EnvType::Windows,
            resolved_at + CODEX_INSTALL_CACHE_TTL - Duration::from_nanos(1)
        ));
        assert!(
            !cache.is_fresh_at(EnvType::Windows, resolved_at + CODEX_INSTALL_CACHE_TTL),
            "an expired entry is cold again"
        );

        // Failures are discarded, never served warm: the next read probes.
        let failures = CodexInstallCache::default();
        let _ = failures.discover(EnvType::Windows, || {
            Err::<CodexInstall, String>("Codex is unavailable".into())
        });
        assert!(
            !failures.is_fresh(EnvType::Windows),
            "a failed probe leaves no warm entry"
        );
    }

    /// The probe bounds must stay generous enough for a cold WSL distro
    /// start (seconds) but finite. A zero/absurd value would make the
    /// timeout above a self-inflicted outage rather than a backstop.
    #[test]
    fn codex_probe_timeouts_are_finite_and_non_trivial() {
        assert!(CODEX_PROBE_TIMEOUT >= std::time::Duration::from_secs(5));
        assert!(CODEX_LOOKUP_TIMEOUT >= std::time::Duration::from_secs(5));
        // Lookups are plain host/guest reads, so they stay tighter than the
        // CLI capability probes (which may go through PowerShell).
        assert!(CODEX_LOOKUP_TIMEOUT < CODEX_PROBE_TIMEOUT);
    }

    /// How long a probe may wait for its siblings before the concurrency
    /// assertions call the chain serial.
    const PROBE_RENDEZVOUS: std::time::Duration = std::time::Duration::from_secs(2);

    /// An all-participants rendezvous, so the concurrency assertions are
    /// deterministic rather than a sleep-and-hope margin: every probe blocks
    /// until all of its siblings have arrived, so they can only all return if
    /// they were genuinely in flight together. A regression to the serial chain
    /// strands the first probe until the bound fires, which turns into a failing
    /// test instead of a hung one.
    struct Rendezvous {
        arrived: Mutex<usize>,
        released: std::sync::Condvar,
        participants: usize,
    }

    impl Rendezvous {
        fn new(participants: usize) -> Self {
            Self {
                arrived: Mutex::new(0),
                released: std::sync::Condvar::new(),
                participants,
            }
        }

        fn wait(&self) -> Result<(), String> {
            let mut arrived = self.arrived.lock().unwrap_or_else(|p| p.into_inner());
            *arrived += 1;
            // Loop on the predicate, not on a single wait: a condvar may wake
            // spuriously, and passing on that wakeup would let a probe return
            // "overlapped" before its siblings had arrived - exactly the false
            // pass this rendezvous exists to prevent.
            loop {
                if *arrived >= self.participants {
                    self.released.notify_all();
                    return Ok(());
                }
                let (guard, timeout) = self
                    .released
                    .wait_timeout(arrived, PROBE_RENDEZVOUS)
                    .unwrap_or_else(|p| p.into_inner());
                arrived = guard;
                if timeout.timed_out() && *arrived < self.participants {
                    return Err(format!(
                        "probe never overlapped its {} siblings within {PROBE_RENDEZVOUS:?} - the chain ran serially",
                        self.participants - 1
                    ));
                }
            }
        }
    }

    /// Issue #1934: the three identity probes are independent, so they must be
    /// issued concurrently. Serially they cost their sum — three `wsl.exe`
    /// starts, ~340ms each warm and seconds on a cold distro — where the user
    /// only ever waits for the slowest one.
    ///
    /// This proves the joiner overlaps its probes. That
    /// `discover_supported_install_uncached` routes through it is enforced by
    /// the compiler, not here: the call site passes bare `probe_codex_*` items,
    /// so dropping or reordering one is a type error.
    #[test]
    fn codex_identity_probes_are_issued_concurrently() {
        let rendezvous = Rendezvous::new(3);
        let (version, executable, home) = probe_codex_identity_concurrently(
            EnvType::Windows,
            None,
            |_, _| {
                rendezvous
                    .wait()
                    .map(|_| CodexVersion("0.158.0".to_string()))
            },
            |_, _| {
                rendezvous
                    .wait()
                    .map(|_| CodexExecutable("/usr/bin/codex".to_string()))
            },
            |_, _| {
                rendezvous
                    .wait()
                    .map(|_| CodexHome("/home/dev/.codex".to_string()))
            },
        )
        .expect("all three probes must rendezvous and succeed");
        assert_eq!(version.0, "0.158.0");
        assert_eq!(executable.0, "/usr/bin/codex");
        assert_eq!(home.0, "/home/dev/.codex");
    }

    /// The concurrent chain must not change which message a broken install
    /// produces. A missing `codex` fails all three probes at once, and the
    /// user has to keep seeing the version failure — the root cause — rather
    /// than the location/home failures that only follow from it.
    #[test]
    fn codex_identity_error_precedence_keeps_the_serial_order() {
        assert_eq!(
            probe_codex_identity_concurrently(
                EnvType::Windows,
                None,
                |_, _| Err("Codex version check failed".to_string()),
                |_, _| Err("Codex executable identity is unavailable".to_string()),
                |_, _| Err("WSL Codex home identity is unavailable".to_string()),
            )
            .expect_err("every probe failed"),
            "Codex version check failed",
            "a broken install must report the version failure, not a downstream symptom"
        );
        assert_eq!(
            probe_codex_identity_concurrently(
                EnvType::Windows,
                None,
                |_, _| Ok(CodexVersion("0.158.0".to_string())),
                |_, _| Err("Codex executable identity is unavailable".to_string()),
                |_, _| Err("WSL Codex home identity is unavailable".to_string()),
            )
            .expect_err("location and home failed"),
            "Codex executable identity is unavailable",
            "the version probe passing must expose the location failure next"
        );
        assert_eq!(
            probe_codex_identity_concurrently(
                EnvType::Windows,
                None,
                |_, _| Ok(CodexVersion("0.158.0".to_string())),
                |_, _| Ok(CodexExecutable("/usr/bin/codex".to_string())),
                |_, _| Err("WSL Codex home identity is unavailable".to_string()),
            )
            .expect_err("only the home probe failed"),
            "WSL Codex home identity is unavailable"
        );
    }

    /// Issue #1934: the two `--help` capability probes are independent reads of
    /// the same CLI, so they overlap too.
    #[test]
    fn proxy_cli_help_probes_are_issued_concurrently() {
        let rendezvous = Rendezvous::new(2);
        let (fresh, resume) = probe_proxy_cli_help_concurrently(
            EnvType::Windows,
            None,
            |_, _| {
                rendezvous
                    .wait()
                    .map(|_| FreshHelp("fresh help".to_string()))
            },
            |_, _| {
                rendezvous
                    .wait()
                    .map(|_| ResumeHelp("resume help".to_string()))
            },
        )
        .expect("both help probes must rendezvous and succeed");
        assert_eq!(fresh.0, "fresh help");
        assert_eq!(resume.0, "resume help");
    }

    /// The help pair keeps probe-order precedence, matching both the serial
    /// chain and the `("fresh", ..)` order [`validate_proxy_cli_help`] reports
    /// a missing flag in.
    #[test]
    fn proxy_cli_help_error_precedence_reports_fresh_first() {
        assert_eq!(
            probe_proxy_cli_help_concurrently(
                EnvType::Windows,
                None,
                |_, _| Err("Codex fresh capability check failed".to_string()),
                |_, _| Err("Codex resume capability check failed".to_string()),
            )
            .expect_err("both help probes failed"),
            "Codex fresh capability check failed"
        );
        assert_eq!(
            probe_proxy_cli_help_concurrently(
                EnvType::Windows,
                None,
                |_, _| Ok(FreshHelp("fresh help".to_string())),
                |_, _| Err("Codex resume capability check failed".to_string()),
            )
            .expect_err("only the resume probe failed"),
            "Codex resume capability check failed"
        );
    }

    /// Finding #1: a probe panic must reach the boundary above carrying the
    /// message that named the probe. `join().unwrap()` replaces the payload
    /// with `Any { .. }`, so the joiners re-raise with `resume_unwind` instead;
    /// this pins that the payload survives.
    #[test]
    fn a_panicking_probe_keeps_its_message_at_the_join_boundary() {
        let boundary = std::panic::catch_unwind(|| {
            let _ = probe_codex_identity_concurrently(
                EnvType::Windows,
                None,
                |_, _| -> Result<CodexVersion, String> {
                    panic!("WSL Codex location probe blew up")
                },
                |_, _| Ok(CodexExecutable("/usr/bin/codex".to_string())),
                |_, _| Ok(CodexHome("/home/dev/.codex".to_string())),
            );
        });
        let payload =
            boundary.expect_err("the panicking probe must re-raise on the joining thread");
        let message = payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|m| m.to_string()))
            .expect("panic payload must still be a string");
        assert_eq!(
            message, "WSL Codex location probe blew up",
            "the probe's own message must survive the join, not degrade to `Any {{ .. }}`"
        );
    }

    #[test]
    fn multiline_automated_prefill_uses_pty_transport() {
        assert!(!CODEX.prefill_requires_pty("review the PR"));
        assert!(CODEX.prefill_requires_pty("review the diff\n+ added line"));
        assert!(CODEX.prefill_requires_pty("review the diff\r+ added line"));
        assert!(CODEX.prefill_requires_pty("- review this change"));
        assert!(CODEX.prefill_requires_pty("+ review this change"));
    }

    #[test]
    fn initial_prompt_waits_for_the_codex_composer() {
        // Real frames captured from a live Codex 0.158 PTY. The model is only
        // ever a bare `loading` line on boot and a `GPT-6-Luna default · <dir>`
        // status footer once resolved — neither the old `model: <name>
        // /model to change` banner nor a non-"loading" model header is ever
        // rendered, so the previous gate never fired (run 261, node 4618).
        let boot = ">_ OpenAI Codex (v0.158.0)\nloading\n\
                    › Ask Codex to do anything\n? for shortcuts";
        let loaded = ">_ OpenAI Codex (v0.158.0)\nloading\n\
                      › Ask Codex to do anything\n? for shortcuts\nF:\\src\\nestlin\n\
                      GPT-6-Luna default · F:\\src\\nestlin\nxhigh · F:\\src\\nestlin";
        let resumed = "› Ask a follow-up question";
        let pre_composer = ">_ OpenAI Codex (v0.158.0)\nloading";
        assert!(!CODEX.ready_for_initial_prompt(""));
        assert!(!CODEX.ready_for_initial_prompt(pre_composer));
        assert!(CODEX.ready_for_initial_prompt(boot));
        assert!(CODEX.ready_for_initial_prompt(loaded));
        assert!(CODEX.ready_for_initial_prompt(resumed));
    }

    #[test]
    fn stable_profile_identity_survives_endpoint_and_model_edits() {
        let before = stable_profile_name("codex", "minimax");
        let after = stable_profile_name("codex", "minimax");
        assert_eq!(before, after);
        assert!(before.starts_with("buildmesh_"));
        assert_ne!(before, stable_profile_name("codex", "another-provider"));
    }

    #[test]
    fn codex_home_resolution_honours_explicit_and_default_locations() {
        let explicit = native_codex_home_from(
            Some(std::ffi::OsString::from("/custom/codex")),
            Some(std::ffi::OsString::from("/home/user")),
        )
        .unwrap();
        assert_eq!(explicit, std::path::PathBuf::from("/custom/codex"));
        let default =
            native_codex_home_from(None, Some(std::ffi::OsString::from("/home/user"))).unwrap();
        assert_eq!(default, std::path::PathBuf::from("/home/user/.codex"));
        assert!(native_codex_home_from(None, None).is_err());
    }

    #[test]
    fn wsl_codex_home_uses_guest_default_without_host_override() {
        assert!(WSL_CODEX_HOME_SCRIPT.contains("${CODEX_HOME:-$HOME/.codex}"));
        assert!(!CODEX.wsl_passthrough_env().contains(&"CODEX_HOME"));
        assert_ne!(
            wsl_runtime_identity("Ubuntu", "/home/user/.codex"),
            wsl_runtime_identity("Debian", "/home/user/.codex")
        );
        assert_ne!(
            wsl_runtime_identity("Ubuntu", "/home/user/.codex"),
            wsl_runtime_identity("Ubuntu", "/custom/codex")
        );
    }

    /// Issue #1937 review: a successful WSL probe resolves
    /// `wsl:{distro}:{codex_home}` where the home carries the guest
    /// username. The logged value must keep the distro (which host was
    /// slow) and drop the path — this is the value-level coverage for the
    /// successful-probe arm of the derivation log (driving a real probe
    /// here would need a live Codex install per runtime).
    #[test]
    fn log_safe_runtime_identity_omits_wsl_codex_home() {
        let wsl = CodexInstall {
            executable: "/home/alice/.local/bin/codex".into(),
            version: "1.2.3".into(),
            runtime_identity: wsl_runtime_identity("Ubuntu", "/home/alice/.codex"),
            codex_home: "/home/alice/.codex".into(),
            wsl_distro: Some("Ubuntu".into()),
        };
        assert_eq!(wsl.runtime_identity, "wsl:Ubuntu:/home/alice/.codex");
        let logged = log_safe_runtime_identity(&wsl);
        assert_eq!(logged, "wsl:Ubuntu");
        assert!(
            !logged.contains("alice"),
            "guest username must not reach logs: {logged}"
        );
        let native = CodexInstall {
            executable: "codex".into(),
            version: "1.2.3".into(),
            runtime_identity: "native-windows".into(),
            codex_home: String::new(),
            wsl_distro: None,
        };
        assert_eq!(log_safe_runtime_identity(&native), "native-windows");
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires a local WSL distribution"]
    fn wsl_profile_materializes_in_default_and_explicit_codex_home() {
        let distro = crate::env::detect_default_wsl_distro().expect("WSL distribution");
        let default_home = crate::process_util::command_no_window("wsl.exe")
            .args(["-d", &distro, "--exec", "sh", "-lc", WSL_CODEX_HOME_SCRIPT])
            .env_remove("CODEX_HOME")
            .output()
            .unwrap();
        assert!(default_home.status.success());
        let default_home = crate::env::parse_wsl_codex_home_output(&default_home.stdout)
            .expect("WSL Codex home marker")
            .to_string_lossy()
            .into_owned();
        assert!(!default_home.is_empty());
        let explicit_home = format!("/tmp/buildmesh-codex-profile-test-{}", std::process::id());

        for (index, home) in [default_home, explicit_home.clone()]
            .into_iter()
            .enumerate()
        {
            let profile = format!("buildmesh_wsl_contract_{}_{}", std::process::id(), index);
            let install = CodexInstall {
                executable: "/usr/bin/codex".into(),
                version: "test".into(),
                runtime_identity: wsl_runtime_identity(&distro, &home),
                codex_home: home.clone(),
                wsl_distro: Some(distro.clone()),
            };
            let expected =
                render_proxy_profile(&profile, "WSL contract", "https://example.invalid/v1");
            materialize_proxy_profile(
                EnvType::Wsl,
                &install,
                &profile,
                "WSL contract",
                "https://example.invalid/v1",
            )
            .unwrap();
            let output = crate::process_util::command_no_window("wsl.exe")
                .args([
                    "-d",
                    &distro,
                    "--exec",
                    "sh",
                    "-c",
                    "cat \"$1/$2.config.toml\"",
                    "buildmesh-test",
                    &home,
                    &profile,
                ])
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
            let _ = crate::process_util::command_no_window("wsl.exe")
                .args([
                    "-d",
                    &distro,
                    "--exec",
                    "sh",
                    "-c",
                    "rm -f \"$1/$2.config.toml\"",
                    "buildmesh-test",
                    &home,
                    &profile,
                ])
                .status();
        }
        let _ = crate::process_util::command_no_window("wsl.exe")
            .args(["-d", &distro, "--exec", "rmdir", &explicit_home])
            .status();
    }

    #[test]
    fn proxy_profile_is_exact_secret_free_and_toml_escaped() {
        let rendered = render_proxy_profile(
            "buildmesh_1234",
            "Provider \"quoted\"",
            "https://example.com/v1/\"quoted\"",
        );
        assert_eq!(
            rendered,
            "model_provider = \"buildmesh_1234\"\n\n[model_providers.buildmesh_1234]\nname = \"Buildmesh: Provider \\\"quoted\\\"\"\nbase_url = \"https://example.com/v1/\\\"quoted\\\"\"\nwire_api = \"responses\"\nenv_key = \"BUILDMESH_CODEX_PROVIDER_KEY\"\nrequires_openai_auth = false\n"
        );
        assert!(!rendered.contains("OPENAI_API_KEY"));
    }

    #[test]
    fn supported_version_floor_is_strict() {
        assert_eq!(parse_version("codex-cli 0.144.0").unwrap().0, 0);
        let old = parse_version("codex-cli 0.143.9").unwrap();
        assert!((old.0, old.1, old.2) < MIN_PROXY_CODEX_VERSION);
        assert!(parse_version("custom build").is_none());
    }

    #[test]
    fn proxy_cli_requires_profile_and_model_for_fresh_and_resume() {
        let complete = "Usage: codex [OPTIONS]\n  --profile <NAME>\n  --model <MODEL>";
        assert!(validate_proxy_cli_help(complete, complete).is_ok());
        let error = validate_proxy_cli_help(complete, "Usage: codex resume").unwrap_err();
        assert!(error.contains("resume"));
        assert!(error.contains("--profile"));
        assert!(error.contains("--model"));
    }

    /// Opt-in real-CLI profile-routing check. CI installs the exact version
    /// named in `.github/workflows/build.yml` before selecting this test.
    #[test]
    #[ignore = "requires the pinned Codex CLI installed by workflow_dispatch"]
    fn pinned_codex_cli_loads_profile_for_fresh_and_resume() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let cold_start = std::time::Instant::now();
        let mut install = discover_supported_install(EnvType::Windows).unwrap();
        assert_eq!(install.version, "0.147.0");
        assert!(cold_start.elapsed() < std::time::Duration::from_secs(5));

        let temp = TempDir::new().unwrap();
        install.codex_home = temp.path().to_string_lossy().into_owned();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for index in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut chunk = [0u8; 8192];
                loop {
                    let read = stream.read(&mut chunk).unwrap();
                    request.extend_from_slice(&chunk[..read]);
                    let text = String::from_utf8_lossy(&request);
                    let Some(header_end) = text.find("\r\n\r\n") else {
                        continue;
                    };
                    let length = text[..header_end]
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_string)
                        })
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if request.len() >= header_end + 4 + length {
                        break;
                    }
                }
                let request = String::from_utf8(request).unwrap();
                assert!(
                    request.starts_with("POST /v1/responses HTTP/1.1"),
                    "{request}"
                );
                assert!(request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer pinned-secret"));
                assert!(request.contains("\"model\":\"MiniMax-M3\""));
                let response_id = format!("resp_{}", index + 1);
                let message_id = format!("msg_{}", index + 1);
                let completed = serde_json::json!({
                    "id": response_id,
                    "object": "response",
                    "created_at": 1_700_000_000,
                    "status": "completed",
                    "error": null,
                    "incomplete_details": null,
                    "instructions": null,
                    "max_output_tokens": null,
                    "model": "MiniMax-M3",
                    "output": [{
                        "id": message_id,
                        "type": "message",
                        "status": "completed",
                        "role": "assistant",
                        "content": [{"type":"output_text","text":"verified","annotations":[]}]
                    }],
                    "parallel_tool_calls": false,
                    "previous_response_id": null,
                    "reasoning": {"effort":"medium","summary":null},
                    "store": false,
                    "temperature": null,
                    "text": {"format":{"type":"text"}},
                    "tool_choice": "auto",
                    "tools": [],
                    "top_p": null,
                    "truncation": "disabled",
                    "usage": {"input_tokens":1,"input_tokens_details":{"cached_tokens":0},"output_tokens":1,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":2},
                    "user": null,
                    "metadata": {}
                });
                let body = format!(
                    "data: {{\"type\":\"response.output_text.delta\",\"item_id\":\"{message_id}\",\"output_index\":0,\"content_index\":0,\"delta\":\"verified\"}}\n\ndata: {}\n\n",
                    serde_json::json!({"type":"response.completed","response":completed})
                );
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                )
                .unwrap();
                stream.flush().unwrap();
            }
        });

        let profile = "buildmesh_pinned_contract";
        materialize_proxy_profile(
            EnvType::Windows,
            &install,
            profile,
            "Pinned fake Responses",
            &endpoint,
        )
        .unwrap();
        let run = |args: &[&str]| {
            let mut command = if cfg!(windows) {
                let mut command = std::process::Command::new("cmd.exe");
                command.args(["/d", "/c", &install.executable]);
                command
            } else {
                std::process::Command::new(&install.executable)
            };
            command
                .args(args)
                .current_dir(temp.path())
                .env("CODEX_HOME", temp.path())
                .env(PROXY_CREDENTIAL_ENV, "pinned-secret")
                .env_remove("OPENAI_API_KEY")
                .env_remove("OPENAI_BASE_URL")
                .output()
                .unwrap()
        };
        let fresh = run(&[
            "--profile",
            profile,
            "--model",
            "MiniMax-M3",
            "exec",
            "--skip-git-repo-check",
            "reply with verified",
        ]);
        assert!(
            fresh.status.success(),
            "{}",
            String::from_utf8_lossy(&fresh.stderr)
        );
        let resume = run(&[
            "--profile",
            profile,
            "--model",
            "MiniMax-M3",
            "exec",
            "resume",
            "--last",
            "--skip-git-repo-check",
            "reply with verified again",
        ]);
        assert!(
            resume.status.success(),
            "{}",
            String::from_utf8_lossy(&resume.stderr)
        );
        server.join().unwrap();
    }

    #[test]
    fn native_profile_repairs_stale_content_and_preserves_user_config() {
        let home = TempDir::new().unwrap();
        let user_config = home.path().join("config.toml");
        std::fs::write(&user_config, "model = \"user-choice\"\n").unwrap();
        let profile = "buildmesh_1234";
        let target = home.path().join(format!("{profile}.config.toml"));
        std::fs::write(&target, "edited = true\n").unwrap();
        let expected = render_proxy_profile(profile, "MiniMax", "https://api.minimax.io/v1");

        materialize_native_profile_at(home.path(), profile, &expected).unwrap();
        assert_eq!(std::fs::read_to_string(target).unwrap(), expected);
        assert_eq!(
            std::fs::read_to_string(user_config).unwrap(),
            "model = \"user-choice\"\n"
        );
    }

    #[test]
    fn profile_materialization_fails_closed_when_home_is_not_a_directory() {
        let temp = TempDir::new().unwrap();
        let invalid_home = temp.path().join("codex-home-file");
        std::fs::write(&invalid_home, "occupied").unwrap();
        let error = materialize_native_profile_at(
            &invalid_home,
            "buildmesh_1234",
            "model_provider = \"buildmesh_1234\"\n",
        )
        .unwrap_err();
        assert!(error.contains("create Codex home"));
        assert_eq!(std::fs::read_to_string(invalid_home).unwrap(), "occupied");
    }

    #[test]
    fn concurrent_profile_materialization_is_deterministic() {
        let home = TempDir::new().unwrap();
        let home_path = home.path().to_path_buf();
        let profile = "buildmesh_concurrent";
        let expected = render_proxy_profile(profile, "MiniMax", "https://api.minimax.io/v1");
        let mut workers = Vec::new();
        for _ in 0..8 {
            let home_path = home_path.clone();
            let expected = expected.clone();
            workers.push(std::thread::spawn(move || {
                materialize_native_profile_at(&home_path, profile, &expected)
            }));
        }
        for worker in workers {
            worker.join().unwrap().unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(home.path().join(format!("{profile}.config.toml"))).unwrap(),
            expected
        );
    }

    #[test]
    fn legacy_cleanup_requires_both_owned_name_and_exact_shape() {
        let home = TempDir::new().unwrap();
        let legacy_name = "bm1234567890abcdef";
        let owned = home.path().join(format!("{legacy_name}.config.toml"));
        let suspicious = home.path().join("bmabcdefabcdefabcd.config.toml");
        let user = home.path().join("bm-user.config.toml");
        std::fs::write(
            &owned,
            format!("model_provider = \"{legacy_name}\"\n\n[model_providers.{legacy_name}]\nname = \"Buildmesh proxy {legacy_name}\"\nbase_url = \"https://legacy.example/v1\"\nenv_key = \"OPENAI_API_KEY\"\nrequires_openai_auth = true\n"),
        )
        .unwrap();
        std::fs::write(
            &suspicious,
            "name = \"Buildmesh proxy bmabcdefabcdefabcd\"\nenv_key = \"OPENAI_API_KEY\"\nrequires_openai_auth = true\nuser_setting = true\n",
        )
        .unwrap();
        std::fs::write(&user, "name = \"my profile\"\n").unwrap();
        let expected = render_proxy_profile("buildmesh_new", "MiniMax", "https://example.com/v1");

        materialize_native_profile_at(home.path(), "buildmesh_new", &expected).unwrap();
        assert!(!owned.exists());
        assert!(suspicious.exists());
        assert!(user.exists());
    }

    /// Local hooks only run headlessly with the trust bypass (issue #884) —
    /// both the fresh and resume spawn paths must carry the flag, or Codex
    /// blocks on an interactive workspace-review prompt.
    #[test]
    fn spawn_recipes_carry_the_hook_trust_bypass() {
        let bypass = "--dangerously-bypass-hook-trust".to_string();
        let fresh = CODEX.spawn_recipe(Platform::Windows, EnvType::Windows);
        assert!(
            fresh.base_args.contains(&bypass),
            "fresh: {:?}",
            fresh.base_args
        );
        let resume = CODEX
            .spawn_recipe_for_resume(Platform::Windows, "sid-123")
            .expect("codex has a resume recipe");
        assert!(
            resume.base_args.contains(&bypass),
            "resume: {:?}",
            resume.base_args
        );
    }

    #[test]
    // This guards Buildmesh's launch choice only; Codex's TUI owns the resize
    // reflow behavior (`codex-rs/tui/src/app/resize_reflow.rs`). The separate
    // `tests/unit/terminal-resize-scheduler.test.ts` covers only when Buildmesh
    // forwards the PTY resize; neither test executes Codex's TUI.
    fn spawn_recipes_allow_codex_fullscreen_transcript() {
        let no_alt_screen = "--no-alt-screen".to_string();
        let fresh = CODEX.spawn_recipe(Platform::Linux, EnvType::Wsl);
        assert!(
            !fresh.base_args.contains(&no_alt_screen),
            "fresh: {:?}",
            fresh.base_args
        );
        let resume = CODEX
            .spawn_recipe_for_resume(Platform::Linux, "sid-123")
            .expect("codex has a resume recipe");
        assert!(
            !resume.base_args.contains(&no_alt_screen),
            "resume: {:?}",
            resume.base_args
        );

        let fresh_windows = CODEX.spawn_recipe(Platform::Windows, EnvType::Windows);
        assert!(
            !fresh_windows.base_args.contains(&no_alt_screen),
            "fresh Windows: {:?}",
            fresh_windows.base_args
        );
        let resume_windows = CODEX
            .spawn_recipe_for_resume(Platform::Windows, "sid-123")
            .expect("codex has a Windows resume recipe");
        assert!(
            !resume_windows.base_args.contains(&no_alt_screen),
            "resume Windows: {:?}",
            resume_windows.base_args
        );
    }

    #[test]
    fn effort_uses_codex_config_override() {
        assert_eq!(
            CODEX.effort_args("xhigh"),
            vec!["-c", "model_reasoning_effort=\"xhigh\""]
        );
    }

    #[test]
    fn effort_config_override_escapes_embedded_quotes() {
        assert_eq!(
            CODEX.effort_args("weird\"name"),
            vec!["-c", r#"model_reasoning_effort="weird\"name""#]
        );
    }

    #[test]
    fn codex_declares_attention_hook_and_readable_transcript() {
        assert!(CODEX.requires_attention_hook());
        assert!(CODEX.produces_readable_transcript());
        let crate::agent::capabilities::AttentionCapability::Hook {
            events,
            min_version,
            ..
        } = CODEX.attention_capability()
        else {
            panic!("Codex must expose native hook capability");
        };
        assert!(events.contains(&crate::agent::session_lifecycle::LifecycleKind::QuestionRequested));
        assert_eq!(min_version.as_deref(), Some(CODEX_MIN_HOOK_VERSION));
    }

    fn read_hooks_json(project: &Path) -> serde_json::Value {
        let content = std::fs::read_to_string(project.join(".codex").join("hooks.json"))
            .expect("hooks.json not written");
        serde_json::from_str(&content).expect("hooks.json is not valid JSON")
    }

    /// Every provisioned command must POST the hook's stdin JSON as the
    /// request body: curl reads it from `@-`, the WSL relay from a quoted
    /// `'@-'`, and the PowerShell callbacks from the console.
    fn forwards_hook_stdin(command: &str) -> bool {
        command.contains("--data-binary @-")
            || command.contains("--data-binary '@-'")
            || (command.contains("[Console]::In.ReadToEnd()")
                && command.contains("Invoke-WebRequest"))
    }

    /// A Windows callback is installed as encoded PowerShell, so decode it
    /// before asserting on the script it carries.
    fn decoded_hook_command(command: &str) -> String {
        crate::env::decode_powershell_command(command).unwrap_or_else(|| command.to_string())
    }

    fn provision_codex(project: &Path) {
        let path = project.to_string_lossy().into_owned();
        let resolved = ResolvedPath {
            host_path: path.clone(),
            spawn_path: path.clone(),
            raw_path: path,
            env_type: EnvType::Windows,
        };
        CODEX
            .provision_attention_hooks(&resolved, &LaunchRuntime::default(), 42)
            .unwrap();
    }

    /// Injection writes both files: the feature flag and all seven attention
    /// webhooks in Codex's nested matcher/event
    /// schema, POSTing the hook's stdin to the attention endpoint. The
    /// request_user_input pre-hook remains narrowly matched, while
    /// PostToolUse is catch-all so an approved permission for any tool can
    /// clear its marker before the terminal Stop fallback.
    #[test]
    fn inject_writes_config_and_hooks() {
        let temp = TempDir::new().unwrap();
        provision_codex(temp.path());

        let config = std::fs::read_to_string(temp.path().join(".codex").join("config.toml"))
            .expect("config.toml not written");
        assert!(config.contains("[features]"), "config: {config}");
        assert!(config.contains("hooks = true"), "config: {config}");

        let hooks = read_hooks_json(temp.path());
        for event in [
            "SessionStart",
            "Stop",
            "PermissionRequest",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "Interrupt",
        ] {
            let command = hooks["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .map(decoded_hook_command)
                .unwrap_or_else(|| panic!("{event} hook missing: {hooks:#}"));
            assert!(
                command.contains("/api/attention/"),
                "{event} must POST to the attention endpoint: {command}"
            );
            assert!(
                forwards_hook_stdin(&command),
                "{event} must forward the hook stdin as the POST body: {command}"
            );
            if event == "PreToolUse" {
                assert_eq!(
                    hooks["hooks"][event][0]["matcher"].as_str(),
                    Some("^request_user_input$")
                );
            } else if event == "PostToolUse" {
                assert!(
                    hooks["hooks"][event][0].get("matcher").is_none(),
                    "PostToolUse must be catch-all so approved permissions correlate"
                );
            }
        }
    }

    #[test]
    fn attention_hook_commands_fail_open_and_emit_json() {
        let url = "http://localhost:1992/api/attention/42";

        let unix = attention_hook_unix_command(url);
        assert!(unix.contains("2>/dev/null || true; printf '{}'"), "{unix}");

        let windows = attention_hook_windows_fallback_command(url);
        let script = crate::env::decode_powershell_command(&windows)
            .expect("native Windows callback is valid in either PowerShell or cmd.exe");
        assert!(script.contains("[Console]::In.ReadToEnd()"), "{script}");
        assert!(script.contains("Invoke-WebRequest"), "{script}");
        assert!(script.contains("catch { }"), "{script}");
        assert!(
            script.contains("[Console]::Out.WriteLine('{}')"),
            "{script}"
        );
        assert!(script.ends_with("exit 0"), "{script}");
    }

    #[test]
    fn codex_hook_relay_override_is_opt_in_and_loopback_only() {
        assert_eq!(
            attention_hook_url(42, 2992, Some("http://127.0.0.1:43123/")),
            "http://127.0.0.1:43123/api/attention/42?forward_port=2992"
        );
        for relay in [
            "https://127.0.0.1:43123",
            "http://example.test:43123",
            "http://127.0.0.1:0",
            "http://127.0.0.1:43123/path",
            "http://user@127.0.0.1:43123",
        ] {
            assert_eq!(
                attention_hook_url(42, 2992, Some(relay)),
                "http://localhost:2992/api/attention/42",
                "invalid relay must leave the app's local callback intact: {relay}"
            );
        }
        assert_eq!(
            attention_hook_url(42, 1992, None),
            "http://localhost:1992/api/attention/42"
        );
        assert!(base_flags_with_sandbox("read-only")
            .windows(2)
            .any(|pair| pair == ["--sandbox", "read-only"]));
        assert!(base_flags_with_sandbox("danger-full-access")
            .windows(2)
            .any(|pair| pair == ["--sandbox", "danger-full-access"]));
    }

    #[cfg(unix)]
    #[test]
    fn unix_attention_hook_emits_json_after_callback_failure() {
        use std::process::Command;

        let command = attention_hook_unix_command("http://localhost:1992/api/attention/42");
        let script = format!("set -e; curl() {{ return 22; }}; {command}");
        let output = Command::new("sh")
            .args(["-c", &script])
            .output()
            .expect("run attention hook in a POSIX shell");

        assert!(
            output.status.success(),
            "hook failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"{}");
    }

    /// The `command` a WSL node runs: a POSIX script, not the cmd-runnable
    /// PowerShell a Windows node gets. `sh` has no `curl.exe`, so the probe
    /// takes its Linux branch; stubbing `curl` then fails the callback, which
    /// must still emit `{}`.
    #[cfg(unix)]
    #[test]
    fn wsl_shell_attention_hook_emits_json_after_callback_failure() {
        use std::process::Command;

        let command = attention_hook_wsl_shell_command("http://localhost:1992/api/attention/42");
        let script = format!("set -e; curl() {{ return 22; }}; {command}");
        let output = Command::new("sh")
            .args(["-c", &script])
            .output()
            .expect("run the WSL attention hook in a POSIX shell");

        assert!(
            output.status.success(),
            "hook failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"{}");
    }

    #[cfg(windows)]
    #[test]
    fn windows_fallback_emits_json_after_callback_failure() {
        use std::os::windows::process::CommandExt;
        use std::process::Command;

        let command =
            attention_hook_windows_fallback_command("http://localhost:1/api/attention/42");
        let output = Command::new("cmd.exe")
            .args(["/d", "/c", &command])
            .creation_flags(0x08000000)
            .output()
            .expect("run attention hook in cmd.exe");

        assert!(
            output.status.success(),
            "hook failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "{}");
    }

    /// Re-running injection over an already-correct project is a no-op.
    #[test]
    fn inject_is_idempotent() {
        let temp = TempDir::new().unwrap();
        provision_codex(temp.path());
        let config_first =
            std::fs::read_to_string(temp.path().join(".codex").join("config.toml")).unwrap();
        let hooks_first = read_hooks_json(temp.path());

        provision_codex(temp.path());
        let config_second =
            std::fs::read_to_string(temp.path().join(".codex").join("config.toml")).unwrap();
        assert_eq!(config_first, config_second);
        assert_eq!(hooks_first, read_hooks_json(temp.path()));
    }

    /// A user's existing config.toml keys survive; the flag lands under an
    /// existing `[features]` section instead of duplicating it (a duplicate
    /// table is a TOML parse error that would break Codex's whole config).
    #[test]
    fn config_merge_preserves_content_and_existing_features_section() {
        let temp = TempDir::new().unwrap();
        let codex_dir = temp.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(
            codex_dir.join("config.toml"),
            "model = \"gpt-5.2-codex\"\n\n[features]\nweb_search = true\n",
        )
        .unwrap();

        provision_codex(temp.path());

        let config = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            config.contains("model = \"gpt-5.2-codex\""),
            "config: {config}"
        );
        assert!(config.contains("web_search = true"), "config: {config}");
        assert!(config.contains("hooks = true"), "config: {config}");
        assert_eq!(
            config.matches("[features]").count(),
            1,
            "must not duplicate the [features] table: {config}"
        );
    }

    /// A config without a `[features]` section gets one appended, keeping the
    /// user's content intact.
    #[test]
    fn config_merge_appends_features_section_when_missing() {
        let temp = TempDir::new().unwrap();
        let codex_dir = temp.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(codex_dir.join("config.toml"), "model = \"gpt-5.2-codex\"\n").unwrap();

        provision_codex(temp.path());

        let config = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(config.contains("model = \"gpt-5.2-codex\""));
        assert!(
            config.contains("[features]\nhooks = true"),
            "config: {config}"
        );
    }

    /// Injection only owns the `hooks` key of hooks.json — unrelated keys the
    /// user added survive.
    #[test]
    fn hooks_json_merge_preserves_unrelated_keys() {
        let temp = TempDir::new().unwrap();
        let codex_dir = temp.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(codex_dir.join("hooks.json"), r#"{"custom":"kept"}"#).unwrap();

        provision_codex(temp.path());

        let hooks = read_hooks_json(temp.path());
        assert_eq!(hooks["custom"], "kept");
        assert!(hooks["hooks"]["Stop"].is_array());
    }

    #[test]
    fn hooks_json_merge_preserves_existing_event_handlers() {
        let temp = TempDir::new().unwrap();
        let codex_dir = temp.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(
            codex_dir.join("hooks.json"),
            r#"{
                "description": "user config",
                "hooks": {
                    "Stop": [{"matcher":".*","hooks":[{"type":"command","command":"user-stop"}]}],
                    "PermissionRequest": [{"matcher":"Bash","hooks":[{"type":"command","command":"user-permission"}]}]
                }
            }"#,
        )
        .unwrap();

        provision_codex(temp.path());

        let hooks = read_hooks_json(temp.path());
        assert_eq!(hooks["description"], "user config");
        assert_eq!(hooks["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert_eq!(
            hooks["hooks"]["PermissionRequest"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            hooks["hooks"]["Stop"][0]["hooks"][0]["command"],
            "user-stop"
        );
        assert_eq!(
            hooks["hooks"]["PermissionRequest"][0]["hooks"][0]["command"],
            "user-permission"
        );
        assert!(hooks["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .any(|group| {
                group["hooks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(is_buildmesh_hook_handler)
            }));
    }

    #[test]
    fn inject_migrates_old_post_tool_matcher_to_catch_all() {
        let temp = TempDir::new().unwrap();
        let codex_dir = temp.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let old_hook = attention_hook_handler(42, EnvType::Windows);
        let old = serde_json::json!({
            "hooks": {
                "PostToolUse": [{
                    "matcher": "^request_user_input$",
                    "hooks": [old_hook]
                }]
            }
        });
        std::fs::write(
            codex_dir.join("hooks.json"),
            serde_json::to_string(&old).unwrap(),
        )
        .unwrap();

        provision_codex(temp.path());

        let hooks = read_hooks_json(temp.path());
        assert!(hooks["hooks"]["PostToolUse"][0].get("matcher").is_none());
    }

    /// Issue #2036 upgrade path: a node provisioned by an older Buildmesh has
    /// the bash-script `command` on disk. That handler still carries the
    /// Buildmesh `statusMessage`, so re-provisioning has to replace it in
    /// place — appending a second handler would leave cmd.exe parsing the
    /// script forever.
    #[test]
    fn stale_windows_command_is_replaced_not_duplicated() {
        let temp = TempDir::new().unwrap();
        let codex_dir = temp.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let stale = serde_json::json!({
            "hooks": {
                "Stop": [{
                    "hooks": [{
                        "type": "command",
                        "command": attention_hook_wsl_shell_command("http://localhost:1992/api/attention/42"),
                        "commandWindows": attention_hook_windows_command("http://localhost:1992/api/attention/42"),
                        "statusMessage": BUILDMESH_HOOK_STATUS_MESSAGE,
                    }]
                }]
            }
        });
        std::fs::write(
            codex_dir.join("hooks.json"),
            serde_json::to_string(&stale).unwrap(),
        )
        .unwrap();

        provision_codex(temp.path());

        let hooks = read_hooks_json(temp.path());
        let handlers = hooks["hooks"]["Stop"][0]["hooks"]
            .as_array()
            .expect("Stop handlers");
        assert_eq!(
            handlers.len(),
            1,
            "a stale Buildmesh handler must be replaced, not duplicated: {hooks:#}"
        );
        let url = format!(
            "http://localhost:{}/api/attention/42",
            crate::http_server::current_http_port()
        );
        let expected = attention_hook_default_command(&url, EnvType::Windows);
        assert_eq!(
            handlers[0]["command"].as_str(),
            Some(expected.as_str()),
            "Stop must be re-provisioned with the cmd-runnable callback: {handlers:#?}"
        );
    }

    #[test]
    fn hooks_json_merge_does_not_overwrite_malformed_user_file() {
        let temp = TempDir::new().unwrap();
        let codex_dir = temp.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let path = codex_dir.join("hooks.json");
        std::fs::write(&path, "{not json").unwrap();

        let error = ensure_hooks_json_content(
            &std::fs::read_to_string(&path).unwrap(),
            &attention_hook_handler(42, EnvType::Windows),
        )
        .unwrap_err();
        assert!(error.contains("parse hooks.json"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), "{not json");
    }

    #[test]
    fn config_feature_merge_replaces_false_without_duplicate_keys() {
        let existing =
            "model = \"gpt-5.2-codex\"\n\n[features]\nhooks = false\nweb_search = true\n";
        let updated = ensure_hooks_feature_content(existing).unwrap();
        assert_eq!(updated.matches("hooks =").count(), 1);
        assert!(updated.contains("hooks = true"));
        assert!(updated.contains("web_search = true"));
        assert!(updated.contains("model = \"gpt-5.2-codex\""));
    }

    #[test]
    fn config_feature_merge_leaves_existing_true_aliases_untouched() {
        for existing in [
            "[features]\nhooks = true # user enabled\n",
            "[features]\ncodex_hooks = true # legacy user setting\n",
        ] {
            let updated = ensure_hooks_feature_content(existing).unwrap();
            assert_eq!(updated, existing);
        }
    }

    #[test]
    fn project_trust_merge_adds_exact_path_and_preserves_other_projects() {
        let project = r#"F:\src\buildmesh\.claude\worktrees\linked"#;
        let existing = "model = \"gpt-5.2-codex\"\n\n[projects.\"F:\\\\src\\\\buildmesh\"]\ntrust_level = \"trusted\"\n\n[features]\nweb_search = true\n";
        let updated = ensure_project_trust_content(existing, project, EnvType::Windows).unwrap();

        assert!(updated.contains("model = \"gpt-5.2-codex\""));
        assert!(updated.contains("[projects.\"F:\\\\src\\\\buildmesh\"]"));
        assert!(updated.contains("[features]\nweb_search = true"));
        assert!(
            updated.contains("trust_level = \"trusted\""),
            "new project trust entry missing: {updated}"
        );
        assert_eq!(updated.matches("trust_level = \"trusted\"").count(), 2);
    }

    #[test]
    fn project_trust_merge_updates_existing_untrusted_entry_in_place() {
        let project = r#"F:\src\buildmesh\.claude\worktrees\linked"#;
        let header = format!("[projects.{}]", toml_string(project));
        let existing = format!("{header}\ntrust_level = \"untrusted\"\nother = true\n");
        let updated = ensure_project_trust_content(&existing, project, EnvType::Windows).unwrap();

        assert_eq!(updated.matches(&header).count(), 1);
        assert_eq!(updated.matches("trust_level =").count(), 1);
        assert!(updated.contains("trust_level = \"trusted\""));
        assert!(updated.contains("other = true"));
    }

    #[test]
    fn project_trust_merge_matches_comments_quote_styles_and_windows_case() {
        let project = r#"F:\src\buildmesh"#;
        let existing = r#"# [projects."F:\src\buildmesh"] is only a note
[projects.'f:\src\buildmesh'] # worktree root
trust_level = "untrusted" # preserve this explanation
"#;
        let updated = ensure_project_trust_content(existing, project, EnvType::Windows).unwrap();
        let document = updated.parse::<DocumentMut>().unwrap();
        let projects = document["projects"].as_table_like().unwrap();
        assert_eq!(
            projects.iter().count(),
            1,
            "must not create a duplicate table"
        );
        let (_, project_table) = projects.iter().next().unwrap();
        assert_eq!(
            project_table["trust_level"].as_str(),
            Some("trusted"),
            "the existing single-quoted project key must be updated"
        );
        assert!(updated.contains("# worktree root"));
        assert!(updated.contains("# preserve this explanation"));
    }

    #[test]
    fn project_trust_merge_normalizes_windows_separators_and_trailing_slashes() {
        let existing = r#"[projects."F:/src/buildmesh/"]
trust_level = "untrusted"
"#;
        let updated =
            ensure_project_trust_content(existing, r#"F:\src\buildmesh"#, EnvType::Windows)
                .unwrap();
        let document = updated.parse::<DocumentMut>().unwrap();
        let projects = document["projects"].as_table_like().unwrap();
        assert_eq!(projects.iter().count(), 1);
        assert_eq!(
            projects
                .get("F:/src/buildmesh/")
                .unwrap()
                .as_table_like()
                .unwrap()
                .get("trust_level")
                .and_then(Item::as_value)
                .and_then(|item| item.as_str()),
            Some("trusted")
        );
    }

    #[test]
    fn project_trust_merge_keeps_case_distinct_wsl_paths() {
        let existing = r#"[projects."/home/alice/Code"]
trust_level = "trusted"
"#;
        let updated =
            ensure_project_trust_content(existing, "/home/alice/code", EnvType::Wsl).unwrap();
        let document = updated.parse::<DocumentMut>().unwrap();
        let projects = document["projects"].as_table_like().unwrap();
        assert_eq!(projects.iter().count(), 2);
        assert!(updated.contains("[projects.\"/home/alice/Code\"]"));
        assert!(updated.contains("[projects.\"/home/alice/code\"]"));
    }

    #[test]
    fn wsl_write_script_reads_payloads_from_stdin() {
        assert!(WSL_ATOMIC_WRITE_FILES_SCRIPT.contains("IFS= read -r encoded"));
        assert!(!WSL_ATOMIC_WRITE_FILES_SCRIPT.contains("printenv"));
        assert!(!WSL_ATOMIC_WRITE_FILES_SCRIPT.contains("BUILDMESH_CODEX_CONTENT"));
    }

    #[test]
    fn config_feature_merge_preserves_inline_comments_and_bracket_comments() {
        let existing = r#"# [ignored]
[features]
hooks = false # needs manual review
web_search = true
"#;
        let updated = ensure_hooks_feature_content(existing).unwrap();
        let document = updated.parse::<DocumentMut>().unwrap();
        assert_eq!(document["features"]["hooks"].as_bool(), Some(true));
        assert!(updated.contains("# needs manual review"));
        assert!(updated.contains("# [ignored]"));
    }

    #[test]
    fn malformed_project_trust_toml_is_rejected_without_a_rewrite() {
        let existing = "[projects.\"broken\"\ntrust_level = \"untrusted\"\n";
        let error = ensure_project_trust_content(existing, "broken", EnvType::Windows).unwrap_err();
        assert!(error.contains("parse Codex trust config"));
    }

    #[test]
    fn trust_project_path_uses_runtime_path_for_wsl() {
        let resolved = ResolvedPath {
            host_path: r#"\\wsl$\Ubuntu\home\alice\repo"#.to_string(), // allow-wsl-path
            spawn_path: "/home/alice/repo".to_string(),
            raw_path: "/home/alice/repo".to_string(),
            env_type: EnvType::Wsl,
        };
        assert_eq!(trust_project_path(&resolved), "/home/alice/repo");
    }

    /// The native fallback is encoded PowerShell so Codex can invoke it through
    /// either PowerShell or cmd.exe without shell-specific quoting. Both
    /// `command` and `commandWindows` carry it on a native Windows node, so
    /// exercise both: a pre-0.131.0 Codex ignores the override and runs
    /// `command` through cmd.exe (issue #2036).
    #[cfg(windows)]
    #[test]
    fn windows_attention_hook_posts_stdin_through_powershell_and_cmd() {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        for field in ["command", "commandWindows"] {
            for shell in ["powershell.exe", "cmd.exe"] {
                let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
                let endpoint = format!("http://{}/api/attention/42", server.server_addr());
                let handler = attention_hook_handler(42, EnvType::Windows);
                let original_url = format!(
                    "http://localhost:{}/api/attention/42",
                    crate::http_server::current_http_port()
                );
                let encoded_command = handler[field]
                    .as_str()
                    .unwrap_or_else(|| panic!("{field} hook missing"));
                let script = crate::env::decode_powershell_command(encoded_command)
                    .expect("Windows hook command must be encoded PowerShell")
                    .replace(&original_url, &endpoint);
                let command = format!(
                    "powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {}",
                    crate::env::encode_powershell(&script)
                );
                let receiver = std::thread::spawn(move || {
                    let mut request = server
                        .recv_timeout(std::time::Duration::from_secs(15))
                        .unwrap()?;
                    let mut body = String::new();
                    request.as_reader().read_to_string(&mut body).unwrap();
                    let path = request.url().to_owned();
                    request.respond(tiny_http::Response::empty(200)).unwrap();
                    Some((path, body))
                });
                let mut process = Command::new(shell);
                if shell == "powershell.exe" {
                    process.args(["-NoProfile", "-NonInteractive", "-Command"]);
                    process.arg(&command);
                } else {
                    process.args(["/D", "/C"]);
                    // `/C` consumes a shell command line. Passing it through
                    // `arg()` adds Windows quoting that changes cmd's parse.
                    process.raw_arg(format!(" {command}"));
                }
                let mut child = process
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .creation_flags(0x08000000)
                    .spawn()
                    .unwrap();
                let payload = r#"{"hook_event_name":"Stop","session_id":"controlled-session"}"#;
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(payload.as_bytes())
                    .unwrap();
                let output = child.wait_with_output().unwrap();
                let received = receiver.join().unwrap();
                assert!(
                    output.status.success(),
                    "{field} through {shell} command {command:?}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(
                    received,
                    Some(("/api/attention/42".into(), payload.into())),
                    "{field} through {shell}"
                );
                assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "{}");
            }
        }
    }

    #[test]
    fn windows_hook_is_not_double_wrapped_and_does_not_rely_on_process_env() {
        let temp = TempDir::new().unwrap();
        provision_codex(temp.path());
        let hooks = read_hooks_json(temp.path());
        for event in ["Stop", "PermissionRequest", "SessionStart"] {
            let handler = &hooks["hooks"][event][0]["hooks"][0];
            let command = handler["command"]
                .as_str()
                .map(decoded_hook_command)
                .unwrap_or_else(|| panic!("{event} command missing: {hooks:#}"));
            let windows = handler["commandWindows"]
                .as_str()
                .map(decoded_hook_command)
                .unwrap_or_else(|| panic!("{event} commandWindows missing: {hooks:#}"));
            for (field, value) in [("command", &command), ("commandWindows", &windows)] {
                assert!(
                    !value.contains("cmd.exe") && !value.contains("sh -c"),
                    "{event} {field} must not nest a shell Codex already launches: {value}"
                );
                assert!(
                    !value.contains("BUILDMESH_PORT") && !value.contains("BUILDMESH_SESSION_ID"),
                    "{event} {field} must bake the callback URL; Codex hook env is a Core snapshot that drops BUILDMESH_*: {value}"
                );
                assert!(
                    value.contains("/api/attention/42"),
                    "{event} {field} must bake the node id into the callback URL: {value}"
                );
                assert!(
                    value.contains("http://localhost:"),
                    "{event} {field} must use localhost (WSL loopback relay), not 127.0.0.1: {value}"
                );
                assert!(
                    forwards_hook_stdin(value),
                    "{event} {field} must POST the hook stdin as the request body: {value}"
                );
                assert!(
                    value.contains("-o /dev/null") || value.contains("Out-Null"),
                    "{event} {field} must discard the HTTP response body: {value}"
                );
            }
        }
    }

    /// Issue #2036: `command` is the fallback a pre-0.131.0 Codex runs when it
    /// does not know `commandWindows`, and it is also what a non-Windows Codex
    /// always runs. It therefore has to be written for the shell that will
    /// really parse it — the target Codex binary, not the Buildmesh host.
    #[test]
    fn command_is_written_for_the_target_codex_binary() {
        let url = "http://localhost:1992/api/attention/42";

        // A WSL node runs a Linux Codex on either host, so `command` reaches
        // `$SHELL -lc` and must stay a POSIX script.
        let wsl = attention_hook_default_command(url, EnvType::Wsl);
        assert!(
            crate::env::decode_powershell_command(&wsl).is_none(),
            "a WSL Codex parses `command` with $SHELL -lc, never PowerShell: {wsl}"
        );
        assert!(wsl.contains("printf '{}'"), "{wsl}");

        // Every runtime whose Codex is a Windows binary gets the cmd-runnable
        // callback in both fields.
        let windows_targets: &[EnvType] = if cfg!(windows) {
            &[EnvType::Windows, EnvType::WindowsInterop]
        } else {
            &[EnvType::WindowsInterop]
        };
        for env_type in windows_targets {
            assert_eq!(
                attention_hook_default_command(url, *env_type),
                attention_hook_windows_command(url),
                "{env_type} Codex falls back to `command` on pre-0.131.0 binaries, so it must \
                 be the same cmd-runnable callback as commandWindows"
            );
        }
    }

    /// `codex resume [OPTIONS] [SESSION_ID] [PROMPT]`. Flags after the UUID
    /// become the optional prompt, so a restarted node "resumes" into a
    /// garbage turn instead of restoring the conversation.
    #[test]
    fn resume_recipe_puts_options_before_session_id() {
        let resume = CODEX
            .spawn_recipe_for_resume(Platform::Windows, "sid-123")
            .expect("codex has a resume recipe");
        assert_eq!(resume.trailing_args, vec!["sid-123".to_string()]);
        assert!(
            !resume.base_args.iter().any(|arg| arg == "sid-123"),
            "session id must not live in base_args: {:?}",
            resume.base_args
        );
        let resume_at = resume
            .base_args
            .iter()
            .position(|arg| arg == "resume")
            .expect("resume subcommand");
        let flag_at = resume
            .base_args
            .iter()
            .position(|arg| arg == "--ask-for-approval")
            .expect("approval flag");
        assert!(
            resume_at < flag_at,
            "expected `codex resume [OPTIONS]` then trailing id, got {:?} + {:?}",
            resume.base_args,
            resume.trailing_args
        );
    }
}
