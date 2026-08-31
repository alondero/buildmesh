//! Pairing compatibility matching — the descriptor extractor, decision
//! calculator, and "could potentially match" predicate that drives attach
//! eligibility, verification, and the preflight gate.

use super::super::model::{ApiSurface, ProviderPairing};
use crate::agent::provider::compatibility::{
    self, CompatibilityDecision, EndpointModelDescriptor, ProviderAuthMode, WireApi,
};

/// Reconstruct the exact endpoint/model descriptor from persisted pairing
/// state. `ApiSurface::OpenAI` is deliberately not proof of Responses
/// compatibility: known Kimi rows are classified as Chat Completions, and
/// MiniMax's retired `[1m]` alias is left capability-incomplete.
pub fn endpoint_model_descriptor(pairing: &ProviderPairing) -> EndpointModelDescriptor {
    let endpoint = pairing.base_url.as_deref().unwrap_or("").trim().to_string();
    let model_id = pairing
        .model_tiers
        .default
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_string();
    let (wire_api, capabilities) = match pairing.surface {
        ApiSurface::Anthropic => (
            WireApi::AnthropicMessages,
            compatibility::complete_agent_capabilities(),
        ),
        ApiSurface::OpenAI if pairing.provider_id == "kimi" => (
            WireApi::ChatCompletions,
            compatibility::complete_agent_capabilities(),
        ),
        ApiSurface::OpenAI
            if pairing.provider_id == "minimax" && model_id != "MiniMax-M3" =>
        {
            (WireApi::Responses, Default::default())
        }
        ApiSurface::OpenAI => (
            WireApi::Responses,
            compatibility::complete_agent_capabilities(),
        ),
    };
    EndpointModelDescriptor {
        provider_id: pairing.provider_id.clone(),
        endpoint,
        wire_api,
        model_id,
        capabilities,
        auth_modes: vec![ProviderAuthMode::BearerEnv],
        context_window: None,
        reasoning_effort: None,
    }
}

/// One matcher drives attach eligibility, verification, spawn-menu visibility,
/// and the final backend preflight.
pub fn pairing_compatibility(pairing: &ProviderPairing) -> CompatibilityDecision {
    let descriptor = endpoint_model_descriptor(pairing);
    let requirements = match pairing.surface {
        ApiSurface::Anthropic => compatibility::claude_requirements(),
        ApiSurface::OpenAI => compatibility::codex_requirements(),
    };
    let mut decision = compatibility::match_descriptor(&descriptor, &requirements);
    if decision.compatible && descriptor.endpoint.is_empty() {
        decision.compatible = false;
        decision.reason = Some("endpoint is required".into());
    }
    if decision.compatible && descriptor.model_id.is_empty() {
        decision.compatible = false;
        decision.reason = Some("explicit model is required".into());
    }
    if pairing.provider_id == "minimax"
        && pairing.surface == ApiSurface::OpenAI
        && descriptor.model_id != "MiniMax-M3"
    {
        decision.compatible = false;
        decision.reason = Some(
            "MiniMax Codex pairings require the current Responses model ID 'MiniMax-M3'"
                .into(),
        );
    }
    decision
}

pub(crate) fn pairing_can_potentially_match(pairing: &ProviderPairing) -> bool {
    let mut candidate = pairing.clone();
    if candidate.model_tiers.default.as_deref().is_none_or(str::is_empty) {
        candidate.model_tiers.default = Some("model-selected-during-attach".into());
    }
    pairing_compatibility(&candidate).compatible
}