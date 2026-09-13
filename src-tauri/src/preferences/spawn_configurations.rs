use serde::{Deserialize, Serialize};
use ts_rs::TS;
use tauri::Emitter;

use super::{AppPreferences, HarnessConfigValue};

/// Named, sparse launch overrides belonging to one Spawn Option.
#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[ts(export, export_to = "SpawnConfiguration.ts")]
pub struct SpawnConfiguration {
    pub id: String,
    pub name: String,
    pub spawn_option_id: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub extra_args: Option<String>,
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
    if value.spawn_option_id != option {
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
) -> Result<SpawnConfiguration, String> {
    let mut value = validate(value).map_err(|e| e.to_string())?;
    normalize_id(&mut value);
    super::update(|prefs| {
        if let Some(existing) = prefs
            .spawn_configurations
            .iter_mut()
            .find(|c| c.id == value.id)
        {
            *existing = value.clone();
        } else {
            prefs.spawn_configurations.push(value.clone());
        }
    })?;
    let _ = app.emit("provider-list-changed", ());
    Ok(value)
}

fn normalize_id(value: &mut SpawnConfiguration) {
    value.id = value.id.trim().to_string();
    if value.id.is_empty() {
        value.id = uuid::Uuid::new_v4().to_string();
    }
}

#[tauri::command]
pub fn delete_spawn_configuration(app: tauri::AppHandle, id: String) -> Result<(), String> {
    super::update(|prefs| prefs.spawn_configurations.retain(|c| c.id != id))?;
    let _ = app.emit("provider-list-changed", ());
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
        }
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
        assert!(value.id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));

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
