//! Operator diagnostics for the observation strategy a harness declares.
//! Harness execution support does not imply lifecycle or ownership authority.
//!
//! Everything here is rendered from the adapter's own
//! [`ObservationStrategy`](crate::circuit::strategy::ObservationStrategy), the
//! same declaration the worker executes (issue #2128). The typed `coverage` is
//! the machine-readable contract; the strings are display text derived from it,
//! so a prose prefix never stands in for a capability.

use crate::circuit::strategy::{
    HarnessSelector, ObservationCoverage, ObservationStrategy, OwnedWorkCoverage,
};

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
    pub coverage: ObservationCoverage,
}

pub(crate) fn for_agent(agent: &crate::models::AgentNode) -> CircuitObserverCapabilities {
    for_selector(&crate::circuit::strategy::selector_for_agent(agent))
}

/// Diagnostics for a stored provider string: a harness id, a custom harness
/// profile, or a proxied `harness:account` option. Unknown harnesses render as
/// unwired.
pub(crate) fn for_provider(provider: &str) -> CircuitObserverCapabilities {
    for_selector(&HarnessSelector::Stored(provider.to_owned()))
}

/// Resolving a selector can read preferences, so callers holding a database
/// connection take the selector (pure) first and resolve it after releasing.
pub(crate) fn for_selector(selector: &HarnessSelector) -> CircuitObserverCapabilities {
    let id = match selector {
        HarnessSelector::Frozen(id) | HarnessSelector::Stored(id) => id.as_str(),
    };
    // Resolve once: the label and the strategy must describe the same harness.
    let provider = selector.provider();
    render(
        provider.map_or(id, |resolved| resolved.adapter().id()),
        &crate::circuit::strategy::strategy_of(provider),
    )
}

/// Diagnostics for a step whose agent record no longer exists.
pub(crate) fn for_missing_agent() -> CircuitObserverCapabilities {
    render("missing-agent", &ObservationStrategy::UNWIRED)
}

fn render(harness: &str, strategy: &ObservationStrategy) -> CircuitObserverCapabilities {
    let notes = &strategy.notes;
    let foreground = if strategy.has_foreground_source() {
        notes.foreground.to_owned()
    } else {
        format!("Unavailable: {}", notes.foreground)
    };
    let owned_work = match strategy.owned_work {
        OwnedWorkCoverage::Unavailable { .. } => format!("Unavailable: {}", notes.owned_work),
        OwnedWorkCoverage::HookRegistry => notes.owned_work.to_owned(),
    };
    CircuitObserverCapabilities {
        harness: harness.into(),
        foreground,
        owned_work,
        final_report: notes.final_report.into(),
        reconciliation: format!("{}. Stable yielded transcript reports can advance after input/session freshness checks and known-work checks; this does not establish complete native ownership coverage.", notes.reconciliation),
        yielded_budget_ms: strategy.reconciliation.yielded_budget_ms,
        active_budget_ms: super::observation::ACTIVE_WAIT_MS as u32,
        coverage: strategy.coverage(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circuit_policy_is_rendered_from_each_harnesss_own_declaration() {
        use crate::circuit::strategy::OwnedWorkKind;
        for provider in crate::models::Provider::all() {
            let id = provider.adapter().id();
            let strategy = provider.adapter().circuit_observation();
            let policy = for_provider(id);
            assert_eq!(policy.harness, id);
            assert!(policy.yielded_budget_ms > 0 && policy.yielded_budget_ms <= 90_000);
            // The typed coverage is the contract; the strings are derived.
            assert_eq!(policy.coverage, strategy.coverage(), "{id}");
            assert_eq!(
                policy.yielded_budget_ms, strategy.reconciliation.yielded_budget_ms,
                "{id}: the budget the worker waits on is the one displayed"
            );
            assert_eq!(
                policy.foreground.starts_with("Unavailable:"),
                !strategy.has_foreground_source(),
                "{id}"
            );
            assert_eq!(
                policy.owned_work.starts_with("Unavailable:"),
                policy.coverage.owned_work == OwnedWorkKind::Unavailable,
                "{id}"
            );
            if id == "opencode" {
                // Issue #1899: explicit unsupported contract. No Circuit
                // source is declared (Circuit execution stays Unverified), but
                // the provenance records the inspected version, platform,
                // hook/pull sources, and the missing ownership registry
                // instead of the generic fallback.
                assert!(!strategy.has_foreground_source());
                assert!(
                    policy.foreground.contains("OpenCode"),
                    "must record the inspected harness version"
                );
                assert!(
                    policy.foreground.contains("session.idle"),
                    "must name the hook source that was inspected"
                );
                assert_eq!(policy.coverage.owned_work, OwnedWorkKind::Unavailable);
                assert!(
                    policy.owned_work.contains("child/background"),
                    "must state the ownership coverage gap"
                );
                assert_eq!(policy.yielded_budget_ms, 30_000);
            } else if id == "cline" {
                // Issue #1902: same explicit-unsupported shape for Cline.
                // A validated *turn* signal (`agent_end` -> `TurnCompleted`)
                // must not leak into a claim about foreground termination or
                // ownership, so neither half declares Circuit coverage.
                assert!(!strategy.has_foreground_source());
                assert!(
                    policy.foreground.contains("Cline 3.0.62"),
                    "must record the inspected harness version"
                );
                assert!(
                    policy.foreground.contains("agent_end"),
                    "must name the hook source that was inspected"
                );
                assert_eq!(policy.coverage.owned_work, OwnedWorkKind::Unavailable);
                assert!(
                    policy.owned_work.contains("child/background"),
                    "must state the ownership coverage gap"
                );
                assert_eq!(policy.yielded_budget_ms, 30_000);
            } else if !matches!(id, "anthropic" | "codex" | "agy") {
                assert_eq!(
                    policy.foreground,
                    "Unavailable: no authoritative Circuit lifecycle adapter is wired"
                );
                assert_eq!(
                    policy.owned_work,
                    "Unavailable: no authoritative Circuit ownership adapter is wired"
                );
            }
        }
        assert_eq!(
            for_provider("codex").coverage.owned_work,
            OwnedWorkKind::Unavailable
        );
        assert_eq!(
            for_provider("anthropic").coverage.owned_work,
            OwnedWorkKind::HookRegistry
        );
    }

    fn node_for(provider: &str) -> crate::models::AgentNode {
        crate::models::AgentNode {
            provider: provider.into(),
            ..Default::default()
        }
    }

    #[test]
    fn proxied_provider_accounts_are_diagnosed_as_their_executing_harness() {
        // Issue #2128: the diagnostics must name the strategy the worker will
        // actually run. A proxied row ("claude:minimax") executes Claude Code,
        // so it must not read as an unwired harness.
        for (stored, harness) in [
            ("claude:minimax", "anthropic"),
            ("codex:openrouter", "codex"),
            ("agy", "agy"),
        ] {
            let policy = for_agent(&node_for(stored));
            assert_eq!(policy.harness, harness, "{stored}");
            assert_ne!(
                policy.coverage.push,
                crate::circuit::strategy::PushKind::None,
                "{stored} executes {harness}, which wires a Circuit hook strategy"
            );
        }
    }

    #[test]
    fn a_launch_snapshot_decides_the_diagnosed_harness_over_the_current_profile() {
        use crate::preferences::launch_configurations::{capture, LaunchOverrides};
        // Launched as Codex, stored provider since remapped to Claude: the
        // diagnostics must describe the harness that is actually running.
        let mut node = node_for("claude");
        node.launch_configuration = Some(
            crate::preferences::spawn_configurations::SpawnConfiguration {
                resolved: Some(
                    capture(
                        &crate::preferences::AppPreferences::default(),
                        "codex",
                        &LaunchOverrides::default(),
                    )
                    .unwrap(),
                ),
                ..Default::default()
            },
        );
        let policy = for_agent(&node);
        assert_eq!(policy.harness, "codex");
        assert_eq!(
            policy.coverage.pull,
            crate::circuit::strategy::PullKind::NativeTurnCompletion
        );
    }

    #[test]
    fn unknown_providers_stay_unwired_rather_than_borrowing_another_strategy() {
        for stored in ["future-harness", "future-harness:account", ""] {
            let policy = for_agent(&node_for(stored));
            assert_eq!(
                policy.coverage,
                ObservationStrategy::UNWIRED.coverage(),
                "{stored:?} must not inherit another harness's strategy"
            );
        }
    }

    #[test]
    fn circuit_policy_records_validated_agy_contract_without_claiming_completion() {
        // Issue #1901: the AGY adapter wires Stop-hook evidence only. The
        // policy must advertise the validated foreground source while
        // keeping ownership visibly unavailable so a settled turn can never
        // read as assigned-work completion.
        let policy = for_provider("agy");
        assert_eq!(policy.harness, "agy");
        assert!(
            policy.foreground.contains("Stop-hook"),
            "foreground names the validated hook source"
        );
        assert!(
            policy.foreground.contains("never authoritative"),
            "foreground disclaims completion authority"
        );
        assert_eq!(
            policy.coverage.owned_work,
            crate::circuit::strategy::OwnedWorkKind::Unavailable,
            "ownership stays unavailable"
        );
        assert!(policy.owned_work.contains("no child/background registry"));
        assert_eq!(
            policy.yielded_budget_ms, 30_000,
            "no validated basis to change the default budget"
        );
        assert_eq!(
            policy.active_budget_ms,
            super::super::observation::ACTIVE_WAIT_MS as u32
        );
    }

    #[test]
    fn claude_policy_records_the_submission_contract_without_claiming_live_delivery() {
        // Issue #1898: Claude's contract names both halves of the correlation
        // mechanism, and must say plainly that no live run has exercised it.
        // An over-claim here is how a fixture test turns into a runtime
        // promise nobody verified.
        let policy = for_provider("anthropic");
        assert_eq!(policy.harness, "anthropic");
        assert!(
            policy.foreground.contains("UserPromptSubmit"),
            "names the event that carries the prompt echo"
        );
        assert!(
            policy.foreground.contains("prompt_id"),
            "names the native turn token"
        );
        assert!(
            policy.foreground.contains("reduced confidence"),
            "states what an unprovable receipt is worth"
        );
        assert!(
            policy.foreground.contains("unverified"),
            "live delivery is not established in this environment"
        );
        assert!(
            policy.reconciliation.contains("recorded submission"),
            "the binding is through a submission Buildmesh recorded"
        );
        assert_eq!(
            policy.yielded_budget_ms, 90_000,
            "no validated basis to change the budget"
        );
    }

    #[test]
    fn opencode_policy_advertises_no_authoritative_evidence() {
        use crate::circuit::observation::{
            CircuitObservation, ObservationDisposition, ObservationIdentity, ObservedWorkFact,
            WorkEvidence,
        };
        // Identity-fenced opencode-shaped lifecycle facts: a mismatched
        // `ses_…` session is rejected, a repeated source is a duplicate, and
        // even an accepted foreground termination without owned-work coverage
        // never verifies completion.
        let identity = ObservationIdentity {
            run_id: 7,
            step_id: "work".into(),
            attempt: 1,
            agent_node_id: 11,
            session_incarnation: Some("1".into()),
            session_id: Some("ses_fc52ccfb9ffek1jl23ZwpRuSP7".into()),
            turn_id: None,
            report_revision: None,
        };
        let mut evidence = WorkEvidence::default();
        let foreground = CircuitObservation {
            identity: identity.clone(),
            source: "agent_status_projection".into(),
            source_id: Some("opencode-status-1".into()),
            observed_at_ms: 100,
            authoritative: false,
            fact: ObservedWorkFact::ForegroundTerminated,
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
            identity: wrong_session,
            source: "agent_status_projection".into(),
            source_id: Some("opencode-status-2".into()),
            observed_at_ms: 101,
            authoritative: false,
            fact: ObservedWorkFact::ForegroundTerminated,
        };
        assert_eq!(
            evidence.observe(&identity, &stale),
            ObservationDisposition::Rejected
        );
        // Unknown child/background termination never completes the step:
        // the work id was never registered as owned, so closing it cannot
        // supply the missing ownership coverage.
        let unknown_child = CircuitObservation {
            identity: identity.clone(),
            source: "agent_status_projection".into(),
            source_id: Some("opencode-child-1".into()),
            observed_at_ms: 102,
            authoritative: false,
            fact: ObservedWorkFact::OwnedTerminated {
                work_id: "task:unknown".into(),
            },
        };
        evidence.observe(&identity, &unknown_child);
        assert!(
            !evidence.completion_verified(),
            "unknown owned work must never become completion"
        );
        assert!(
            !evidence.lifecycle_verified(),
            "foreground alone without ownership coverage is unverified"
        );
    }

    #[test]
    fn cline_policy_advertises_no_authoritative_evidence() {
        use crate::circuit::observation::{
            CircuitObservation, HumanWaitKind, ObservationDisposition, ObservationIdentity,
            ObservedWorkFact, WorkEvidence,
        };
        // Read the arm this test names. Without this the rest of the test
        // would still pass if the `"cline"` arm were deleted outright, because
        // the evidence below is built directly rather than through the
        // policy — so the fencing assertions would guard nothing.
        let policy = super::for_provider("cline");
        assert_eq!(
            policy.foreground,
            "Unavailable: Cline 3.0.62 (Windows npm .cmd via cmd.exe /c; macOS/Linux direct) has no validated Circuit lifecycle adapter; the agent_end TaskComplete file hook marks a completed turn but carries no turn id, prompt echo or input stamp, and Cline dispatches no clean-exit event",
            "Cline must keep its explicit unavailable lifecycle contract"
        );
        assert_eq!(
            policy.owned_work,
            "Unavailable: Cline exposes no child/background registry to Buildmesh (its --kanban/--zen/--team-name surfaces are never passed); unknown child/background work never establishes completion",
            "Cline must keep its explicit unavailable ownership contract"
        );
        assert!(
            policy.reconciliation.contains("no native Circuit receipt"),
            "the recorded reconciliation must state that Cline yields no native receipt"
        );
        assert_eq!(
            policy.yielded_budget_ms, 30_000,
            "Cline's recorded yielded budget must stay bounded"
        );

        // The identity a Cline turn would have to prove. Cline's `agent_end`
        // payload supplies `taskId` (the session) and nothing else, so this
        // is deliberately turn-token-free: `turn_id` stays `None` because no
        // Cline event carries one.
        let identity = ObservationIdentity {
            run_id: 12,
            step_id: "work".into(),
            attempt: 1,
            agent_node_id: 21,
            session_incarnation: Some("1000".into()),
            session_id: Some("session_1790003303940_9ouga".into()),
            turn_id: None,
            report_revision: None,
        };
        let cline_event =
            |source_id: &str, at_ms: i64, fact: ObservedWorkFact| CircuitObservation {
                identity: identity.clone(),
                source: "cline_attention_hook".into(),
                source_id: Some(source_id.into()),
                observed_at_ms: at_ms,
                // Cline's attention payload is a turn receipt, never a lifecycle
                // authority: nothing in the file-hook layer can prove termination.
                authoritative: false,
                fact,
            };

        // 1. Foreground: a Cline turn completion is accepted as reduced
        //    confidence only, and repeats are deduplicated.
        let mut evidence = WorkEvidence::default();
        let turn = cline_event("cline-turn-1", 10, ObservedWorkFact::ForegroundTerminated);
        assert_eq!(
            evidence.observe(&identity, &turn),
            ObservationDisposition::ReducedConfidence
        );
        assert_eq!(
            evidence.observe(&identity, &turn),
            ObservationDisposition::Duplicate,
            "the same turn receipt twice must stay one receipt"
        );

        // 2. Completion: even a terminated foreground cannot complete the
        //    step, because no Cline source ever establishes ownership.
        assert!(
            !evidence.completion_verified(),
            "a Cline turn end must never verify completion on its own"
        );
        assert!(
            !evidence.lifecycle_verified(),
            "turn completion without ownership coverage stays unverified"
        );

        // 3. Stale: a receipt carrying a foreign session id is rejected
        //    outright rather than merged into this node's evidence.
        let mut wrong_session = identity.clone();
        wrong_session.session_id = Some("session_1790003303999_zzzzz".into());
        let stale = CircuitObservation {
            identity: wrong_session,
            source: "cline_attention_hook".into(),
            source_id: Some("cline-turn-stale".into()),
            observed_at_ms: 11,
            authoritative: false,
            fact: ObservedWorkFact::ForegroundTerminated,
        };
        assert_eq!(
            evidence.observe(&identity, &stale),
            ObservationDisposition::Rejected,
            "another Cline session's turn must never fence as this one"
        );

        // 4. Restart: a new session incarnation is a different incarnation,
        //    so the previous turn cannot carry over.
        let mut restarted = identity.clone();
        restarted.session_incarnation = Some("2000".into());
        let after_restart = CircuitObservation {
            identity: restarted,
            source: "cline_attention_hook".into(),
            source_id: Some("cline-turn-restart".into()),
            observed_at_ms: 12,
            authoritative: false,
            fact: ObservedWorkFact::ForegroundTerminated,
        };
        assert_eq!(
            evidence.observe(&identity, &after_restart),
            ObservationDisposition::Rejected,
            "a post-restart turn must not be read as the pre-restart attempt"
        );

        // 5. Wait: an unanswered request blocks verification on its own.
        let mut waiting = WorkEvidence::default();
        waiting.observe(
            &identity,
            &cline_event(
                "cline-wait-1",
                10,
                ObservedWorkFact::HumanWaitRequested {
                    wait_kind: HumanWaitKind::Permission,
                    request_id: "req-1".into(),
                },
            ),
        );
        assert!(
            waiting.has_human_wait(),
            "a Cline-shaped request must register as a wait, not be dropped"
        );
        assert!(!waiting.lifecycle_verified(), "an open wait never verifies");

        // 6. Cancellation: Cline's abort branch fires while the session is
        //    still live, so it carries explicit unavailable ownership and
        //    can never be read as a settled, completed step.
        let mut cancelled = WorkEvidence::default();
        cancelled.observe(
            &identity,
            &cline_event(
                "cline-cancel-1",
                10,
                ObservedWorkFact::OwnershipUnavailable {
                    reason: "cline agent_abort leaves the session live".into(),
                },
            ),
        );
        assert!(
            !cancelled.lifecycle_verified(),
            "cancelled turn is not a verified lifecycle"
        );
        assert!(
            !cancelled.completion_verified(),
            "a cancelled Cline turn must never become completion"
        );

        // 7. Child / background: Cline exposes no ownership registry, so an
        //    unregistered work id closing is not evidence of anything. This
        //    is the property that keeps `ownership_covered` false.
        let mut owned = WorkEvidence::default();
        owned.observe(
            &identity,
            &cline_event("cline-child-1", 10, ObservedWorkFact::ForegroundTerminated),
        );
        owned.observe(
            &identity,
            &cline_event(
                "cline-child-term",
                11,
                ObservedWorkFact::OwnedTerminated {
                    work_id: "task:unknown".into(),
                },
            ),
        );
        assert!(
            !owned.completion_verified(),
            "unknown child/background work must never become completion"
        );
        assert!(
            !owned.lifecycle_verified(),
            "unknown child/background work must never verify the lifecycle"
        );
    }
}
