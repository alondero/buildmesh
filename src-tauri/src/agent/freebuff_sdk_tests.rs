use std::collections::BTreeSet;
use std::path::Path;

use super::*;
use crate::agent::provider::adapters::FREEBUFF;
use crate::agent::provider::AgentProvider;
use crate::circuit::observation::{
    CircuitObservation, ObservationDisposition, ObservationIdentity, WorkEvidence,
};
use serde_json::json;

fn state(marker: &str) -> Value {
    json!({
        "sessionState": { "marker": marker },
        "output": { "type": "text" },
        "traceSessionId": marker
    })
}

fn observation(fact: ObservedWorkFact, index: i64) -> CircuitObservation {
    CircuitObservation {
        identity: ObservationIdentity {
            run_id: 1,
            step_id: "step".into(),
            attempt: 1,
            agent_node_id: 7,
            session_incarnation: Some("inc".into()),
            session_id: Some("sess".into()),
            turn_id: Some("turn".into()),
            report_revision: None,
        },
        source: "freebuff_sdk".into(),
        source_id: Some(format!("event-{index}")),
        observed_at_ms: index,
        authoritative: true,
        fact,
    }
}

fn apply(facts: &[ObservedWorkFact]) -> WorkEvidence {
    let mut evidence = WorkEvidence::default();
    for (index, fact) in facts.iter().cloned().enumerate() {
        let disposition = evidence.observe(
            &observation(fact.clone(), index as i64).identity,
            &observation(fact, index as i64),
        );
        assert_eq!(disposition, ObservationDisposition::Accepted);
    }
    evidence
}

#[test]
fn installed_cli_does_not_bundle_the_sdk() {
    let availability = validated_sdk_availability();
    assert!(!availability.bundled_with_installed_cli);
    assert_eq!(availability.package, "@codebuff/sdk");
    assert!(availability.requires_api_key_or_byok);
    assert!(!availability.cli_bearer_is_sdk_api_key);
}

#[test]
fn interactive_cli_stays_unwired() {
    let strategy = FREEBUFF.circuit_observation();
    assert!(strategy.hooks().is_none());
    assert!(strategy.native_pull().is_none());
    assert!(strategy.passive_watcher.is_none());
}

#[test]
fn controlled_run_resumes_persisted_state_and_maps_events() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = DirectoryRunStateStore::new(dir.path());
    let first = integrate_controlled_run(
        &store,
        "node-7",
        &state("run-1"),
        &[
            json!({"type": "start", "agentId": "root"}),
            json!({"type": "text", "text": "Ship the parser."}),
            json!({"type": "subagent_start", "agentId": "child-a", "agentType": "editor"}),
            json!({"type": "tool_call", "toolCallId": "t1", "toolName": "read_files", "input": {}}),
            json!({"type": "tool_result", "toolCallId": "t1", "toolName": "read_files", "output": []}),
            json!({"type": "reasoning_delta", "text": "hidden", "agentId": "root", "runId": "r", "ancestorRunIds": []}),
            json!({"type": "download", "version": "0", "status": "complete"}),
            json!({"type": "subagent_finish", "agentId": "child-a", "agentType": "editor"}),
            json!({"type": "finish", "agentId": "root"}),
        ],
    )
    .expect("first run");
    assert!(first.previous_run.is_none());
    assert_eq!(first.persisted_run, state("run-1"));
    assert_eq!(store.load("node-7").expect("load"), Some(state("run-1")));
    match &first.child_work {
        ChildWork::Tracked { open, closed } => {
            assert!(open.is_empty());
            assert_eq!(closed, &BTreeSet::from(["child-a".to_string()]));
        }
        ChildWork::Unavailable { reason } => panic!("child work unavailable: {reason}"),
    }
    assert!(first.lifecycle.contains(&LifecycleKind::WorkResumed));
    assert!(first.lifecycle.contains(&LifecycleKind::TurnCompleted));
    assert!(first.facts.iter().any(|fact| matches!(
        fact,
        ObservedWorkFact::AssistantReport { text, .. } if text == "Ship the parser."
    )));
    assert!(first
        .facts
        .contains(&ObservedWorkFact::ForegroundTerminated));
    let evidence = apply(&first.facts);
    assert!(evidence.lifecycle_verified());
    assert_eq!(
        evidence.report.as_ref().map(|report| report.text.as_str()),
        Some("Ship the parser.")
    );

    let second = integrate_controlled_run(
        &store,
        "node-7",
        &state("run-2"),
        &[
            json!({"type": "start", "agentId": "root"}),
            json!({"type": "text", "text": "Continue."}),
            json!({"type": "finish", "agentId": "root"}),
        ],
    )
    .expect("resumed run");
    assert_eq!(second.previous_run, Some(state("run-1")));
    assert_eq!(second.persisted_run, state("run-2"));
    assert_eq!(store.load("node-7").expect("reload"), Some(state("run-2")));
}

#[test]
fn open_child_is_tracked_and_does_not_complete_the_turn() {
    let folded = fold_sdk_events(&[
        json!({"type": "start"}),
        json!({"type": "subagent_start", "agentId": "child-b"}),
        json!({"type": "finish"}),
    ]);
    match &folded.child_work {
        ChildWork::Tracked { open, closed } => {
            assert!(closed.is_empty());
            assert_eq!(open, &BTreeSet::from(["child-b".to_string()]));
        }
        ChildWork::Unavailable { reason } => panic!("child work unavailable: {reason}"),
    }
    assert!(folded.lifecycle.contains(&LifecycleKind::BackgroundRunning));
    assert!(!folded.lifecycle.contains(&LifecycleKind::TurnCompleted));
    assert!(folded.facts.iter().any(|fact| matches!(
        fact,
        ObservedWorkFact::OwnershipSnapshot { active_work }
            if active_work == &vec!["freebuff-agent:child-b".to_string()]
    )));
    let evidence = apply(&folded.facts);
    assert!(!evidence.lifecycle_verified());
}

#[test]
fn malformed_child_event_is_explicitly_unavailable() {
    let folded = fold_sdk_events(&[
        json!({"type": "start"}),
        json!({"type": "subagent_start"}),
        json!({"type": "text", "text": "partial"}),
        json!({"type": "finish"}),
    ]);
    assert!(matches!(
        folded.child_work,
        ChildWork::Unavailable { ref reason } if reason.contains("agentId")
    ));
    assert!(folded.lifecycle.contains(&LifecycleKind::SignalUnavailable));
    assert!(!folded.lifecycle.contains(&LifecycleKind::TurnCompleted));
    assert!(folded
        .facts
        .iter()
        .any(|fact| matches!(fact, ObservedWorkFact::OwnershipUnavailable { .. })));
    let mut evidence = WorkEvidence::default();
    let mut saw_unavailable = false;
    for (index, fact) in folded.facts.iter().cloned().enumerate() {
        let event = observation(fact, index as i64);
        let disposition = evidence.observe(&event.identity, &event);
        if disposition == ObservationDisposition::Unavailable {
            saw_unavailable = true;
        }
    }
    assert!(saw_unavailable);
    assert!(!evidence.lifecycle_verified());
}

#[test]
fn unknown_event_type_does_not_invent_a_child_registry() {
    let folded = fold_sdk_events(&[
        json!({"type": "start"}),
        json!({"type": "subagent_chunk", "agentId": "ghost"}),
        json!({"type": "finish"}),
    ]);
    assert!(matches!(folded.child_work, ChildWork::Unavailable { .. }));
    assert!(!folded.lifecycle.contains(&LifecycleKind::TurnCompleted));
}

#[test]
fn retryable_error_does_not_settle_the_run() {
    let folded = fold_sdk_events(&[
        json!({"type": "start"}),
        json!({"type": "error", "message": "cut", "source": "stream-interrupted"}),
        json!({"type": "text", "text": "recovered"}),
        json!({"type": "finish"}),
    ]);
    assert!(matches!(folded.child_work, ChildWork::Tracked { .. }));
    assert!(folded.lifecycle.contains(&LifecycleKind::TurnCompleted));
    assert!(!folded.lifecycle.contains(&LifecycleKind::Error));
}

#[test]
fn unsourced_tool_error_does_not_settle_a_finished_run() {
    let folded = fold_sdk_events(&[
        json!({"type": "start", "agentId": "root"}),
        json!({"type": "text", "text": "Before."}),
        json!({"type": "error", "message": "Tool `foo` is not currently available."}),
        json!({"type": "text", "text": "Recovered."}),
        json!({"type": "subagent_start", "agentId": "child-a", "agentType": "editor", "displayName": "Editor", "onlyChild": true}),
        json!({"type": "subagent_finish", "agentId": "child-a", "agentType": "editor", "displayName": "Editor", "onlyChild": true}),
        json!({"type": "finish", "agentId": "root"}),
    ]);
    assert!(matches!(
        folded.child_work,
        ChildWork::Tracked { ref closed, .. } if closed.contains("child-a")
    ));
    assert!(folded.lifecycle.contains(&LifecycleKind::TurnCompleted));
    assert!(!folded.lifecycle.contains(&LifecycleKind::Error));
    let evidence = apply(&folded.facts);
    assert_eq!(
        evidence.report.as_ref().map(|report| report.text.as_str()),
        Some("Before.Recovered.")
    );
    assert!(evidence.lifecycle_verified());
}

#[test]
fn unsourced_error_without_finish_fails_the_run() {
    let folded = fold_sdk_events(&[
        json!({"type": "start", "agentId": "root"}),
        json!({"type": "error", "message": "boom"}),
    ]);
    assert!(matches!(folded.child_work, ChildWork::Unavailable { .. }));
    assert!(folded.lifecycle.contains(&LifecycleKind::Error));
    assert!(!folded.lifecycle.contains(&LifecycleKind::TurnCompleted));
    assert!(folded
        .facts
        .iter()
        .any(|fact| matches!(fact, ObservedWorkFact::Unavailable)));
    let mut evidence = WorkEvidence::default();
    let mut saw_unavailable = false;
    for (index, fact) in folded.facts.iter().cloned().enumerate() {
        let event = observation(fact, index as i64);
        if evidence.observe(&event.identity, &event) == ObservationDisposition::Unavailable {
            saw_unavailable = true;
        }
    }
    assert!(saw_unavailable);
    assert!(!evidence.lifecycle_verified());
}

#[test]
fn terminal_error_is_not_resumable_and_does_not_replace_stored_state() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = DirectoryRunStateStore::new(dir.path());
    integrate_controlled_run(&store, "node-7", &state("kept"), &[]).expect("seed");
    let error = integrate_controlled_run(
        &store,
        "node-7",
        &json!({"output": {"type": "error", "message": "nope"}}),
        &[json!({"type": "error", "message": "nope"})],
    )
    .expect_err("error output");
    assert!(matches!(error, SdkRunError::InvalidRunState(_)));
    assert_eq!(
        store.load("node-7").expect("unchanged"),
        Some(state("kept"))
    );
}

#[test]
fn run_state_without_output_is_refused() {
    let error = validate_run_state(&json!({"traceSessionId": "x"}));
    assert!(matches!(error, Err(SdkRunError::InvalidRunState(_))));
}

#[test]
fn run_keys_reject_path_traversal() {
    let store = DirectoryRunStateStore::for_app_data(Path::new("app-data"));
    assert_eq!(
        store.root(),
        Path::new("app-data").join("freebuff-sdk-runs")
    );
    assert!(matches!(
        store.load("../secret"),
        Err(SdkRunError::InvalidKey)
    ));
    assert!(matches!(store.load(""), Err(SdkRunError::InvalidKey)));
}

#[test]
fn print_mode_contract_lists_the_validated_event_types() {
    assert!(PRINT_MODE_EVENT_TYPES.contains(&"subagent_start"));
    assert!(PRINT_MODE_EVENT_TYPES.contains(&"subagent_finish"));
    assert!(PRINT_MODE_EVENT_TYPES.contains(&"finish"));
    assert!(!PRINT_MODE_EVENT_TYPES.contains(&"subagent_chunk"));
}
