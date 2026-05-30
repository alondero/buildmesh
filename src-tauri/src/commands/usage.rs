//! Tauri commands for provider usage fetching.

use crate::preferences;
use crate::services::usage::{self, ProviderUsage};
use tauri::command;

#[command]
pub async fn get_provider_usage(
    provider: String,
    force_refresh: bool,
) -> Result<ProviderUsage, String> {
    if !force_refresh {
        if let Some(cached) = usage::get_cached_usage(&provider) {
            return Ok(cached);
        }
    }

    let prefs = preferences::load()?;

    let result = match provider.as_str() {
        "anthropic" => usage::anthropic_usage(),
        "codex" => usage::codex_usage(),
        "minimax" => {
            let key = prefs.minimax_api_key.as_deref().unwrap_or("");
            usage::minimax_usage(key)
        }
        _ => {
            return Err(format!("Unknown provider: {}", provider));
        }
    };

    usage::set_cached_usage(&provider, result.clone());
    Ok(result)
}

#[command]
pub async fn get_all_provider_usage(force_refresh: bool) -> Result<Vec<ProviderUsage>, String> {
    let prefs = preferences::load()?;

    let anthropic = if !force_refresh {
        usage::get_cached_usage("anthropic")
            .unwrap_or_else(|| {
                let u = usage::anthropic_usage();
                usage::set_cached_usage("anthropic", u.clone());
                u
            })
    } else {
        usage::anthropic_usage()
    };

    let codex = if !force_refresh {
        usage::get_cached_usage("codex")
            .unwrap_or_else(|| {
                let u = usage::codex_usage();
                usage::set_cached_usage("codex", u.clone());
                u
            })
    } else {
        usage::codex_usage()
    };

    let minimax = if !force_refresh {
        usage::get_cached_usage("minimax")
            .unwrap_or_else(|| {
                let key = prefs.minimax_api_key.as_deref().unwrap_or("");
                let u = usage::minimax_usage(key);
                usage::set_cached_usage("minimax", u.clone());
                u
            })
    } else {
        let key = prefs.minimax_api_key.as_deref().unwrap_or("");
        usage::minimax_usage(key)
    };

    Ok(vec![anthropic, codex, minimax])
}

#[command]
pub async fn set_minimax_api_key(key: Option<String>) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    prefs.minimax_api_key = key;
    preferences::save(prefs)?;
    usage::invalidate_cache();
    Ok(())
}