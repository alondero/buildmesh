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

pub fn targets_for(prefs: &super::AppPreferences, profiles: Vec<super::HarnessProfile>) -> Vec<LaunchTarget> {
    let catalogue = provider_catalogue();
    profiles.into_iter().flat_map(|harness| {
        let caps = crate::models::Provider::from_db_str(&harness.harness).adapter().capabilities();
        let native = LaunchTarget {
            id: harness.id.clone(), harness_id: harness.id.clone(), harness_name: harness.name.clone(),
            provider_name: "Native authentication".into(), models: Vec::new(), efforts: allowed_efforts(&caps, None),
            manual_model: true, supports_model: caps.supports_model_override, supports_extra_args: caps.supports_extra_args,
            route: None, route_attached: false, default_model: None, verification_required: None,
        };
        let mut targets = vec![native.clone()];
        let executor = crate::models::Provider::from_db_str(&harness.harness);
        let surface = super::surface_for_executor(executor);
        for account in &prefs.provider_accounts {
            let stored = prefs.provider_pairings.iter().find(|p| p.harness_id == harness.id && p.provider_id == account.id);
            if stored.is_none() && (!account.enabled || account.api_key.as_deref().is_none_or(|key| key.trim().is_empty())) { continue; }
            let Some(surface) = stored.filter(|_| executor == crate::models::Provider::Cline).map(|p| p.surface).or(surface) else { continue; };
            if !super::provider_surfaces(account).contains(&surface) { continue; }
            let route = stored.cloned().unwrap_or_else(|| {
                let endpoint = super::first_class_surfaces(&account.id).into_iter().find(|e| e.surface == surface);
                super::ProviderPairing {
                    harness_id: harness.id.clone(), provider_id: account.id.clone(), surface,
                    base_url: endpoint.as_ref().map(|e| e.base_url.clone()),
                    model_tiers: endpoint.map(|e| e.model_tiers).unwrap_or_default(),
                }
            });
            if route.surface != surface { continue; }
            let entry = catalogue.iter().find(|p| p.id == route.provider_id);
            let models = entry.map(|p| p.models.iter().filter(|m| m.surface == route.surface).cloned()
                .map(|mut m| { m.efforts = Some(allowed_efforts(&caps, Some(&m))); m }).collect()).unwrap_or_default();
            targets.push(LaunchTarget {
                id: format!("{}:{}", harness.id, route.provider_id),
                provider_name: prefs.provider_accounts.iter().find(|a| a.id == route.provider_id).map_or_else(|| route.provider_id.clone(), |a| a.name.clone()),
                efforts: if entry.is_some_and(|p| !p.manual_model) { Vec::new() } else { native.efforts.clone() },
                default_model: route.model_tiers.default.clone(), route: Some(route), route_attached: stored.is_some(),
                verification_required: (executor == crate::models::Provider::Codex).then_some(true),
                models, manual_model: entry.is_none_or(|p| p.manual_model), ..native.clone()
            });
        }
        targets
    }).collect()
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
        if account.id == "minimax" {
            // Model availability is independent of the Claude alias map. Sources:
            // platform.minimax.io/docs/api-reference/text-anthropic-api and /docs/token-plan/codex.
            models = ["MiniMax-M3[1m]", "MiniMax-M3", "MiniMax-M2.7", "MiniMax-M2.7-highspeed",
                "MiniMax-M2.5", "MiniMax-M2.5-highspeed", "MiniMax-M2.1", "MiniMax-M2.1-highspeed", "MiniMax-M2"]
                .into_iter().map(|id| CatalogueModel {
                    id: id.into(), name: id.replace('-', " "), surface: ApiSurface::Anthropic,
                    efforts: Some(Vec::new()),
                }).collect();
            models.push(CatalogueModel {
                id: "MiniMax-M3".into(), name: "MiniMax M3".into(), surface: ApiSurface::OpenAI,
                efforts: Some(vec!["none".into(), "high".into()]),
            });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_targets_offer_unattached_keyed_providers_without_mutating_routes() {
        let prefs: super::super::AppPreferences = serde_json::from_value(serde_json::json!({
            "provider_accounts": [
                {"id":"minimax","name":"MiniMax","enabled":true,"api_key":"test","billing_mode":"pay_as_you_go"},
                {"id":"custom","name":"Custom","enabled":true,"api_key":"test","claude_compatible":true,"billing_mode":"pay_as_you_go"},
                {"id":"kimi","name":"Kimi","enabled":true,"billing_mode":"pay_as_you_go"}
            ],
            "detached_provider_routes": ["claude:minimax", "codex:minimax"]
        })).unwrap();
        let profiles = ["claude", "codex"].map(|id| super::super::HarnessProfile {
            id: id.into(), name: id.into(), harness: if id == "claude" { "anthropic".into() } else { id.into() },
            runtime: None, wsl_distro: None, executable: None,
        });
        let targets = targets_for(&prefs, profiles.to_vec());
        let claude = targets.iter().find(|t| t.id == "claude:minimax").unwrap();
        assert!(!claude.route_attached);
        assert_eq!(claude.default_model.as_deref(), Some("MiniMax-M3[1m]"));
        assert!(claude.models.iter().any(|m| m.id == "MiniMax-M2.7-highspeed"));
        let codex = targets.iter().find(|t| t.id == "codex:minimax").unwrap();
        assert_eq!(codex.models.len(), 1);
        assert_eq!(codex.models[0].efforts.as_ref().unwrap(), &["none", "high"]);
        assert!(targets.iter().any(|t| t.id == "codex:custom" && t.route.as_ref().unwrap().base_url.is_none()));
        assert!(!targets.iter().any(|t| t.id.ends_with(":kimi")));
        assert!(prefs.provider_pairings.is_empty());
    }

    #[test]
    fn stored_cline_openai_route_remains_editable_without_codex_verification() {
        let prefs = serde_json::from_value(serde_json::json!({
            "provider_accounts":[{"id":"minimax","name":"MiniMax","enabled":true,"api_key":"test","billing_mode":"pay_as_you_go"}],
            "provider_pairings":[{"harness_id":"cline","provider_id":"minimax","surface":"openai","base_url":"https://api.minimax.io/v1","model_tiers":{"default":"MiniMax-M3"}}]
        })).unwrap();
        let targets = targets_for(&prefs, vec![super::super::HarnessProfile {
            id:"cline".into(), name:"Cline".into(), harness:"cline".into(), runtime:None, wsl_distro:None, executable:None,
        }]);
        let target = targets.iter().find(|t| t.id == "cline:minimax").unwrap();
        assert!(target.route_attached);
        assert_eq!(target.route.as_ref().unwrap().surface, ApiSurface::OpenAI);
        assert_eq!(target.verification_required, None);
    }
}
