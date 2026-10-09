//! The pull half of a harness's observation strategy: re-read the harness's
//! own native turn-completion record while a step is unsettled. Whether a
//! harness has such a pull, and what it is called, comes from the adapter's
//! declaration (`crate::circuit::strategy`); this module owns only the shared
//! freshness fencing and normalization into the observation vocabulary.
//!
//! A native completion establishes only the foreground turn. A harness whose
//! record carries no child/background registry (Codex rollouts, including work
//! started by code-mode calls) cannot establish ownership coverage, so every
//! pull emits an explicit ownership gap.

use super::{CircuitEvent, CircuitNodeKind, RunView, StepView};
use crate::circuit::observation::{CircuitObservation, ObservationIdentity, ObservedWorkFact};
use crate::circuit::stepper::ObservationInputFence;
use crate::circuit::strategy::NativePull;

/// The live reads a pull depends on, supplied lazily so a step the pull does
/// not apply to never touches the process registry, the database or the disk.
/// Tests supply fixed values; production reads the real sources.
pub(super) struct PullInputs<'a> {
    pub input_stamp: &'a mut dyn FnMut() -> Option<String>,
    /// `<incarnation>:<generation>` as stored by `agent_turn_stamp`.
    pub turn_stamp: &'a mut dyn FnMut() -> Option<String>,
    pub completion:
        &'a mut dyn FnMut() -> Option<crate::services::transcript_reader::NativeTurnSnapshot>,
}

pub(super) fn observe(
    view: &RunView,
    step: &StepView,
    node: &crate::models::AgentNode,
) -> Option<CircuitEvent> {
    // Resolve the harness once: the pull and the reader that serves it must
    // describe the same harness even if a profile is remapped mid-read.
    let provider = crate::circuit::strategy::provider_for_agent(node)?;
    let strategy = crate::circuit::strategy::strategy_of(Some(provider));
    let pull = strategy.native_pull()?;
    observe_with(
        view,
        step,
        node,
        pull,
        PullInputs {
            input_stamp: &mut || crate::agent::process::PROCESS_REGISTRY.input_stamp(node.id),
            turn_stamp: &mut || crate::db::agent_turn_stamp(node.id).ok().flatten(),
            completion: &mut || {
                crate::coordinator::enrichment::native_turn_completion_for(node, provider)
            },
        },
    )
}

pub(super) fn observe_with(
    view: &RunView,
    step: &StepView,
    node: &crate::models::AgentNode,
    pull: &NativePull,
    inputs: PullInputs<'_>,
) -> Option<CircuitEvent> {
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
    let label = pull.label;
    let unavailable = |reason: &str| recheck_unavailable(view, step, pull, reason);
    let Some(input) = (inputs.input_stamp)() else {
        return unavailable(&format!(
            "{label} input is changing or cannot be fenced; no current completion was established."
        ));
    };
    let Some(incarnation) = (inputs.turn_stamp)().and_then(|stamp| {
        stamp
            .split_once(':')
            .map(|(incarnation, _)| incarnation.to_owned())
    }) else {
        return unavailable(&format!(
            "{label} session incarnation is unavailable; no current completion was established."
        ));
    };
    let Some(snapshot) = (inputs.completion)() else {
        return unavailable(&format!(
            "No current {label} foreground completion could be re-established."
        ));
    };
    let Some(incarnation_ms) = incarnation.parse::<i64>().ok() else {
        return unavailable(&format!("{label} session incarnation cannot be interpreted; no current completion was established."));
    };
    if snapshot.completion.completed_at_ms < incarnation_ms || !snapshot.is_current() {
        return unavailable(&format!(
            "The {label} completion no longer matches the current session input."
        ));
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
        pull,
        identity,
        Some(input),
        snapshot.completion.completed_at_ms,
        snapshot.completion.final_report.clone(),
    );
    if let Some(evidence) = view
        .context
        .get(&format!("node.{}.evidence.{}", step.node_id, step.attempt))
        .and_then(|json| {
            serde_json::from_str::<crate::circuit::observation::WorkEvidence>(json).ok()
        })
    {
        append_foreground_reconciliation(
            pull,
            &mut event,
            &evidence,
            chrono::Utc::now().timestamp_millis(),
        );
    }
    if let CircuitEvent::ObservationBatch {
        input_guard: Some(guard),
        ..
    } = &mut event
    {
        guard.transcript_guard = Some(snapshot);
    }
    Some(event)
}

fn recheck_unavailable(
    view: &RunView,
    step: &StepView,
    pull: &NativePull,
    reason: &str,
) -> Option<CircuitEvent> {
    (step.status == crate::circuit::stepper::StepStatus::Running
        && view
            .context
            .get(&format!("node.{}.recheck_only", step.node_id))
            == Some("1"))
    .then(|| CircuitEvent::EffectUncertain {
        node_id: step.node_id.clone(),
        attempt: step.attempt,
        reason: format!(
            "{} evidence recheck remains Unverified: {reason}",
            pull.label
        ),
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
    // The completion carries the source its own harness's pull declared, so a
    // recheck is only ever re-issued for the harness that produced it.
    let pull = observations.iter().find_map(|observation| {
        crate::circuit::strategy::native_pull_for_source(&observation.source)
    });
    (pull.is_some()
        && view.context.get(&format!("node.{}.recheck_only", expected.step_id)) == Some("1")
        && expected.run_id == view.run_id
        && expected.attempt == step.attempt
        && step.status == crate::circuit::stepper::StepStatus::Running
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
            &pull?,
            "the completion was rejected by the current input or session freshness fence; no current completion was established.",
        )
    })
    .flatten()
}

fn append_foreground_reconciliation(
    pull: &NativePull,
    event: &mut CircuitEvent,
    evidence: &crate::circuit::observation::WorkEvidence,
    observed_at_ms: i64,
) {
    use crate::circuit::observation::EvidenceConflictKind;
    let CircuitEvent::ObservationBatch {
        expected,
        observations,
        input_guard: Some(_),
        stale: false,
        ..
    } = event
    else {
        return;
    };
    // Only the harness's own validated exact-session pull can issue this fact.
    // Resolving foreground uncertainty never supplies owned-work coverage.
    for conflict in &evidence.conflicts {
        if conflict.kind != EvidenceConflictKind::Foreground
            || conflict.identity.run_id != expected.run_id
            || conflict.identity.step_id != expected.step_id
            || conflict.identity.attempt != expected.attempt
            || conflict.identity.agent_node_id != expected.agent_node_id
            || conflict.identity.session_incarnation != expected.session_incarnation
            || conflict.identity.session_id != expected.session_id
            || conflict.identity.turn_id != expected.turn_id
        {
            continue;
        }
        observations.push(CircuitObservation {
            identity: expected.clone(),
            source: pull.recheck_source.into(),
            source_id: Some(format!("reconcile:{}", conflict.id)),
            observed_at_ms,
            authoritative: true,
            fact: ObservedWorkFact::ForegroundReconciled {
                conflict_id: conflict.id.clone(),
            },
        });
    }
}

fn normalize(
    pull: &NativePull,
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
                transcript_guard: None,
                report_guard: None,
                agent_node_id: identity.agent_node_id,
                input_stamp,
                observed_at_ms: completed_at_ms,
                session_id,
                session_incarnation,
            },
        );
    let mut facts = vec![
        ObservedWorkFact::ForegroundTerminated,
        ObservedWorkFact::OwnershipUnavailable {
            reason: pull.ownership_limit.into(),
        },
    ];
    if let Some(text) = final_report {
        let revision = format!("{:x}", Sha256::digest(text.as_bytes()));
        facts.push(ObservedWorkFact::AssistantReport { text, revision });
    }
    let observations = facts
        .into_iter()
        .enumerate()
        .map(|(index, fact)| CircuitObservation {
            identity: identity.clone(),
            source: pull.source.into(),
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
            &codex_pull(),
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
                == crate::circuit::observation::ObservationDisposition::Duplicate));
        // A recheck restores this attempt and retains observation deduplication.
        run.steps[0].status = StepStatus::Running;
        run.context.set("node.spawn.recheck_only", "1");
        let rechecked = advance(&mut run, &event);
        assert!(rechecked.effects.is_empty());
        assert_eq!(run.steps[0].status, StepStatus::Unverified);
        assert!(rechecked.input_guard.is_some());
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
        assert!(
            recheck_unavailable(&view, &view.steps[0], &codex_pull(), "no current turn").is_none()
        );
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
    fn a_current_completion_is_fenced_guarded_and_parks_the_step_unverified() {
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
        assert_eq!(run.steps[0].status, StepStatus::Unverified);
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
}
