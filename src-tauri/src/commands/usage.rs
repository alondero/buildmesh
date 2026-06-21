//! Tauri commands for provider usage fetching.

use crate::preferences::{self, ProviderAccount};
use crate::services::usage::{self, ProviderUsage};
use tauri::command;

/// Provider ids Buildmesh can actually fetch usage for. An enabled account whose
/// id isn't here (e.g. a custom Claude-compatible provider) is configurable but
/// has no usage endpoint yet, so it's skipped by the poller.
const FETCHABLE: [&str; 4] = ["anthropic", "codex", "minimax", "agy"];

/// The provider ids to poll: enabled accounts intersected with [`FETCHABLE`],
/// preserving account order. Self-contained (filters both conditions) so a caller
/// can't accidentally poll a disabled account; pure so it's unit-tested without
/// touching the network (issue #537, AC3).
fn accounts_to_poll(accounts: &[ProviderAccount]) -> Vec<String> {
    accounts
        .iter()
        .filter(|a| a.enabled && FETCHABLE.contains(&a.id.as_str()))
        .map(|a| a.id.clone())
        .collect()
}

/// Fetches a single provider's usage, serving a fresh cache entry unless
/// `force_refresh` is set, and caching whatever it fetches.
fn cached_or_fetch(provider: &str, force_refresh: bool) -> ProviderUsage {
    if !force_refresh {
        if let Some(cached) = usage::get_cached_usage(provider) {
            return cached;
        }
    }

    let result = match provider {
        "anthropic" => usage::anthropic_usage(),
        "codex" => usage::codex_usage(),
        // `minimax_api_key_resolved` already reads the account key then the legacy
        // flat field, so it's the single source of truth here.
        "minimax" => usage::minimax_usage(
            preferences::minimax_api_key_resolved().as_deref().unwrap_or(""),
        ),
        "agy" => usage::agy_usage(),
        other => unreachable!("cached_or_fetch called with unknown provider: {other}"),
    };

    usage::set_cached_usage(provider, result.clone());
    result
}

#[command]
pub async fn get_all_provider_usage(force_refresh: bool) -> Result<Vec<ProviderUsage>, String> {
    // Only poll the providers the user has enabled (issue #537, AC3) — previously
    // this fanned out over a hardcoded list of all four, so an unconfigured
    // account produced slow timeouts / error badges on every panel open.
    let providers = accounts_to_poll(&preferences::provider_accounts());
    // Each fetch is a blocking HTTP round-trip to a different vendor; running
    // them serially made the Accounts & Usage panel wait for the sum of all
    // of them. Fan out on blocking threads and collect in order.
    let handles: Vec<_> = providers
        .into_iter()
        .map(|provider| {
            tauri::async_runtime::spawn_blocking(move || {
                cached_or_fetch(&provider, force_refresh)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preferences::BillingMode;

    fn account(id: &str, enabled: bool) -> ProviderAccount {
        ProviderAccount {
            id: id.to_string(),
            name: id.to_string(),
            enabled,
            billing_mode: BillingMode::Plan,
            claude_compatible: crate::preferences::is_claude_compatible_id(id),
            api_key: None,
            base_url: None,
            model_tiers: crate::preferences::ModelTiers::default(),
            models: Vec::new(),
        }
    }

    #[test]
    fn accounts_to_poll_keeps_only_enabled_fetchable() {
        let accounts = vec![
            account("anthropic", true),
            account("codex", false),   // disabled → excluded
            account("minimax", true),
            account("deepseek", true),  // custom, not fetchable → excluded
        ];
        assert_eq!(accounts_to_poll(&accounts), vec!["anthropic", "minimax"]);
    }

    #[test]
    fn accounts_to_poll_empty_when_all_disabled() {
        let accounts = vec![account("anthropic", false), account("minimax", false)];
        assert!(accounts_to_poll(&accounts).is_empty());
    }

    #[test]
    fn accounts_to_poll_empty_when_none_fetchable() {
        let accounts = vec![account("deepseek", true), account("glm", true)];
        assert!(accounts_to_poll(&accounts).is_empty());
    }
}
