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
        // Issue #1899: OpenCode 1.18.3 (Windows `.cmd` via `cmd.exe /c`,
        // Linux/macOS direct spawn) was inspected, not assumed. Its project
        // plugin forwards `session.idle` / `question.asked` /
        // `permission.asked` (plus capture-only `session.created`) and its
        // `opencode.db` SQLite store yields a readable transcript/report —
        // but no event carries a turn/input fence and no pull source exposes
        // a turn completion or a complete child/background registry. Every
        // string below stays `Unavailable:`-prefixed so Circuit execution
        // remains visibly unsupported/Unverified for this provider; a live
        // controlled run is still required before any of these may claim
        // authority.
        "opencode" => (
            "Unavailable: OpenCode 1.18.3 (Windows .cmd via cmd.exe /c; Linux/macOS direct) has no validated Circuit lifecycle adapter; the session.idle plugin hook carries no turn/input fence and cannot establish foreground termination",
            "Unavailable: OpenCode exposes no complete child/background registry to Buildmesh; unknown child/background work never establishes completion",
            "Transcript (opencode.db SQLite) or PTY text may inform interpretation; complete native report unavailable",
            "Status and report discovery only; unsupported lifecycle remains unverified",
            30_000,
        ),
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
        // Validated against agy 1.2.11 on Windows interactive sessions
        // (issue #1901). `-p` print mode emits no `Stop` hook, so hook
        // evidence covers only Buildmesh-launched PTY sessions.
        "agy" => (
            "Stop-hook turn receipts (fullyIdle settled vs background-busy); session-fenced with no per-turn token, never authoritative",
            "Unavailable: Antigravity exposes no child/background registry; settled turns cannot verify owned work",
            "Transcript or PTY text may inform interpretation; complete native report unavailable",
            "Durable Stop receipt replay with session and incarnation fences; input fencing is unavailable (no UserPromptSubmit binding), freshness recheck parks Unverified",
            30_000,
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
        final_report: final_report.into(),
        reconciliation: format!("{reconciliation}. Stable yielded transcript reports can advance after input/session freshness checks and known-work checks; this does not establish complete native ownership coverage."),
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
            if id == "opencode" {
                // Issue #1899: explicit unsupported contract. Still
                // `Unavailable:`-prefixed (Circuit execution stays
                // Unverified), but records the inspected version, platform,
                // hook/pull sources, and the missing ownership registry
                // instead of the generic fallback.
                assert!(policy.foreground.starts_with("Unavailable:"), "opencode foreground must stay unavailable");
                assert!(policy.foreground.contains("OpenCode"), "must record the inspected harness version");
                assert!(policy.foreground.contains("session.idle"), "must name the hook source that was inspected");
                assert!(policy.owned_work.starts_with("Unavailable:"), "opencode ownership must stay unavailable");
                assert!(policy.owned_work.contains("child/background"), "must state the ownership coverage gap");
                assert_eq!(policy.yielded_budget_ms, 30_000);
            } else if !matches!(id, "anthropic" | "codex" | "agy") {
                assert_eq!(policy.foreground, "Unavailable: no authoritative Circuit lifecycle adapter is wired");
                assert_eq!(policy.owned_work, "Unavailable: no authoritative Circuit ownership adapter is wired");
            }
        }
        assert!(for_provider("codex").owned_work.starts_with("Unavailable:"));
    }

    #[test]
    fn circuit_policy_records_validated_agy_contract_without_claiming_completion() {
        // Issue #1901: the AGY adapter wires Stop-hook evidence only. The
        // policy must advertise the validated foreground source while
        // keeping ownership visibly unavailable so a settled turn can never
        // read as assigned-work completion.
        let policy = for_provider("agy");
        assert_eq!(policy.harness, "agy");
        assert!(policy.foreground.contains("Stop-hook"), "foreground names the validated hook source");
        assert!(policy.foreground.contains("never authoritative"), "foreground disclaims completion authority");
        assert!(policy.owned_work.starts_with("Unavailable:"), "ownership stays unavailable");
        assert!(policy.owned_work.contains("no child/background registry"));
        assert_eq!(policy.yielded_budget_ms, 30_000, "no validated basis to change the default budget");
        assert_eq!(policy.active_budget_ms, super::super::ACTIVE_WAIT_MS as u32);
    }

    #[test]
    fn opencode_policy_advertises_no_authoritative_evidence() {
        use crate::autopilot::circuit::observation::{
            CircuitObservation, ObservationDisposition, ObservationIdentity, ObservedWorkFact,
            WorkEvidence,
        };
        // Identity-fenced opencode-shaped lifecycle facts: a mismatched
        // `ses_…` session is rejected, a repeated source is a duplicate, and
        // even an accepted foreground termination without owned-work coverage
        // never verifies completion.
        let identity = ObservationIdentity {
            run_id: 7, step_id: "work".into(), attempt: 1, agent_node_id: 11,
            session_incarnation: Some("1".into()),
            session_id: Some("ses_fc52ccfb9ffek1jl23ZwpRuSP7".into()),
            turn_id: None, report_revision: None,
        };
        let mut evidence = WorkEvidence::default();
        let foreground = CircuitObservation {
            identity: identity.clone(), source: "agent_status_projection".into(),
            source_id: Some("opencode-status-1".into()), observed_at_ms: 100,
            authoritative: false, fact: ObservedWorkFact::ForegroundTerminated,
        };
        assert_eq!(
            evidence.observe(&identity, &foreground),
            ObservationDisposition::ReducedConfidence
        );
        assert_eq!(
            evidence.observe(&identity, &foreground),
            ObservationDisposition::Duplicate
        );
        let mut wrong_session = identity.clone();
        wrong_session.session_id = Some("ses_00000000000000000000000000".into());
        let stale = CircuitObservation {
            identity: wrong_session, source: "agent_status_projection".into(),
            source_id: Some("opencode-status-2".into()), observed_at_ms: 101,
            authoritative: false, fact: ObservedWorkFact::ForegroundTerminated,
        };
        assert_eq!(
            evidence.observe(&identity, &stale),
            ObservationDisposition::Rejected
        );
        // Unknown child/background termination never completes the step:
        // the work id was never registered as owned, so closing it cannot
        // supply the missing ownership coverage.
        let unknown_child = CircuitObservation {
            identity: identity.clone(), source: "agent_status_projection".into(),
            source_id: Some("opencode-child-1".into()), observed_at_ms: 102,
            authoritative: false,
            fact: ObservedWorkFact::OwnedTerminated { work_id: "task:unknown".into() },
        };
        evidence.observe(&identity, &unknown_child);
        assert!(!evidence.completion_verified(), "unknown owned work must never become completion");
        assert!(!evidence.lifecycle_verified(), "foreground alone without ownership coverage is unverified");
    }
}
