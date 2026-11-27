//! Static provider metadata. Endpoints and tier translations share the existing route catalogue.

use super::{ApiSurface, SurfaceEndpoint};
use crate::agent::capabilities::{EffortControlKind, HarnessCapabilities};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[ts(export, export_to = "CatalogueModel.ts")]
pub struct CatalogueModel {
    pub id: String,
    pub name: String,
    pub surface: ApiSurface,
    /// None defers to the harness vocabulary; an empty list means no effort control.
    pub efforts: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "ProviderCatalogueEntry.ts")]
pub struct ProviderCatalogueEntry {
    pub id: String,
    pub name: String,
    pub endpoints: Vec<SurfaceEndpoint>,
    pub models: Vec<CatalogueModel>,
    pub manual_model: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "LaunchTarget.ts")]
pub struct LaunchTarget {
    pub id: String,
    pub harness_id: String,
    pub harness_name: String,
    pub provider_name: String,
    pub models: Vec<CatalogueModel>,
    pub efforts: Vec<String>,
    pub manual_model: bool,
    pub supports_model: bool,
    #[serde(default)]
    pub supports_model_effort_discovery: bool,
    pub supports_extra_args: bool,
    #[serde(default)]
    pub supports_agent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub route: Option<super::ProviderPairing>,
    #[serde(default)]
    pub route_attached: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub default_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub verification_required: Option<bool>,
}

#[tauri::command]
pub fn get_launch_targets() -> Result<Vec<LaunchTarget>, String> {
    let prefs = super::load()?;
    Ok(targets_for(&prefs, super::harness_profiles()))
}

fn catalog_login_for(
    prefs: &super::AppPreferences,
    harness_adapter_id: &str,
    login_id: Option<&str>,
    env_type: crate::models::EnvType,
) -> Result<Option<super::harness_logins::HarnessLoginRef>, String> {
    let login = super::harness_logins::resolve_login(prefs, harness_adapter_id, login_id)?;
    if let Some(login) = &login {
        if let Some(refusal) = super::harness_logins::runtime_refusal(login, env_type) {
            return Err(refusal);
        }
    }
    Ok(login)
}

pub fn harness_model_catalog_blocking(
    harness_id: &str,
    login_id: Option<&str>,
) -> Result<crate::agent::model_catalog::HarnessModelCatalog, String> {
    let prefs = super::load()?;
    let profiles = super::harness_profiles();
    let spawn_option = crate::agent::provider::SpawnOptionId::from(harness_id);
    let detection = crate::agent::detection::detection_snapshot();
    if profiles.iter().any(|profile| {
        profile.id == spawn_option.harness_id() && !detection.permits_versioned_profile(profile)
    }) {
        return Err("This harness installation has not passed its CLI version check".into());
    }
    let profile = super::resolver::harness::resolved_harness_profile_from(&profiles, harness_id);
    let executor_id = profile
        .as_ref()
        .map(|profile| profile.harness.as_str())
        .unwrap_or_else(|| spawn_option.harness_id());
    let provider = crate::models::Provider::try_from_db_str(executor_id)
        .ok_or_else(|| "Unknown harness".to_string())?;
    let runtime = profile.as_ref().and_then(|profile| profile.runtime);
    let env_type = runtime.unwrap_or_else(crate::agent::model_catalog::host_env_type);
    let login = catalog_login_for(&prefs, executor_id, login_id, env_type)?;
    crate::agent::model_catalog::discover(
        provider.adapter(),
        crate::agent::model_catalog::ModelCatalogTarget {
            executable: profile
                .as_ref()
                .and_then(|profile| profile.executable.as_deref()),
            runtime,
            wsl_distro: profile
                .as_ref()
                .and_then(|profile| profile.wsl_distro.as_deref()),
            login_home: login.as_ref().map(|login| login.home.as_str()),
        },
    )
}

#[tauri::command]
pub async fn get_harness_model_catalog(
    harness_id: String,
    login_id: Option<String>,
) -> Result<crate::agent::model_catalog::HarnessModelCatalog, String> {
    crate::commands::run_blocking("get_harness_model_catalog", move || {
        harness_model_catalog_blocking(&harness_id, login_id.as_deref())
    })
    .await
}

pub fn targets_for(
    prefs: &super::AppPreferences,
    profiles: Vec<super::HarnessProfile>,
) -> Vec<LaunchTarget> {
    let catalogue = provider_catalogue();
    let detection = crate::agent::detection::detection_snapshot();
    profiles
        .into_iter()
        .flat_map(|harness| {
            let caps = crate::agent::capabilities::capabilities_for(
                crate::models::Provider::from_db_str(&harness.harness).adapter(),
            );
            let native = LaunchTarget {
                id: harness.id.clone(),
                harness_id: harness.id.clone(),
                harness_name: harness.name.clone(),
                provider_name: "Native authentication".into(),
                models: Vec::new(),
                efforts: allowed_efforts(&caps, None),
                manual_model: true,
                supports_model: caps.supports_model_override,
                supports_model_effort_discovery: caps.supports_model_effort_discovery
                    && detection.permits_versioned_profile(&harness),
                supports_extra_args: caps.supports_extra_args,
                supports_agent: caps.supports_agent,
                route: None,
                route_attached: false,
                default_model: None,
                verification_required: None,
            };
            let mut targets = vec![native.clone()];
            let executor = crate::models::Provider::from_db_str(&harness.harness);
            let surface = super::surface_for_executor(executor);
            for account in &prefs.provider_accounts {
                let stored = prefs
                    .provider_pairings
                    .iter()
                    .find(|p| p.harness_id == harness.id && p.provider_id == account.id);
                if stored.is_none()
                    && (!account.enabled
                        || account
                            .api_key
                            .as_deref()
                            .is_none_or(|key| key.trim().is_empty()))
                {
                    continue;
                }
                let Some(surface) = stored
                    .filter(|_| executor == crate::models::Provider::Cline)
                    .map(|p| p.surface)
                    .or(surface)
                else {
                    continue;
                };
                if !super::provider_surfaces(account).contains(&surface) {
                    continue;
                }
                let route = stored.cloned().unwrap_or_else(|| {
                    let endpoint = super::first_class_surfaces(&account.id)
                        .into_iter()
                        .find(|e| e.surface == surface);
                    super::ProviderPairing {
                        harness_id: harness.id.clone(),
                        provider_id: account.id.clone(),
                        surface,
                        base_url: endpoint.as_ref().map(|e| e.base_url.clone()),
                        model_tiers: endpoint.map(|e| e.model_tiers).unwrap_or_default(),
                    }
                });
                if route.surface != surface {
                    continue;
                }
                let entry = catalogue.iter().find(|p| p.id == route.provider_id);
                let models = entry
                    .map(|p| {
                        p.models
                            .iter()
                            .filter(|m| m.surface == route.surface)
                            .cloned()
                            .map(|mut m| {
                                m.efforts = Some(allowed_efforts(&caps, Some(&m)));
                                m
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                targets.push(LaunchTarget {
                    id: format!("{}:{}", harness.id, route.provider_id),
                    provider_name: prefs
                        .provider_accounts
                        .iter()
                        .find(|a| a.id == route.provider_id)
                        .map_or_else(|| route.provider_id.clone(), |a| a.name.clone()),
                    efforts: if entry.is_some_and(|p| !p.manual_model) {
                        Vec::new()
                    } else {
                        native.efforts.clone()
                    },
                    default_model: route.model_tiers.default.clone(),
                    route: Some(route),
                    route_attached: stored.is_some(),
                    verification_required: (executor == crate::models::Provider::Codex)
                        .then_some(true),
                    models,
                    manual_model: entry.is_none_or(|p| p.manual_model),
                    ..native.clone()
                });
            }
            targets
        })
        .collect()
}

pub fn provider_catalogue() -> Vec<ProviderCatalogueEntry> {
    super::keyed_first_class_catalog()
        .into_iter()
        .map(|account| {
            let endpoints = super::first_class_surfaces(&account.id);
            let mut models = Vec::new();
            for endpoint in &endpoints {
                let tiers = &endpoint.model_tiers;
                for id in [
                    &tiers.default,
                    &tiers.opus,
                    &tiers.fable,
                    &tiers.sonnet,
                    &tiers.haiku,
                    &tiers.small_fast,
                ]
                .into_iter()
                .flatten()
                {
                    if models.iter().any(|model: &CatalogueModel| {
                        model.id == *id && model.surface == endpoint.surface
                    }) {
                        continue;
                    }
                    models.push(CatalogueModel {
                        id: id.clone(),
                        name: id.replace('-', " "),
                        surface: endpoint.surface,
                        // The published proxy contracts do not advertise an effort vocabulary.
                        efforts: Some(Vec::new()),
                    });
                }
            }
            if account.id == "minimax" {
                // Model availability is independent of the Claude alias map. Sources:
                // platform.minimax.io/docs/api-reference/text-anthropic-api and /docs/token-plan/codex.
                models = [
                    "MiniMax-M3.1-Flash[1m]",
                    "MiniMax-M3.1-Flash",
                    "MiniMax-M3[1m]",
                    "MiniMax-M3",
                    "MiniMax-M2.7",
                    "MiniMax-M2.7-highspeed",
                    "MiniMax-M2.5",
                    "MiniMax-M2.5-highspeed",
                    "MiniMax-M2.1",
                    "MiniMax-M2.1-highspeed",
                    "MiniMax-M2",
                ]
                .into_iter()
                .map(|id| CatalogueModel {
                    id: id.into(),
                    name: id.replace('-', " "),
                    surface: ApiSurface::Anthropic,
                    efforts: Some(Vec::new()),
                })
                .collect();
                // Responses-API models. Source: platform.minimax.io/docs/api-reference/responses-create.
                // M3.1 Flash always reasons (`none` returns HTTP 400); M3 reasons only when an
                // effort other than `none` is set. `allowed_efforts` intersects these with
                // Codex's own vocabulary, so `max` is dropped for Codex.
                models.push(CatalogueModel {
                    id: "MiniMax-M3.1-Flash".into(),
                    name: "MiniMax M3.1 Flash".into(),
                    surface: ApiSurface::OpenAI,
                    efforts: Some(
                        ["low", "medium", "high", "xhigh", "max"]
                            .map(String::from)
                            .to_vec(),
                    ),
                });
                models.push(CatalogueModel {
                    id: "MiniMax-M3".into(),
                    name: "MiniMax M3".into(),
                    surface: ApiSurface::OpenAI,
                    efforts: Some(vec!["none".into(), "high".into()]),
                });
            }
            ProviderCatalogueEntry {
                id: account.id,
                name: account.name,
                manual_model: models.is_empty(),
                endpoints,
                models,
            }
        })
        .collect()
}

pub fn allowed_efforts(caps: &HarnessCapabilities, model: Option<&CatalogueModel>) -> Vec<String> {
    let allowed = match &caps.effort_control {
        EffortControlKind::None => return Vec::new(),
        EffortControlKind::Closed { allowed } | EffortControlKind::InlineConfig { allowed, .. } => {
            allowed
        }
    };
    allowed
        .iter()
        .filter(|effort| {
            model
                .and_then(|m| m.efforts.as_ref())
                .is_none_or(|values| values.contains(effort))
        })
        .cloned()
        .collect()
}

#[tauri::command]
pub fn get_provider_catalogue() -> Vec<ProviderCatalogueEntry> {
    provider_catalogue()
}

#[cfg(test)]
#[path = "launch_catalog_tests.rs"]
mod tests;
