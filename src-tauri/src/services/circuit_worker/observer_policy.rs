//! Capabilities of the observation strategies actually wired into Circuits.
//! Harness execution support does not imply lifecycle or ownership authority.

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitObserverCapabilities.ts")]
pub struct CircuitObserverCapabilities {
    pub harness: String,
    pub foreground: String,
    pub owned_work: String,
    pub final_report: String,
    pub reconciliation: String,
    pub yielded_budget_ms: u32,
    pub active_budget_ms: u32,
}

pub(crate) fn for_agent(agent: &crate::models::AgentNode) -> CircuitObserverCapabilities {
    let harness = agent.launch_configuration.as_ref().and_then(|configuration| configuration.resolved.as_ref())
        .map(|plan| plan.harness.harness.as_str()).unwrap_or(agent.provider.as_str());
    for_provider(harness)
}

pub(crate) fn for_provider(provider: &str) -> CircuitObserverCapabilities {
    let id = crate::agent::harness_catalog::HARNESS_PROFILE_ALIASES.iter()
        .find(|(alias, _)| *alias == provider).map(|(_, id)| *id).unwrap_or(provider);
    let (foreground, owned_work, final_report, reconciliation, yielded_budget_ms) = match id {
        "anthropic" => (
            "Native hook receipts only; authoritative submission correlation unavailable",
            "Child hooks and explicit task/cron registries; missing coverage remains unverified",
            "Scrubbed Stop response when supplied; otherwise unavailable",
            "Durable hook receipt replay and status discovery; no authoritative ownership pull is available",
            90_000,
        ),
        "codex" => (
            "Identity-bound rollout task_complete pull",
            "Unavailable: rollouts do not expose a complete child/background registry",
            "Scrubbed task_complete response; oversized or missing records remain unavailable",
            "Bounded native rollout pull with session, turn, input and timestamp fences",
            60_000,
        ),
        _ => (
            "Unavailable: no authoritative Circuit lifecycle adapter is wired",
            "Unavailable: no authoritative Circuit ownership adapter is wired",
            "Transcript or PTY text may inform interpretation; complete native report unavailable",
            "Status and report discovery only; unsupported lifecycle remains unverified",
            30_000,
        ),
    };
    CircuitObserverCapabilities {
        harness: id.into(), foreground: foreground.into(), owned_work: owned_work.into(),
        final_report: final_report.into(), reconciliation: reconciliation.into(),
        yielded_budget_ms, active_budget_ms: super::ACTIVE_WAIT_MS as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circuit_policy_covers_every_harness_without_borrowing_another_parser() {
        for provider in crate::models::Provider::all() {
            let id = provider.adapter().id();
            let policy = for_provider(id);
            assert_eq!(policy.harness, id);
            assert!(policy.yielded_budget_ms > 0 && policy.yielded_budget_ms <= 90_000);
            if !matches!(id, "anthropic" | "codex") {
                assert_eq!(policy.foreground, "Unavailable: no authoritative Circuit lifecycle adapter is wired");
                assert_eq!(policy.owned_work, "Unavailable: no authoritative Circuit ownership adapter is wired");
            }
        }
        assert!(for_provider("codex").owned_work.starts_with("Unavailable:"));
    }
}
