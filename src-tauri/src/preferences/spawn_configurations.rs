use serde::{Deserialize, Serialize};
use ts_rs::TS;
use tauri::Emitter;

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
    let id = id.map(str::trim).filter(|id| !id.is_empty());
    if id.is_none() {
        return Ok(None);
    }
    let prefs = super::load().map_err(SpawnConfigurationError::Storage)?;
    resolve(&prefs, option, id).map_err(SpawnConfigurationError::Invalid)
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
    save_with_route(value, None)
}

pub fn save_with_route(value: SpawnConfiguration, route: Option<super::ProviderPairing>) -> Result<SpawnConfiguration, String> {
    let mut value = validate(value)?;
    normalize_id(&mut value);
    value.resolved = None;
    super::storage::try_update(|prefs| {
        prepare_route(prefs, &value, route.clone())?;
        value.generated = prefs.spawn_configurations.iter().find(|c| c.id == value.id)
            .and_then(|c| c.generated.clone()).map(|mut g| { g.user_owned = true; g });
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
fn prepare_route(prefs: &mut AppPreferences, value: &SpawnConfiguration, route: Option<super::ProviderPairing>) -> Result<(), String> {
    let Some(mut route) = route else { return Ok(()); };
    if value.spawn_option_id != format!("{}:{}", route.harness_id, route.provider_id) {
        return Err("Provider Route does not match the configuration".into());
    }
    if let Some(existing) = prefs.provider_pairings.iter().find(|p| p.harness_id == route.harness_id && p.provider_id == route.provider_id) {
        if existing != &route { return Err("Provider Route changed; reopen the configuration to use its current settings".into()); }
        return Ok(());
    }
    let account = prefs.provider_accounts.iter().find(|a| a.id == route.provider_id)
        .ok_or("Provider account is missing; add its credential in Providers")?;
    if !account.enabled || account.api_key.as_deref().is_none_or(|key| key.trim().is_empty()) {
        return Err("Enable this provider and add its credential in Providers".into());
    }
    if !super::provider_surfaces(account).contains(&route.surface) {
        return Err("Provider does not support this API surface".into());
    }
    route.base_url = route.base_url.map(|url| url.trim().to_string());
    if route.model_tiers.default.as_deref().is_none_or(|model| model.trim().is_empty()) {
        route.model_tiers.default = value.model.clone();
    }
    let url = reqwest::Url::parse(route.base_url.as_deref().unwrap_or("")).map_err(|_| "Enter a valid provider endpoint URL")?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err("Provider endpoint must use HTTPS without credentials, query, or fragment".into());
    }
    super::upsert_provider_pairing(prefs, route);
    Ok(())
}

pub fn verify_draft(value: SpawnConfiguration, route: Option<super::ProviderPairing>) -> Result<super::PairingVerification, String> {
    let mut value = validate(value)?;
    normalize_id(&mut value);
    let mut prefs = super::load()?;
    prepare_route(&mut prefs, &value, route)?;
    prefs.spawn_configurations.retain(|c| c.id != value.id);
    prefs.spawn_configurations.push(value.clone());
    let plan = super::launch_configurations::resolve_for_edit(&prefs, &value.id)?;
    let route = plan.route.ok_or("Select a proxied provider to verify")?;
    if route.surface != super::ApiSurface::OpenAI { return Err("This route does not require Responses verification".into()); }
    let account = prefs.provider_accounts.iter().find(|a| a.id == route.provider_id).ok_or("Provider account is missing")?;
    crate::services::provider_verification::verify_route_blocking(&route, account, plan.harness.runtime.unwrap_or(crate::models::EnvType::Windows))
}

#[tauri::command]
pub async fn verify_launch_configuration(value: SpawnConfiguration, route: Option<super::ProviderPairing>) -> Result<super::PairingVerification, String> {
    crate::commands::run_blocking("verify_launch_configuration", move || verify_draft(value, route)).await
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
    super::update(|prefs| {
        prefs.spawn_configurations.retain(|c| c.id != id);
        if !prefs.deleted_launch_configurations.iter().any(|deleted| deleted == id) {
            prefs.deleted_launch_configurations.push(id.into());
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configuration(option: &str) -> SpawnConfiguration {
        SpawnConfiguration {
            id: "sol".into(),
            name: " Sol ".into(),
            spawn_option_id: option.into(),
            model: Some(" gpt-5.6-sol ".into()),
            effort: None,
            extra_args: None,
            ..Default::default()
        }
    }

    #[test]
    fn configuration_and_pairing_save_atomically_and_preserve_other_routes() {
        let tmp = tempfile::tempdir().unwrap();
        super::super::init_for_tests(tmp.path().into());
        let mut prefs: AppPreferences = serde_json::from_value(serde_json::json!({
            "provider_accounts":[{"id":"minimax","name":"MiniMax","enabled":true,"api_key":"test","billing_mode":"pay_as_you_go"}],
            "detached_provider_routes":["claude:minimax", "codex:minimax"]
        })).unwrap();
        super::super::save(prefs.clone()).unwrap();
        let endpoint = super::super::first_class_surfaces("minimax").remove(0);
        let route = super::super::ProviderPairing {
            harness_id: "claude".into(), provider_id: "minimax".into(), surface: endpoint.surface,
            base_url: Some(endpoint.base_url), model_tiers: endpoint.model_tiers,
        };
        let mut value = configuration("claude:minimax");
        value.model = Some("MiniMax-M2.7-highspeed".into());
        value.effort = Some("high".into());
        assert!(save_with_route(value.clone(), Some(route.clone())).unwrap_err().contains("Effort"));
        prefs = super::super::load().unwrap();
        assert!(prefs.provider_pairings.is_empty());
        assert!(prefs.spawn_configurations.is_empty());
        value.effort = None;
        let saved = save_with_route(value.clone(), Some(route.clone())).unwrap();
        prefs = super::super::load().unwrap();
        assert_eq!(prefs.provider_pairings, vec![route.clone()]);
        assert_eq!(prefs.spawn_configurations, vec![saved]);
        let mut changed = route;
        changed.base_url = Some("https://other.example/v1".into());
        assert!(save_with_route(value, Some(changed)).unwrap_err().contains("changed"));
        assert_eq!(super::super::load().unwrap(), prefs);
        super::super::reset_for_tests();
    }

    #[test]
    fn spawn_configurations_preserve_sparse_values_and_roundtrip_old_preferences() {
        let mut prefs: AppPreferences = serde_json::from_str("{}").unwrap();
        assert!(prefs.spawn_configurations.is_empty());
        let mut value = configuration("codex");
        value.effort = Some("  ".into());
        value.extra_args = Some("  ".into());
        let value = validate(value).unwrap();
        assert_eq!(value.name, "Sol");
        assert_eq!(value.model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(value.effort, None);
        assert_eq!(value.extra_args, None);
        prefs.spawn_configurations.push(value.clone());
        let restored: AppPreferences =
            serde_json::from_str(&serde_json::to_string(&prefs).unwrap()).unwrap();
        assert_eq!(
            resolve(&restored, "codex", Some("sol")).unwrap(),
            Some(value)
        );
        assert_eq!(resolve(&restored, "codex", None).unwrap(), None);
    }

    #[test]
    fn spawn_configurations_reject_unsupported_fields_and_effort_vocabulary() {
        assert!(validate(configuration("terminal"))
            .unwrap_err()
            .contains("model"));
        let mut value = configuration("codex");
        value.effort = Some("not-an-effort".into());
        assert!(validate(value).unwrap_err().contains("not allowed"));
        let mut value = configuration("opencode");
        value.effort = Some("high".into());
        assert!(validate(value).unwrap_err().contains("does not support"));
        let mut value = configuration("terminal");
        value.model = None;
        value.extra_args = Some("--model unsafe".into());
        assert!(validate(value).unwrap_err().contains("extra arguments"));
        let mut value = configuration("codex");
        value.name = " ".into();
        assert!(validate(value).is_err());
    }

    #[test]
    fn save_spawn_configuration_replaces_whitespace_only_ids() {
        let mut value = configuration("codex");
        value.id = "   ".into();
        normalize_id(&mut value);
        assert!(!value.id.is_empty());
        assert!(uuid::Uuid::parse_str(value.id.strip_prefix("launch/").unwrap()).is_ok());

        value.id = " sol ".into();
        normalize_id(&mut value);
        assert_eq!(value.id, "sol");
    }

    #[test]
    fn spawn_configurations_cannot_cross_pairings_or_silently_fall_back_when_deleted() {
        let mut prefs = AppPreferences::default();
        prefs
            .spawn_configurations
            .push(configuration("claude:openrouter"));
        assert!(resolve(&prefs, "claude", Some("sol"))
            .unwrap_err()
            .contains("different"));
        assert!(resolve(&prefs, "codex:openrouter", Some("sol")).is_err());
        assert!(resolve(&prefs, "claude:openrouter", Some("missing"))
            .unwrap_err()
            .contains("no longer exists"));
    }
}
