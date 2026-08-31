use crate::agent::capabilities::{EffortControlKind, CODEX_EFFORT_ALLOWED, CODEX_EFFORT_KEY};
use crate::agent::provider::{
    AgentProvider, LaunchRuntime, Platform, SpawnRecipe, UiMeta, WindowsShell,
};
use crate::env::ResolvedPath;
use crate::models::EnvType;
use base64::Engine;
use once_cell::sync::Lazy;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use toml_edit::{value, DocumentMut, Item, Table};

pub struct CodexAdapter;
pub static CODEX: CodexAdapter = CodexAdapter;

fn shell_for(platform: Platform) -> WindowsShell {
    match platform {
        Platform::Macos | Platform::Linux => WindowsShell::Direct,
        Platform::Windows => WindowsShell::PowerShell,
    }
}

fn base_flags() -> Vec<String> {
    vec![
        "--ask-for-approval".into(),
        "never".into(),
        "--sandbox".into(),
        "danger-full-access".into(),
        // Buildmesh owns the terminal surface and persists its scrollback.
        // Codex's inline mode keeps completed output in that scrollback instead
        // of confining it to the alternate screen buffer (issue #1089). This
        // flag is TUI-only; it must not be carried into a future `codex exec`
        // recipe because that subcommand rejects it.
        "--no-alt-screen".into(),
        // Run the project-local `.codex/hooks.json` hooks without Codex's
        // interactive hook-review prompt (issue #884) — a headless spawn
        // must never block on a trust prompt. The adapter also provisions the
        // runtime's project trust entry before launch.
        "--dangerously-bypass-hook-trust".into(),
    ]
}

/// The callback command Codex hooks run. Codex pipes the hook's stdin JSON
/// (`{hook_event_name, session_id, transcript_path, …}` — issue #884) into the
/// command; `--data-binary @-` forwards it as the POST body so
/// `http/routes/attention.rs` can classify the event. The port/session env
/// vars are set per-agent by `spawn_environment` and inherited by the hook
/// process; Codex executes the command string itself (no implicit login
/// shell), so each platform wraps in the shell that expands its own env-var
/// syntax. A short connect/total timeout keeps a broken local server from
/// hanging the agent, while `-S` and the non-zero exit status expose curl,
/// HTTP, and shell failures to Codex.
fn hook_command(platform: Platform) -> String {
    match platform {
        Platform::Windows => {
            "cmd.exe /c \"curl -fsS --connect-timeout 2 --max-time 10 -X POST --data-binary @- http://localhost:%BUILDMESH_PORT%/api/attention/%BUILDMESH_SESSION_ID%\""
                .to_string()
        }
        _ => {
            "sh -c \"curl -fsS --connect-timeout 2 --max-time 10 -X POST --data-binary @- http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID\""
                .to_string()
        }
    }
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

pub fn runtime_identity(env_type: EnvType) -> &'static str {
    match env_type {
        EnvType::Wsl => "wsl",
        EnvType::Windows if cfg!(target_os = "windows") => "native-windows",
        EnvType::Windows if cfg!(target_os = "macos") => "native-macos",
        EnvType::Windows => "native-linux",
    }
}

fn wsl_runtime_identity(distro: &str, codex_home: &str) -> String {
    format!("wsl:{distro}:{codex_home}")
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
    let user_home_key = if cfg!(target_os = "windows") { "USERPROFILE" } else { "HOME" };
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
    if hash.len() != 16 || !hash.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)) {
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
                tracing::warn!("failed to remove owned legacy Codex profile {:?}: {error}", path);
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
    temp.persist(&target)
        .map_err(|e| format!("failed to atomically replace {}: {}", target.display(), e.error))?;
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
const WSL_CODEX_HOME_SCRIPT: &str = "printf %s \"${CODEX_HOME:-$HOME/.codex}\"";

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
        Err(format!("WSL Codex profile materialization exited with {status}"))
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
    let mut command = wsl_command_with_values(distro, WSL_READ_FILES_SCRIPT, &[], &path_args);
    let output = command
        .output()
        .map_err(|e| format!("failed to read WSL Codex files: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "WSL Codex file read exited with {}",
            output.status
        ));
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
    let mut command = wsl_command_with_values(
        distro,
        WSL_ATOMIC_WRITE_FILES_SCRIPT,
        &[],
        &arg_refs,
    );
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
        Err(format!(
            "WSL Codex file write exited with {status}"
        ))
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
                Err(error) => Err(format!("failed to read Codex file {}: {error}", path.display())),
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
    command.args([
        "-d",
        &distro,
        "--exec",
        "sh",
        "-c",
        WSL_CODEX_HOME_SCRIPT,
    ]);
    if std::env::var_os("CODEX_HOME").is_some() {
        let mut wslenv = std::env::var("WSLENV").unwrap_or_default();
        crate::agent::spawn_environment::append_to_wslenv(&mut wslenv, "CODEX_HOME", "/u");
        command.env("WSLENV", wslenv);
    }
    let output = command
        .output()
        .map_err(|e| format!("failed to resolve WSL Codex home for trust: {e}"))?;
    let home = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || home.is_empty() {
        return Err("WSL Codex home identity is unavailable for trust".to_string());
    }
    Ok((PathBuf::from(home), Some(distro)))
}

fn runtime_wsl_distro(
    env_type: EnvType,
    runtime: &LaunchRuntime,
) -> Result<Option<String>, String> {
    if env_type == EnvType::Windows {
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
    let path = if resolved.env_type == EnvType::Wsl {
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
        .map_err(|error| format!("failed to read Codex trust config {}: {error}", config_path.display()))?
        .into_iter()
        .next()
        .expect("one Codex trust file was requested");
    let updated = ensure_project_trust_content(&existing, &project_path, resolved.env_type)?;
    if updated == existing {
        return Ok(());
    }
    write_runtime_files(&[(&config_path, &updated)], distro.as_deref())
        .map_err(|e| format!("failed to write Codex trust config {}: {e}", config_path.display()))
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
    if env_type == EnvType::Windows {
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
        EnvType::Windows => {
            materialize_native_profile_at(Path::new(&install.codex_home), profile_name, &content)
        }
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

fn codex_output(
    env_type: EnvType,
    wsl_distro: Option<&str>,
    args: &[&str],
) -> Result<std::process::Output, String> {
    let mut command = if env_type == EnvType::Wsl {
        let mut command = crate::process_util::command_no_window("wsl.exe");
        command.args([
            "-d",
            wsl_distro.ok_or_else(|| "WSL distribution identity is unavailable".to_string())?,
            "--exec",
            "codex",
        ]);
        if std::env::var_os("CODEX_HOME").is_some() {
            let mut wslenv = std::env::var("WSLENV").unwrap_or_default();
            crate::agent::spawn_environment::append_to_wslenv(&mut wslenv, "CODEX_HOME", "/u");
            command.env("WSLENV", wslenv);
        }
        command
    } else if cfg!(target_os = "windows") {
        // npm installs Codex as a `.cmd` shim. `std::process::Command` cannot
        // execute batch files directly on Windows, so capability probes use
        // the same non-interactive cmd relay as other shim-backed providers.
        let mut command = crate::process_util::command_no_window("cmd.exe");
        command.args(["/d", "/c", "codex"]);
        command
    } else {
        crate::process_util::command_no_window("codex")
    };
    command
        .args(args)
        .output()
        .map_err(|e| format!("Codex executable is unavailable: {e}"))
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
    let wsl_distro = if env_type == EnvType::Wsl {
        Some(
            crate::env::detect_default_wsl_distro()
                .ok_or_else(|| "default WSL distribution is unavailable".to_string())?,
        )
    } else {
        None
    };
    let output = codex_output(env_type, wsl_distro.as_deref(), &["--version"])?;
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
    let executable = if env_type == EnvType::Wsl {
        let out = crate::process_util::command_no_window("wsl.exe")
            .args([
                "-d",
                wsl_distro.as_deref().expect("WSL distribution was resolved"),
                "--exec",
                "sh",
                "-c",
                "command -v codex",
            ])
            .output()
            .map_err(|e| format!("failed to locate WSL Codex executable: {e}"))?;
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    } else {
        let locator = if cfg!(target_os = "windows") {
            "where.exe"
        } else {
            "which"
        };
        let out = crate::process_util::command_no_window(locator)
            .arg("codex")
            .output()
            .map_err(|e| format!("failed to locate Codex executable: {e}"))?;
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
            candidates.lines().next().unwrap_or_default().trim().to_string()
        }
    };
    if executable.is_empty() {
        return Err("Codex executable identity is unavailable".into());
    }
    let codex_home = if let Some(distro) = wsl_distro.as_deref() {
        let mut command = crate::process_util::command_no_window("wsl.exe");
        command.args([
            "-d",
            distro,
            "--exec",
            "sh",
            "-c",
            WSL_CODEX_HOME_SCRIPT,
        ]);
        if std::env::var_os("CODEX_HOME").is_some() {
            let mut wslenv = std::env::var("WSLENV").unwrap_or_default();
            crate::agent::spawn_environment::append_to_wslenv(&mut wslenv, "CODEX_HOME", "/u");
            command.env("WSLENV", wslenv);
        }
        let output = command
            .output()
            .map_err(|e| format!("failed to resolve WSL Codex home: {e}"))?;
        let home = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !output.status.success() || home.is_empty() {
            return Err("WSL Codex home identity is unavailable".into());
        }
        home
    } else {
        native_codex_home()?.to_string_lossy().into_owned()
    };
    let runtime = if let Some(distro) = wsl_distro.as_deref() {
        wsl_runtime_identity(distro, &codex_home)
    } else {
        runtime_identity(env_type).to_string()
    };
    let capability_key = format!(
        "{}\0{}\0{}",
        runtime, executable, version
    );
    let capabilities_are_cached = CLI_CAPABILITY_CACHE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(&capability_key);
    if !capabilities_are_cached {
        let fresh_help = successful_help(env_type, wsl_distro.as_deref(), &["--help"], "fresh")?;
        let resume_help = successful_help(
            env_type,
            wsl_distro.as_deref(),
            &["resume", "--help"],
            "resume",
        )?;
        validate_proxy_cli_help(&fresh_help, &resume_help)?;
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

/// Ensure `<project>/.codex/hooks.json` carries the Stop + PermissionRequest
/// attention webhooks. Codex's matcher/event schema nests hook entries one
/// level deeper than Claude Code's (each event maps to matcher groups, each
/// carrying a `hooks` array — issue #884). Idempotent, and preserves any
/// unrelated top-level keys the user added.
/// Return updated hooks JSON, or `None` when the existing document already
/// contains the current Buildmesh handlers. The caller owns the runtime-aware
/// read/write so WSL can batch guest file operations.
fn ensure_hooks_json_content(
    existing: &str,
    command: &str,
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

    let hook = serde_json::json!({
        "type": "command",
        "command": command,
        "statusMessage": BUILDMESH_HOOK_STATUS_MESSAGE,
    });
    let mut changed = false;
    for event in ["Stop", "PermissionRequest"] {
        let groups = events
            .entry(event)
            .or_insert_with(|| serde_json::json!([]));
        let Some(groups) = groups.as_array_mut() else {
            return Err(format!("hooks.json event '{event}' must be an array"));
        };

        let mut found = false;
        for group in groups.iter_mut() {
            let Some(handlers) = group.get_mut("hooks").and_then(|v| v.as_array_mut()) else {
                continue;
            };
            if let Some(index) = handlers.iter().position(is_buildmesh_hook_handler) {
                if handlers[index] != hook {
                    handlers[index] = hook.clone();
                    changed = true;
                }
                found = true;
                break;
            }
        }
        if !found {
            groups.push(serde_json::json!({ "hooks": [hook.clone()] }));
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
    temp.persist(path)
        .map(|_| ())
        .map_err(|error| error.error)
}

fn ensure_codex_project_files(
    resolved: &ResolvedPath,
    runtime: &LaunchRuntime,
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

    // Read and validate both files before writing either one. A malformed
    // hooks.json must not leave a half-applied project configuration behind.
    let config_path = project_dir.join("config.toml");
    let hooks_path = project_dir.join("hooks.json");
    let existing = read_runtime_files(&[&config_path, &hooks_path], distro.as_deref())?;
    let config_existing = &existing[0];
    let config_updated = ensure_hooks_feature_content(config_existing)?;
    let hooks_existing = &existing[1];
    let platform = if resolved.env_type == EnvType::Wsl {
        Platform::Linux
    } else {
        Platform::current()
    };
    let hooks_updated = ensure_hooks_json_content(hooks_existing, &hook_command(platform))?;

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
            windows_shell: shell_for(platform),
        }
    }

    fn spawn_recipe_for_resume(
        &self,
        platform: Platform,
        session_id: &str,
    ) -> Option<SpawnRecipe> {
        let mut args = vec!["resume".into(), session_id.into()];
        args.extend(base_flags());
        Some(SpawnRecipe {
            binary: "codex",
            base_args: args,
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
    ) -> Result<(), String> {
        ensure_codex_project_files(resolved, runtime)
    }

    fn wsl_passthrough_env(&self) -> &'static [&'static str] {
        &["CODEX_HOME"]
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
    fn after_fresh_spawn(&self, node_id: i64, spawn_path: &str, env_type: EnvType) {
        crate::services::codex_session::start_capture_poller(
            node_id,
            spawn_path.to_string(),
            env_type,
        );
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
        vec![
            "-c".into(),
            format!("model_reasoning_effort={effort:?}"),
        ]
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
        let default = native_codex_home_from(
            None,
            Some(std::ffi::OsString::from("/home/user")),
        )
        .unwrap();
        assert_eq!(default, std::path::PathBuf::from("/home/user/.codex"));
        assert!(native_codex_home_from(None, None).is_err());
    }

    #[test]
    fn wsl_codex_home_uses_default_and_propagates_explicit_override_once() {
        assert!(WSL_CODEX_HOME_SCRIPT.contains("${CODEX_HOME:-$HOME/.codex}"));
        let mut empty = String::new();
        crate::agent::spawn_environment::append_to_wslenv(&mut empty, "CODEX_HOME", "/u");
        assert_eq!(empty, "CODEX_HOME/u");
        let mut existing = "SSH_AUTH_SOCK/up".to_string();
        crate::agent::spawn_environment::append_to_wslenv(
            &mut existing,
            "CODEX_HOME",
            "/u",
        );
        assert_eq!(existing, "SSH_AUTH_SOCK/up:CODEX_HOME/u");
        let mut already_present = "CODEX_HOME/u:SSH_AUTH_SOCK/up".to_string();
        crate::agent::spawn_environment::append_to_wslenv(
            &mut already_present,
            "CODEX_HOME",
            "/u",
        );
        assert_eq!(already_present, "CODEX_HOME/u:SSH_AUTH_SOCK/up");
        assert_ne!(
            wsl_runtime_identity("Ubuntu", "/home/user/.codex"),
            wsl_runtime_identity("Debian", "/home/user/.codex")
        );
        assert_ne!(
            wsl_runtime_identity("Ubuntu", "/home/user/.codex"),
            wsl_runtime_identity("Ubuntu", "/custom/codex")
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires a local WSL distribution"]
    fn wsl_profile_materializes_in_default_and_explicit_codex_home() {
        let distro = crate::env::detect_default_wsl_distro().expect("WSL distribution");
        let default_home = crate::process_util::command_no_window("wsl.exe")
            .args(["-d", &distro, "--exec", "sh", "-c", WSL_CODEX_HOME_SCRIPT])
            .env_remove("CODEX_HOME")
            .output()
            .unwrap();
        assert!(default_home.status.success());
        let default_home = String::from_utf8_lossy(&default_home.stdout).trim().to_string();
        assert!(!default_home.is_empty());
        let explicit_home = format!("/tmp/buildmesh-codex-profile-test-{}", std::process::id());

        for (index, home) in [default_home, explicit_home.clone()].into_iter().enumerate() {
            let profile = format!("buildmesh_wsl_contract_{}_{}", std::process::id(), index);
            let install = CodexInstall {
                executable: "/usr/bin/codex".into(),
                version: "test".into(),
                runtime_identity: wsl_runtime_identity(&distro, &home),
                codex_home: home.clone(),
                wsl_distro: Some(distro.clone()),
            };
            let expected = render_proxy_profile(&profile, "WSL contract", "https://example.invalid/v1");
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
                    "-d", &distro, "--exec", "sh", "-c", "cat \"$1/$2.config.toml\"",
                    "buildmesh-test", &home, &profile,
                ])
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
            let _ = crate::process_util::command_no_window("wsl.exe")
                .args([
                    "-d", &distro, "--exec", "sh", "-c",
                    "rm -f \"$1/$2.config.toml\"", "buildmesh-test", &home, &profile,
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
                    let Some(header_end) = text.find("\r\n\r\n") else { continue };
                    let length = text[..header_end]
                        .lines()
                        .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length: ").map(str::to_string))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if request.len() >= header_end + 4 + length { break; }
                }
                let request = String::from_utf8(request).unwrap();
                assert!(request.starts_with("POST /v1/responses HTTP/1.1"), "{request}");
                assert!(request.to_ascii_lowercase().contains("authorization: bearer pinned-secret"));
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
            "--profile", profile, "--model", "MiniMax-M3", "exec",
            "--skip-git-repo-check", "reply with verified",
        ]);
        assert!(fresh.status.success(), "{}", String::from_utf8_lossy(&fresh.stderr));
        let resume = run(&[
            "--profile", profile, "--model", "MiniMax-M3", "exec", "resume",
            "--last", "--skip-git-repo-check", "reply with verified again",
        ]);
        assert!(resume.status.success(), "{}", String::from_utf8_lossy(&resume.stderr));
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
        assert_eq!(std::fs::read_to_string(user_config).unwrap(), "model = \"user-choice\"\n");
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
        assert!(fresh.base_args.contains(&bypass), "fresh: {:?}", fresh.base_args);
        let resume = CODEX
            .spawn_recipe_for_resume(Platform::Windows, "sid-123")
            .expect("codex has a resume recipe");
        assert!(resume.base_args.contains(&bypass), "resume: {:?}", resume.base_args);
    }

    #[test]
    fn spawn_recipes_preserve_buildmesh_terminal_scrollback() {
        let inline = "--no-alt-screen".to_string();
        let fresh = CODEX.spawn_recipe(Platform::Linux, EnvType::Wsl);
        assert!(fresh.base_args.contains(&inline), "fresh: {:?}", fresh.base_args);
        let resume = CODEX
            .spawn_recipe_for_resume(Platform::Linux, "sid-123")
            .expect("codex has a resume recipe");
        assert!(resume.base_args.contains(&inline), "resume: {:?}", resume.base_args);
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
    }

    fn read_hooks_json(project: &Path) -> serde_json::Value {
        let content = std::fs::read_to_string(project.join(".codex").join("hooks.json"))
            .expect("hooks.json not written");
        serde_json::from_str(&content).expect("hooks.json is not valid JSON")
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
            .provision_attention_hooks(&resolved, &LaunchRuntime::default())
            .unwrap();
    }

    /// Injection writes both files: the feature flag and the two webhooks in
    /// Codex's nested matcher/event schema, POSTing the hook's stdin to the
    /// attention endpoint.
    #[test]
    fn inject_writes_config_and_hooks() {
        let temp = TempDir::new().unwrap();
        provision_codex(temp.path());

        let config = std::fs::read_to_string(temp.path().join(".codex").join("config.toml"))
            .expect("config.toml not written");
        assert!(config.contains("[features]"), "config: {config}");
        assert!(config.contains("hooks = true"), "config: {config}");

        let hooks = read_hooks_json(temp.path());
        for event in ["Stop", "PermissionRequest"] {
            let command = hooks["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap_or_else(|| panic!("{event} hook missing: {hooks:#}"));
            assert!(
                command.contains("/api/attention/"),
                "{event} must POST to the attention endpoint: {command}"
            );
            assert!(
                command.contains("--data-binary @-"),
                "{event} must forward the hook stdin as the POST body: {command}"
            );
        }
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
        assert!(config.contains("model = \"gpt-5.2-codex\""), "config: {config}");
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
        assert!(config.contains("[features]\nhooks = true"), "config: {config}");
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
        assert_eq!(hooks["hooks"]["PermissionRequest"].as_array().unwrap().len(), 2);
        assert_eq!(hooks["hooks"]["Stop"][0]["hooks"][0]["command"], "user-stop");
        assert_eq!(
            hooks["hooks"]["PermissionRequest"][0]["hooks"][0]["command"],
            "user-permission"
        );
        assert!(hooks["hooks"]["Stop"].as_array().unwrap().iter().any(|group| {
            group["hooks"]
                .as_array()
                .unwrap()
                .iter()
                .any(is_buildmesh_hook_handler)
        }));
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
            &hook_command(Platform::Windows),
        )
        .unwrap_err();
        assert!(error.contains("parse hooks.json"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), "{not json");
    }

    #[test]
    fn config_feature_merge_replaces_false_without_duplicate_keys() {
        let existing = "model = \"gpt-5.2-codex\"\n\n[features]\nhooks = false\nweb_search = true\n";
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
        assert_eq!(projects.iter().count(), 1, "must not create a duplicate table");
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

    /// The Windows hook command must expand env vars with cmd syntax (`%VAR%`)
    /// under an explicit `cmd.exe /c`, and the Unix one with sh syntax —
    /// Codex executes the command string without a login shell of its own.
    #[test]
    fn hook_command_uses_platform_env_syntax() {
        let win = hook_command(Platform::Windows);
        assert!(win.starts_with("cmd.exe /c"), "win: {win}");
        assert!(win.contains("%BUILDMESH_PORT%"), "win: {win}");
        assert!(win.contains("%BUILDMESH_SESSION_ID%"), "win: {win}");
        assert!(win.contains("--connect-timeout 2 --max-time 10"), "win: {win}");
        assert!(!win.contains("|| true"), "win: {win}");
        for platform in [Platform::Macos, Platform::Linux] {
            let unix = hook_command(platform);
            assert!(unix.starts_with("sh -c"), "unix: {unix}");
            assert!(unix.contains("$BUILDMESH_PORT"), "unix: {unix}");
            assert!(unix.contains("$BUILDMESH_SESSION_ID"), "unix: {unix}");
            assert!(unix.contains("--connect-timeout 2 --max-time 10"), "unix: {unix}");
            assert!(!unix.contains("|| true"), "unix: {unix}");
        }
    }

}
