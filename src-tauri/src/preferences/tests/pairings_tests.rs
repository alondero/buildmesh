//! Tests for the resolver::pairings submodule — pairing upsert/remove,
//! proxied-provider ordering, attach-form defaults.

use super::with_temp_dir;
use crate::preferences::{
    AppPreferences, ModelTiers, ProviderPairing, ApiSurface,
};

#[test]
fn upsert_and_remove_provider_pairing_by_harness_provider_key() {
    let mut prefs = AppPreferences::default();
    crate::preferences::upsert_provider_pairing(
        &mut prefs,
        ProviderPairing {
            harness_id: "claude".to_string(),
            provider_id: "minimax".to_string(),
            surface: ApiSurface::Anthropic,
            base_url: Some("https://api.minimax.io/anthropic".to_string()),
            model_tiers: ModelTiers::default(),
        },
    );
    assert_eq!(prefs.provider_pairings.len(), 1);
    // Replace in place.
    crate::preferences::upsert_provider_pairing(
        &mut prefs,
        ProviderPairing {
            harness_id: "claude".to_string(),
            provider_id: "minimax".to_string(),
            surface: ApiSurface::OpenAI,
            base_url: Some("https://api.minimax.io/v1".to_string()),
            model_tiers: ModelTiers::default(),
        },
    );
    assert_eq!(prefs.provider_pairings.len(), 1);
    assert_eq!(prefs.provider_pairings[0].surface, ApiSurface::OpenAI);
    crate::preferences::remove_provider_pairing(&mut prefs, "claude", "minimax");
    assert!(prefs.provider_pairings.is_empty());
    // The detach decision is recorded separately from recipe deletions so
    // reconcile's route materialization does not recreate the pairing on
    // the following save — while retired-id alias migration still heals.
    assert!(prefs.detached_provider_routes.contains(&"claude:minimax".to_string()));
    assert!(!prefs.deleted_launch_configurations.contains(&"launch/claude:minimax".to_string()));
}

#[test]
fn set_proxied_provider_order_round_trips() {
    with_temp_dir(|_| {
        crate::preferences::set_proxied_provider_order(
            "claude".to_string(),
            vec!["minimax".to_string(), "kimi".to_string()],
        )
        .unwrap();
        let stored = super::super::storage::load().unwrap().proxied_provider_order;
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].harness_id, "claude");
        assert_eq!(
            stored[0].provider_ids,
            vec!["minimax".to_string(), "kimi".to_string()]
        );
    });
}

#[test]
fn set_proxied_provider_order_normalises_empty_to_drop_entry() {
    with_temp_dir(|_| {
        crate::preferences::set_proxied_provider_order(
            "claude".to_string(),
            vec!["minimax".to_string()],
        )
        .unwrap();
        crate::preferences::set_proxied_provider_order("claude".to_string(), vec![]).unwrap();
        let stored = super::super::storage::load().unwrap().proxied_provider_order;
        assert!(stored.is_empty());
    });
}

#[test]
fn proxied_order_for_returns_none_when_harness_unset() {
    with_temp_dir(|_| {
        assert!(crate::preferences::proxied_order_for("claude").is_none());
    });
}

/// Issue #1935 — the one-call attach-picker map is a faithful hoist of the
/// per-harness resolver, and it is keyed by every harness the Settings pane can
/// render: the effective profiles *and* the harness ids that only reach the pane
/// through a stored pairing.
#[test]
fn compatible_providers_by_harness_covers_every_renderable_harness() {
    with_temp_dir(|_| {
        let mut prefs = crate::preferences::load().unwrap();
        // A keyed generic account is what makes a harness's attach picker
        // non-empty, so the map must actually carry one.
        crate::preferences::upsert_provider_account(
            &mut prefs,
            crate::preferences::ProviderAccount {
                id: "deepseek".to_string(),
                name: "DeepSeek".to_string(),
                enabled: true,
                billing_mode: crate::preferences::BillingMode::PayAsYouGo,
                claude_compatible: true,
                api_key: Some("sk-test".to_string()),
            },
        );
        prefs.harness_profiles.push(crate::preferences::HarnessProfile {
            id: "claude".to_string(),
            name: "Claude Code".to_string(),
            harness: "anthropic".to_string(),
            runtime: None,
            wsl_distro: None,
            executable: None,
        });
        // A route stored against a harness that has no profile of its own — it
        // still renders as a row, so it still needs a picker entry.
        crate::preferences::upsert_provider_pairing(
            &mut prefs,
            ProviderPairing {
                harness_id: "grok".to_string(),
                provider_id: "deepseek".to_string(),
                surface: ApiSurface::Anthropic,
                base_url: Some("https://api.deepseek.com/anthropic".to_string()),
                model_tiers: ModelTiers::default(),
            },
        );
        // Same for a saved recipe: `configuration_menu` synthesises a row for
        // a configuration whose harness has no profile, and that row must not
        // end up with an empty picker.
        prefs
            .spawn_configurations
            .push(crate::preferences::spawn_configurations::SpawnConfiguration {
                id: "cfg-codex".to_string(),
                name: "Codex via DeepSeek".to_string(),
                spawn_option_id: "codex:deepseek".to_string(),
                ..Default::default()
            });
        crate::preferences::save(prefs).unwrap();

        let map = crate::preferences::compatible_providers_by_harness();

        // Every entry is the single-harness computation, unchanged.
        for (harness_id, accounts) in &map {
            assert_eq!(
                accounts,
                &crate::preferences::compatible_providers_for_harness(harness_id),
                "map entry for '{harness_id}' diverged from the per-harness resolver",
            );
        }
        // The profile-backed harness carries the keyed account it can proxy.
        let claude = map
            .get("claude")
            .expect("profile-backed harness missing from the map");
        assert_eq!(
            claude.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            vec!["deepseek"],
        );
        // A harness that speaks no proxy surface is present with the honest
        // empty list, not a missing key.
        assert_eq!(map.get("terminal"), Some(&vec![]));
        // The pairing-only harness id is a key even though `harness_profiles()`
        // never mentioned it.
        assert!(
            map.contains_key("grok"),
            "a harness reachable only through a stored pairing must still have a picker entry",
        );
        // So is the harness half of a saved recipe's spawn option.
        assert!(
            map.contains_key("codex"),
            "a harness reachable only through a saved launch configuration must still have a picker entry",
        );
    });
}