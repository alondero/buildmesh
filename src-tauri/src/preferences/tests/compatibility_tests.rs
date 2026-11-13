//! Tests for the compatibility layer — env translation and harness-default
//! validation.
//!
//! Issue #1482: every test that touches preferences constructs its own
//! [`super::super::store::PreferencesStore`] over a temporary directory (via
//! the shared [`with_store`] fixture) and passes the handle into the code
//! under test. No test swaps a process-wide current store.

use super::super::compatibility::{
    harness_default_for, normalize_harness_default, preflight_resolve_provider_env_from,
    remove_harness_default, resolve_provider_env_from, upsert_harness_default,
    validate_harness_default,
};
use super::super::model::{HarnessConfigValue, ModelTiers, ProviderPairing};
use super::with_store;
use crate::agent::provider::SpawnOptionId;
use crate::preferences::ApiSurface;
use crate::preferences::{AppPreferences, ProviderAccount};

#[test]
fn preflight_pairing_env_passes_for_no_pairing() {
    // Re-implement the same pure logic via the public surface to keep
    // coverage parity with the original tests module.
    with_store(|store| {
        let result = preflight_resolve_provider_env_from(store, "claude:minimax");
        assert!(result.is_ok());
    });
}

#[test]
fn provider_account_env_injects_from_stored_claude_pairing() {
    with_store(|store| {
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
        store.save(prefs).unwrap();
        let env = resolve_provider_env_from(store, "minimax");
        let env_map: std::collections::HashMap<_, _> = env.into_iter().collect();
        assert_eq!(
            env_map.get("ANTHROPIC_AUTH_TOKEN").map(|s| s.as_str()),
            Some("sk-test")
        );
        assert_eq!(
            env_map.get("ANTHROPIC_BASE_URL").map(|s| s.as_str()),
            Some("https://api.minimax.io/anthropic")
        );
    });
}

#[test]
fn resolve_provider_env_reads_pairing_for_bare_id() {
    with_store(|store| {
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
        store.save(prefs).unwrap();
        let env = resolve_provider_env_from(store, "minimax");
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
    with_store(|store| {
        assert!(resolve_provider_env_from(store, "anthropic").is_empty());
        assert!(resolve_provider_env_from(store, "totally-unknown").is_empty());
    });
}

#[test]
fn resolve_provider_env_proxies_minimax_via_codex_with_openai_vars() {
    with_store(|store| {
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
        store.save(prefs).unwrap();
        let env = resolve_provider_env_from(store, "codex:minimax");
        let env_map: std::collections::HashMap<_, _> = env.into_iter().collect();
        assert_eq!(
            env_map.get("OPENAI_BASE_URL").map(|s| s.as_str()),
            Some("https://api.minimax.io/v1")
        );
        assert_eq!(
            env_map.get("OPENAI_API_KEY").map(|s| s.as_str()),
            Some("sk-test")
        );
    });
}

#[test]
fn resolve_provider_env_composite_without_stored_pairing_is_empty() {
    with_store(|store| {
        let mut prefs = AppPreferences::default();
        prefs.provider_accounts.push(ProviderAccount {
            id: "minimax".to_string(),
            name: "MiniMax".to_string(),
            enabled: true,
            billing_mode: crate::preferences::BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: Some("sk-test".to_string()),
        });
        store.save(prefs).unwrap();
        let env = resolve_provider_env_from(store, "codex:minimax");
        assert!(
            env.is_empty(),
            "expected empty env for unstored pairing, got {:?}",
            env
        );
    });
}

#[test]
fn resolve_provider_env_composite_uses_stored_pairing_tiers() {
    with_store(|store| {
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
        store.save(prefs).unwrap();
        let env = resolve_provider_env_from(store, "claude:minimax");
        let env_map: std::collections::HashMap<_, _> = env.into_iter().collect();
        assert_eq!(
            env_map.get("ANTHROPIC_MODEL").map(|s| s.as_str()),
            Some("MiniMax-M3[1m]")
        );
        assert_eq!(
            env_map
                .get("ANTHROPIC_SMALL_FAST_MODEL")
                .map(|s| s.as_str()),
            Some("MiniMax-M2.7")
        );
    });
}

#[test]
fn preflight_pairing_env_fails_for_custom_endpoint_with_empty_default_tier() {
    with_store(|store| {
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
        store.save(prefs).unwrap();
        let err = preflight_resolve_provider_env_from(store, "claude:openrouter").unwrap_err();
        assert!(err.contains("Default model"));
    });
}

#[test]
fn preflight_pairing_env_passes_for_custom_endpoint_with_default_tier_filled() {
    with_store(|store| {
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
        store.save(prefs).unwrap();
        assert!(preflight_resolve_provider_env_from(store, "claude:openrouter").is_ok());
    });
}

#[test]
fn normalize_harness_default_trims_blanks_and_whitespace() {
    let raw = HarnessConfigValue {
        model: Some("   ".to_string()),
        effort: Some("  high  ".to_string()),
        permission_mode: None,
        agent: None,
    };
    let norm = normalize_harness_default(raw);
    assert_eq!(norm.model, None);
    assert_eq!(norm.effort.as_deref(), Some("high"));
}

#[test]
fn harness_default_for_reads_back_stored_value() {
    let mut prefs = AppPreferences::default();
    prefs.harness_defaults.insert(
        "claude".to_string(),
        HarnessConfigValue {
            model: Some("opus-4-1".to_string()),
            effort: None,
            permission_mode: None,
            agent: None,
        },
    );
    let read = harness_default_for(&prefs, &SpawnOptionId::from("claude")).unwrap();
    assert_eq!(read.model.as_deref(), Some("opus-4-1"));
}

/// Issue #2038: a Proxied Spawn Option reads the harness half's entry (so
/// native and proxied rows share one map key) but never inherits the native
/// model/effort — those name a model on the harness's own endpoint.
#[test]
fn harness_default_for_proxy_drops_native_model_and_effort() {
    let mut prefs = AppPreferences::default();
    prefs.harness_defaults.insert(
        "codex".to_string(),
        HarnessConfigValue {
            model: Some("gpt-5.1-codex".to_string()),
            effort: Some("high".to_string()),
            permission_mode: None,
            agent: None,
        },
    );
    let read = harness_default_for(&prefs, &SpawnOptionId::from("codex:minimax"));
    assert_eq!(read, None);
    // The same stored default still applies to the native row.
    let native = harness_default_for(&prefs, &SpawnOptionId::from("codex")).unwrap();
    assert_eq!(native.model.as_deref(), Some("gpt-5.1-codex"));
    assert_eq!(native.effort.as_deref(), Some("high"));
}

/// The permission mode is a harness-CLI concern, not an endpoint concern, so
/// a proxied spawn keeps it — only model/effort are endpoint-scoped.
#[test]
fn harness_default_for_proxy_keeps_permission_mode() {
    let mut prefs = AppPreferences::default();
    prefs.harness_defaults.insert(
        "claude".to_string(),
        HarnessConfigValue {
            model: Some("opus-4-1".to_string()),
            effort: None,
            permission_mode: Some("prompt".to_string()),
            agent: None,
        },
    );
    let read = harness_default_for(&prefs, &SpawnOptionId::from("claude:minimax")).unwrap();
    assert_eq!(read.model, None);
    assert_eq!(read.effort, None);
    assert_eq!(read.permission_mode.as_deref(), Some("prompt"));
}

/// No stored entry for the harness half is absent for both row shapes — the
/// proxy exclusion is not what makes an absent default absent.
#[test]
fn harness_default_for_unset_harness_is_absent_for_both_row_shapes() {
    let prefs = AppPreferences::default();
    assert!(harness_default_for(&prefs, &SpawnOptionId::from("codex")).is_none());
    assert!(harness_default_for(&prefs, &SpawnOptionId::from("codex:minimax")).is_none());
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
    with_store(|store| {
        let mut prefs = AppPreferences::default();
        upsert_harness_default(
            &mut prefs,
            "claude",
            HarnessConfigValue {
                model: Some("opus-4-1".to_string()),
                effort: None,
                permission_mode: None,
                agent: None,
            },
        )
        .unwrap();
        store.save(prefs).unwrap();
        let stored = store.load().unwrap();
        assert_eq!(
            stored
                .harness_defaults
                .get("claude")
                .and_then(|v| v.model.clone()),
            Some("opus-4-1".to_string())
        );
    });
}

#[test]
fn upsert_harness_default_removes_entry_when_all_fields_blank() {
    let mut prefs = AppPreferences::default();
    prefs.harness_defaults.insert(
        "claude".to_string(),
        HarnessConfigValue {
            model: Some("opus-4-1".to_string()),
            effort: None,
            permission_mode: None,
            agent: None,
        },
    );
    upsert_harness_default(
        &mut prefs,
        "claude",
        HarnessConfigValue {
            model: Some("   ".to_string()),
            effort: None,
            permission_mode: None,
            agent: None,
        },
    )
    .unwrap();
    assert!(!prefs.harness_defaults.contains_key("claude"));
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
            permission_mode: None,
            agent: None,
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
            permission_mode: None,
            agent: None,
        },
    )
    .unwrap_err();
    assert!(err.contains("does not support"));
}

/// Issue #2151: a permission mode that names one of the harness's own
/// modes validates and round-trips; an unknown mode — or any mode for a
/// harness with no modes (terminal) — is refused at the write boundary.
#[test]
fn upsert_harness_default_validates_permission_mode() {
    let mut prefs = AppPreferences::default();
    upsert_harness_default(
        &mut prefs,
        "claude",
        HarnessConfigValue {
            model: None,
            effort: None,
            permission_mode: Some("prompt".to_string()),
            agent: None,
        },
    )
    .expect("prompt is a Claude Code mode");
    assert_eq!(
        prefs
            .harness_defaults
            .get("claude")
            .and_then(|v| v.permission_mode.clone()),
        Some("prompt".to_string())
    );

    let err = upsert_harness_default(
        &mut prefs,
        "claude",
        HarnessConfigValue {
            model: None,
            effort: None,
            permission_mode: Some("turbo".to_string()),
            agent: None,
        },
    )
    .unwrap_err();
    assert!(err.contains("permission mode"), "{err}");

    let err = upsert_harness_default(
        &mut prefs,
        "terminal",
        HarnessConfigValue {
            model: None,
            effort: None,
            permission_mode: Some("prompt".to_string()),
            agent: None,
        },
    )
    .unwrap_err();
    assert!(err.contains("permission mode"), "{err}");
}

/// Issue #1552 / #2716: the agent slot is capability-gated like model/
/// effort — OpenCode and OpenCode 2 accept it, every other harness
/// refuses it at the write boundary. Whitespace collapses to absent;
/// a space-bearing name fails before spawn (adapter validation).
#[test]
fn upsert_harness_default_validates_agent() {
    let mut prefs = AppPreferences::default();
    for harness in ["opencode", "opencode2"] {
        upsert_harness_default(
            &mut prefs,
            harness,
            HarnessConfigValue {
                model: None,
                effort: None,
                permission_mode: None,
                agent: Some("build".to_string()),
            },
        )
        .unwrap_or_else(|e| panic!("{harness} must accept an agent: {e}"));
        assert_eq!(
            prefs
                .harness_defaults
                .get(harness)
                .and_then(|v| v.agent.clone()),
            Some("build".to_string())
        );
    }
    // Whitespace-only collapses to absent (removes the sparse entry).
    let mut prefs = AppPreferences::default();
    upsert_harness_default(
        &mut prefs,
        "opencode",
        HarnessConfigValue {
            model: None,
            effort: None,
            permission_mode: None,
            agent: Some("   ".to_string()),
        },
    )
    .unwrap();
    assert!(!prefs.harness_defaults.contains_key("opencode"));
    // Harnesses without the control reject any value.
    for harness in ["claude", "codex", "terminal"] {
        let err = upsert_harness_default(
            &mut prefs,
            harness,
            HarnessConfigValue {
                model: None,
                effort: None,
                permission_mode: None,
                agent: Some("build".to_string()),
            },
        )
        .unwrap_err();
        assert!(
            err.contains("does not support an agent"),
            "{harness}: {err}"
        );
    }
    // Malformed names fail at the adapter grammar (before spawn).
    let err = upsert_harness_default(
        &mut prefs,
        "opencode",
        HarnessConfigValue {
            model: None,
            effort: None,
            permission_mode: None,
            agent: Some("has space".to_string()),
        },
    )
    .unwrap_err();
    assert!(err.contains("agent"), "{err}");
    // A proxied row keeps the agent (harness-side persona, like
    // permission_mode) while dropping the native model.
    let mut prefs = AppPreferences::default();
    prefs.harness_defaults.insert(
        "opencode".to_string(),
        HarnessConfigValue {
            model: Some("anthropic/claude-sonnet-4-5".to_string()),
            effort: None,
            permission_mode: None,
            agent: Some("build".to_string()),
        },
    );
    let read = harness_default_for(&prefs, &SpawnOptionId::from("opencode:minimax")).unwrap();
    assert_eq!(read.agent.as_deref(), Some("build"));
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
            permission_mode: None,
            agent: None,
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
            permission_mode: None,
            agent: None,
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
            permission_mode: None,
            agent: None,
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
            permission_mode: None,
            agent: None,
        },
    )
    .unwrap();
}

#[test]
fn failed_upsert_harness_default_leaves_cache_unchanged() {
    with_store(|_| {
        let mut prefs = AppPreferences::default();
        prefs.harness_defaults.insert(
            "claude".to_string(),
            HarnessConfigValue {
                model: Some("opus-4-1".to_string()),
                effort: None,
                permission_mode: None,
                agent: None,
            },
        );
        let err = upsert_harness_default(
            &mut prefs,
            "totally-unknown",
            HarnessConfigValue {
                model: Some("m".to_string()),
                effort: None,
                permission_mode: None,
                agent: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("unknown"));
        assert_eq!(
            prefs
                .harness_defaults
                .get("claude")
                .and_then(|v| v.model.clone()),
            Some("opus-4-1".to_string())
        );
    });
}

#[test]
fn old_prefs_file_without_harness_defaults_loads_as_empty_map() {
    with_store(|store| {
        std::fs::write(
            store.profile_dir().join("preferences.json"),
            r#"{"default_provider": null}"#,
        )
        .unwrap();
        let loaded = store.load().unwrap();
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
            permission_mode: None,
            agent: None,
        },
    )
    .unwrap();
    assert_eq!(normalized.model.as_deref(), Some("opus-4-1"));
    assert_eq!(normalized.effort.as_deref(), Some("high"));
}
