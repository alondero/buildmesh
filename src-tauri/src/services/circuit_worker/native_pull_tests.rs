//! Tests for `services::circuit_worker::native_pull`.

use super::*;

fn codex_pull() -> NativePull {
    *crate::circuit::strategy::for_stored("codex")
        .native_pull()
        .expect("Codex declares a native pull")
}
use crate::circuit::{
    context::CircuitContext,
    model::CircuitGraph,
    stepper::{advance, RunState, StepStatus},
};

#[test]
fn codex_recheck_resolves_only_the_exact_foreground_conflict_and_retains_ownership_limit() {
    use crate::circuit::observation::{ObservationDisposition, WorkEvidence};
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
    let mut evidence = WorkEvidence::default();
    for (index, fact) in [
        ObservedWorkFact::ForegroundTerminated,
        ObservedWorkFact::Working,
    ]
    .into_iter()
    .enumerate()
    {
        evidence.observe(
            &identity,
            &CircuitObservation {
                identity: identity.clone(),
                source: "native".into(),
                source_id: Some(index.to_string()),
                observed_at_ms: 2000 + index as i64,
                authoritative: true,
                fact,
            },
        );
    }
    assert!(evidence.conflicted);
    let mut recheck = normalize(
        &codex_pull(),
        identity.clone(),
        Some("1:2".into()),
        2000,
        None,
    );
    append_foreground_reconciliation(&codex_pull(), &mut recheck, &evidence, 3000);
    let CircuitEvent::ObservationBatch { observations, .. } = &recheck else {
        panic!("expected batch")
    };
    assert!(observations
        .iter()
        .any(|o| matches!(o.fact, ObservedWorkFact::ForegroundReconciled { .. })));
    for observation in observations {
        evidence.observe(&identity, observation);
    }
    assert!(!evidence.conflicted);
    assert!(evidence.foreground_terminated);
    assert!(!evidence.ownership_covered);
    assert!(!evidence.completion_verified());
    let last = observations.last().unwrap();
    assert_eq!(
        evidence.observe(&identity, last),
        ObservationDisposition::Duplicate
    );
    let mut unfenced = normalize(&codex_pull(), identity, None, 2000, None);
    append_foreground_reconciliation(&codex_pull(), &mut unfenced, &evidence, 4000);
    let CircuitEvent::ObservationBatch { observations, .. } = unfenced else {
        panic!("expected batch")
    };
    assert!(!observations
        .iter()
        .any(|o| matches!(o.fact, ObservedWorkFact::ForegroundReconciled { .. })));
}

#[test]
fn codex_foreground_completion_waits_for_handoff_without_claiming_owned_work_ended() {
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
        &codex_pull(),
        identity,
        Some("1:2".into()),
        2000,
        Some("Complete synthetic report".into()),
    );
    let transition = advance(&mut run, &event);
    // Codex declares that its rollout cannot cover owned work. That gap is
    // recorded, never read as verified completion, and is the normal route
    // to a report-based handoff, so it raises no checkpoint by itself.
    assert_eq!(run.steps[0].status, StepStatus::Running);
    assert_eq!(run.steps[0].error, None);
    let evidence: crate::circuit::observation::WorkEvidence =
        serde_json::from_str(run.context.get("node.spawn.evidence.1").unwrap()).unwrap();
    assert!(evidence.foreground_terminated);
    assert!(!evidence.ownership_covered);
    assert!(!evidence.lifecycle_verified());
    assert!(
        run.observation_pending("spawn"),
        "readiness keeps checking it"
    );
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
    assert!(
        duplicate.observations.is_empty(),
        "a repeated report must not record duplicate observations"
    );
    assert!(!duplicate.context_changed);
    // A recheck keeps this attempt and retains observation deduplication, but
    // the same completion again cannot resolve the checkpoint being rechecked,
    // so the recheck gets a definite answer instead of leaving the step
    // Running with its checkpoint hidden.
    let delivery = "Prompt delivery is unverified: the terminal did not echo the prompt";
    for (stored, expected) in [
        (None, codex_pull().ownership_limit.to_owned()),
        (Some(delivery), delivery.to_owned()),
    ] {
        let mut rechecked_run = run.clone();
        rechecked_run.context.set("node.spawn.recheck_only", "1");
        if let Some(stored) = stored {
            rechecked_run
                .context
                .set("node.spawn.recheck_checkpoint", stored);
        }
        let rechecked = advance(&mut rechecked_run, &event);
        assert!(rechecked.effects.is_empty());
        assert!(rechecked.observations.is_empty());
        assert_eq!(rechecked.step_writes.len(), 1, "{stored:?}");
        let step = &rechecked_run.steps[0];
        assert_eq!(step.status, StepStatus::Unverified, "{stored:?}");
        assert_eq!(step.attempt, 1);
        assert!(
            step.error
                .as_deref()
                .is_some_and(|error| error.contains(&expected)),
            "{stored:?}: {:?}",
            step.error
        );
        // The answer is stable: the pull keeps returning the same completion,
        // and must not flip the step back to Running on the next poll.
        for poll in 0..2 {
            let repeat = advance(&mut rechecked_run, &event);
            assert!(repeat.step_writes.is_empty(), "{stored:?} poll {poll}");
            assert_eq!(
                rechecked_run.steps[0].status,
                StepStatus::Unverified,
                "{stored:?} poll {poll}"
            );
        }
    }
    // A decision that cannot bind a current report is still a checkpoint.
    let classified = advance(
        &mut run,
        &CircuitEvent::TurnClassified {
            binding: None,
            node_id: "spawn".into(),
            classification: Some(crate::circuit::evaluator::Classification::Completed),
            output: Some("Completed".into()),
        },
    );
    assert_eq!(run.steps[0].status, StepStatus::Unverified);
    assert!(classified.effects.is_empty());
    assert_eq!(run.steps[0].attempt, 1);
}

#[test]
fn codex_recheck_without_current_completion_returns_to_unverified_same_attempt() {
    use crate::circuit::{
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
    let event = recheck_unavailable(&view, &view.steps[0], &codex_pull(), "no current turn")
        .expect("a pending explicit recheck must produce an outcome");

    let transition = advance(&mut view, &event);
    assert_eq!(view.steps[0].status, StepStatus::Unverified);
    assert_eq!(view.steps[0].attempt, 1);
    assert!(view.steps[0]
        .error
        .as_deref()
        .unwrap()
        .contains("no current turn"));
    assert!(transition.effects.is_empty());

    view.steps[0].status = StepStatus::Running;
    view.context.set("node.spawn.recheck_only", "0");
    assert!(recheck_unavailable(&view, &view.steps[0], &codex_pull(), "no current turn").is_none());
}

// ----- Production pull seam (issue #2128 review) -----------------------
//
// These drive `observe_with`, the function the worker runs each tick: it
// selects the declared pull, checks the input and session fences, reads the
// completion and attaches the transcript guard. Only the three live reads
// are replaced by fixed values; the rollout is a real file whose stability
// is really checked.

const COMPLETED_AT_MS: i64 = 1_789_324_053_252;

fn rollout(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("rollout.jsonl");
    std::fs::write(
        &path,
        concat!(
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"turn-1\"}}\n",
            "{\"timestamp\":\"2026-09-13T18:27:33.252Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"turn-1\",\"last_agent_message\":\"All done\"}}\n",
        ),
    )
    .unwrap();
    path
}

fn pull_view(recheck_only: bool) -> RunView {
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
    run.graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "spawn")
        .unwrap()
        .kind = CircuitNodeKind::LlmTurnClassifier {
        target_node_id: Some("$source".into()),
    };
    run.context.set("source.agent_id", "9");
    if recheck_only {
        run.context.set("node.spawn.recheck_only", "1");
    }
    run
}

/// A node launched as `launched` whose stored provider now says `stored`
/// (for example after its profile was remapped).
fn node_launched_as(launched: &str, stored: &str) -> crate::models::AgentNode {
    use crate::preferences::launch_configurations::{capture, LaunchOverrides};
    crate::models::AgentNode {
        id: 9,
        provider: stored.into(),
        cli_session_id: Some("session".into()),
        launch_configuration: Some(
            crate::preferences::spawn_configurations::SpawnConfiguration {
                resolved: Some(
                    capture(
                        &crate::preferences::AppPreferences::default(),
                        launched,
                        &LaunchOverrides::default(),
                    )
                    .unwrap(),
                ),
                ..Default::default()
            },
        ),
        ..Default::default()
    }
}

fn observe_fixed(
    view: &RunView,
    node: &crate::models::AgentNode,
    input: Option<&str>,
    turn_stamp: Option<&str>,
    snapshot: Option<crate::services::transcript_reader::NativeTurnSnapshot>,
) -> Option<CircuitEvent> {
    let strategy = crate::circuit::strategy::for_agent(node);
    let pull = strategy.native_pull().expect("node runs a pull harness");
    let mut snapshot = snapshot;
    observe_with(
        view,
        &view.steps[0],
        node,
        pull,
        PullInputs {
            input_stamp: &mut || input.map(str::to_owned),
            turn_stamp: &mut || turn_stamp.map(str::to_owned),
            completion: &mut || snapshot.take(),
        },
    )
}

#[test]
fn a_current_completion_is_fenced_guarded_and_keeps_the_step_running() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot = crate::services::transcript_reader::test_support::native_snapshot(
        &rollout(dir.path()),
        crate::services::transcript_reader::TranscriptFormat::Codex,
    );
    let view = pull_view(false);
    let node = node_launched_as("codex", "codex");
    let event = observe_fixed(&view, &node, Some("1:2"), Some("1000:3"), Some(snapshot))
        .expect("a current completion is observed");
    let CircuitEvent::ObservationBatch {
        expected,
        observations,
        input_guard,
        stale,
        ..
    } = &event
    else {
        panic!("native batch expected")
    };
    assert!(!stale);
    assert_eq!(expected.turn_id.as_deref(), Some("turn-1"));
    assert_eq!(expected.session_id.as_deref(), Some("session"));
    assert_eq!(expected.session_incarnation.as_deref(), Some("1000"));
    let guard = input_guard.as_ref().expect("input fence attached");
    assert_eq!(guard.input_stamp, "1:2");
    assert_eq!(guard.session_id, "session");
    assert_eq!(guard.session_incarnation, "1000");
    assert!(
        guard.transcript_guard.is_some(),
        "the transcript guard is attached so later activity rejects the commit"
    );
    assert!(observations.iter().all(|o| o.source == codex_pull().source));
    assert!(observations.iter().all(|o| o.authoritative));
    let mut run = view.clone();
    let transition = advance(&mut run, &event);
    assert_eq!(run.steps[0].status, StepStatus::Running);
    assert!(transition.step_writes.is_empty(), "no checkpoint is raised");
    assert!(transition.effects.is_empty());
    assert_eq!(run.context.get("node.spawn.output"), Some("All done"));
}

#[test]
fn stale_or_unfenced_completions_produce_no_evidence_and_a_recheck_stays_unverified() {
    let dir = tempfile::tempdir().unwrap();
    let path = rollout(dir.path());
    let format = crate::services::transcript_reader::TranscriptFormat::Codex;
    let snapshot =
        || crate::services::transcript_reader::test_support::native_snapshot(&path, format);
    let node = node_launched_as("codex", "codex");

    // Without a pending recheck an unavailable pull is simply silent.
    let quiet = pull_view(false);
    let cases: Vec<(&str, Option<CircuitEvent>)> = vec![
        (
            "input cannot be fenced",
            observe_fixed(&quiet, &node, None, Some("1000:3"), Some(snapshot())),
        ),
        (
            "no incarnation",
            observe_fixed(&quiet, &node, Some("1:2"), None, Some(snapshot())),
        ),
        (
            "incarnation not numeric",
            observe_fixed(&quiet, &node, Some("1:2"), Some("abc:3"), Some(snapshot())),
        ),
        (
            "no completion",
            observe_fixed(&quiet, &node, Some("1:2"), Some("1000:3"), None),
        ),
        (
            "completed before this session began",
            observe_fixed(
                &quiet,
                &node,
                Some("1:2"),
                Some(&format!("{}:3", COMPLETED_AT_MS + 1)),
                Some(snapshot()),
            ),
        ),
    ];
    for (name, event) in cases {
        assert!(event.is_none(), "{name}: no evidence without a recheck");
    }

    // The transcript changing after it was read is not a current completion.
    let stable = snapshot();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .and_then(|mut file| {
            use std::io::Write;
            file.write_all(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"turn-2\"}}\n")
        })
        .unwrap();
    assert!(observe_fixed(&quiet, &node, Some("1:2"), Some("1000:3"), Some(stable)).is_none());

    // An explicit recheck that cannot be satisfied returns the same attempt
    // to Unverified instead of leaving it Running.
    let recheck = pull_view(true);
    let event = observe_fixed(&recheck, &node, Some("1:2"), Some("1000:3"), None)
        .expect("a pending recheck reports why it stays unverified");
    let mut run = recheck.clone();
    advance(&mut run, &event);
    assert_eq!(run.steps[0].status, StepStatus::Unverified);
    assert_eq!(run.steps[0].attempt, 1);
    assert!(run.steps[0].error.as_deref().unwrap().contains("Codex"));
}

#[test]
fn steps_a_pull_does_not_apply_to_are_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot = crate::services::transcript_reader::test_support::native_snapshot(
        &rollout(dir.path()),
        crate::services::transcript_reader::TranscriptFormat::Codex,
    );
    let mut view = pull_view(false);
    view.graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "spawn")
        .unwrap()
        .kind = CircuitNodeKind::SetNodeStatus {
        target_node_id: Some("$source".into()),
        status: crate::circuit::model::SessionStatusKind::Idle,
    };
    let node = node_launched_as("codex", "codex");
    let mut reads = 0;
    let strategy = crate::circuit::strategy::for_agent(&node);
    let mut snapshot = Some(snapshot);
    let event = observe_with(
        &view,
        &view.steps[0],
        &node,
        strategy.native_pull().unwrap(),
        PullInputs {
            input_stamp: &mut || {
                reads += 1;
                Some("1:2".into())
            },
            turn_stamp: &mut || Some("1000:3".into()),
            completion: &mut || snapshot.take(),
        },
    );
    assert!(event.is_none());
    assert_eq!(reads, 0, "a step the pull does not apply to reads nothing");
}

#[test]
fn a_declared_pull_is_always_served_by_the_reader_of_the_same_harness() {
    // `observe` resolves the harness once and hands that value to both the
    // strategy and the reader, so the pairing is fixed by the declaration:
    // every harness that declares a pull has a reader, and it is its own.
    use crate::services::transcript_reader::TranscriptFormat;
    for provider in crate::models::Provider::all() {
        let strategy = crate::circuit::strategy::strategy_of(Some(*provider));
        let format = crate::coordinator::enrichment::native_completion_format_for(*provider);
        if strategy.native_pull().is_some() {
            assert_eq!(
                format,
                TranscriptFormat::for_harness(provider.adapter().id()),
                "{}: a pull is read with its own harness's format",
                provider.adapter().id()
            );
            assert!(format.is_some(), "{}", provider.adapter().id());
        }
    }
    // The reader is chosen by the resolved harness alone, never by the
    // node's stored provider.
    assert_eq!(
        crate::coordinator::enrichment::native_completion_format_for(
            crate::models::Provider::Codex
        ),
        Some(TranscriptFormat::Codex)
    );
}

#[test]
fn the_pull_and_its_reader_follow_the_launch_snapshot_not_the_current_profile() {
    use crate::services::transcript_reader::TranscriptFormat;
    let format = crate::coordinator::enrichment::native_completion_format;
    let pulls = |node: &crate::models::AgentNode| {
        crate::circuit::strategy::for_agent(node)
            .native_pull()
            .is_some()
    };
    // Launched as Codex, profile since remapped to Claude: still Codex.
    let remapped = node_launched_as("codex", "claude");
    assert!(pulls(&remapped));
    assert_eq!(format(&remapped), Some(TranscriptFormat::Codex));
    // Launched as Claude, profile since remapped to Codex: never Codex's
    // reader, so a rollout cannot be read as a Claude node's completion.
    let launched_claude = node_launched_as("claude", "codex");
    assert!(!pulls(&launched_claude));
    assert_eq!(format(&launched_claude), Some(TranscriptFormat::ClaudeCode));
    // No snapshot: the stored provider decides, and unknown has no reader.
    let stored = |provider: &str| crate::models::AgentNode {
        provider: provider.into(),
        ..Default::default()
    };
    assert_eq!(
        format(&stored("codex:openrouter")),
        Some(TranscriptFormat::Codex)
    );
    assert_eq!(format(&stored("future-harness")), None);
}
