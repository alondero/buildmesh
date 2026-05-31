//! Tauri commands for provider usage fetching.

use crate::preferences::{self, AppPreferences};
use crate::services::usage::{self, ProviderUsage};
use tauri::command;

const PROVIDERS: [&str; 4] = ["anthropic", "codex", "minimax", "agy"];

/// Fetches a single provider's usage, serving a fresh cache entry unless
/// `force_refresh` is set, and caching whatever it fetches.
fn cached_or_fetch(provider: &str, force_refresh: bool, prefs: &AppPreferences) -> ProviderUsage {
    if !force_refresh {
        if let Some(cached) = usage::get_cached_usage(provider) {
            return cached;
        }
    }

    let result = match provider {
        "anthropic" => usage::anthropic_usage(),
        "codex" => usage::codex_usage(),
        "minimax" => usage::minimax_usage(prefs.minimax_api_key.as_deref().unwrap_or("")),
        "agy" => usage::agy_usage(prefs.google_cloud_project.as_deref().unwrap_or("cloudshell-gca")),
        other => unreachable!("cached_or_fetch called with unknown provider: {other}"),
    };

    usage::set_cached_usage(provider, result.clone());
    result
}

#[command]
pub async fn get_provider_usage(
    provider: String,
    force_refresh: bool,
) -> Result<ProviderUsage, String> {
    if !PROVIDERS.contains(&provider.as_str()) {
        return Err(format!("Unknown provider: {}", provider));
    }
    let prefs = preferences::load()?;
    Ok(cached_or_fetch(&provider, force_refresh, &prefs))
}

#[command]
pub async fn get_all_provider_usage(force_refresh: bool) -> Result<Vec<ProviderUsage>, String> {
    let prefs = preferences::load()?;
    Ok(PROVIDERS
        .iter()
        .map(|p| cached_or_fetch(p, force_refresh, &prefs))
        .collect())
}

#[command]
pub async fn set_minimax_api_key(key: Option<String>) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    prefs.minimax_api_key = key;
    preferences::save(prefs)?;
    usage::invalidate_cache();
    Ok(())
}
