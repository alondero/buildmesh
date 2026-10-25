//! Native and WSL config-file access and Codex home resolution.

use super::*;
pub(super) const WSL_FILE_END_MARKER: &str = "__BUILDMESH_CODEX_FILE_END__";

pub(super) const WSL_READ_FILES_SCRIPT: &str = r#"set -eu
for p in "$@"; do
  if [ -f "$p" ]; then base64 "$p"; fi
  printf '%s\n' '__BUILDMESH_CODEX_FILE_END__'
done
"#;

pub(super) const WSL_ATOMIC_WRITE_FILES_SCRIPT: &str = r#"set -eu
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

pub(super) fn wsl_command_with_values(
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

pub(super) fn wsl_read_files(distro: &str, paths: &[&Path]) -> Result<Vec<String>, String> {
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

pub(super) fn wsl_write_files(distro: &str, files: &[(&Path, &str)]) -> Result<(), String> {
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

pub(super) fn read_runtime_files(
    paths: &[&Path],
    distro: Option<&str>,
) -> Result<Vec<String>, String> {
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

pub(super) fn write_runtime_files(
    files: &[(&Path, &str)],
    distro: Option<&str>,
) -> Result<(), String> {
    match distro {
        // A WSL guest's files are written through the guest CLI, so the host's
        // atomic rename cannot reach them. The native arm uses the shared
        // writer; the directory is created first because Codex expects
        // `.codex/` to exist before it will read a config from it.
        Some(distro) => wsl_write_files(distro, files),
        None => files.iter().try_for_each(|(path, content)| {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("failed to create Codex dir {}: {e}", parent.display()))?;
            }
            managed_config::atomic_write(path, content)
                .map_err(|e| format!("failed to write Codex file {}: {e}", path.display()))
        }),
    }
}

/// Resolve the Codex home used by the runtime that will execute a spawn.
/// Proxy preflight supplies the exact home/distro used by the child. Native
/// launches without a prepared install resolve the ordinary environment once.
/// WSL paths remain guest paths here so all WSL file operations use the guest
/// CLI rather than host-side UNC writes.
pub(super) fn runtime_codex_home(
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

pub(super) fn runtime_wsl_distro(
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
