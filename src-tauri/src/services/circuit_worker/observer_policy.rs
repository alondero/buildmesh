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
        // Issue #1902: Cline 3.0.62 (Windows npm `.cmd` resolved through
        // `cmd.exe /c`; macOS/Linux direct) was inspected, not assumed.
        // Cline's file-hook layer dispatches `agent_end` from `afterRun`
        // **only** when `result.status === "completed"`, so a completed turn
        // is a real attention signal. What that payload cannot do is decide
        // a Circuit step: it carries `taskId` (the session) but no turn id,
        // no prompt echo and no input stamp, so no Buildmesh submission can
        // be correlated, and the shutdown event is not a clean exit:
        // `SessionShutdown` maps to `session_shutdown`, but 3.0.62 wires it
        // only into the abort branch of `afterRun`, where the session is
        // still live. Ownership is absent too: Buildmesh never passes
        // Cline's own background surfaces (`--kanban`, `-z`/`--zen`,
        // `--team-name`), so no registry reaches us, and
        // `NativeHook::parse_value` gates Cline out, so this provider
        // produces no native Circuit receipt at all. A validated turn signal
        // therefore opens the attention gate while Circuit execution stays
        // visibly Unverified until a controlled live run exists.
        "cline" => (
            "Unavailable: Cline 3.0.62 (Windows npm .cmd via cmd.exe /c; macOS/Linux direct) has no validated Circuit lifecycle adapter; the agent_end TaskComplete file hook marks a completed turn but carries no turn id, prompt echo or input stamp, and Cline dispatches no clean-exit event",
            "Unavailable: Cline exposes no child/background registry to Buildmesh (its --kanban/--zen/--team-name surfaces are never passed); unknown child/background work never establishes completion",
            "Transcript (<cline data dir>/sessions/<id>/<id>.messages.json) or PTY text may inform interpretation; complete native report unavailable",
            "Attention turn receipts only (agent_end -> TurnCompleted) and status/report discovery; Cline yields no native Circuit receipt, so unsupported lifecycle remains unverified",
            30_000,
        ),
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
        // Issue #1898: Claude Code's documented hook contract supplies both
        // halves needed to tie a turn to a Buildmesh submission —
        // `UserPromptSubmit` carries the verbatim `prompt` the harness says
        // it received plus a `prompt_id` turn token (v2.1.196+), and `Stop`
        // carries that same token. Buildmesh binds the token to a submission
        // it recorded, and only while that submission is still the newest
        // one for the agent, so a delayed, duplicate, prior-turn or
        // cross-run hook cannot acknowledge a different submission. No live
        // environment has exercised this yet, so delivery is stated as
        // unverified rather than claimed.
        "anthropic" => (
            "UserPromptSubmit prompt echo bound to a recorded submission, then prompt_id inherited by Stop; receipts without a provable binding stay reduced confidence; live delivery unverified",
            "Child hooks and explicit task/cron registries; missing coverage remains unverified",
            "Scrubbed Stop response when supplied; otherwise unavailable",
            "Durable hook receipt replay bound through the recorded submission; no authoritative ownership pull is available",
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
        yielded_budget_ms, active_budget_ms: super::observation::ACTIVE_WAIT_MS as u32,
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
            } else if id == "cline" {
                // Issue #1902: same explicit-unsupported shape for Cline.
                // A validated *turn* signal (`agent_end` -> `TurnCompleted`)
                // must not leak into a claim about foreground termination or
                // ownership, so both halves stay `Unavailable:`-prefixed.
                assert!(
                    policy.foreground.starts_with("Unavailable:"),
                    "cline foreground must stay unavailable"
                );
                assert!(
                    policy.foreground.contains("Cline 3.0.62"),
                    "must record the inspected harness version"
                );
                assert!(
                    policy.foreground.contains("agent_end"),
                    "must name the hook source that was inspected"
                );
                assert!(
                    policy.owned_work.starts_with("Unavailable:"),
                    "cline ownership must stay unavailable"
                );
                assert!(
                    policy.owned_work.contains("child/background"),
                    "must state the ownership coverage gap"
                );
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
        assert_eq!(policy.active_budget_ms, super::super::observation::ACTIVE_WAIT_MS as u32);
    }

    #[test]
    fn claude_policy_records_the_submission_contract_without_claiming_live_delivery() {
        // Issue #1898: Claude's contract names both halves of the correlation
        // mechanism, and must say plainly that no live run has exercised it.
        // An over-claim here is how a fixture test turns into a runtime
        // promise nobody verified.
        let policy = for_provider("anthropic");
        assert_eq!(policy.harness, "anthropic");
        assert!(policy.foreground.contains("UserPromptSubmit"), "names the event that carries the prompt echo");
        assert!(policy.foreground.contains("prompt_id"), "names the native turn token");
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
        assert_eq!(policy.yielded_budget_ms, 90_000, "no validated basis to change the budget");
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
