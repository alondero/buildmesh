//! Tests for the legacy `preferences.json` read migration (ADR-0025).

use super::super::migrations::migrate_prefs_json;
use super::with_temp_dir;
use crate::agent::provider::compatibility::WireApi;
use crate::preferences::{
    load, save, ApiSurface, AppPreferences, ProviderAccount,
};

#[test]
fn provider_accounts_migrates_stored_kimi_via_claude_into_first_class_kimi() {
    with_temp_dir(|_| {
        save(AppPreferences {
            provider_accounts: vec![
                ProviderAccount {
                    id: "kimi-via-claude".to_string(),
                    name: "Kimi (via Claude)".to_string(),
                    enabled: true,
                    billing_mode: crate::preferences::BillingMode::PayAsYouGo,
                    claude_compatible: true,
                    api_key: Some("key-from-companion".to_string()),
                },
            ],
            ..Default::default()
        })
        .unwrap();
        let accounts = crate::preferences::provider_accounts();
        assert!(!accounts.iter().any(|a| a.id == "kimi-via-claude"));
        let kimi = accounts.iter().find(|a| a.id == "kimi").unwrap();
        assert_eq!(kimi.api_key.as_deref(), Some("key-from-companion"));
    });
}

#[test]
fn migrate_legacy_account_endpoint_into_claude_pairing() {
    with_temp_dir(|tmp| {
        // Write the prefs file directly (bypassing save/CACHE) so that the
        // migration runs on the next `load()` call.
        std::fs::write(
            tmp.join("preferences.json"),
            r#"{"provider_accounts": [{"id": "minimax", "name": "MiniMax", "enabled": true, "billing_mode": "pay_as_you_go", "claude_compatible": true, "api_key": "sk-test"}], "provider_pairings": []}"#,
        ).unwrap();
        let pairings = crate::preferences::provider_pairings();
        let mm = pairings
            .iter()
            .find(|p| p.provider_id == "minimax")
            .expect("minimax pairing");
        assert_eq!(mm.surface, ApiSurface::Anthropic);
        assert_eq!(
            mm.base_url.as_deref(),
            Some("https://api.minimax.io/anthropic")
        );
    });
}

#[test]
fn migrate_prefs_json_pure_creates_pairing_and_strips_fields() {
    let mut value = serde_json::json!({
        "provider_accounts": [
            {
                "id": "minimax",
                "name": "MiniMax",
                "enabled": true,
                "api_key": "sk-test",
                "billing_mode": "pay_as_you_go",
                "claude_compatible": true,
                "base_url": "https://api.minimax.io/anthropic",
                "model_tiers": {"default": "MiniMax-M3[1m]"}
            }
        ],
        "provider_pairings": [],
        "harness_profiles": []
    });
    let changed = migrate_prefs_json(&mut value);
    assert!(changed);
    let accounts = value.get("provider_accounts").unwrap().as_array().unwrap();
    assert!(!accounts[0].as_object().unwrap().contains_key("base_url"));
    assert!(!accounts[0].as_object().unwrap().contains_key("model_tiers"));
    assert_eq!(
        value
            .get("ad0025_account_pairings_migrated")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[test]
fn migrate_prefs_json_does_not_auto_pair_after_flag_set() {
    let mut value = serde_json::json!({
        "ad0025_account_pairings_migrated": true,
        "provider_accounts": [
            {
                "id": "minimax",
                "name": "MiniMax",
                "enabled": true,
                "api_key": "sk-test",
                "billing_mode": "pay_as_you_go",
                "claude_compatible": true
            }
        ],
        "provider_pairings": [],
        "harness_profiles": []
    });
    let changed = migrate_prefs_json(&mut value);
    assert!(!changed);
    let pairings = value
        .get("provider_pairings")
        .unwrap()
        .as_array()
        .unwrap();
    assert!(pairings.is_empty());
}

#[test]
fn migrate_prefs_json_skips_keyed_generic_without_endpoint() {
    let mut value = serde_json::json!({
        "provider_accounts": [
            {
                "id": "my-generic",
                "name": "Custom",
                "enabled": true,
                "api_key": "sk-test",
                "billing_mode": "pay_as_you_go",
                "claude_compatible": true
            }
        ],
        "provider_pairings": [],
        "harness_profiles": []
    });
    let changed = migrate_prefs_json(&mut value);
    assert!(changed);
    let pairings = value
        .get("provider_pairings")
        .unwrap()
        .as_array()
        .unwrap();
    assert!(pairings.is_empty());
}

#[test]
fn load_full_round_trip_includes_load_completed() {
    with_temp_dir(|tmp| {
        // Write a real-looking prefs file that triggers the migration.
        std::fs::write(
            tmp.join("preferences.json"),
            r#"{"provider_accounts": [{"id": "anthropic", "name": "Anthropic", "enabled": true, "billing_mode": "plan", "claude_compatible": false, "api_key": null}], "provider_pairings": []}"#,
        ).unwrap();
        let loaded = load().unwrap();
        assert!(loaded.ad0025_account_pairings_migrated);
    });
}

#[test]
fn pairing_compatibility_rejects_unverified_minimax_alias() {
    use crate::preferences::{pairing_compatibility, ProviderPairing};
    let pairing = ProviderPairing {
        harness_id: "codex".to_string(),
        provider_id: "minimax".to_string(),
        surface: ApiSurface::OpenAI,
        base_url: Some("https://api.minimax.io/v1".to_string()),
        model_tiers: crate::preferences::ModelTiers {
            default: Some("MiniMax-M3[1m]".to_string()),
            ..Default::default()
        },
    };
    let decision = pairing_compatibility(&pairing);
    assert!(!decision.compatible);
    assert!(decision.reason.is_some());
    // The descriptor surfaces WireApi::Responses for minimax non-M3.
    let descriptor = crate::preferences::endpoint_model_descriptor(&pairing);
    assert_eq!(descriptor.wire_api, WireApi::Responses);
}