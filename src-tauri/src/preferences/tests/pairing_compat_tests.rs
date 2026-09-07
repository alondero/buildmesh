//! Tests for the resolver::pairing_compat submodule.

use crate::preferences::{
    endpoint_model_descriptor, pairing_compatibility, ApiSurface, ModelTiers, ProviderPairing,
};

#[test]
fn pairing_compatibility_rejects_unverified_minimax_alias() {
    let pairing = ProviderPairing {
        harness_id: "codex".to_string(),
        provider_id: "minimax".to_string(),
        surface: ApiSurface::OpenAI,
        base_url: Some("https://api.minimax.io/v1".to_string()),
        model_tiers: ModelTiers {
            default: Some("MiniMax-M3[1m]".to_string()),
            ..Default::default()
        },
    };
    let decision = pairing_compatibility(&pairing);
    assert!(!decision.compatible);
    assert!(decision.reason.is_some());
    // The descriptor surfaces WireApi::Responses for minimax non-M3.
    let descriptor = endpoint_model_descriptor(&pairing);
    assert_eq!(
        descriptor.wire_api,
        crate::agent::provider::compatibility::WireApi::Responses
    );
}

#[test]
fn pairing_compatibility_accepts_known_anthropic_pairing() {
    let pairing = ProviderPairing {
        harness_id: "claude".to_string(),
        provider_id: "minimax".to_string(),
        surface: ApiSurface::Anthropic,
        base_url: Some("https://api.minimax.io/anthropic".to_string()),
        model_tiers: ModelTiers {
            default: Some("MiniMax-M3[1m]".to_string()),
            ..Default::default()
        },
    };
    let decision = pairing_compatibility(&pairing);
    assert!(decision.compatible);
}
