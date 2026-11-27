use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use ts_rs::TS;

use crate::agent::provider::{AgentProvider, Platform, SpawnRecipe};
use crate::models::EnvType;

const CATALOG_CACHE_TTL: Duration = Duration::from_secs(300);
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "ModelEffortStorage.ts")]
pub enum ModelEffortStorage {
    Separate,
    EncodedInModel,
    Undiscovered,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[ts(export, export_to = "HarnessCatalogEffort.ts")]
pub struct HarnessCatalogEffort {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[ts(export, export_to = "HarnessCatalogModel.ts")]
pub struct HarnessCatalogModel {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    pub efforts: Vec<HarnessCatalogEffort>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[ts(export, export_to = "HarnessModelCatalog.ts")]
pub struct HarnessModelCatalog {
    pub harness_id: String,
    pub effort_storage: ModelEffortStorage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub allow_unlisted_models: Option<bool>,
    pub models: Vec<HarnessCatalogModel>,
}

pub struct ModelCatalogProbe {
    pub binary: Option<&'static str>,
    pub args: Vec<String>,
    pub stdin: Option<&'static str>,
    pub effort_storage: ModelEffortStorage,
    pub allow_unlisted_models: bool,
    pub login_home_env: Option<&'static str>,
    pub env_remove: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ModelCatalogTarget<'a> {
    pub executable: Option<&'a std::path::Path>,
    pub runtime: Option<crate::models::EnvType>,
    pub wsl_distro: Option<&'a str>,
    pub login_home: Option<&'a str>,
}

struct CacheEntry {
    checked: Instant,
    catalog: HarnessModelCatalog,
}

pub(super) fn models_without_effort<'a>(
    rows: impl Iterator<Item = (&'a str, Option<&'a str>)>,
) -> Vec<HarnessCatalogModel> {
    let mut seen = std::collections::HashSet::new();
    rows.filter(|(id, _)| {
        id.starts_with(|c: char| c.is_ascii_alphanumeric())
            && id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-._:/".contains(&c))
            && seen.insert(*id)
    })
    .map(|(id, description)| HarnessCatalogModel {
        id: id.into(),
        name: id.into(),
        description: description.map(str::to_owned),
        efforts: Vec::new(),
    })
    .collect()
}

static CACHE: LazyLock<Mutex<HashMap<String, CacheEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn discover(
    adapter: &'static dyn AgentProvider,
    target: ModelCatalogTarget<'_>,
) -> Result<HarnessModelCatalog, String> {
    discover_with_cache_ttl(adapter, target, CATALOG_CACHE_TTL)
}

fn discover_with_cache_ttl(
    adapter: &'static dyn AgentProvider,
    target: ModelCatalogTarget<'_>,
    cache_ttl: Duration,
) -> Result<HarnessModelCatalog, String> {
    let Some(probe) = adapter.model_catalog_probe() else {
        return Err("Harness does not support model and effort discovery".into());
    };
    let env_type = target.runtime.unwrap_or_else(host_env_type);
    let key = format!(
        "{}:{}:{}:{}:{}",
        adapter.id(),
        env_type,
        target.wsl_distro.unwrap_or_default(),
        target
            .executable
            .map(|p| p.to_string_lossy())
            .unwrap_or_default(),
        target.login_home.unwrap_or_default()
    );
    if let Some(entry) = CACHE.lock().unwrap().get(&key) {
        if entry.checked.elapsed() < cache_ttl {
            return Ok(entry.catalog.clone());
        }
    }
    let catalog = probe_uncached(adapter, target, env_type, probe)?;
    CACHE.lock().unwrap().insert(
        key,
        CacheEntry {
            checked: Instant::now(),
            catalog: catalog.clone(),
        },
    );
    Ok(catalog)
}

pub(crate) fn host_env_type() -> crate::models::EnvType {
    if cfg!(windows) {
        crate::models::EnvType::Windows
    } else {
        crate::models::EnvType::Wsl
    }
}

fn probe_uncached(
    adapter: &'static dyn AgentProvider,
    target: ModelCatalogTarget<'_>,
    env_type: EnvType,
    probe: ModelCatalogProbe,
) -> Result<HarnessModelCatalog, String> {
    let mut recipe: SpawnRecipe = adapter.spawn_recipe(
        target
            .runtime
            .map_or_else(Platform::current, Platform::for_runtime),
        env_type,
    );
    if let Some(binary) = probe.binary {
        recipe.binary = binary;
    }
    recipe.base_args = probe.args;
    recipe.trailing_args = Vec::new();
    let host_resolved = if env_type == host_env_type() {
        target
            .executable
            .map(std::path::Path::to_path_buf)
            .or_else(|| crate::agent::detection::resolve_spawn_binary(recipe.binary))
    } else {
        target.executable.map(std::path::Path::to_path_buf)
    };
    let mut command = crate::agent::spawn_environment::background_command_for_runtime(
        &recipe,
        host_resolved.as_deref(),
        env_type,
        target.wsl_distro,
    )?;
    command.current_dir(std::env::temp_dir());
    if let (Some(variable), Some(home)) = (probe.login_home_env, target.login_home) {
        command.env(variable, home);
    }
    for key in probe.env_remove {
        command.env_remove(key);
    }
    command.env_remove("FORCE_COLOR");
    command.env("NO_COLOR", "1");
    let output = crate::agent::provider::adapters::opencode_version::run_private_command(
        command,
        "harness model catalog",
        PROBE_TIMEOUT,
        probe.stdin,
    )?;
    if !output.status.success() {
        return Err(format!(
            "Harness model catalog failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let models = adapter.parse_model_catalog(&stdout)?;
    if models.is_empty() {
        return Err("Harness model catalog returned no models".into());
    }
    Ok(HarnessModelCatalog {
        harness_id: adapter.id().into(),
        effort_storage: probe.effort_storage,
        allow_unlisted_models: probe.allow_unlisted_models.then_some(true),
        models,
    })
}

#[cfg(test)]
#[path = "model_catalog_tests.rs"]
mod tests;
