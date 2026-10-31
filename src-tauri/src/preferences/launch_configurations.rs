//! Launch identity, catalogue and resolution shared by interactive and background callers.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{AppPreferences, HarnessProfile, ProviderPairing};
use crate::agent::capabilities::{
    capabilities_for, resolve_agent_config, AgentConfigInputs, FieldInputs,
};
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
        if let Some(existing) = profiles.iter_mut().find(|h| h.id == profile.id) {
            *existing = profile.clone();
        } else {
            profiles.push(profile.clone());
        }
    }
    profiles
}

pub fn reconcile(prefs: &mut AppPreferences) {
    let profiles = effective_profiles(prefs);
    for profile in &profiles {
        let surface = super::surface_for_executor(Provider::from_db_str(&profile.harness));
        for account in prefs.provider_accounts.iter().filter(|a| a.enabled) {
            let source = format!("{}:{}", profile.id, account.id);
            if prefs
                .spawn_configurations
                .iter()
                .any(|c| c.generated.as_ref().is_some_and(|g| g.source == source))
                || prefs
                    .deleted_launch_configurations
                    .contains(&format!("launch/{source}"))
                || prefs.detached_provider_routes.iter().any(|d| d == &source)
            {
                continue;
            }
            if let Some(endpoint) = super::first_class_surfaces(&account.id)
                .into_iter()
                .find(|e| Some(e.surface) == surface)
            {
                if !prefs
                    .provider_pairings
                    .iter()
                    .any(|p| p.harness_id == profile.id && p.provider_id == account.id)
                {
                    prefs.provider_pairings.push(ProviderPairing {
                        harness_id: profile.id.clone(),
                        provider_id: account.id.clone(),
                        surface: endpoint.surface,
                        base_url: Some(endpoint.base_url),
                        model_tiers: endpoint.model_tiers,
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
            // Carry that detach decision into the detach record so the
            // route stays detached — without tombstoning the recipe id,
            // so saved references still heal. Harness-only sources gate
            // nothing.
            if let Some(source) = c.generated.as_ref().map(|g| g.source.clone()) {
                let id = SpawnOptionId::from(source.as_str());
                let paired = id.provider_id.as_deref().is_none_or(|provider| {
                    prefs
                        .provider_pairings
                        .iter()
                        .any(|p| p.harness_id == id.harness_id && p.provider_id == provider)
                });
                if id.is_proxied()
                    && !paired
                    && !prefs.detached_provider_routes.iter().any(|d| d == &source)
                {
                    detached.push(source);
                }
            }
            purged.push((c.id.clone(), c.spawn_option_id.clone()));
            false
        } else {
            true
        }
    });
    prefs.detached_provider_routes.extend(detached);
    for value in &mut prefs.spawn_configurations {
        let _ = normalize_identity(value);
    }
    // Selections that pointed at a purged recipe fall back to its bare Spawn
    // Option (same harness/route, native defaults) so existing nodes and
    // app-wide defaults keep resolving. Other selections are left alone:
    // bare Spawn Option ids resolve directly, and surviving configuration
    // ids keep working.
    if !purged.is_empty() {
        for selection in [
            &mut prefs.default_provider,
            &mut prefs.reviewer_provider,
            &mut prefs.naming_provider,
        ] {
            if let Some(value) = selection.as_mut() {
                if let Some((_, option)) = purged.iter().find(|(id, _)| id == value) {
                    *value = option.clone();
                }
            }
        }
    }
}

pub fn normalize_identity(
    value: &mut super::spawn_configurations::SpawnConfiguration,
) -> Result<(), String> {
    let legacy = SpawnOptionId::from(value.spawn_option_id.as_str());
    let harness = value
        .harness_id
        .clone()
        .unwrap_or_else(|| legacy.harness_id.clone());
    let route = value
        .provider_route_id
        .clone()
        .or_else(|| legacy.is_proxied().then(|| value.spawn_option_id.clone()));
    if route
        .as_ref()
        .is_some_and(|r| !SpawnOptionId::from(r.as_str()).is_proxied())
    {
        return Err("Provider Route must identify both a harness and a provider".into());
    }
    if route
        .as_ref()
        .is_some_and(|r| SpawnOptionId::from(r.as_str()).harness_id != harness)
    {
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
    /// The Harness Login this launch uses; absent means the default login.
    /// Frozen with the home path so a resume returns to the home that holds
    /// the session even after the login is renamed or removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub harness_login: Option<super::harness_logins::HarnessLoginRef>,
}

pub fn resolve(
    prefs: &AppPreferences,
    selection: &str,
    overrides: &LaunchOverrides,
) -> Result<ResolvedLaunchPlan, String> {
    resolve_plan(prefs, selection, overrides, true)
}

pub fn resolve_for_edit(
    prefs: &AppPreferences,
    selection: &str,
) -> Result<ResolvedLaunchPlan, String> {
    resolve_plan(prefs, selection, &Default::default(), false)
}

/// Freeze effective settings without probing installed processes or reading preferences.
/// Availability and credentials are checked again when the frozen plan launches.
pub fn capture(
    prefs: &AppPreferences,
    selection: &str,
    overrides: &LaunchOverrides,
) -> Result<ResolvedLaunchPlan, String> {
    resolve_plan(prefs, selection, overrides, false)
}

pub fn capture_legacy(
    prefs: &AppPreferences,
    selection: &str,
) -> Result<ResolvedLaunchPlan, String> {
    resolve_plan(prefs, selection, &Default::default(), false)
}

fn resolve_plan(
    prefs: &AppPreferences,
    selection: &str,
    overrides: &LaunchOverrides,
    require_available: bool,
) -> Result<ResolvedLaunchPlan, String> {
    let configuration = prefs
        .spawn_configurations
        .iter()
        .find(|c| c.id == selection);
    if configuration.is_none() && selection.starts_with("launch/") {
        return Err("Launch Configuration no longer exists; select another configuration".into());
    }
    let option = configuration.map_or(selection, |c| c.spawn_option_id.as_str());
    let id = SpawnOptionId::from(option);
    let harness = prefs
        .harness_profiles
        .iter()
        .find(|h| h.id == id.harness_id())
        .cloned()
        .or_else(|| {
            crate::agent::provider::BUILTIN_HARNESS_IDS
                .contains(&id.harness_id())
                .then(|| HarnessProfile {
                    id: id.harness_id.clone(),
                    name: id.harness_id.clone(),
                    // The `claude` → `anthropic` mapping comes from the one
                    // alias table (issue #2532) — not a hand-rolled copy.
                    harness: crate::agent::harness_catalog::harness_adapter_id(id.harness_id())
                        .to_string(),
                    runtime: None,
                    wsl_distro: None,
                    executable: None,
                })
        })
        .ok_or_else(|| {
            "Harness is missing; reinstall it or select another configuration".to_string()
        })?;
    if require_available
        && (crate::agent::detection::currently_installed_profiles(vec![harness.clone()]).is_empty()
            || crate::agent::provider_menu::provider_info_for(
                &harness,
                crate::agent::provider::Platform::current(),
            )
            .is_none())
    {
        return Err("Harness is unavailable; install it for the selected runtime".into());
    }
    let caps = capabilities_for(Provider::from_db_str(&harness.harness).adapter());
    // Harness defaults describe the harness's native provider, so a proxy
    // row takes its model from the route instead — the shared reader applies
    // that exclusion here and on the live launch seam (#2038).
    let app = super::harness_default_for(prefs, &id);
    let configured_model = configuration.and_then(|c| c.model.as_deref());
    let configured_effort = configuration.and_then(|c| c.effort.as_deref());
    let nonblank = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let model = nonblank(overrides.model.as_deref()).or_else(|| nonblank(configured_model));
    let effort = nonblank(overrides.effort.as_deref()).or_else(|| nonblank(configured_effort));
    let extra = nonblank(overrides.extra_args.as_deref())
        .or_else(|| configuration.and_then(|c| nonblank(c.extra_args.as_deref())));
    let mut config = resolve_agent_config(
        &caps,
        AgentConfigInputs {
            model: FieldInputs {
                explicit: model.as_deref(),
                mesh: None,
                application: app.as_ref().and_then(|c| c.model.as_deref()),
            },
            effort: FieldInputs {
                explicit: effort.as_deref(),
                mesh: None,
                application: app.as_ref().and_then(|c| c.effort.as_deref()),
            },
            // Issue #2151, review round 1: launch recipes carry no
            // permission slot of their own, and the plan carries none
            // either — the per-harness Settings default resolves live at
            // spawn time (never frozen into a recipe or plan), same as a
            // direct spawn. Feeding a permission layer into
            // `resolve_agent_config` here would compute a value the plan
            // cannot project, so it is deliberately absent.
            permission_mode: FieldInputs {
                explicit: None,
                mesh: None,
                application: None,
            },
        },
        extra.as_deref(),
    );
    let route = if let Some(provider_id) = id.provider_id() {
        let account = prefs
            .provider_accounts
            .iter()
            .find(|a| a.id == provider_id)
            .ok_or_else(|| {
                format!("Provider account '{provider_id}' is missing; restore its credential")
            })?;
        if require_available && !account.enabled {
            return Err(format!(
                "Provider '{}' is disabled; enable it in Providers",
                account.name
            ));
        }
        if require_available
            && account
                .api_key
                .as_deref()
                .is_none_or(|key| key.trim().is_empty())
        {
            return Err(format!(
                "Provider '{}' has no credential; add its key in Providers",
                account.name
            ));
        }
        let executor = Provider::from_db_str(&harness.harness);
        let mut route = prefs
            .provider_pairings
            .iter()
            .find(|p| p.harness_id == harness.id && p.provider_id == provider_id)
            .cloned();
        // Cline's native auth flow can consume an existing Claude- or
        // Codex-attached pairing for the same account. Preserve that legacy
        // surface fallback when resolving a Launch Configuration; otherwise
        // the common resolver fails before launch_routing can emit Cline's
        // consumer-specific environment.
        if route.is_none() && executor == Provider::Cline {
            route = prefs
                .provider_accounts
                .iter()
                .find(|a| a.id == provider_id)
                .and_then(|account| {
                    super::compatibility::resolve_pairing(
                        "cline",
                        account,
                        &prefs.provider_pairings,
                    )
                });
        }
        let mut route = route.ok_or_else(|| {
            "Provider Route is missing; restore it in Launch Configurations".to_string()
        })?;
        let supports_surface = executor == Provider::Cline
            || super::surface_for_executor(executor) == Some(route.surface);
        if !supports_surface {
            return Err("Provider Route uses an API surface this harness does not support".into());
        }
        if let Some(model) = config.model.as_ref() {
            route.model_tiers.default = Some(model.clone());
        } else {
            config.model = route.model_tiers.default.clone();
        }
        let catalogue = super::launch_catalog::provider_catalogue();
        let entry = catalogue.iter().find(|p| p.id == provider_id);
        let model = entry.and_then(|p| {
            p.models
                .iter()
                .find(|m| m.surface == route.surface && Some(&m.id) == config.model.as_ref())
        });
        // A custom model is allowed, but absent metadata is not evidence of effort support.
        let allowed = if model.is_none() && entry.is_some_and(|p| !p.manual_model) {
            Vec::new()
        } else {
            super::launch_catalog::allowed_efforts(&caps, model)
        };
        if effort
            .as_ref()
            .or(config.effort.as_ref())
            .is_some_and(|e| !allowed.contains(e))
        {
            if effort.is_some() {
                return Err("Effort is not supported by this provider/model and harness".into());
            }
            config.effort = None;
        }
        let decision = super::pairing_compatibility(&route);
        if !decision.compatible {
            return Err(decision
                .reason
                .unwrap_or_else(|| "Provider Route is incompatible".into()));
        }
        super::compatibility::preflight_pairing_env(Some(&route), provider_id)?;
        Some(route)
    } else {
        None
    };
    // The pairing verification is advisory: a route that was never checked,
    // whose check went stale (a routine `codex-cli` npm update changes the
    // installed version) or whose check failed still launches. The record is
    // carried so the spawn can warn the user and re-check the pairing in the
    // background — refusing to start on an unproven route would mean the
    // user cannot even try the combination they configured.
    let verification = route.as_ref().and_then(|route| {
        prefs
            .pairing_verifications
            .iter()
            .filter(|v| {
                v.harness_id == route.harness_id
                    && v.provider_id == route.provider_id
                    && Some(v.endpoint.as_str()) == route.base_url.as_deref()
                    && Some(v.model_id.as_str()) == route.model_tiers.default.as_deref()
            })
            .max_by_key(|v| matches!(v.status, super::PairingVerificationStatus::Verified))
            .cloned()
    });
    let requested_login = configuration
        .and_then(|c| super::harness_logins::normalize_login_id(c.login_id.as_deref()));
    let harness_login = login_for_plan(prefs, &harness.harness, route.is_some(), requested_login)?;
    Ok(ResolvedLaunchPlan {
        configuration_id: configuration
            .map_or_else(|| format!("launch/{selection}"), |c| c.id.clone()),
        configuration_name: configuration.map_or_else(|| harness.name.clone(), |c| c.name.clone()),
        spawn_option_id: option.into(),
        harness,
        route,
        verification,
        model: config.model,
        effort: config.effort,
        extra_args: config.extra_args,
        harness_login,
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
/// silently rerouting to defaults). Detached routes are still mapped: the
/// reference heals to the bare option, which resolves again once the route
/// is re-attached (and reports the missing route, not a missing recipe,
/// while detached).
pub(crate) fn retired_configuration_aliases(
    prefs: &AppPreferences,
) -> std::collections::HashMap<String, String> {
    let live: std::collections::HashSet<&str> = prefs
        .spawn_configurations
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    let deleted: std::collections::HashSet<&str> = prefs
        .deleted_launch_configurations
        .iter()
        .map(String::as_str)
        .collect();
    let mut aliases = std::collections::HashMap::new();
    for source in effective_profiles(prefs)
        .into_iter()
        .map(|p| p.id)
        .chain(
            prefs
                .provider_pairings
                .iter()
                .map(|p| format!("{}:{}", p.harness_id, p.provider_id)),
        )
        .chain(prefs.detached_provider_routes.iter().cloned())
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
                return Err(
                    "Launch Configuration no longer exists; select another configuration".into(),
                );
            }
            return Ok(selection.into());
        }
        Err(error) => return Err(error),
    };
    if let Some(value) = prefs
        .spawn_configurations
        .iter()
        .find(|c| c.id == selection)
    {
        return Ok(value.spawn_option_id.clone());
    }
    if selection.starts_with("launch/") {
        return Err("Launch Configuration no longer exists; select another configuration".into());
    }
    Ok(selection.into())
}

pub fn snapshot(plan: ResolvedLaunchPlan) -> super::spawn_configurations::SpawnConfiguration {
    let mut value = super::spawn_configurations::SpawnConfiguration {
        id: plan.configuration_id.clone(),
        name: plan.configuration_name.clone(),
        spawn_option_id: plan.spawn_option_id.clone(),
        model: plan.model.clone(),
        effort: plan.effort.clone(),
        extra_args: plan.extra_args.clone(),
        resolved: Some(plan),
        generated: None,
        ..Default::default()
    };
    let _ = normalize_identity(&mut value);
    value
}

pub fn resolve_snapshot(
    plan: &ResolvedLaunchPlan,
    overrides: &LaunchOverrides,
) -> Result<ResolvedLaunchPlan, String> {
    let prefs = AppPreferences {
        harness_profiles: vec![plan.harness.clone()],
        provider_accounts: super::provider_accounts(),
        provider_pairings: plan.route.iter().cloned().collect(),
        pairing_verifications: plan.verification.iter().cloned().collect(),
        spawn_configurations: vec![snapshot(plan.clone())],
        ..Default::default()
    };
    // The frozen login is part of the snapshot, not a live preference: a
    // resume returns to the home that holds the session.
    let mut resolved = resolve(&prefs, &plan.configuration_id, overrides)?;
    resolved.harness_login = plan.harness_login.clone();
    Ok(resolved)
}

/// Resolve the Harness Login a plan launches with. A Provider Route signs in
/// with its own key, so a login on a routed plan is refused rather than
/// silently ignored.
fn login_for_plan(
    prefs: &AppPreferences,
    harness_adapter_id: &str,
    routed: bool,
    requested: Option<&str>,
) -> Result<Option<super::harness_logins::HarnessLoginRef>, String> {
    let requested = super::harness_logins::normalize_login_id(requested);
    if requested.is_some() && routed {
        return Err(
            "A Provider Route signs in with its own key; choose a login only for native Codex"
                .into(),
        );
    }
    super::harness_logins::resolve_login(prefs, harness_adapter_id, requested)
}

/// Apply the login picked in the spawn menu to a freshly resolved plan. The
/// pick wins over the configuration's pinned login; `None` keeps the plan's
/// own login (the pin, else the default).
pub fn apply_login_pick(
    prefs: &AppPreferences,
    plan: &mut ResolvedLaunchPlan,
    picked: Option<&str>,
) -> Result<(), String> {
    let Some(picked) = picked.map(str::trim).filter(|id| !id.is_empty()) else {
        return Ok(());
    };
    plan.harness_login = login_for_plan(
        prefs,
        &plan.harness.harness,
        plan.route.is_some(),
        Some(picked),
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "launch_configurations_tests.rs"]
mod tests;
