//! Tests for the resolver::pairings submodule — pairing upsert/remove,
//! proxied-provider ordering, attach-form defaults.

use super::with_temp_dir;
use crate::preferences::{ApiSurface, AppPreferences, ModelTiers, ProviderPairing};

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
}

#[test]
fn set_proxied_provider_order_round_trips() {
    with_temp_dir(|_| {
        crate::preferences::set_proxied_provider_order(
            "claude".to_string(),
            vec!["minimax".to_string(), "kimi".to_string()],
        )
        .unwrap();
        let stored = super::super::storage::load()
            .unwrap()
            .proxied_provider_order;
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
        let stored = super::super::storage::load()
            .unwrap()
            .proxied_provider_order;
        assert!(stored.is_empty());
    });
}

#[test]
fn proxied_order_for_returns_none_when_harness_unset() {
    with_temp_dir(|_| {
        assert!(crate::preferences::proxied_order_for("claude").is_none());
    });
}
