//! Proxy profile naming, rendering, and atomic native/WSL materialisation.

use super::*;
pub(super) static PROFILE_WRITE_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

pub fn stable_profile_name(harness_id: &str, provider_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(harness_id.as_bytes());
    digest.update([0]);
    digest.update(provider_id.as_bytes());
    format!("buildmesh_{}", &hex::encode(digest.finalize())[..16])
}

pub(super) fn toml_string(value: &str) -> String {
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

pub(super) fn native_codex_home_from(
    codex_home: Option<std::ffi::OsString>,
    user_home: Option<std::ffi::OsString>,
) -> Result<std::path::PathBuf, String> {
    codex_home
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| user_home.map(|p| std::path::PathBuf::from(p).join(".codex")))
        .ok_or_else(|| "could not resolve the runtime Codex home".to_string())
}

pub(super) fn native_codex_home() -> Result<std::path::PathBuf, String> {
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

pub(super) fn is_owned_legacy_profile(path: &Path, content: &str) -> bool {
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

pub(super) fn cleanup_owned_legacy_profiles(home: &Path) {
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

pub(super) fn materialize_native_profile_at(
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

pub(super) const WSL_PROFILE_SCRIPT: &str = r#"set -eu
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
pub(super) const WSL_CODEX_HOME_SCRIPT: &str =
    "printf '__BUILDMESH_WSL_CODEX_HOME__%s\\n' \"${CODEX_HOME:-$HOME/.codex}\"";

pub(super) fn materialize_wsl_profile(
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
