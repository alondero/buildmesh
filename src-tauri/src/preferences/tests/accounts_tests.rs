//! Tests for the resolver::accounts submodule — effective-account resolution,
//! API key lookups, mutators (upsert/remove), set_account_key_if_absent.

use super::super::model::ProviderAccount;
use super::super::resolver::effective_pairings;
use super::with_temp_dir;
use crate::preferences::{
    AppPreferences, ModelTiers, ProviderPairing, ApiSurface,
};

#[test]
fn effective_pairings_returns_only_stored_proxiable() {
    let accounts = vec![ProviderAccount {
        id: "minimax".to_string(),
        name: "MiniMax".to_string(),
        enabled: true,
        billing_mode: crate::preferences::BillingMode::PayAsYouGo,
        claude_compatible: true,
        api_key: Some("sk".to_string()),
    }];
    let stored = vec![ProviderPairing {
        harness_id: "claude".to_string(),
        provider_id: "minimax".to_string(),
        surface: ApiSurface::Anthropic,
        base_url: Some("https://api.minimax.io/anthropic".to_string()),
        model_tiers: ModelTiers::default(),
    }];
    let eff = effective_pairings(&accounts, &stored);
    assert_eq!(eff.len(), 1);
    assert_eq!(eff[0].provider_id, "minimax");
}

#[test]
fn effective_pairings_skips_unkeyed_disabled_or_self_auth_accounts() {
    let accounts = vec![
        ProviderAccount {
            // keyed + enabled + claude_compatible → proxiable
            id: "minimax".to_string(),
            name: "MiniMax".to_string(),
            enabled: true,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk".to_string()),
        },
        ProviderAccount {
            // disabled → not proxiable
            id: "kimi".to_string(),
            name: "Kimi".to_string(),
            enabled: false,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk".to_string()),
        },
        ProviderAccount {
            // keyed but not claude_compatible (self-auth built-in) → not proxiable
            id: "anthropic".to_string(),
            name: "Anthropic".to_string(),
            enabled: true,
            billing_mode: crate::preferences::BillingMode::Plan,
            claude_compatible: false,
            api_key: Some("sk".to_string()),
        },
        ProviderAccount {
            // claude_compatible + enabled but no key → not proxiable
            id: "openrouter".to_string(),
            name: "OpenRouter".to_string(),
            enabled: true,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: None,
        },
    ];
    let stored = vec![
        ProviderPairing {
            harness_id: "claude".to_string(),
            provider_id: "minimax".to_string(),
            surface: ApiSurface::Anthropic,
            base_url: Some("https://api.minimax.io/anthropic".to_string()),
            model_tiers: ModelTiers::default(),
        },
        ProviderPairing {
            harness_id: "claude".to_string(),
            provider_id: "kimi".to_string(),
            surface: ApiSurface::Anthropic,
            base_url: Some("https://api.moonshot.ai/anthropic".to_string()),
            model_tiers: ModelTiers::default(),
        },
        ProviderPairing {
            harness_id: "claude".to_string(),
            provider_id: "anthropic".to_string(),
            surface: ApiSurface::Anthropic,
            base_url: None,
            model_tiers: ModelTiers::default(),
        },
        ProviderPairing {
            harness_id: "claude".to_string(),
            provider_id: "openrouter".to_string(),
            surface: ApiSurface::Anthropic,
            base_url: Some("https://openrouter.ai/api".to_string()),
            model_tiers: ModelTiers::default(),
        },
    ];
    let eff = effective_pairings(&accounts, &stored);
    let ids: Vec<&str> = eff.iter().map(|p| p.provider_id.as_str()).collect();
    assert_eq!(ids, vec!["minimax"]);
}

#[test]
fn minimax_api_key_resolved_prefers_account_then_legacy_field() {
    with_temp_dir(|_| {
        // Legacy field only
        super::super::storage::update(|p| {
            p.minimax_api_key = Some("legacy".to_string());
        })
        .unwrap();
        assert_eq!(
            crate::preferences::minimax_api_key_resolved(),
            Some("legacy".to_string())
        );
        // Account wins over legacy
        let mut prefs = super::super::storage::load().unwrap();
        prefs.provider_accounts.push(ProviderAccount {
            id: "minimax".to_string(),
            name: "MiniMax".to_string(),
            enabled: true,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("account-key".to_string()),
        });
        super::super::storage::save(prefs).unwrap();
        assert_eq!(
            crate::preferences::minimax_api_key_resolved(),
            Some("account-key".to_string())
        );
    });
}

#[test]
fn provider_accounts_folds_legacy_minimax_key_into_effective_snapshot() {
    with_temp_dir(|_| {
        super::super::storage::update(|prefs| {
            prefs.minimax_api_key = Some("legacy".to_string());
            prefs.provider_accounts.push(ProviderAccount {
                id: "minimax".to_string(),
                name: "MiniMax".to_string(),
                enabled: true,
                billing_mode: crate::preferences::BillingMode::PayAsYouGo,
                claude_compatible: true,
                api_key: None,
            });
        })
        .unwrap();

        let accounts = crate::preferences::provider_accounts();
        let minimax = accounts
            .iter()
            .find(|account| account.id == "minimax")
            .expect("materialized MiniMax account");
        assert_eq!(minimax.api_key.as_deref(), Some("legacy"));
    });
}

#[test]
fn upsert_account_stores_it_without_a_paired_profile() {
    let mut prefs = AppPreferences::default();
    crate::preferences::upsert_provider_account(
        &mut prefs,
        ProviderAccount {
            id: "deepseek".to_string(),
            name: "DeepSeek".to_string(),
            enabled: true,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk".to_string()),
        },
    );
    assert_eq!(prefs.provider_accounts.len(), 1);
    assert_eq!(prefs.provider_pairings.len(), 0);
}

#[test]
fn upsert_existing_account_overrides_in_place() {
    let mut prefs = AppPreferences::default();
    crate::preferences::upsert_provider_account(
        &mut prefs,
        ProviderAccount {
            id: "deepseek".to_string(),
            name: "DeepSeek".to_string(),
            enabled: true,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-1".to_string()),
        },
    );
    crate::preferences::upsert_provider_account(
        &mut prefs,
        ProviderAccount {
            id: "deepseek".to_string(),
            name: "DeepSeek".to_string(),
            enabled: false,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-2".to_string()),
        },
    );
    assert_eq!(prefs.provider_accounts.len(), 1);
    assert!(!prefs.provider_accounts[0].enabled);
    assert_eq!(prefs.provider_accounts[0].api_key.as_deref(), Some("sk-2"));
}

#[test]
fn remove_account_drops_it() {
    let mut prefs = AppPreferences::default();
    crate::preferences::upsert_provider_account(
        &mut prefs,
        ProviderAccount {
            id: "deepseek".to_string(),
            name: "DeepSeek".to_string(),
            enabled: true,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk".to_string()),
        },
    );
    crate::preferences::remove_provider_account(&mut prefs, "deepseek");
    assert!(prefs.provider_accounts.is_empty());
}

#[test]
fn set_account_key_if_absent_only_fills_an_empty_key() {
    let mut prefs = AppPreferences::default();
    // Already keyed → refuse overwrite.
    crate::preferences::upsert_provider_account(
        &mut prefs,
        ProviderAccount {
            id: "deepseek".to_string(),
            name: "DeepSeek".to_string(),
            enabled: true,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("existing".to_string()),
        },
    );
    let wrote = crate::preferences::set_account_key_if_absent(&mut prefs, "deepseek", "fresh");
    assert!(!wrote);
    assert_eq!(prefs.provider_accounts[0].api_key.as_deref(), Some("existing"));
    // Empty → fill.
    crate::preferences::remove_provider_account(&mut prefs, "deepseek");
    let wrote = crate::preferences::set_account_key_if_absent(&mut prefs, "deepseek", "fresh");
    assert!(wrote);
    assert_eq!(prefs.provider_accounts[0].api_key.as_deref(), Some("fresh"));
}
