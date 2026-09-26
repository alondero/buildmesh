//! Codex rollout completion establishes only the foreground turn. The rollout
//! does not expose a complete child/background registry, including work started
//! by code-mode calls, so it cannot establish ownership coverage.

use super::{CircuitEvent, CircuitNodeKind, RunView, StepView};
use crate::autopilot::circuit::observation::{
    CircuitObservation, ObservationIdentity, ObservedWorkFact,
};
use crate::autopilot::circuit::stepper::ObservationInputFence;

pub(super) fn observe(
    view: &RunView,
    step: &StepView,
    node: &crate::models::AgentNode,
) -> Option<CircuitEvent> {
    if crate::preferences::resolve_harness_provider(&node.provider)
        .adapter()
        .id()
        != "codex"
    {
        return None;
    }
    match view.graph.node(&step.node_id).map(|n| &n.kind) {
        Some(CircuitNodeKind::SpawnAgentNode { prompt, .. })
            if !view.context.resolve(prompt).trim().is_empty() => {}
        Some(
            CircuitNodeKind::AwaitAgentTurn { .. }
            | CircuitNodeKind::LlmTurnClassifier { .. }
            | CircuitNodeKind::ReviewVerdict { .. },
        ) => {}
        _ => return None,
    }
    let unavailable = |reason| recheck_unavailable(view, step, reason);
    let Some(input) = crate::agent::process::PROCESS_REGISTRY.input_stamp(node.id) else {
        return unavailable("Codex input is changing or cannot be fenced; no current completion was established.");
    };
    let Some(incarnation) = crate::db::agent_turn_stamp(node.id)
        .ok()
        .flatten()
        .and_then(|stamp| stamp.split_once(':').map(|(incarnation, _)| incarnation.to_owned())) else {
        return unavailable("Codex session incarnation is unavailable; no current completion was established.");
    };
    let Some(snapshot) = crate::coordinator::enrichment::native_turn_completion(node) else {
        return unavailable("No current Codex foreground completion could be re-established.");
    };
    let Some(incarnation_ms) = incarnation.parse::<i64>().ok() else {
        return unavailable("Codex session incarnation cannot be interpreted; no current completion was established.");
    };
    if snapshot.completion.completed_at_ms < incarnation_ms || !snapshot.is_current() {
        return unavailable("The Codex completion no longer matches the current session input.");
    }
    let identity = ObservationIdentity {
        run_id: view.run_id,
        step_id: step.node_id.clone(),
        attempt: step.attempt,
        agent_node_id: node.id,
        session_incarnation: Some(incarnation),
        session_id: node.cli_session_id.clone(),
        turn_id: Some(snapshot.completion.turn_id.clone()),
        report_revision: None,
    };
    let mut event = normalize(
        identity,
        Some(input),
        snapshot.completion.completed_at_ms,
        snapshot.completion.final_report.clone(),
    );
    if let Some(evidence) = view.context.get(&format!("node.{}.evidence.{}", step.node_id, step.attempt))
        .and_then(|json| serde_json::from_str::<crate::autopilot::circuit::observation::WorkEvidence>(json).ok()) {
        append_foreground_reconciliation(&mut event, &evidence, chrono::Utc::now().timestamp_millis());
    }
    if let CircuitEvent::ObservationBatch { input_guard: Some(guard), .. } = &mut event {
        guard.transcript_guard = Some(snapshot);
    }
    Some(event)
}

fn recheck_unavailable(view: &RunView, step: &StepView, reason: &str) -> Option<CircuitEvent> {
    (step.status == crate::autopilot::circuit::stepper::StepStatus::Running
        && view.context.get(&format!("node.{}.recheck_only", step.node_id)) == Some("1"))
    .then(|| CircuitEvent::EffectUncertain {
        node_id: step.node_id.clone(),
        attempt: step.attempt,
        reason: format!("Codex evidence recheck remains Unverified: {reason}"),
    })
}

pub(super) fn freshness_rejection_recheck(
    view: &RunView,
    event: &CircuitEvent,
) -> Option<CircuitEvent> {
    let CircuitEvent::ObservationBatch {
        expected,
        observations,
        stale: false,
        input_guard: Some(guard),
        ..
    } = event
    else {
        return None;
    };
    let step = view.step(&expected.step_id)?;
    let is_codex_completion = observations
        .iter()
        .any(|observation| observation.source == "codex_rollout_task_complete");
    (is_codex_completion
        && view.context.get(&format!("node.{}.recheck_only", expected.step_id)) == Some("1")
        && expected.run_id == view.run_id
        && expected.attempt == step.attempt
        && step.status == crate::autopilot::circuit::stepper::StepStatus::Running
        && step
            .agent_node_id
            .or_else(|| view.resolve_target_agent(&expected.step_id))
            == Some(expected.agent_node_id)
        && guard.agent_node_id == expected.agent_node_id
        && guard.session_id == expected.session_id.as_deref().unwrap_or_default()
        && guard.session_incarnation == expected.session_incarnation.as_deref().unwrap_or_default())
    .then(|| {
        recheck_unavailable(
            view,
            step,
            "the completion was rejected by the current input or session freshness fence; no current completion was established.",
        )
    })
    .flatten()
}

fn append_foreground_reconciliation(
    event: &mut CircuitEvent,
    evidence: &crate::autopilot::circuit::observation::WorkEvidence,
    observed_at_ms: i64,
) {
    use crate::autopilot::circuit::observation::EvidenceConflictKind;
    let CircuitEvent::ObservationBatch { expected, observations, input_guard: Some(_), stale: false, .. } = event else { return; };
    // Only this adapter's validated exact-session pull can issue this fact.
    // Resolving foreground uncertainty never supplies owned-work coverage.
    for conflict in &evidence.conflicts {
        if conflict.kind != EvidenceConflictKind::Foreground
            || conflict.identity.run_id != expected.run_id || conflict.identity.step_id != expected.step_id
            || conflict.identity.attempt != expected.attempt || conflict.identity.agent_node_id != expected.agent_node_id
            || conflict.identity.session_incarnation != expected.session_incarnation
            || conflict.identity.session_id != expected.session_id || conflict.identity.turn_id != expected.turn_id {
            continue;
        }
        observations.push(CircuitObservation {
            identity: expected.clone(), source: "codex_rollout_foreground_recheck".into(),
            source_id: Some(format!("reconcile:{}", conflict.id)), observed_at_ms, authoritative: true,
            fact: ObservedWorkFact::ForegroundReconciled { conflict_id: conflict.id.clone() },
        });
    }
}

fn normalize(
    identity: ObservationIdentity,
    input: Option<String>,
    completed_at_ms: i64,
    final_report: Option<String>,
) -> CircuitEvent {
    use sha2::{Digest, Sha256};
    let input_guard = input
        .zip(identity.session_id.clone())
        .zip(identity.session_incarnation.clone())
        .map(
            |((input_stamp, session_id), session_incarnation)| ObservationInputFence {
                transcript_guard: None, report_guard: None,
                agent_node_id: identity.agent_node_id,
                input_stamp,
                observed_at_ms: completed_at_ms,
                session_id,
                session_incarnation,
            },
        );
    let mut facts = vec![ObservedWorkFact::ForegroundTerminated, ObservedWorkFact::OwnershipUnavailable {
        reason: "Codex recorded a completed foreground turn, but its rollout cannot verify all owned child and background work. Inspect the agent and Recheck evidence; this turn does not establish assigned-work completion.".into(),
    }];
    if let Some(text) = final_report {
        let revision = format!("{:x}", Sha256::digest(text.as_bytes()));
        facts.push(ObservedWorkFact::AssistantReport { text, revision });
    }
    let observations = facts
        .into_iter()
        .enumerate()
        .map(|(index, fact)| CircuitObservation {
            identity: identity.clone(),
            source: "codex_rollout_task_complete".into(),
            source_id: Some(format!(
                "{}:{completed_at_ms}:{index}",
                identity.turn_id.as_deref().unwrap_or_default()
            )),
            observed_at_ms: completed_at_ms,
            authoritative: input_guard.is_some(),
            fact,
        })
        .collect();
    CircuitEvent::ObservationBatch {
        receipt_id: 0,
        expected: identity,
        observations,
        stale: false,
        input_guard,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autopilot::circuit::{
        context::CircuitContext,
        model::CircuitGraph,
        stepper::{advance, RunState, StepStatus},
    };

    #[test]
    fn codex_recheck_resolves_only_the_exact_foreground_conflict_and_retains_ownership_limit() {
        use crate::autopilot::circuit::observation::{WorkEvidence, ObservationDisposition};
        let identity = ObservationIdentity { run_id: 42, step_id: "spawn".into(), attempt: 1, agent_node_id: 9,
            session_incarnation: Some("1000".into()), session_id: Some("session".into()), turn_id: Some("turn".into()), report_revision: None };
        let mut evidence = WorkEvidence::default();
        for (index, fact) in [ObservedWorkFact::ForegroundTerminated, ObservedWorkFact::Working].into_iter().enumerate() {
            evidence.observe(&identity, &CircuitObservation { identity: identity.clone(), source: "native".into(),
                source_id: Some(index.to_string()), observed_at_ms: 2000 + index as i64, authoritative: true, fact });
        }
        assert!(evidence.conflicted);
        let mut recheck = normalize(identity.clone(), Some("1:2".into()), 2000, None);
        append_foreground_reconciliation(&mut recheck, &evidence, 3000);
        let CircuitEvent::ObservationBatch { observations, .. } = &recheck else { panic!("expected batch") };
        assert!(observations.iter().any(|o|matches!(o.fact,ObservedWorkFact::ForegroundReconciled { .. })));
        for observation in observations { evidence.observe(&identity, observation); }
        assert!(!evidence.conflicted);
        assert!(evidence.foreground_terminated);
        assert!(!evidence.ownership_covered);
        assert!(!evidence.completion_verified());
        let last = observations.last().unwrap();
        assert_eq!(evidence.observe(&identity,last),ObservationDisposition::Duplicate);
        let mut unfenced = normalize(identity, None, 2000, None);
        append_foreground_reconciliation(&mut unfenced, &evidence, 4000);
        let CircuitEvent::ObservationBatch { observations, .. } = unfenced else { panic!("expected batch") };
        assert!(!observations.iter().any(|o|matches!(o.fact,ObservedWorkFact::ForegroundReconciled { .. })));
    }

    #[test]
    fn codex_foreground_completion_parks_without_claiming_owned_work_ended() {
        let mut run = RunView {
            run_id: 42,
            state: RunState::Running,
            graph: CircuitGraph::walking_skeleton("work"),
            context: CircuitContext::default(),
            steps: vec![StepView {
                node_id: "spawn".into(),
                attempt: 1,
                status: StepStatus::Running,
                outcome: None,
                error: None,
                agent_node_id: Some(9),
            }],
        };
        run.context.set("observer.receipt_cursor", "100");
        run.graph
            .nodes
            .iter_mut()
            .find(|node| node.id == "spawn")
            .unwrap()
            .kind = CircuitNodeKind::LlmTurnClassifier {
            target_node_id: Some("$source".into()),
        };
        run.context.set("source.agent_id", "9");
        let identity = ObservationIdentity {
            run_id: 42,
            step_id: "spawn".into(),
            attempt: 1,
            agent_node_id: 9,
            session_incarnation: Some("1000".into()),
            session_id: Some("session".into()),
            turn_id: Some("turn".into()),
            report_revision: None,
        };
        let event = normalize(
            identity,
            Some("1:2".into()),
            2000,
            Some("Complete synthetic report".into()),
        );
        let transition = advance(&mut run, &event);
        assert_eq!(run.steps[0].status, StepStatus::Unverified);
        assert!(run.steps[0].error.as_ref().unwrap().contains("Codex"));
        assert!(transition.effects.is_empty());
        assert_eq!(transition.observations.len(), 3);
        assert_eq!(
            run.context.get("node.spawn.output"),
            Some("Complete synthetic report")
        );
        assert!(transition.input_guard.is_some());
        assert_eq!(run.context.get("observer.receipt_cursor"), Some("100"));
        assert_eq!(run.state, RunState::Running);
        let duplicate = advance(&mut run, &event);
        assert!(duplicate.effects.is_empty());
        assert!(duplicate
            .observations
            .iter()
            .all(|record| record.disposition
                == crate::autopilot::circuit::observation::ObservationDisposition::Duplicate));
        // A recheck restores this attempt and retains observation deduplication.
        run.steps[0].status = StepStatus::Running;
        run.context.set("node.spawn.recheck_only", "1");
        let rechecked = advance(&mut run, &event);
        assert!(rechecked.effects.is_empty());
        assert_eq!(run.steps[0].status, StepStatus::Unverified);
        assert!(rechecked.input_guard.is_some());
        let classified = advance(
            &mut run,
            &CircuitEvent::TurnClassified { binding: None,
                node_id: "spawn".into(),
                classification: Some(crate::autopilot::evaluator::Classification::Completed),
                output: Some("Completed".into()),
            },
        );
        assert_eq!(run.steps[0].status, StepStatus::Unverified);
        assert!(classified.effects.is_empty());
        assert_eq!(run.steps[0].attempt, 1);
    }

    #[test]
    fn codex_recheck_without_current_completion_returns_to_unverified_same_attempt() {
        use crate::autopilot::circuit::{
            model::CircuitGraph,
            stepper::{advance, RunState, StepStatus, StepView},
        };
        let mut view = RunView {
            run_id: 42,
            state: RunState::Running,
            graph: CircuitGraph::walking_skeleton("work"),
            context: CircuitContext::default(),
            steps: vec![StepView {
                node_id: "spawn".into(),
                attempt: 1,
                status: StepStatus::Running,
                outcome: None,
                error: None,
                agent_node_id: Some(9),
            }],
        };
        view.context.set("node.spawn.recheck_only", "1");
        let event = recheck_unavailable(&view, &view.steps[0], "no current turn")
            .expect("a pending explicit recheck must produce an outcome");

        let transition = advance(&mut view, &event);
        assert_eq!(view.steps[0].status, StepStatus::Unverified);
        assert_eq!(view.steps[0].attempt, 1);
        assert!(view.steps[0].error.as_deref().unwrap().contains("no current turn"));
        assert!(transition.effects.is_empty());

        view.steps[0].status = StepStatus::Running;
        view.context.set("node.spawn.recheck_only", "0");
        assert!(recheck_unavailable(&view, &view.steps[0], "no current turn").is_none());
    }
}
