use serde::{Deserialize, Serialize};
use tauri::Emitter;
use ts_rs::TS;

use super::{AppPreferences, HarnessConfigValue};

/// Named, sparse launch overrides belonging to one Spawn Option.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS, PartialEq, Eq)]
#[ts(export, export_to = "SpawnConfiguration.ts")]
pub struct SpawnConfiguration {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub spawn_option_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub harness_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub provider_route_id: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub extra_args: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub generated: Option<super::launch_configurations::GeneratedLaunch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub resolved: Option<super::launch_configurations::ResolvedLaunchPlan>,
    /// Harness Login this configuration always launches with (Codex only).
    /// Absent means the login picked in the spawn menu, else the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub login_id: Option<String>,
}

#[derive(Debug)]
pub enum SpawnConfigurationError {
    Invalid(String),
    Storage(String),
}

impl std::fmt::Display for SpawnConfigurationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) | Self::Storage(message) => write!(f, "{message}"),
        }
    }
}

pub fn validate(mut value: SpawnConfiguration) -> Result<SpawnConfiguration, String> {
    super::launch_configurations::normalize_identity(&mut value)?;
    value.name = value.name.trim().to_string();
    if value.name.is_empty() {
        return Err("Configuration name is required".into());
    }
    let option = crate::agent::provider::SpawnOptionId::from(value.spawn_option_id.as_str());
    let caps = super::harness_capabilities_for(option.harness_id())
        .ok_or_else(|| "Unknown harness".to_string())?;
    let normalized = super::validate_harness_default(
        option.harness_id(),
        HarnessConfigValue {
            model: value.model,
            effort: value.effort,
            // Saved recipes carry no permission slot (issue #2151 adds
            // only the per-harness Settings default).
            permission_mode: None,
        },
    )?;
    value.model = normalized.model;
    value.effort = normalized.effort;
    value.extra_args = value
        .extra_args
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if value.model.is_some() && !caps.supports_model_override {
        return Err("This harness does not support model overrides".into());
    }
    if value.extra_args.is_some() && !caps.supports_extra_args {
        return Err("This harness does not support extra arguments".into());
    }
    value.login_id =
        super::harness_logins::normalize_login_id(value.login_id.as_deref()).map(str::to_string);
    if value.login_id.is_some() {
        if !super::harness_logins::supports_logins(option.harness_id()) {
            return Err("Only Codex configurations can choose a login".into());
        }
        if option.is_proxied() {
            return Err(
                "A Provider Route signs in with its own key; choose a login only for native Codex"
                    .into(),
            );
        }
    }
    Ok(value)
}

pub fn resolve(
    prefs: &AppPreferences,
    option: &str,
    id: Option<&str>,
) -> Result<Option<SpawnConfiguration>, String> {
    let Some(id) = id.map(str::trim).filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    let value = prefs
        .spawn_configurations
        .iter()
        .find(|c| c.id == id)
        .ok_or_else(|| "Saved configuration no longer exists".to_string())?;
    validate_for_option(option, value.clone()).map(Some)
}

pub fn validate_for_option(
    option: &str,
    value: SpawnConfiguration,
) -> Result<SpawnConfiguration, String> {
    if value.spawn_option_id != option && value.id != option {
        return Err("Configuration belongs to a different Spawn Option".into());
    }
    validate(value)
}

pub fn resolve_saved(
    option: &str,
    id: Option<&str>,
) -> Result<Option<SpawnConfiguration>, SpawnConfigurationError> {
    match super::store::ambient_store() {
        Ok(store) => resolve_saved_from(&store, option, id),
        Err(error) => Err(SpawnConfigurationError::Storage(error)),
    }
}

/// Explicit-store path — the caller owns the store.
///
/// A test constructs its own [`super::store::PreferencesStore`] over a
/// temporary directory and passes it here, so the preferences state this call
/// sees is the one the caller set up (issue #1482).
pub fn resolve_saved_from(
    store: &super::store::PreferencesStore,
    option: &str,
    id: Option<&str>,
) -> Result<Option<SpawnConfiguration>, SpawnConfigurationError> {
    let id = id.map(str::trim).filter(|id| !id.is_empty());
    if id.is_none() {
        return Ok(None);
    }
    let prefs = store.load().map_err(SpawnConfigurationError::Storage)?;
    resolve(&prefs, option, id).map_err(SpawnConfigurationError::Invalid)
}

/// Fold the login picked in the spawn menu into the configuration a new node
/// is created from.
///
/// A picked saved configuration launches with the picked login instead of
/// its pin. "Spawn with defaults" has no configuration, so the defaults are
/// resolved here and frozen with the login — node creation then takes that
/// plan verbatim. No pick returns `configuration` unchanged.
pub fn with_login_pick(
    prefs: &AppPreferences,
    option: &str,
    configuration: Option<SpawnConfiguration>,
    login: Option<&str>,
) -> Result<Option<SpawnConfiguration>, String> {
    let Some(login) = login.map(str::trim).filter(|id| !id.is_empty()) else {
        return Ok(configuration);
    };
    let mut selected_preferences = prefs.clone();
    let selection = if let Some(mut value) = configuration {
        value.login_id = Some(login.to_string());
        value.resolved = None;
        let id = value.id.clone();
        selected_preferences
            .spawn_configurations
            .retain(|c| c.id != id);
        selected_preferences.spawn_configurations.push(value);
        id
    } else {
        option.to_string()
    };
    let mut plan = super::launch_configurations::capture(
        &selected_preferences,
        &selection,
        &Default::default(),
    )?;
    super::launch_configurations::apply_login_pick(prefs, &mut plan, Some(login))?;
    Ok(Some(super::launch_configurations::snapshot(plan)))
}

/// Resolve the selection emitted by every Spawn Menu against one preferences
/// snapshot. The login pick wins over a saved configuration's pin.
pub fn resolve_menu_selection(
    prefs: &AppPreferences,
    option: &str,
    configuration_id: Option<&str>,
    login_id: Option<&str>,
) -> Result<Option<SpawnConfiguration>, String> {
    let configuration = resolve(prefs, option, configuration_id)?;
    with_login_pick(prefs, option, configuration, login_id)
}

pub fn resolve_menu_selection_from(
    store: Option<&super::store::PreferencesStore>,
    option: &str,
    configuration_id: Option<&str>,
    login_id: Option<&str>,
) -> Result<Option<SpawnConfiguration>, String> {
    let prefs = match store {
        Some(store) => store.load()?,
        None => AppPreferences::default(),
    };
    resolve_menu_selection(&prefs, option, configuration_id, login_id)
}

#[tauri::command]
pub fn list_spawn_configurations() -> Result<Vec<SpawnConfiguration>, String> {
    Ok(super::load()?.spawn_configurations)
}

#[tauri::command]
pub fn save_spawn_configuration(
    app: tauri::AppHandle,
    value: SpawnConfiguration,
    route: Option<super::ProviderPairing>,
) -> Result<SpawnConfiguration, String> {
    let value = save_with_route(value, route)?;
    let _ = app.emit("provider-list-changed", ());
    Ok(value)
}

pub fn save_value(value: SpawnConfiguration) -> Result<SpawnConfiguration, String> {
    match super::store::ambient_store() {
        Ok(store) => save_value_from(&store, value),
        Err(error) => Err(error),
    }
}

/// Explicit-store path — the caller owns the store.
///
/// A test constructs its own [`super::store::PreferencesStore`] over a
/// temporary directory and passes it here, so the preferences state this call
/// sees is the one the caller set up (issue #1482).
pub fn save_value_from(
    store: &super::store::PreferencesStore,
    value: SpawnConfiguration,
) -> Result<SpawnConfiguration, String> {
    save_with_route_from(store, value, None)
}

pub fn save_with_route(
    value: SpawnConfiguration,
    route: Option<super::ProviderPairing>,
) -> Result<SpawnConfiguration, String> {
    // Issue #1948: time persistence here rather than at the call sites so
    // every save path (desktop command, mobile HTTP route) reports
    // separately from the spawn-menu refresh the frontend runs next
    // (`list_providers`, timed in `provider_menu`). Secret-free by
    // construction: only the wall-clock, whether a route was attached, and
    // success travel — never names, ids, URLs, models, or keys.
    let persistence_started = std::time::Instant::now();
    let has_route = route.is_some();
    let result = match super::store::ambient_store() {
        Ok(store) => save_with_route_from(&store, value, route),
        Err(error) => Err(error),
    };
    tracing::info!(
        has_route,
        success = result.is_ok(),
        persistence_duration_ms = persistence_started.elapsed().as_millis(),
        "launch configuration save completed"
    );
    result
}

/// Explicit-store path — the caller owns the store.
///
/// A test constructs its own [`super::store::PreferencesStore`] over a
/// temporary directory and passes it here, so the preferences state this call
/// sees is the one the caller set up (issue #1482).
pub fn save_with_route_from(
    store: &super::store::PreferencesStore,
    value: SpawnConfiguration,
    route: Option<super::ProviderPairing>,
) -> Result<SpawnConfiguration, String> {
    save_with_route_inner(store, value, route)
}

fn save_with_route_inner(
    store: &super::store::PreferencesStore,
    value: SpawnConfiguration,
    route: Option<super::ProviderPairing>,
) -> Result<SpawnConfiguration, String> {
    let mut value = validate(value)?;
    normalize_id(&mut value);
    value.resolved = None;
    store.try_update(|prefs| {
        prepare_route(prefs, &value, route.clone())?;
        value.generated = prefs
            .spawn_configurations
            .iter()
            .find(|c| c.id == value.id)
            .and_then(|c| c.generated.clone())
            .map(|mut g| {
                g.user_owned = true;
                g
            });
        if let Some(existing) = prefs
            .spawn_configurations
            .iter_mut()
            .find(|c| c.id == value.id)
        {
            *existing = value.clone();
        } else {
            prefs.spawn_configurations.push(value.clone());
        }
        super::launch_configurations::resolve_for_edit(prefs, &value.id)?;
        Ok(())
    })?;
    Ok(value)
}

/// Only creation is accepted here: editing a shared route belongs to Advanced Provider Routes.
/// The caller's preference transaction also owns saving the configuration.
fn prepare_route(
    prefs: &mut AppPreferences,
    value: &SpawnConfiguration,
    route: Option<super::ProviderPairing>,
) -> Result<(), String> {
    let Some(mut route) = route else {
        return Ok(());
    };
    if value.spawn_option_id != format!("{}:{}", route.harness_id, route.provider_id) {
        return Err("Provider Route does not match the configuration".into());
    }
    if let Some(existing) = prefs
        .provider_pairings
        .iter()
        .find(|p| p.harness_id == route.harness_id && p.provider_id == route.provider_id)
    {
        if existing != &route {
            return Err(
                "Provider Route changed; reopen the configuration to use its current settings"
                    .into(),
            );
        }
        return Ok(());
    }
    let account = prefs
        .provider_accounts
        .iter()
        .find(|a| a.id == route.provider_id)
        .ok_or("Provider account is missing; add its credential in Providers")?;
    if !account.enabled
        || account
            .api_key
            .as_deref()
            .is_none_or(|key| key.trim().is_empty())
    {
        return Err("Enable this provider and add its credential in Providers".into());
    }
    if !super::provider_surfaces(account).contains(&route.surface) {
        return Err("Provider does not support this API surface".into());
    }
    route.base_url = route.base_url.map(|url| url.trim().to_string());
    if route
        .model_tiers
        .default
        .as_deref()
        .is_none_or(|model| model.trim().is_empty())
    {
        route.model_tiers.default = value.model.clone();
    }
    super::resolver::pairing_compat::validate_endpoint_url(
        route.base_url.as_deref().unwrap_or(""),
    )?;
    super::upsert_provider_pairing(prefs, route);
    Ok(())
}

pub fn verify_draft(
    value: SpawnConfiguration,
    route: Option<super::ProviderPairing>,
) -> Result<super::PairingVerification, String> {
    match super::store::ambient_store() {
        Ok(store) => verify_draft_from(&store, value, route),
        Err(error) => Err(error),
    }
}

/// Explicit-store path — the caller owns the store.
///
/// A test constructs its own [`super::store::PreferencesStore`] over a
/// temporary directory and passes it here, so the preferences state this call
/// sees is the one the caller set up (issue #1482).
pub fn verify_draft_from(
    store: &super::store::PreferencesStore,
    value: SpawnConfiguration,
    route: Option<super::ProviderPairing>,
) -> Result<super::PairingVerification, String> {
    let mut value = validate(value)?;
    normalize_id(&mut value);
    let mut prefs = store.load()?;
    prepare_route(&mut prefs, &value, route)?;
    prefs.spawn_configurations.retain(|c| c.id != value.id);
    prefs.spawn_configurations.push(value.clone());
    let plan = super::launch_configurations::resolve_for_edit(&prefs, &value.id)?;
    let route = plan.route.ok_or("Select a proxied provider to verify")?;
    if route.surface != super::ApiSurface::OpenAI {
        return Err("This route does not require Responses verification".into());
    }
    let account = prefs
        .provider_accounts
        .iter()
        .find(|a| a.id == route.provider_id)
        .ok_or("Provider account is missing")?;
    crate::services::provider_verification::verify_route_blocking(
        &route,
        account,
        plan.harness
            .runtime
            .unwrap_or(crate::models::EnvType::Windows),
    )
}

#[tauri::command]
pub async fn verify_launch_configuration(
    value: SpawnConfiguration,
    route: Option<super::ProviderPairing>,
) -> Result<super::PairingVerification, String> {
    crate::commands::run_blocking("verify_launch_configuration", move || {
        verify_draft(value, route)
    })
    .await
}

fn normalize_id(value: &mut SpawnConfiguration) {
    value.id = value.id.trim().to_string();
    if value.id.is_empty() {
        value.id = format!("launch/{}", uuid::Uuid::new_v4());
    }
}

#[tauri::command]
pub fn delete_spawn_configuration(app: tauri::AppHandle, id: String) -> Result<(), String> {
    delete_value(&id)?;
    let _ = app.emit("provider-list-changed", ());
    Ok(())
}

pub fn delete_value(id: &str) -> Result<(), String> {
    match super::store::ambient_store() {
        Ok(store) => delete_value_from(&store, id),
        Err(error) => Err(error),
    }
}

/// Explicit-store path — the caller owns the store.
///
/// A test constructs its own [`super::store::PreferencesStore`] over a
/// temporary directory and passes it here, so the preferences state this call
/// sees is the one the caller set up (issue #1482).
pub fn delete_value_from(store: &super::store::PreferencesStore, id: &str) -> Result<(), String> {
    store.update(|prefs| {
        prefs.spawn_configurations.retain(|c| c.id != id);
        if !prefs
            .deleted_launch_configurations
            .iter()
            .any(|deleted| deleted == id)
        {
            prefs.deleted_launch_configurations.push(id.into());
        }
    })?;
    Ok(())
}

#[cfg(test)]
#[path = "spawn_configurations_tests.rs"]
mod tests;
