//! Static provider metadata. Endpoints and tier translations share the existing route catalogue.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use super::{ApiSurface, SurfaceEndpoint};
use crate::agent::capabilities::{EffortControlKind, HarnessCapabilities};

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

pub const REVISION: u32 = 1;

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
    pub supports_extra_args: bool,
}

#[tauri::command]
pub fn get_launch_targets() -> Result<Vec<LaunchTarget>, String> {
    let prefs = super::load()?;
    let catalogue = provider_catalogue();
    Ok(super::harness_profiles().into_iter().flat_map(|harness| {
        let caps = crate::models::Provider::from_db_str(&harness.harness).adapter().capabilities();
        let native = LaunchTarget {
            id: harness.id.clone(), harness_id: harness.id.clone(), harness_name: harness.name.clone(),
            provider_name: "Native authentication".into(), models: Vec::new(), efforts: allowed_efforts(&caps, None),
            manual_model: true, supports_model: caps.supports_model_override, supports_extra_args: caps.supports_extra_args,
        };
        let mut targets = vec![native.clone()];
        for route in prefs.provider_pairings.iter().filter(|p| p.harness_id == harness.id) {
            if super::surface_for_executor(crate::models::Provider::from_db_str(&harness.harness)) != Some(route.surface) { continue; }
            let entry = catalogue.iter().find(|p| p.id == route.provider_id);
            let models = entry.map(|p| p.models.iter().filter(|m| m.surface == route.surface).cloned()
                .map(|mut m| { m.efforts = Some(allowed_efforts(&caps, Some(&m))); m }).collect()).unwrap_or_default();
            targets.push(LaunchTarget {
                id: format!("{}:{}", harness.id, route.provider_id),
                provider_name: prefs.provider_accounts.iter().find(|a| a.id == route.provider_id).map_or_else(|| route.provider_id.clone(), |a| a.name.clone()),
                efforts: if entry.is_some_and(|p| !p.manual_model) { Vec::new() } else { native.efforts.clone() },
                models, manual_model: entry.is_none_or(|p| p.manual_model), ..native.clone()
            });
        }
        targets
    }).collect())
}

pub fn provider_catalogue() -> Vec<ProviderCatalogueEntry> {
    super::keyed_first_class_catalog().into_iter().map(|account| {
        let endpoints = super::first_class_surfaces(&account.id);
        let mut models = Vec::new();
        for endpoint in &endpoints {
            let tiers = &endpoint.model_tiers;
            for id in [&tiers.default, &tiers.opus, &tiers.fable, &tiers.sonnet, &tiers.haiku, &tiers.small_fast].into_iter().flatten() {
                if models.iter().any(|model: &CatalogueModel| model.id == *id && model.surface == endpoint.surface) { continue; }
                models.push(CatalogueModel {
                    id: id.clone(), name: id.replace('-', " "), surface: endpoint.surface,
                    // The published proxy contracts do not advertise an effort vocabulary.
                    efforts: Some(Vec::new()),
                });
            }
        }
        ProviderCatalogueEntry { id: account.id, name: account.name, manual_model: models.is_empty(), endpoints, models }
    }).collect()
}

pub fn allowed_efforts(caps: &HarnessCapabilities, model: Option<&CatalogueModel>) -> Vec<String> {
    let allowed = match &caps.effort_control {
        EffortControlKind::None => return Vec::new(),
        EffortControlKind::Closed { allowed } | EffortControlKind::InlineConfig { allowed, .. } => allowed,
    };
    allowed.iter().filter(|effort| model.and_then(|m| m.efforts.as_ref()).is_none_or(|values| values.contains(effort))).cloned().collect()
}

#[tauri::command]
pub fn get_provider_catalogue() -> Vec<ProviderCatalogueEntry> { provider_catalogue() }
