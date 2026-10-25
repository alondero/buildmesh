//! Codex project trust configuration; preserve existing user configuration.

use super::*;
pub(super) fn codex_trust_config_path(
    env_type: EnvType,
    runtime: &LaunchRuntime,
) -> Result<(PathBuf, Option<String>), String> {
    let (home, distro) = runtime_codex_home(env_type, runtime)?;
    Ok((home.join("config.toml"), distro))
}

pub(super) fn trust_project_path(resolved: &ResolvedPath) -> String {
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
pub(super) fn ensure_codex_project_trusted(
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
pub(super) fn ensure_project_trust_content(
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

pub(super) fn project_keys_match(existing: &str, candidate: &str, env_type: EnvType) -> bool {
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
