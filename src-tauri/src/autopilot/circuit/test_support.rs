//! Synthetic authoritative adapter facts for graph-routing tests. Negative
//! evidence tests call `advance` directly and must not use this fixture.

use super::observation::{CircuitObservation, ObservationIdentity, ObservedWorkFact, WorkEvidence};
use super::stepper::{
    advance, CircuitEvent, ClassificationBinding, ObservationInputFence, RunView, Transition,
};

pub(crate) fn advance_with_report_evidence(run: &mut RunView, event: &CircuitEvent) -> Transition {
    let event = bind_report_evidence(run, event);
    advance(run, &event)
}

pub(crate) fn bind_report_evidence(run: &mut RunView, event: &CircuitEvent) -> CircuitEvent {
    let mut event = event.clone();
    if let CircuitEvent::TurnClassified {
        node_id,
        classification: Some(_),
        output,
        binding,
    } = &mut event
    {
        if binding.is_none() {
            let text = output
                .clone()
                .unwrap_or_else(|| "Synthetic final report".into());
            *binding = Some(record_report_evidence(run, node_id, &text));
            *output = Some(text);
        }
    }
    event
}

pub(crate) fn record_report_evidence(
    run: &mut RunView,
    owner: &str,
    text: &str,
) -> ClassificationBinding {
    use sha2::{Digest, Sha256};
    record_report_evidence_for_turn(run, owner, text, &format!("turn-{:x}", Sha256::digest(text.as_bytes())))
}

pub(crate) fn record_report_evidence_for_turn(run: &mut RunView, owner: &str, text: &str, turn_id: &str) -> ClassificationBinding {
    use sha2::{Digest, Sha256};
    let step = run.step(owner).expect("native evidence fixture owner");
    let agent_node_id = step
        .agent_node_id
        .or_else(|| run.resolve_target_agent(owner))
        .expect("native evidence fixture target");
    let attempt = step.attempt;
    let revision = format!("{:x}", Sha256::digest(text.as_bytes()));
    let identity = ObservationIdentity {
        run_id: run.run_id,
        step_id: owner.into(),
        attempt,
        agent_node_id,
        session_incarnation: Some("100".into()),
        session_id: Some(format!("session-{agent_node_id}")),
        turn_id: Some(turn_id.into()),
        report_revision: None,
    };
    let input_guard = ObservationInputFence {
                transcript_guard: None,
        agent_node_id,
        input_stamp: format!("input-{turn_id}"),
        observed_at_ms: 1000,
        session_id: identity.session_id.clone().unwrap(),
        session_incarnation: "100".into(),
    };
    let observations = [
        ObservedWorkFact::ForegroundTerminated,
        ObservedWorkFact::OwnershipCovered,
        ObservedWorkFact::AssistantReport {
            text: text.into(),
            revision: revision.clone(),
        },
    ]
    .into_iter()
    .enumerate()
    .map(|(index, fact)| CircuitObservation {
        identity: identity.clone(),
        source: "synthetic_adapter".into(),
        source_id: Some(format!("{revision}:{index}")),
        observed_at_ms: 1000,
        authoritative: true,
        fact,
    })
    .collect();
    advance(
        run,
        &CircuitEvent::ObservationBatch {
            receipt_id: 0,
            expected: identity,
            observations,
            stale: false,
            input_guard: Some(input_guard.clone()),
        },
    );
    let evidence: WorkEvidence = serde_json::from_str(
        run.context
            .get(&format!("node.{owner}.evidence.{attempt}"))
            .expect("recorded evidence"),
    )
    .unwrap();
    ClassificationBinding {
        owner: evidence.identity.unwrap(),
        report_revision: revision,
        input_guard,
    }
}

/// Graph-routing fixtures must supply terminal ownership evidence; a legacy
/// process/status callback is deliberately insufficient for assigned work.
pub(crate) fn advance_with_completion_evidence(run: &mut RunView, event: &CircuitEvent) -> Transition {
    if let CircuitEvent::AgentFinished { agent_node_id, success: true, output } = event {
        if let Some(step) = run.steps.iter().find(|step| step.status == super::stepper::StepStatus::Running
            && step.agent_node_id == Some(*agent_node_id)) {
            let identity = ObservationIdentity { run_id:run.run_id,step_id:step.node_id.clone(),attempt:step.attempt,
                agent_node_id:*agent_node_id,session_incarnation:Some("100".into()),session_id:Some(format!("session-{agent_node_id}")),
                turn_id:Some(format!("finished-{}",step.attempt)),report_revision:None };
            let mut facts = vec![ObservedWorkFact::ForegroundTerminated, ObservedWorkFact::OwnershipCovered,
                ObservedWorkFact::AssignedWorkCompleted];
            if let Some(text) = output {
                facts.push(ObservedWorkFact::AssistantReport { text:text.clone(), revision:"completion-report".into() });
            }
            let observations = facts
                .into_iter().enumerate().map(|(index,fact)| CircuitObservation { identity:identity.clone(),
                    source:"synthetic_completion_adapter".into(),source_id:Some(index.to_string()),observed_at_ms:1000,
                    authoritative:true,fact }).collect();
            return advance(run,&CircuitEvent::ObservationBatch {receipt_id:0,expected:identity,observations,stale:false,input_guard:None});
        }
    }
    advance(run,event)
}
