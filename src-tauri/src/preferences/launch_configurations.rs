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
///
/// Spawn-menu submenus list only what the user saved: reconciliation never
/// creates launch configurations. Catalogue-generated recipes from older
/// installs (where every harness shipped a `launch/<harness>` entry) are
/// purged unless the user edited them into user-owned recipes. Provider
/// routes for enabled accounts are still materialized so proxied harness
/// rows keep appearing without a manual attach.
/// Effective harness profiles for reconciliation: the code-defined defaults
/// with stored profiles merged over them by id. Shared by [`reconcile`] and
/// [`retired_configuration_aliases`] so the two never disagree on the
/// profile set.
fn effective_profiles(prefs: &AppPreferences) -> Vec<HarnessProfile> {
    let mut profiles = super::default_harness_profiles();
    for profile in &prefs.harness_profiles {
        if let Some(existing) = profiles.iter_mut().find(|h| h.id == profile.id) { *existing = profile.clone(); }
        else { profiles.push(profile.clone()); }
    }
    profiles
}

pub fn reconcile(prefs: &mut AppPreferences) {
    let profiles = effective_profiles(prefs);
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
                    prefs.provider_pairings.push(ProviderPairing {
                        harness_id: profile.id.clone(), provider_id: account.id.clone(), surface: endpoint.surface,
                        base_url: Some(endpoint.base_url), model_tiers: endpoint.model_tiers,
                    });
                }
            }
        }
    }
    // Drop catalogue-generated recipes the user never touched. User-owned
    // edits (saved through the editor, flagged `user_owned`) and
    // user-created recipes (`generated: None`) are the user's own and stay.
    let mut purged: Vec<(String, String)> = Vec::new();
    let mut detached: Vec<String> = Vec::new();
    prefs.spawn_configurations.retain(|c| {
        let catalogue_owned = c.generated.as_ref().is_some_and(|g| !g.user_owned);
        if catalogue_owned {
            // Upgrade window: a catalogue recipe whose route pairing is
            // already gone was detached under the old regime, where the
            // recipe itself was the marker blocking re-materialization.
            // Carry that detach decision into the tombstone list so the
            // route stays detached. Harness-only sources gate nothing.
            if let Some(source) = c.generated.as_ref().map(|g| g.source.clone()) {
                let id = SpawnOptionId::from(source.as_str());
                let tombstone = format!("launch/{source}");
                let paired = id.provider_id.as_deref().is_none_or(|provider| {
                    prefs.provider_pairings.iter()
                        .any(|p| p.harness_id == id.harness_id && p.provider_id == provider)
                });
                if id.is_proxied() && !paired
                    && !prefs.deleted_launch_configurations.iter().any(|d| d == &tombstone)
                {
                    detached.push(tombstone);
                }
            }
            purged.push((c.id.clone(), c.spawn_option_id.clone()));
            false
        } else {
            true
        }
    });
    prefs.deleted_launch_configurations.extend(detached);
    for value in &mut prefs.spawn_configurations { let _ = normalize_identity(value); }
    // Selections that pointed at a purged recipe fall back to its bare Spawn
    // Option (same harness/route, native defaults) so existing nodes and
    // app-wide defaults keep resolving. Other selections are left alone:
    // bare Spawn Option ids resolve directly, and surviving configuration
    // ids keep working.
    if !purged.is_empty() {
        for selection in [&mut prefs.default_provider, &mut prefs.reviewer_provider, &mut prefs.naming_provider] {
            if let Some(value) = selection.as_mut() {
                if let Some((_, option)) = purged.iter().find(|(id, _)| id == value) {
                    *value = option.clone();
                }
            }
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

/// Aliases that heal stored references to retired catalogue-generated
/// configurations (`launch/<harness>` / `launch/<harness>:<provider>`)
/// back to their bare Spawn Option after the purge in [`reconcile`].
/// Same harness/route, native defaults — so meshes and circuits that
/// pointed at a pre-made recipe keep resolving. Pure so the mapping is
/// the unit-test seam; the DB write lives in
/// `services::agent_node::migrate_launch_history`.
///
/// A retired id stays unmapped when its recipe still exists (a user-owned
/// edit keeps working as-is) or when the user explicitly deleted it
/// (those references must keep surfacing "no longer exists" rather than
/// silently rerouting to defaults).
pub(crate) fn retired_configuration_aliases(prefs: &AppPreferences) -> std::collections::HashMap<String, String> {
    let live: std::collections::HashSet<&str> = prefs.spawn_configurations.iter().map(|c| c.id.as_str()).collect();
    let deleted: std::collections::HashSet<&str> = prefs.deleted_launch_configurations.iter().map(String::as_str).collect();
    let mut aliases = std::collections::HashMap::new();
    for source in effective_profiles(prefs).into_iter().map(|p| p.id)
        .chain(prefs.provider_pairings.iter().map(|p| format!("{}:{}", p.harness_id, p.provider_id)))
    {
        let retired = format!("launch/{source}");
        if !live.contains(retired.as_str()) && !deleted.contains(retired.as_str()) {
            aliases.insert(retired, source);
        }
    }
    aliases
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
        assert!(resolve(&prefs, "claude", &Default::default(), &Default::default()).unwrap_err().contains("unavailable"));
        assert!(resolve_for_edit(&prefs, "claude").is_ok());
    }

    fn generated_preferences() -> AppPreferences {
        serde_json::from_value(serde_json::json!({
            "harness_profiles": [{"id":"claude","name":"Claude Code","harness":"anthropic"}],
            "provider_accounts": [{"id":"minimax","name":"MiniMax","enabled":true,"billing_mode":"pay_as_you_go","api_key":"private-key"}]
        })).unwrap()
    }

    #[test]
    fn reconciliation_materializes_routes_once_and_creates_no_configurations() {
        let mut prefs = generated_preferences();
        reconcile(&mut prefs);
        assert_eq!(prefs.provider_pairings.len(), 1);
        assert!(prefs.spawn_configurations.is_empty());
        let once = prefs.clone();
        reconcile(&mut prefs);
        assert_eq!(prefs, once);
        // A user-saved recipe survives reconciliation untouched.
        prefs.spawn_configurations.push(serde_json::from_value(serde_json::json!({
            "id": "launch/my-review", "name": "My review", "spawn_option_id": "claude",
            "model": null, "effort": null, "extra_args": null
        })).unwrap());
        reconcile(&mut prefs);
        assert_eq!(prefs.spawn_configurations.iter().find(|c| c.id == "launch/my-review").unwrap().name, "My review");
        // A deleted route id is not re-materialized.
        prefs.provider_pairings.clear();
        prefs.deleted_launch_configurations.push("launch/claude:minimax".into());
        reconcile(&mut prefs);
        assert!(prefs.provider_pairings.is_empty());
    }

    #[test]
    fn reconcile_leaves_custom_routes_untouched_and_creates_no_configurations() {
        let mut prefs = generated_preferences();
        reconcile(&mut prefs);
        prefs.provider_pairings[0].base_url = Some("https://custom.example/anthropic".into());
        prefs.provider_pairings[0].model_tiers.default = Some("custom-model".into());
        let route = prefs.provider_pairings[0].clone();
        reconcile(&mut prefs);
        assert_eq!(prefs.provider_pairings[0], route);
        assert!(prefs.spawn_configurations.is_empty());
    }

    #[test]
    fn new_harness_creates_no_configuration_and_preserves_user_ids() {
        let mut prefs = generated_preferences();
        prefs.default_provider = Some("custom-choice".into());
        prefs.spawn_configurations.push(serde_json::from_value(serde_json::json!({
            "id":"custom-choice","name":"Personal","spawn_option_id":"claude","model":"sonnet","effort":"high","extra_args":null
        })).unwrap());
        prefs.harness_profiles.push(HarnessProfile { id: "codex".into(), name: "Codex".into(), harness: "codex".into(), runtime: None, wsl_distro: None, executable: None });
        reconcile(&mut prefs);
        assert_eq!(prefs.default_provider.as_deref(), Some("custom-choice"));
        assert!(prefs.spawn_configurations.iter().any(|c| c.id == "custom-choice"));
        assert!(!prefs.spawn_configurations.iter().any(|c| c.id == "launch/codex:minimax"),
            "a new harness must not gain a pre-made configuration: {:?}",
            prefs.spawn_configurations.iter().map(|c| &c.id).collect::<Vec<_>>());
        assert_eq!(prefs.provider_accounts[0].api_key.as_deref(), Some("private-key"));
    }

    fn user_minimax_configuration() -> serde_json::Value {
        serde_json::json!({
            "id": "launch/my-minimax", "name": "My MiniMax", "spawn_option_id": "claude:minimax",
            "model": null, "effort": null, "extra_args": null
        })
    }

    #[test]
    fn resolver_reports_missing_dependencies_without_native_fallback() {
        let mut prefs = generated_preferences();
        reconcile(&mut prefs);
        prefs.spawn_configurations.push(serde_json::from_value(user_minimax_configuration()).unwrap());
        let resolve_current = |prefs: &AppPreferences| resolve(prefs, "launch/my-minimax", &Default::default(), &Default::default());
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
        let plan = resolve(&prefs, "claude", &Default::default(), &mesh).unwrap();
        assert_eq!(plan.model.as_deref(), Some("mesh"));
        assert_eq!(plan.effort.as_deref(), Some("medium"));
        let overrides = LaunchOverrides { model: Some("explicit".into()), effort: Some("high".into()), extra_args: Some("--verbose".into()) };
        let plan = resolve(&prefs, "claude", &overrides, &mesh).unwrap();
        assert_eq!(plan.model.as_deref(), Some("explicit"));
        assert_eq!(plan.extra_args.as_deref(), Some("--verbose"));
        let plan = resolve(&prefs, "terminal", &overrides, &mesh).unwrap();
        assert_eq!((plan.model, plan.effort, plan.extra_args), (None, None, None));
    }

    #[test]
    fn snapshot_survives_deleted_recipe_and_route_but_uses_current_account() {
        let dir = tempfile::tempdir().unwrap();
        super::super::init_for_tests(dir.path().into());
        let mut prefs = generated_preferences();
        reconcile(&mut prefs);
        prefs.spawn_configurations.push(serde_json::from_value(user_minimax_configuration()).unwrap());
        let plan = resolve(&prefs, "launch/my-minimax", &Default::default(), &Default::default()).unwrap();
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
    fn fresh_preferences_start_with_no_spawn_configurations() {
        // Spawn-menu submenus list only what the user saved. A fresh install
        // must not materialize one generated recipe per harness.
        let mut prefs = AppPreferences::default();
        reconcile(&mut prefs);
        assert!(prefs.spawn_configurations.is_empty(),
            "fresh prefs must not pre-create configurations, got {:?}", prefs.spawn_configurations);
        let mut known = generated_preferences();
        reconcile(&mut known);
        assert!(known.spawn_configurations.is_empty(),
            "a known provider must materialize its route, not a configuration: {:?}",
            known.spawn_configurations);
        assert_eq!(known.provider_pairings.len(), 1);
    }

    #[test]
    fn reconcile_purges_catalogue_generated_configurations_but_keeps_user_ones() {
        let mut prefs = generated_preferences();
        prefs.spawn_configurations.push(crate::preferences::spawn_configurations::SpawnConfiguration {
            id: "launch/claude".into(), name: "Claude Code".into(), spawn_option_id: "claude".into(),
            generated: Some(GeneratedLaunch { source: "claude".into(), catalogue_revision: 1, user_owned: false, route: None }),
            ..Default::default()
        });
        prefs.spawn_configurations.push(crate::preferences::spawn_configurations::SpawnConfiguration {
            id: "launch/my-review".into(), name: "My review".into(), spawn_option_id: "claude".into(),
            generated: Some(GeneratedLaunch { source: "claude".into(), catalogue_revision: 1, user_owned: true, route: None }),
            ..Default::default()
        });
        prefs.spawn_configurations.push(serde_json::from_value(serde_json::json!({
            "id": "launch/personal", "name": "Personal", "spawn_option_id": "claude",
            "model": null, "effort": null, "extra_args": null
        })).unwrap());
        reconcile(&mut prefs);
        let ids: Vec<_> = prefs.spawn_configurations.iter().map(|c| c.id.as_str()).collect();
        assert!(!ids.contains(&"launch/claude"), "catalogue-generated recipes must be purged: {ids:?}");
        assert!(ids.contains(&"launch/my-review"), "user-owned recipes must survive: {ids:?}");
        assert!(ids.contains(&"launch/personal"), "user-created recipes must survive: {ids:?}");
    }

    #[test]
    fn detached_route_is_not_rematerialized() {
        // Detaching a route must stick: the save following a detach runs
        // reconcile, which must not recreate the just-removed pairing.
        let mut prefs = generated_preferences();
        reconcile(&mut prefs);
        assert_eq!(prefs.provider_pairings.len(), 1);
        super::super::remove_provider_pairing(&mut prefs, "claude", "minimax");
        reconcile(&mut prefs);
        assert!(prefs.provider_pairings.is_empty(),
            "detach must stick, got {:?}", prefs.provider_pairings);
    }

    #[test]
    fn purge_carries_pre_upgrade_detach_into_tombstone() {
        // Old install: the route was detached (pairing gone) while its
        // catalogue recipe remained as the marker blocking re-creation.
        // Purging that marker must preserve the detach decision.
        let mut prefs = generated_preferences();
        reconcile(&mut prefs);
        prefs.provider_pairings.clear();
        prefs.spawn_configurations.push(crate::preferences::spawn_configurations::SpawnConfiguration {
            id: "launch/claude:minimax".into(), name: "MiniMax".into(), spawn_option_id: "claude:minimax".into(),
            generated: Some(GeneratedLaunch { source: "claude:minimax".into(), catalogue_revision: 1, user_owned: false, route: None }),
            ..Default::default()
        });
        reconcile(&mut prefs);
        assert!(prefs.provider_pairings.is_empty(),
            "pre-upgrade detach must survive the purge, got {:?}", prefs.provider_pairings);
        assert!(prefs.deleted_launch_configurations.contains(&"launch/claude:minimax".to_string()));
    }

    #[test]
    fn retired_aliases_heal_purged_recipes_but_respect_user_choices() {
        let mut prefs = generated_preferences();
        reconcile(&mut prefs);
        // The pairing materialized but no configurations exist: the retired
        // catalogue ids for the harness, its route, and Terminal all heal
        // to their bare Spawn Option.
        let aliases = retired_configuration_aliases(&prefs);
        assert_eq!(aliases.get("launch/claude").map(String::as_str), Some("claude"));
        assert_eq!(aliases.get("launch/claude:minimax").map(String::as_str), Some("claude:minimax"));
        assert_eq!(aliases.get("launch/terminal").map(String::as_str), Some("terminal"));
        // A user-owned edit keeps its id live: no alias reroutes it.
        prefs.spawn_configurations.push(crate::preferences::spawn_configurations::SpawnConfiguration {
            id: "launch/claude".into(), name: "My Claude".into(), spawn_option_id: "claude".into(),
            generated: Some(GeneratedLaunch { source: "claude".into(), catalogue_revision: 1, user_owned: true, route: None }),
            ..Default::default()
        });
        assert!(!retired_configuration_aliases(&prefs).contains_key("launch/claude"));
        // An explicitly deleted recipe is never resurrected as an alias.
        prefs.spawn_configurations.clear();
        prefs.deleted_launch_configurations.push("launch/claude:minimax".into());
        assert!(!retired_configuration_aliases(&prefs).contains_key("launch/claude:minimax"));
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
