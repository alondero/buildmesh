//! Tauri commands for buildmesh-wide preferences.
//!
//! See `crate::preferences` for the persistence layer.

use crate::preferences::{self, AppPreferences};
use tauri::command;

/// Read the persisted buildmesh-wide preferences. Always returns a value —
/// a missing or malformed file yields `AppPreferences::default()`.
#[command]
pub async fn get_app_preferences() -> Result<AppPreferences, String> {
    preferences::load()
}

/// Set the buildmesh-wide default provider. Pass `None` (or an empty string,
/// which is normalised away) to clear the override and restore the hardcoded
/// `anthropic` fallback.
#[command]
pub async fn set_app_default_provider(provider: Option<String>) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    prefs.default_provider = provider.filter(|s| !s.is_empty());
    preferences::save(prefs)
}
