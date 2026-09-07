//! Harness-independent endpoint/model capability contracts (issue #1098).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "WireApi.ts")]
pub enum WireApi {
    AnthropicMessages,
    Responses,
    ChatCompletions,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "ProviderAuthMode.ts")]
pub enum ProviderAuthMode {
    BearerEnv,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "EndpointCapabilities.ts")]
pub struct EndpointCapabilities {
    pub streaming: bool,
    pub text_reasoning: bool,
    pub tool_calls: bool,
    pub tool_results: bool,
    pub parallel_tool_calls: bool,
    pub image_input: bool,
    pub web_search: bool,
    pub websocket_transport: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "EndpointModelDescriptor.ts")]
pub struct EndpointModelDescriptor {
    pub provider_id: String,
    pub endpoint: String,
    pub wire_api: WireApi,
    pub model_id: String,
    pub capabilities: EndpointCapabilities,
    pub auth_modes: Vec<ProviderAuthMode>,
    pub context_window: Option<u64>,
    pub reasoning_effort: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "HarnessRequirements.ts")]
pub struct HarnessRequirements {
    pub harness_id: String,
    pub wire_api: WireApi,
    pub capabilities: EndpointCapabilities,
    pub supported_auth_modes: Vec<ProviderAuthMode>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS)]
#[ts(export, export_to = "CompatibilityDecision.ts")]
pub struct CompatibilityDecision {
    pub compatible: bool,
    pub reason: Option<String>,
}

/// Inputs that determine whether a previously verified pairing still routes
/// to exactly the same provider/runtime/Codex installation. The credential is
/// folded directly into the final digest and is never persisted separately.
pub struct PairingSignatureInputs<'a> {
    pub harness_id: &'a str,
    pub provider_id: &'a str,
    pub endpoint: &'a str,
    pub model_id: &'a str,
    pub credential: &'a str,
    pub auth_mode: &'a str,
    pub runtime: &'a str,
    pub executable: &'a str,
    pub codex_version: &'a str,
}

pub fn pairing_signature(inputs: &PairingSignatureInputs<'_>) -> String {
    let mut digest = Sha256::new();
    for value in [
        inputs.harness_id,
        inputs.provider_id,
        inputs.endpoint.trim_end_matches('/'),
        inputs.model_id,
        inputs.credential,
        inputs.auth_mode,
        inputs.runtime,
        inputs.executable,
        inputs.codex_version,
    ] {
        digest.update((value.len() as u64).to_le_bytes());
        digest.update(value.as_bytes());
    }
    hex::encode(digest.finalize())
}

pub fn complete_agent_capabilities() -> EndpointCapabilities {
    EndpointCapabilities {
        streaming: true,
        text_reasoning: true,
        tool_calls: true,
        tool_results: true,
        parallel_tool_calls: false,
        image_input: false,
        web_search: false,
        websocket_transport: false,
    }
}

pub fn codex_requirements() -> HarnessRequirements {
    HarnessRequirements {
        harness_id: "codex".into(),
        wire_api: WireApi::Responses,
        capabilities: EndpointCapabilities {
            streaming: true,
            text_reasoning: true,
            tool_calls: true,
            tool_results: true,
            parallel_tool_calls: false,
            ..EndpointCapabilities::default()
        },
        supported_auth_modes: vec![ProviderAuthMode::BearerEnv],
    }
}

pub fn claude_requirements() -> HarnessRequirements {
    HarnessRequirements {
        harness_id: "claude".into(),
        wire_api: WireApi::AnthropicMessages,
        capabilities: EndpointCapabilities {
            streaming: true,
            text_reasoning: true,
            tool_calls: true,
            tool_results: true,
            parallel_tool_calls: false,
            ..EndpointCapabilities::default()
        },
        supported_auth_modes: vec![ProviderAuthMode::BearerEnv],
    }
}

pub fn match_descriptor(
    descriptor: &EndpointModelDescriptor,
    requirements: &HarnessRequirements,
) -> CompatibilityDecision {
    if descriptor.wire_api != requirements.wire_api {
        return CompatibilityDecision {
            compatible: false,
            reason: Some(
                format!(
                    "{} requires the {:?} wire API; this endpoint declares {:?}",
                    requirements.harness_id, requirements.wire_api, descriptor.wire_api
                )
                .to_lowercase(),
            ),
        };
    }
    if !descriptor
        .auth_modes
        .iter()
        .any(|mode| requirements.supported_auth_modes.contains(mode))
    {
        return CompatibilityDecision {
            compatible: false,
            reason: Some("endpoint does not declare a supported authentication mode".into()),
        };
    }

    let actual = &descriptor.capabilities;
    let required = &requirements.capabilities;
    let missing = [
        (required.streaming && !actual.streaming, "streaming"),
        (
            required.text_reasoning && !actual.text_reasoning,
            "text/reasoning completion",
        ),
        (
            required.tool_calls && !actual.tool_calls,
            "tool-call arguments",
        ),
        (
            required.tool_results && !actual.tool_results,
            "tool-result round trip",
        ),
        (
            required.parallel_tool_calls && !actual.parallel_tool_calls,
            "parallel tool calls",
        ),
    ]
    .into_iter()
    .filter_map(|(is_missing, label)| is_missing.then_some(label))
    .collect::<Vec<_>>();

    if !missing.is_empty() {
        return CompatibilityDecision {
            compatible: false,
            reason: Some(format!(
                "missing required capabilities: {}",
                missing.join(", ")
            )),
        };
    }

    CompatibilityDecision {
        compatible: true,
        reason: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete(_wire_api: WireApi) -> EndpointCapabilities {
        EndpointCapabilities {
            streaming: true,
            text_reasoning: true,
            tool_calls: true,
            tool_results: true,
            parallel_tool_calls: false,
            ..EndpointCapabilities::default()
        }
    }

    fn descriptor(wire_api: WireApi) -> EndpointModelDescriptor {
        EndpointModelDescriptor {
            provider_id: "minimax".into(),
            endpoint: "https://api.minimax.io/v1".into(),
            wire_api,
            model_id: "MiniMax-M3".into(),
            capabilities: complete(wire_api),
            auth_modes: vec![ProviderAuthMode::BearerEnv],
            ..EndpointModelDescriptor::default()
        }
    }

    #[test]
    fn codex_accepts_complete_responses_contract() {
        assert!(
            match_descriptor(&descriptor(WireApi::Responses), &codex_requirements()).compatible
        );
    }

    #[test]
    fn codex_rejects_chat_completions_contract() {
        let decision =
            match_descriptor(&descriptor(WireApi::ChatCompletions), &codex_requirements());
        assert!(!decision.compatible);
        assert!(decision.reason.unwrap().contains("responses"));
    }

    #[test]
    fn codex_rejects_missing_tool_result_round_trip() {
        let mut candidate = descriptor(WireApi::Responses);
        candidate.capabilities.tool_results = false;
        let decision = match_descriptor(&candidate, &codex_requirements());
        assert!(!decision.compatible);
        assert!(decision.reason.unwrap().contains("tool-result"));
    }

    #[test]
    fn claude_contract_is_independent_of_codex_contract() {
        let candidate = descriptor(WireApi::AnthropicMessages);
        assert!(match_descriptor(&candidate, &claude_requirements()).compatible);
        assert!(!match_descriptor(&candidate, &codex_requirements()).compatible);
    }

    #[test]
    fn pairing_signature_changes_for_every_routing_input() {
        let baseline = [
            "codex",
            "minimax",
            "https://api.minimax.io/v1",
            "MiniMax-M3",
            "secret-a",
            "bearer_env",
            "native-windows",
            "codex",
            "0.144.0",
        ];
        let signature = |values: &[&str; 9]| {
            pairing_signature(&PairingSignatureInputs {
                harness_id: values[0],
                provider_id: values[1],
                endpoint: values[2],
                model_id: values[3],
                credential: values[4],
                auth_mode: values[5],
                runtime: values[6],
                executable: values[7],
                codex_version: values[8],
            })
        };
        let original = signature(&baseline);
        for index in 0..baseline.len() {
            let mut changed = baseline;
            changed[index] = "changed";
            assert_ne!(
                original,
                signature(&changed),
                "input {index} must invalidate"
            );
        }
    }
}
