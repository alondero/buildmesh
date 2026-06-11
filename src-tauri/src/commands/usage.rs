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
        "agy" => usage::agy_usage(),
        other => unreachable!("cached_or_fetch called with unknown provider: {other}"),
    };

    usage::set_cached_usage(provider, result.clone());
    result
}

#[command]
pub async fn get_all_provider_usage(force_refresh: bool) -> Result<Vec<ProviderUsage>, String> {
    let prefs = preferences::load()?;
    // Each fetch is a blocking HTTP round-trip to a different vendor; running
    // them serially made the Accounts & Usage panel wait for the sum of all
    // four. Fan out on blocking threads and collect in PROVIDERS order.
    let handles: Vec<_> = PROVIDERS
        .iter()
        .map(|p| {
            let provider = p.to_string();
            let prefs = prefs.clone();
            tauri::async_runtime::spawn_blocking(move || {
                cached_or_fetch(&provider, force_refresh, &prefs)
            })
        })
        .collect();

    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
        results.push(
            handle
                .await
                .map_err(|e| format!("usage fetch task failed: {}", e))?,
        );
    }
    Ok(results)
}

#[command]
pub async fn set_minimax_api_key(key: Option<String>) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    prefs.minimax_api_key = key;
    preferences::save(prefs)?;
    usage::invalidate_cache();
    Ok(())
}
