//! Launch identity, catalogue and resolution shared by interactive and background callers.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{AppPreferences, HarnessConfigValue, HarnessProfile, ProviderPairing};
use crate::agent::capabilities::{capabilities_for, resolve_agent_config, AgentConfigInputs, FieldInputs};
use crate::agent::provider::SpawnOptionId;
use crate::models::Provider;

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[ts(export, export_to = "GeneratedLaunch.ts")]
pub struct GeneratedLaunch {
    pub source: String,
    pub catalogue_revision: u32,
    pub user_owned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub route: Option<ProviderPairing>,
}

/// Reconciliation is pure and runs inside preference writes. It never reads the cache recursively.
pub fn reconcile(prefs: &mut AppPreferences) {
    use super::spawn_configurations::SpawnConfiguration;
    let mut materialized_routes = Vec::new();
    let mut profiles = super::default_harness_profiles();
    for profile in &prefs.harness_profiles {
        if let Some(existing) = profiles.iter_mut().find(|h| h.id == profile.id) { *existing = profile.clone(); }
        else { profiles.push(profile.clone()); }
    }
    for profile in &profiles {
        let surface = super::surface_for_executor(Provider::from_db_str(&profile.harness));
        for account in prefs.provider_accounts.iter().filter(|a| a.enabled) {
            let source = format!("{}:{}", profile.id, account.id);
            if prefs.spawn_configurations.iter().any(|c| c.generated.as_ref().is_some_and(|g| g.source == source))
                || prefs.deleted_launch_configurations.contains(&format!("launch/{source}")) {
                continue;
            }
            if let Some(endpoint) = super::first_class_surfaces(&account.id).into_iter().find(|e| Some(e.surface) == surface) {
                if !prefs.provider_pairings.iter().any(|p| p.harness_id == profile.id && p.provider_id == account.id) {
                    materialized_routes.push(source);
                    prefs.provider_pairings.push(ProviderPairing {
                        harness_id: profile.id.clone(), provider_id: account.id.clone(), surface: endpoint.surface,
                        base_url: Some(endpoint.base_url), model_tiers: endpoint.model_tiers,
                    });
                }
            }
        }
    }
    let candidates = profiles.iter().map(|h| (h.id.clone(), h.name.clone(), None, None))
        .chain(prefs.provider_pairings.iter().map(|route| {
            let name = prefs.provider_accounts.iter().find(|a| a.id == route.provider_id)
                .map_or_else(|| route.provider_id.clone(), |a| a.name.clone());
            (format!("{}:{}", route.harness_id, route.provider_id), name, route.model_tiers.default.clone(), Some(route.clone()))
        })).collect::<Vec<_>>();
    for (source, name, model, route) in candidates {
        let id = format!("launch/{source}");
        if prefs.deleted_launch_configurations.contains(&id) { continue; }
        if let Some(existing) = prefs.spawn_configurations.iter_mut().find(|c| c.id == id) {
            if existing.generated.as_ref().is_some_and(|g| !g.user_owned && g.catalogue_revision < super::launch_catalog::REVISION) {
                existing.name = name;
                existing.model = model;
                // Only routes still equal to their generated baseline may be refreshed.
                // Advanced endpoint/tier edits remain authoritative.
                if let Some(route) = route.as_ref().filter(|r| existing.generated.as_ref().and_then(|g| g.route.as_ref()) == Some(*r)) {
                    if let Some(endpoint) = super::first_class_surfaces(&route.provider_id).into_iter().find(|e| e.surface == route.surface) {
                        let updated = ProviderPairing { base_url: Some(endpoint.base_url), model_tiers: endpoint.model_tiers, ..route.clone() };
                        existing.model = updated.model_tiers.default.clone();
                        if let Some(stored) = prefs.provider_pairings.iter_mut().find(|p| p.harness_id == route.harness_id && p.provider_id == route.provider_id) {
                            *stored = updated.clone();
                        }
                        existing.generated.as_mut().unwrap().route = Some(updated);
                    }
                }
                existing.generated.as_mut().unwrap().catalogue_revision = super::launch_catalog::REVISION;
            }
        } else {
            let route = route.filter(|_| materialized_routes.contains(&source));
            prefs.spawn_configurations.push(SpawnConfiguration {
                id, name, spawn_option_id: source.clone(), model,
                generated: Some(GeneratedLaunch { source, catalogue_revision: super::launch_catalog::REVISION, user_owned: false, route }),
                ..Default::default()
            });
        }
    }
    for value in &mut prefs.spawn_configurations { let _ = normalize_identity(value); }
    // Keep legacy identities as read aliases, while all future preference writes carry configuration IDs.
    let configurations = &prefs.spawn_configurations;
    for selection in [&mut prefs.default_provider, &mut prefs.reviewer_provider, &mut prefs.naming_provider] {
        if let Some(value) = selection.as_mut() {
            if configurations.iter().any(|c| c.id == *value) { continue; }
            let canonical = format!("launch/{value}");
            if configurations.iter().any(|c| c.id == canonical) { *value = canonical; }
        }
    }
}

pub fn normalize_identity(value: &mut super::spawn_configurations::SpawnConfiguration) -> Result<(), String> {
    let legacy = SpawnOptionId::from(value.spawn_option_id.as_str());
    let harness = value.harness_id.clone().unwrap_or_else(|| legacy.harness_id.clone());
    let route = value.provider_route_id.clone().or_else(|| legacy.is_proxied().then(|| value.spawn_option_id.clone()));
    if route.as_ref().is_some_and(|r| !SpawnOptionId::from(r.as_str()).is_proxied()) {
        return Err("Provider Route must identify both a harness and a provider".into());
    }
    if route.as_ref().is_some_and(|r| SpawnOptionId::from(r.as_str()).harness_id != harness) {
        return Err("Provider Route belongs to a different harness".into());
    }
    value.spawn_option_id = route.clone().unwrap_or_else(|| harness.clone());
    value.harness_id = Some(harness);
    value.provider_route_id = route;
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS, PartialEq, Eq)]
#[ts(export, export_to = "LaunchOverrides.ts")]
pub struct LaunchOverrides {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub extra_args: Option<String>,
}

/// Immutable, secret-free launch facts. Credentials are read from the account at launch.
#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[ts(export, export_to = "ResolvedLaunchPlan.ts")]
pub struct ResolvedLaunchPlan {
    pub configuration_id: String,
    pub configuration_name: String,
    pub spawn_option_id: String,
    pub harness: HarnessProfile,
    pub route: Option<ProviderPairing>,
    #[serde(default)]
    pub verification: Option<super::PairingVerification>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub extra_args: Option<String>,
}

pub fn resolve(
    prefs: &AppPreferences,
    selection: &str,
    overrides: &LaunchOverrides,
    mesh: &HarnessConfigValue,
) -> Result<ResolvedLaunchPlan, String> {
    resolve_plan(prefs, selection, overrides, mesh, true)
}

pub fn resolve_for_edit(prefs: &AppPreferences, selection: &str) -> Result<ResolvedLaunchPlan, String> {
    resolve_plan(prefs, selection, &Default::default(), &Default::default(), false)
}

pub fn capture_legacy(prefs: &AppPreferences, selection: &str, mesh: &HarnessConfigValue) -> Result<ResolvedLaunchPlan, String> {
    resolve_plan(prefs, selection, &Default::default(), mesh, false)
}

fn resolve_plan(
    prefs: &AppPreferences,
    selection: &str,
    overrides: &LaunchOverrides,
    mesh: &HarnessConfigValue,
    require_available: bool,
) -> Result<ResolvedLaunchPlan, String> {
    let configuration = prefs.spawn_configurations.iter().find(|c| c.id == selection);
    if configuration.is_none() && selection.starts_with("launch/") {
        return Err("Launch Configuration no longer exists; select another configuration".into());
    }
    let option = configuration.map_or(selection, |c| c.spawn_option_id.as_str());
    let id = SpawnOptionId::from(option);
    let harness = prefs.harness_profiles.iter().find(|h| h.id == id.harness_id())
        .cloned().or_else(|| {
            crate::agent::provider::BUILTIN_HARNESS_IDS.contains(&id.harness_id()).then(|| HarnessProfile {
                id: id.harness_id.clone(), name: id.harness_id.clone(),
                harness: if id.harness_id == "claude" { "anthropic".into() } else { id.harness_id.clone() },
                runtime: None, wsl_distro: None, executable: None,
            })
        }).ok_or_else(|| "Harness is missing; reinstall it or select another configuration".to_string())?;
    if require_available && (crate::agent::detection::currently_installed_profiles(vec![harness.clone()]).is_empty()
        || crate::agent::provider_menu::provider_info_for(&harness, crate::agent::provider::Platform::current()).is_none())
    {
        return Err("Harness is unavailable; install it for the selected runtime".into());
    }
    let caps = capabilities_for(Provider::from_db_str(&harness.harness).adapter());
    let app = prefs.harness_defaults.get(&harness.id);
    let configured_model = configuration.and_then(|c| c.model.as_deref());
    let configured_effort = configuration.and_then(|c| c.effort.as_deref());
    let nonblank = |value: Option<&str>| value.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let model = nonblank(overrides.model.as_deref()).or_else(|| nonblank(configured_model));
    let effort = nonblank(overrides.effort.as_deref()).or_else(|| nonblank(configured_effort));
    let extra = nonblank(overrides.extra_args.as_deref()).or_else(|| configuration.and_then(|c| nonblank(c.extra_args.as_deref())));
    let mut config = resolve_agent_config(&caps, AgentConfigInputs {
        model: FieldInputs { explicit: model.as_deref(), mesh_override: mesh.model.as_deref(), mesh: None, application: app.and_then(|c| c.model.as_deref()) },
        effort: FieldInputs { explicit: effort.as_deref(), mesh_override: mesh.effort.as_deref(), mesh: None, application: app.and_then(|c| c.effort.as_deref()) },
    }, extra.as_deref());
    let route = if let Some(provider_id) = id.provider_id() {
        let account = prefs.provider_accounts.iter().find(|a| a.id == provider_id)
            .ok_or_else(|| format!("Provider account '{provider_id}' is missing; restore its credential"))?;
        if require_available && !account.enabled { return Err(format!("Provider '{}' is disabled; enable it in Providers", account.name)); }
        if require_available && account.api_key.as_deref().is_none_or(|key| key.trim().is_empty()) {
            return Err(format!("Provider '{}' has no credential; add its key in Providers", account.name));
        }
        let executor = Provider::from_db_str(&harness.harness);
        let mut route = prefs.provider_pairings.iter().find(|p| p.harness_id == harness.id && p.provider_id == provider_id)
            .cloned();
        // Cline's native auth flow can consume an existing Claude- or
        // Codex-attached pairing for the same account. Preserve that legacy
        // surface fallback when resolving a Launch Configuration; otherwise
        // the common resolver fails before launch_routing can emit Cline's
        // consumer-specific environment.
        if route.is_none() && executor == Provider::Cline {
            route = prefs.provider_accounts.iter().find(|a| a.id == provider_id)
                .and_then(|account| super::compatibility::resolve_pairing("cline", account, &prefs.provider_pairings));
        }
        let mut route = route.ok_or_else(|| "Provider Route is missing; restore it in Launch Configurations".to_string())?;
        let supports_surface = executor == Provider::Cline
            || super::surface_for_executor(executor) == Some(route.surface);
        if !supports_surface {
            return Err("Provider Route uses an API surface this harness does not support".into());
        }
        let stored_default = route.model_tiers.default.clone();
        if let Some(model) = config.model.as_ref() { route.model_tiers.default = Some(model.clone()); }
        else { config.model = route.model_tiers.default.clone(); }
        let catalogue = super::launch_catalog::provider_catalogue();
        let entry = catalogue.iter().find(|p| p.id == provider_id);
        let model = entry.and_then(|p| p.models.iter().find(|m| m.surface == route.surface && Some(&m.id) == config.model.as_ref()));
        if let Some(entry) = entry {
            if !entry.manual_model && model.is_none() {
                // Advanced route models remain an escape hatch for newly published endpoints.
                if config.model != stored_default {
                    return Err("Model is not in this provider's catalogue; configure a custom model on the advanced route".into());
                }
            }
        }
        let allowed = super::launch_catalog::allowed_efforts(&caps, model);
        if config.effort.as_ref().is_some_and(|e| !allowed.contains(e)) {
            if effort.is_some() { return Err("Effort is not supported by this provider/model and harness".into()); }
            config.effort = None;
        }
        let decision = super::pairing_compatibility(&route);
        if !decision.compatible { return Err(decision.reason.unwrap_or_else(|| "Provider Route is incompatible".into())); }
        super::compatibility::preflight_pairing_env(Some(&route), provider_id)?;
        Some(route)
    } else { None };
    let verification = route.as_ref().and_then(|route| prefs.pairing_verifications.iter().find(|v|
        v.harness_id == route.harness_id && v.provider_id == route.provider_id
            && Some(v.endpoint.as_str()) == route.base_url.as_deref()
            && Some(v.model_id.as_str()) == route.model_tiers.default.as_deref()
            && v.status == super::PairingVerificationStatus::Verified).cloned());
    if require_available
        && Provider::from_db_str(&harness.harness) != Provider::Cline
        && route.as_ref().is_some_and(|r| r.surface == super::ApiSurface::OpenAI)
        && verification.is_none()
    {
        return Err("Provider Route is unverified or stale for this model; verify it in advanced routes".into());
    }
    Ok(ResolvedLaunchPlan {
        configuration_id: configuration.map_or_else(|| format!("launch/{selection}"), |c| c.id.clone()),
        configuration_name: configuration.map_or_else(|| harness.name.clone(), |c| c.name.clone()),
        spawn_option_id: option.into(), harness, route, verification,
        model: config.model, effort: config.effort, extra_args: config.extra_args,
    })
}

/// Resolve an identity at legacy adapter entrypoints without teaching the parser about storage.
pub fn selection_option(selection: &str) -> Result<String, String> {
    let prefs = match super::load() {
        Ok(prefs) => prefs,
        Err(error) if error == "preferences module not initialized" => {
            // Legacy bare ids remain usable by pure/test callers before Tauri
            // startup wires the preference store. A Launch Configuration id,
            // however, has no safe legacy interpretation and must stay a hard
            // error so deleted configurations never silently fall through.
            if selection.starts_with("launch/") {
                return Err("Launch Configuration no longer exists; select another configuration".into());
            }
            return Ok(selection.into());
        }
        Err(error) => return Err(error),
    };
    if let Some(value) = prefs.spawn_configurations.iter().find(|c| c.id == selection) {
        return Ok(value.spawn_option_id.clone());
    }
    if selection.starts_with("launch/") {
        return Err("Launch Configuration no longer exists; select another configuration".into());
    }
    Ok(selection.into())
}

pub fn snapshot(plan: ResolvedLaunchPlan) -> super::spawn_configurations::SpawnConfiguration {
    let mut value = super::spawn_configurations::SpawnConfiguration {
        id: plan.configuration_id.clone(), name: plan.configuration_name.clone(), spawn_option_id: plan.spawn_option_id.clone(),
        model: plan.model.clone(), effort: plan.effort.clone(), extra_args: plan.extra_args.clone(),
        resolved: Some(plan), generated: None,
        ..Default::default()
    };
    let _ = normalize_identity(&mut value);
    value
}

pub fn resolve_snapshot(plan: &ResolvedLaunchPlan, overrides: &LaunchOverrides) -> Result<ResolvedLaunchPlan, String> {
    let prefs = AppPreferences {
        harness_profiles: vec![plan.harness.clone()],
        provider_accounts: super::provider_accounts(),
        provider_pairings: plan.route.iter().cloned().collect(),
        pairing_verifications: plan.verification.iter().cloned().collect(),
        spawn_configurations: vec![snapshot(plan.clone())],
        ..Default::default()
    };
    resolve(&prefs, &plan.configuration_id, overrides, &Default::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_runtime_is_rejected_at_the_resolver_not_only_in_the_menu() {
        let mut prefs = generated_preferences();
        prefs.harness_profiles[0].runtime = Some(if cfg!(windows) { crate::models::EnvType::WindowsInterop } else { crate::models::EnvType::Wsl });
        reconcile(&mut prefs);
        assert!(resolve(&prefs, "launch/claude", &Default::default(), &Default::default()).unwrap_err().contains("unavailable"));
        assert!(resolve_for_edit(&prefs, "launch/claude").is_ok());
    }

    fn generated_preferences() -> AppPreferences {
        serde_json::from_value(serde_json::json!({
            "harness_profiles": [{"id":"claude","name":"Claude Code","harness":"anthropic"}],
            "provider_accounts": [{"id":"minimax","name":"MiniMax","enabled":true,"billing_mode":"pay_as_you_go","api_key":"private-key"}]
        })).unwrap()
    }

    #[test]
    fn reconciliation_materializes_routes_once_and_preserves_deleted_or_edited_choices() {
        let mut prefs = generated_preferences();
        reconcile(&mut prefs);
        assert_eq!(prefs.provider_pairings.len(), 1);
        let generated = prefs.spawn_configurations.iter().find(|c| c.id == "launch/claude:minimax").unwrap();
        assert_eq!(generated.model.as_deref(), Some("MiniMax-M3[1m]"));
        let once = prefs.clone();
        reconcile(&mut prefs);
        assert_eq!(prefs, once);
        let generated = prefs.spawn_configurations.iter_mut().find(|c| c.id == "launch/claude:minimax").unwrap();
        generated.name = "My review".into();
        generated.generated.as_mut().unwrap().catalogue_revision = 0;
        generated.generated.as_mut().unwrap().user_owned = true;
        reconcile(&mut prefs);
        assert_eq!(prefs.spawn_configurations.iter().find(|c| c.id == "launch/claude:minimax").unwrap().name, "My review");
        prefs.spawn_configurations.retain(|c| c.id != "launch/claude:minimax");
        prefs.deleted_launch_configurations.push("launch/claude:minimax".into());
        reconcile(&mut prefs);
        assert!(!prefs.spawn_configurations.iter().any(|c| c.id == "launch/claude:minimax"));
    }

    #[test]
    fn migrated_custom_route_is_not_owned_by_catalogue_refresh() {
        let mut prefs = generated_preferences();
        reconcile(&mut prefs);
        prefs.spawn_configurations.clear();
        prefs.provider_pairings[0].base_url = Some("https://custom.example/anthropic".into());
        prefs.provider_pairings[0].model_tiers.default = Some("custom-model".into());
        let route = prefs.provider_pairings[0].clone();
        reconcile(&mut prefs);
        let config = prefs.spawn_configurations.iter_mut().find(|c| c.id == "launch/claude:minimax").unwrap();
        assert!(config.generated.as_ref().unwrap().route.is_none());
        config.generated.as_mut().unwrap().catalogue_revision = 0;
        reconcile(&mut prefs);
        assert_eq!(prefs.provider_pairings[0], route);
        assert_eq!(prefs.spawn_configurations.iter().find(|c| c.id == "launch/claude:minimax").unwrap().model.as_deref(), Some("custom-model"));
    }

    #[test]
    fn generated_refresh_and_new_harness_preserve_user_configuration_ids() {
        let mut prefs = generated_preferences();
        prefs.default_provider = Some("custom-choice".into());
        prefs.spawn_configurations.push(serde_json::from_value(serde_json::json!({
            "id":"custom-choice","name":"Personal","spawn_option_id":"claude","model":"sonnet","effort":"high","extra_args":null
        })).unwrap());
        reconcile(&mut prefs);
        let generated = prefs.spawn_configurations.iter_mut().find(|c| c.id == "launch/claude:minimax").unwrap();
        generated.name = "Obsolete catalogue label".into();
        generated.generated.as_mut().unwrap().catalogue_revision = 0;
        generated.model = Some("retired-model".into());
        generated.generated.as_mut().unwrap().route.as_mut().unwrap().model_tiers.default = Some("retired-model".into());
        prefs.provider_pairings[0].model_tiers.default = Some("retired-model".into());
        prefs.harness_profiles.push(HarnessProfile { id: "codex".into(), name: "Codex".into(), harness: "codex".into(), runtime: None, wsl_distro: None, executable: None });
        reconcile(&mut prefs);
        assert_eq!(prefs.default_provider.as_deref(), Some("custom-choice"));
        assert_eq!(prefs.spawn_configurations.iter().find(|c| c.id == "launch/claude:minimax").unwrap().name, "MiniMax");
        assert_eq!(prefs.spawn_configurations.iter().find(|c| c.id == "launch/claude:minimax").unwrap().model.as_deref(), Some("MiniMax-M3[1m]"));
        assert_eq!(prefs.provider_pairings[0].model_tiers.default.as_deref(), Some("MiniMax-M3[1m]"));
        assert!(prefs.spawn_configurations.iter().any(|c| c.id == "launch/codex:minimax"));
        assert_eq!(prefs.provider_accounts[0].api_key.as_deref(), Some("private-key"));
    }

    #[test]
    fn resolver_reports_missing_dependencies_without_native_fallback() {
        let mut prefs = generated_preferences();
        reconcile(&mut prefs);
        let resolve_current = |prefs: &AppPreferences| resolve(prefs, "launch/claude:minimax", &Default::default(), &Default::default());
        assert!(resolve_current(&prefs).is_ok());
        prefs.provider_accounts[0].api_key = None;
        assert!(resolve_current(&prefs).unwrap_err().contains("credential"));
        prefs.provider_accounts[0].api_key = Some("rotated".into());
        prefs.provider_pairings.clear();
        assert!(resolve_current(&prefs).unwrap_err().contains("Route is missing"));
        prefs.spawn_configurations.clear();
        assert!(resolve_current(&prefs).unwrap_err().contains("no longer exists"));
    }

    #[test]
    fn explicit_overrides_win_and_terminal_masks_unsupported_fields() {
        let mut prefs = generated_preferences();
        reconcile(&mut prefs);
        prefs.harness_defaults.insert("claude".into(), HarnessConfigValue { model: Some("app".into()), effort: Some("low".into()) });
        let mesh = HarnessConfigValue { model: Some("mesh".into()), effort: Some("medium".into()) };
        let plan = resolve(&prefs, "launch/claude", &Default::default(), &mesh).unwrap();
        assert_eq!(plan.model.as_deref(), Some("mesh"));
        assert_eq!(plan.effort.as_deref(), Some("medium"));
        let overrides = LaunchOverrides { model: Some("explicit".into()), effort: Some("high".into()), extra_args: Some("--verbose".into()) };
        let plan = resolve(&prefs, "launch/claude", &overrides, &mesh).unwrap();
        assert_eq!(plan.model.as_deref(), Some("explicit"));
        assert_eq!(plan.extra_args.as_deref(), Some("--verbose"));
        let plan = resolve(&prefs, "launch/terminal", &overrides, &mesh).unwrap();
        assert_eq!((plan.model, plan.effort, plan.extra_args), (None, None, None));
    }

    #[test]
    fn snapshot_survives_deleted_recipe_and_route_but_uses_current_account() {
        let dir = tempfile::tempdir().unwrap();
        super::super::init_for_tests(dir.path().into());
        let mut prefs = generated_preferences();
        reconcile(&mut prefs);
        let plan = resolve(&prefs, "launch/claude:minimax", &Default::default(), &Default::default()).unwrap();
        prefs.harness_profiles.clear();
        prefs.provider_pairings.clear();
        prefs.spawn_configurations.clear();
        prefs.deleted_launch_configurations.push(plan.configuration_id.clone());
        prefs.provider_accounts[0].api_key = Some("rotated".into());
        super::super::save(prefs.clone()).unwrap();
        let resumed = resolve_snapshot(&plan, &Default::default()).unwrap();
        assert_eq!(resumed, plan);
        prefs.provider_accounts.clear();
        super::super::save(prefs).unwrap();
        assert!(resolve_snapshot(&plan, &Default::default()).unwrap_err().contains("account"));
        super::super::reset_for_tests();
    }

    #[test]
    fn proxied_configuration_snapshots_route_without_credentials() {
        let prefs: AppPreferences = serde_json::from_value(serde_json::json!({
            "provider_accounts": [{"id":"custom", "name":"Private", "enabled":true, "billing_mode":"pay_as_you_go", "api_key":"secret-test-key"}],
            "provider_pairings": [{"harness_id":"claude", "provider_id":"custom", "surface":"anthropic", "base_url":"https://example.test/anthropic", "model_tiers":{"default":"route-model"}}],
            "spawn_configurations": [{"id":"launch/private", "name":"Private", "spawn_option_id":"claude:custom", "model":"chosen-model", "effort":"high", "extra_args":null}]
        })).unwrap();
        let plan = resolve(&prefs, "launch/private", &LaunchOverrides::default(), &HarnessConfigValue::default()).unwrap();
        assert_eq!(plan.route.as_ref().unwrap().base_url.as_deref(), Some("https://example.test/anthropic"));
        assert_eq!(plan.route.as_ref().unwrap().model_tiers.default.as_deref(), Some("chosen-model"));
        assert!(!serde_json::to_string(&plan).unwrap().contains("secret-test-key"));
        let mut disabled = prefs.clone();
        disabled.provider_accounts[0].enabled = false;
        assert!(resolve(&disabled, "launch/private", &LaunchOverrides::default(), &HarnessConfigValue::default()).unwrap_err().contains("disabled"));
    }

    #[test]
    fn native_configuration_resolves_selected_values_before_defaults() {
        let prefs: super::super::AppPreferences = serde_json::from_value(serde_json::json!({
            "spawn_configurations": [{"id":"launch/fast", "name":"Fast", "spawn_option_id":"codex", "model":"gpt-5.6-sol", "effort":"high", "extra_args":null}]
        })).unwrap();
        let plan = resolve(&prefs, "launch/fast", &LaunchOverrides::default(),
            &super::super::HarnessConfigValue { model: Some("mesh-model".into()), effort: None }).unwrap();
        assert_eq!(plan.configuration_id, "launch/fast");
        assert_eq!(plan.harness.harness, "codex");
        assert_eq!(plan.model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(plan.effort.as_deref(), Some("high"));
        assert!(plan.route.is_none());
    }
}
