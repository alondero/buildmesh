//! Tests for the compatibility layer — env translation and harness-default
//! validation.

use super::super::compatibility::{
    harness_default_for, normalize_harness_default, preflight_resolve_provider_env, remove_harness_default,
    resolve_provider_env, upsert_harness_default, validate_harness_default,
};
use super::super::model::{HarnessConfigValue, ModelTiers, ProviderPairing};
use super::super::storage::{load, save};
use super::with_temp_dir;
use crate::preferences::{
    AppPreferences, ProviderAccount,
};
use crate::preferences::ApiSurface;

#[test]
fn preflight_pairing_env_passes_for_no_pairing() {
    // Re-implement the same pure logic via the public surface to keep
    // coverage parity with the original tests module.
    let result =
        super::super::compatibility::preflight_resolve_provider_env("claude:minimax");
    assert!(result.is_ok());
}

#[test]
fn provider_account_env_injects_from_stored_claude_pairing() {
    with_temp_dir(|_| {
        let mut prefs = AppPreferences::default();
        prefs.provider_accounts.push(ProviderAccount {
            id: "minimax".to_string(),
            name: "MiniMax".to_string(),
            enabled: true,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-test".to_string()),
        });
        prefs.provider_pairings.push(ProviderPairing {
            harness_id: "claude".to_string(),
            provider_id: "minimax".to_string(),
            surface: ApiSurface::Anthropic,
            base_url: Some("https://api.minimax.io/anthropic".to_string()),
            model_tiers: ModelTiers::default(),
        });
        save(prefs).unwrap();
        let env = resolve_provider_env("minimax");
        let env_map: std::collections::HashMap<_, _> = env.into_iter().collect();
        assert_eq!(env_map.get("ANTHROPIC_AUTH_TOKEN").map(|s| s.as_str()), Some("sk-test"));
        assert_eq!(
            env_map.get("ANTHROPIC_BASE_URL").map(|s| s.as_str()),
            Some("https://api.minimax.io/anthropic")
        );
    });
}

#[test]
fn resolve_provider_env_reads_pairing_for_bare_id() {
    with_temp_dir(|_| {
        let mut prefs = AppPreferences::default();
        prefs.provider_accounts.push(ProviderAccount {
            id: "minimax".to_string(),
            name: "MiniMax".to_string(),
            enabled: true,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-test".to_string()),
        });
        prefs.provider_pairings.push(ProviderPairing {
            harness_id: "claude".to_string(),
            provider_id: "minimax".to_string(),
            surface: ApiSurface::Anthropic,
            base_url: Some("https://api.minimax.io/anthropic".to_string()),
            model_tiers: ModelTiers::default(),
        });
        save(prefs).unwrap();
        let env = resolve_provider_env("minimax");
        assert!(
            env.iter()
                .any(|(k, _)| k == "ANTHROPIC_BASE_URL" || k == "ANTHROPIC_AUTH_TOKEN"),
            "expected at least one ANTHROPIC_* env var, got {:?}",
            env
        );
    });
}

#[test]
fn resolve_provider_env_empty_for_anthropic_default_and_unknown() {
    with_temp_dir(|_| {
        assert!(resolve_provider_env("anthropic").is_empty());
        assert!(resolve_provider_env("totally-unknown").is_empty());
    });
}

#[test]
fn resolve_provider_env_proxies_minimax_via_codex_with_openai_vars() {
    with_temp_dir(|_| {
        let mut prefs = AppPreferences::default();
        prefs.provider_accounts.push(ProviderAccount {
            id: "minimax".to_string(),
            name: "MiniMax".to_string(),
            enabled: true,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-test".to_string()),
        });
        prefs.provider_pairings.push(ProviderPairing {
            harness_id: "codex".to_string(),
            provider_id: "minimax".to_string(),
            surface: ApiSurface::OpenAI,
            base_url: Some("https://api.minimax.io/v1".to_string()),
            model_tiers: ModelTiers::default(),
        });
        save(prefs).unwrap();
        let env = resolve_provider_env("codex:minimax");
        let env_map: std::collections::HashMap<_, _> = env.into_iter().collect();
        assert_eq!(env_map.get("OPENAI_BASE_URL").map(|s| s.as_str()), Some("https://api.minimax.io/v1"));
        assert_eq!(env_map.get("OPENAI_API_KEY").map(|s| s.as_str()), Some("sk-test"));
    });
}

#[test]
fn resolve_provider_env_composite_without_stored_pairing_is_empty() {
    with_temp_dir(|_| {
        let mut prefs = AppPreferences::default();
        prefs.provider_accounts.push(ProviderAccount {
            id: "minimax".to_string(),
            name: "MiniMax".to_string(),
            enabled: true,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-test".to_string()),
        });
        save(prefs).unwrap();
        let env = resolve_provider_env("codex:minimax");
        assert!(env.is_empty(), "expected empty env for unstored pairing, got {:?}", env);
    });
}

#[test]
fn resolve_provider_env_composite_uses_stored_pairing_tiers() {
    with_temp_dir(|_| {
        let mut prefs = AppPreferences::default();
        prefs.provider_accounts.push(ProviderAccount {
            id: "minimax".to_string(),
            name: "MiniMax".to_string(),
            enabled: true,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-test".to_string()),
        });
        prefs.provider_pairings.push(ProviderPairing {
            harness_id: "claude".to_string(),
            provider_id: "minimax".to_string(),
            surface: ApiSurface::Anthropic,
            base_url: Some("https://api.minimax.io/anthropic".to_string()),
            model_tiers: ModelTiers {
                default: Some("MiniMax-M3[1m]".to_string()),
                small_fast: Some("MiniMax-M2.7".to_string()),
                ..Default::default()
            },
        });
        save(prefs).unwrap();
        let env = resolve_provider_env("claude:minimax");
        let env_map: std::collections::HashMap<_, _> = env.into_iter().collect();
        assert_eq!(env_map.get("ANTHROPIC_MODEL").map(|s| s.as_str()), Some("MiniMax-M3[1m]"));
        assert_eq!(env_map.get("ANTHROPIC_SMALL_FAST_MODEL").map(|s| s.as_str()), Some("MiniMax-M2.7"));
    });
}

#[test]
fn preflight_pairing_env_fails_for_custom_endpoint_with_empty_default_tier() {
    with_temp_dir(|_| {
        let mut prefs = AppPreferences::default();
        prefs.provider_accounts.push(ProviderAccount {
            id: "openrouter".to_string(),
            name: "OpenRouter".to_string(),
            enabled: true,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-test".to_string()),
        });
        prefs.provider_pairings.push(ProviderPairing {
            harness_id: "claude".to_string(),
            provider_id: "openrouter".to_string(),
            surface: ApiSurface::Anthropic,
            base_url: Some("https://openrouter.ai/api".to_string()),
            model_tiers: ModelTiers::default(),
        });
        save(prefs).unwrap();
        let err = preflight_resolve_provider_env("claude:openrouter").unwrap_err();
        assert!(err.contains("Default model"));
    });
}

#[test]
fn preflight_pairing_env_passes_for_custom_endpoint_with_default_tier_filled() {
    with_temp_dir(|_| {
        let mut prefs = AppPreferences::default();
        prefs.provider_accounts.push(ProviderAccount {
            id: "openrouter".to_string(),
            name: "OpenRouter".to_string(),
            enabled: true,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-test".to_string()),
        });
        prefs.provider_pairings.push(ProviderPairing {
            harness_id: "claude".to_string(),
            provider_id: "openrouter".to_string(),
            surface: ApiSurface::Anthropic,
            base_url: Some("https://openrouter.ai/api".to_string()),
            model_tiers: ModelTiers {
                default: Some("anthropic/claude-3-5-sonnet-latest".to_string()),
                ..Default::default()
            },
        });
        save(prefs).unwrap();
        assert!(preflight_resolve_provider_env("claude:openrouter").is_ok());
    });
}

#[test]
fn normalize_harness_default_trims_blanks_and_whitespace() {
    let raw = HarnessConfigValue {
        model: Some("   ".to_string()),
        effort: Some("  high  ".to_string()),
    };
    let norm = normalize_harness_default(raw);
    assert_eq!(norm.model, None);
    assert_eq!(norm.effort.as_deref(), Some("high"));
}

#[test]
fn harness_default_for_reads_back_stored_value() {
    let mut prefs = AppPreferences::default();
    prefs
        .harness_defaults
        .insert("claude".to_string(), HarnessConfigValue {
            model: Some("opus-4-1".to_string()),
            effort: None,
        });
    let read = harness_default_for(&prefs, "claude").unwrap();
    assert_eq!(read.model.as_deref(), Some("opus-4-1"));
}

#[test]
fn remove_harness_default_is_idempotent() {
    let mut prefs = AppPreferences::default();
    remove_harness_default(&mut prefs, "claude");
    remove_harness_default(&mut prefs, "claude");
    assert!(prefs.harness_defaults.is_empty());
}

#[test]
fn upsert_harness_default_persists_then_round_trips() {
    with_temp_dir(|_| {
        let mut prefs = AppPreferences::default();
        upsert_harness_default(
            &mut prefs,
            "claude",
            HarnessConfigValue {
                model: Some("opus-4-1".to_string()),
                effort: None,
            },
        )
        .unwrap();
        save(prefs).unwrap();
        let stored = load().unwrap();
        assert_eq!(
            stored.harness_defaults.get("claude").and_then(|v| v.model.clone()),
            Some("opus-4-1".to_string())
        );
    });
}

#[test]
fn upsert_harness_default_removes_entry_when_all_fields_blank() {
    with_temp_dir(|_| {
        let mut prefs = AppPreferences::default();
        prefs.harness_defaults.insert(
            "claude".to_string(),
            HarnessConfigValue {
                model: Some("opus-4-1".to_string()),
                effort: None,
            },
        );
        upsert_harness_default(
            &mut prefs,
            "claude",
            HarnessConfigValue {
                model: Some("   ".to_string()),
                effort: None,
            },
        )
        .unwrap();
        assert!(!prefs.harness_defaults.contains_key("claude"));
    });
}

#[test]
fn upsert_harness_default_rejects_unknown_harness_id() {
    let mut prefs = AppPreferences::default();
    let err = upsert_harness_default(
        &mut prefs,
        "totally-unknown",
        HarnessConfigValue {
            model: Some("m".to_string()),
            effort: None,
        },
    )
    .unwrap_err();
    assert!(err.contains("unknown"));
}

#[test]
fn upsert_harness_default_rejects_effort_on_harness_without_effort_control() {
    let mut prefs = AppPreferences::default();
    let err = upsert_harness_default(
        &mut prefs,
        "terminal",
        HarnessConfigValue {
            model: None,
            effort: Some("high".to_string()),
        },
    )
    .unwrap_err();
    assert!(err.contains("does not support"));
}

#[test]
fn upsert_harness_default_accepts_model_only_on_non_effort_harness() {
    let mut prefs = AppPreferences::default();
    upsert_harness_default(
        &mut prefs,
        "terminal",
        HarnessConfigValue {
            model: Some("some-model".to_string()),
            effort: None,
        },
    )
    .unwrap();
}

#[test]
fn upsert_harness_default_rejects_effort_outside_vocabulary() {
    let mut prefs = AppPreferences::default();
    let err = upsert_harness_default(
        &mut prefs,
        "claude",
        HarnessConfigValue {
            model: None,
            effort: Some("extreme".to_string()),
        },
    )
    .unwrap_err();
    assert!(err.contains("not allowed"));
}

#[test]
fn upsert_harness_default_accepts_codex_xhigh() {
    let mut prefs = AppPreferences::default();
    upsert_harness_default(
        &mut prefs,
        "codex",
        HarnessConfigValue {
            model: None,
            effort: Some("xhigh".to_string()),
        },
    )
    .unwrap();
}

#[test]
fn upsert_harness_default_accepts_canonical_lowercase_via_capitalisation() {
    let mut prefs = AppPreferences::default();
    upsert_harness_default(
        &mut prefs,
        "claude",
        HarnessConfigValue {
            model: None,
            effort: Some("medium".to_string()),
        },
    )
    .unwrap();
}

#[test]
fn failed_upsert_harness_default_leaves_cache_unchanged() {
    with_temp_dir(|_| {
        let mut prefs = AppPreferences::default();
        prefs.harness_defaults.insert(
            "claude".to_string(),
            HarnessConfigValue {
                model: Some("opus-4-1".to_string()),
                effort: None,
            },
        );
        let err = upsert_harness_default(
            &mut prefs,
            "totally-unknown",
            HarnessConfigValue {
                model: Some("m".to_string()),
                effort: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("unknown"));
        assert_eq!(
            prefs.harness_defaults.get("claude").and_then(|v| v.model.clone()),
            Some("opus-4-1".to_string())
        );
    });
}

#[test]
fn old_prefs_file_without_harness_defaults_loads_as_empty_map() {
    with_temp_dir(|tmp| {
        std::fs::write(
            tmp.join("preferences.json"),
            r#"{"default_provider": null}"#,
        )
        .unwrap();
        let loaded = load().unwrap();
        assert!(loaded.harness_defaults.is_empty());
    });
}

#[test]
fn validate_harness_default_returns_normalized_value() {
    let normalized = validate_harness_default(
        "claude",
        HarnessConfigValue {
            model: Some(" opus-4-1  ".to_string()),
            effort: Some(" high ".to_string()),
        },
    )
    .unwrap();
    assert_eq!(normalized.model.as_deref(), Some("opus-4-1"));
    assert_eq!(normalized.effort.as_deref(), Some("high"));
}