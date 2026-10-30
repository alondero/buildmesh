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

fn minimax_codex_pairing(model: &str) -> ProviderPairing {
    ProviderPairing {
        harness_id: "codex".to_string(),
        provider_id: "minimax".to_string(),
        surface: ApiSurface::OpenAI,
        base_url: Some("https://api.minimax.io/v1".to_string()),
        model_tiers: ModelTiers {
            default: Some(model.to_string()),
            ..Default::default()
        },
    }
}

/// MiniMax's Responses reference lists both models; Codex must accept each and
/// send reasoning metadata for each (an unknown model name otherwise drops it).
#[test]
fn pairing_compatibility_accepts_every_documented_minimax_responses_model() {
    for model in ["MiniMax-M3", "MiniMax-M3.1-Flash"] {
        let pairing = minimax_codex_pairing(model);
        let decision = pairing_compatibility(&pairing);
        assert!(decision.compatible, "{model}: {:?}", decision.reason);
        assert_eq!(
            endpoint_model_descriptor(&pairing).reasoning_effort,
            Some(true),
            "{model}"
        );
    }
}

#[test]
fn pairing_compatibility_rejection_names_every_accepted_minimax_model() {
    let decision = pairing_compatibility(&minimax_codex_pairing("MiniMax-M2.7"));
    assert!(!decision.compatible);
    let reason = decision.reason.expect("rejection carries a reason");
    assert!(reason.contains("MiniMax-M3.1-Flash"), "{reason}");
    assert!(reason.contains("MiniMax-M3'"), "{reason}");
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
