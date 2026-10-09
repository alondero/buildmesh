//! Turn and reviewer-report classification, including quiet-turn recovery.
//! Classification never publishes after its lifecycle, input or report fence changes.
use super::observation::{observed_agent_for_step, Observations};
use super::*;

/// Classify a yielded agent's report once per gate attempt. A readable
/// transcript also recovers turns produced before restart restored buffering.
pub(super) struct ClassifiedTurn {
    pub(super) classifier_error: Option<String>,
    pub(super) observation_blocker: Option<crate::circuit::observation::CircuitObservationBlocker>,
    pub(super) binding: Option<crate::circuit::stepper::ClassificationBinding>,
    pub(super) agent_node_id: i64,
    pub(super) classification: Option<crate::circuit::evaluator::Classification>,
    pub(super) output: String,
    pub(super) continuation: Option<(String, String, String)>,
    /// The gate deliberately did not classify this report because the agent is
    /// still working. The caller publishes `TurnParked`, not `TurnClassified`.
    pub(super) waiting_for_a_finished_turn: bool,
    /// The finished turn owes a result file that is not there yet. Set only in
    /// that case; the gate reminds the agent rather than classifying the turn.
    pub(super) missing_result: Option<MissingResult>,
}

impl ClassifiedTurn {
    pub(super) fn deferred(
        agent_node_id: i64,
        blocker: crate::circuit::observation::CircuitObservationBlocker,
    ) -> Self {
        Self {
            classifier_error: None,
            observation_blocker: Some(blocker),
            agent_node_id,
            classification: None,
            binding: None,
            output: String::new(),
            continuation: None,
            waiting_for_a_finished_turn: false,
            missing_result: None,
        }
    }
}

/// What the reminder needs: where the agent must save its result, and the
/// turn, report and input the reminder is fenced to.
pub(super) struct MissingResult {
    pub path_for_agent: String,
    pub stamp: String,
    pub revision: String,
    pub input_stamp: String,
}

impl readiness::Candidate {
    /// Finish interpretation against the current assistant turn. The live
    /// observation is deferred until a continuation actually needs it.
    fn into_classified_turn(
        self,
        view: &RunView,
        node_id: &str,
        classification: Option<crate::circuit::evaluator::Classification>,
        readiness: ReviewerReadiness,
        stamp: Option<String>,
        observe_turn: impl FnOnce() -> (Option<String>, bool),
    ) -> ClassifiedTurn {
        use crate::circuit::evaluator::Classification;
        let mut classification = classification.filter(|value| {
            *value != Classification::Continue
                || matches!(
                    view.graph.node(node_id).map(|node| &node.kind),
                    Some(
                        CircuitNodeKind::LlmTurnClassifier { .. }
                            | CircuitNodeKind::AwaitAgentTurn { .. }
                    )
                )
        });
        let continuation = if classification == Some(Classification::Continue) {
            let (revision, alive) = observe_turn();
            let fresh = revision.as_deref() == Some(self.turn_revision.as_str()) && alive;
            if !fresh {
                classification = Some(Classification::Working);
            }
            fresh
                .then(|| {
                    stamp.map(|stamp| {
                        (
                            stamp,
                            self.turn_revision,
                            self.binding.input_guard.input_stamp.clone(),
                        )
                    })
                })
                .flatten()
        } else {
            None
        };
        ClassifiedTurn {
            classifier_error: None,
            observation_blocker: None,
            agent_node_id: self.binding.input_guard.agent_node_id,
            classification,
            output: self.output,
            continuation,
            waiting_for_a_finished_turn: readiness.parks(),
            binding: Some(self.binding),
            missing_result: None,
        }
    }

    /// `Ok(None)` retries an unstable publication on a later observation, before
    /// interpretation or either bounded repair lifecycle can consume it.
    pub(crate) fn with_result_file(
        mut self,
        path: &std::path::Path,
        gate: &StepView,
    ) -> Result<Option<(Self, bool)>, crate::circuit::observation::CircuitObservationBlocker> {
        use crate::services::transcript_reader::report_snapshot::ReportSnapshot;
        match ReportSnapshot::read_result_file(
            path,
            &self.binding.report_revision,
            self.binding.input_guard.observed_at_ms,
            self.binding.input_guard.report_guard.as_ref(),
        ) {
            Ok(Some(report)) => {
                self.output = report.text.clone();
                self.binding.report_revision = report.revision.clone();
                self.binding.owner.report_revision = Some(report.revision.clone());
                // A native receipt can be borrowed from an upstream spawn.
                // This new result is a report handoff at the current gate;
                // the receipt itself keeps its original owner and identity.
                self.binding.owner.step_id = gate.node_id.clone();
                self.binding.owner.attempt = gate.attempt;
                self.binding.input_guard.report_guard = Some(report);
                Ok(Some((self, false)))
            }
            Ok(None) => Ok(Some((self, true))),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            // Invalid bytes are an agent output-format error, not transient I/O.
            // Reuse the bounded missing/blank-result repair lifecycle.
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => Ok(Some((self, true))),
            Err(error) => {
                tracing::warn!(
                    "circuits: could not bind result file {}: {error}",
                    path.display()
                );
                Err(
                    crate::circuit::observation::CircuitObservationBlocker::ResultFileUnavailable {
                        path: path.display().to_string(),
                        reason: error.to_string(),
                    },
                )
            }
        }
    }
}

pub(super) fn classify_step_turn(
    active: &db::ActiveCircuitRun,
    view: &RunView,
    node_id: &str,
) -> Option<ClassifiedTurn> {
    use crate::circuit::evaluator;
    let step = view.step(node_id)?;
    let agent_node_id = step
        .agent_node_id
        .or_else(|| view.resolve_target_agent(node_id))?;
    if view.state != RunState::Running
        || !matches!(step.status, StepStatus::Running | StepStatus::Unverified)
    {
        return None;
    }
    if classifier_budget_exhausted(view, node_id) {
        return None;
    }
    let probe_key = format!("report:{}:{node_id}:{}", active.run.id, step.attempt);
    // A bounded pull runs even without PTY bytes or a correct display status.
    // Polling evidence does not spend classifier budget.
    let generation = evaluator::begin_circuit_probe(agent_node_id, &probe_key, false)?;
    evaluator::note_circuit_probe(agent_node_id, &probe_key, generation);
    let mut agent = db::get_agent_node_by_id(agent_node_id).ok()?;
    if agent.cli_session_id.as_deref().is_none_or(str::is_empty) {
        match crate::services::session_recovery::recover_live_node(agent_node_id) {
            Ok(Some(_)) => agent = db::get_agent_node_by_id(agent_node_id).ok()?,
            Ok(None) => {}
            Err(error) => {
                tracing::warn!("circuits: session recovery for agent {agent_node_id}: {error}")
            }
        }
    }
    let stamp = db::agent_turn_stamp(agent_node_id).ok().flatten();
    let input = crate::agent::process::PROCESS_REGISTRY.input_stamp_result(agent_node_id);
    let snapshot = crate::coordinator::enrichment::circuit_report_snapshot(&agent);
    let lifecycle_blocker = db::agent_node::circuit_lifecycle_blocker(agent_node_id).unwrap_or(
        Some(crate::circuit::observation::CircuitObservationBlocker::LifecycleEvidenceUnavailable),
    );
    let candidate = match readiness::prepare_with_lifecycle_veto(
        view,
        node_id,
        &agent,
        stamp.as_deref(),
        input,
        snapshot,
        lifecycle_blocker,
    ) {
        Ok(Some(candidate)) => candidate,
        Ok(None) => return None,
        Err(blocker) => return Some(ClassifiedTurn::deferred(agent_node_id, blocker)),
    };
    let (candidate, missing_path) = if let Some(path) =
        crate::circuit::handoff::expected_result(active.run.id, agent_node_id)
    {
        let (candidate, missing) = match candidate.with_result_file(&path, step) {
            Ok(Some(result)) => result,
            Ok(None) => return None,
            Err(blocker) => return Some(ClassifiedTurn::deferred(agent_node_id, blocker)),
        };
        (candidate, missing.then_some(path))
    } else {
        (candidate, None)
    };
    let binding = &candidate.binding;
    let output = &candidate.output;
    let status = candidate.status;
    let superseded = || {
        stamp != db::agent_turn_stamp(agent_node_id).ok().flatten()
            || crate::agent::process::PROCESS_REGISTRY
                .input_stamp(agent_node_id)
                .as_deref()
                != Some(binding.input_guard.input_stamp.as_str())
            || binding
                .input_guard
                .report_guard
                .as_ref()
                .is_some_and(|report| !report.is_current())
    };
    // A finished turn that owes a result file is reminded, never classified from
    // its transcript. Checked before readiness and classification, so the
    // classifier is not consulted for it. An unfinished turn (still working, or
    // waiting on a person) keeps the ordinary path.
    let turn_finished = matches!(status, SessionStatus::Ready | SessionStatus::Completed);
    let missing_result = match (missing_path, stamp.as_deref()) {
        (Some(path), Some(stamp)) if turn_finished => Some(MissingResult {
            path_for_agent: crate::circuit::handoff::agent_visible_path(&path, agent.env),
            stamp: stamp.to_string(),
            revision: binding.report_revision.clone(),
            input_stamp: binding.input_guard.input_stamp.clone(),
        }),
        _ => None,
    };
    if let Some(missing_result) = missing_result {
        evaluator::note_evaluation(agent_node_id);
        if superseded() {
            return None;
        }
        return Some(ClassifiedTurn {
            classifier_error: None,
            observation_blocker: None,
            agent_node_id,
            classification: None,
            output: candidate.output,
            continuation: None,
            waiting_for_a_finished_turn: false,
            binding: Some(candidate.binding),
            missing_result: Some(missing_result),
        });
    }
    let since_evaluation_ms = evaluator::millis_since_last_evaluation(agent_node_id);
    let changed_revision = view
        .context
        .get(&format!("node.{node_id}.evaluated_report_revision"))
        .is_some_and(|previous| previous != binding.report_revision);
    if !changed_revision
        && !should_classify_report(view, node_id, status, output, since_evaluation_ms)
    {
        return None;
    }
    let classifier_error = std::cell::RefCell::new(None);
    let classify = |prompt: &str| {
        let result = (|| {
            let preferences = crate::preferences::load()?;
            let provider = classifier_provider(&preferences);
            let backend = crate::circuit::classifier::resolve(provider)
                .map_err(|error| format!("{provider}: {error}"))?;
            evaluator::classify_with_prompt(agent_node_id, &backend, prompt)
                .map_err(|error| format!("{provider}: {error}"))
        })();
        match result {
            Ok(verdict) => Some(verdict),
            Err(error) => {
                *classifier_error.borrow_mut() = Some(error);
                None
            }
        }
    };
    let readiness = reviewer_readiness(view, node_id, status, output, classify);
    evaluator::note_evaluation(agent_node_id);
    let classification = match readiness {
        ReviewerReadiness::Working | ReviewerReadiness::Unavailable => None,
        ReviewerReadiness::Reportable => {
            classify_gate_report(view, node_id, status, output, classify)
        }
    };
    if superseded() {
        return None;
    }
    let mut classified =
        candidate.into_classified_turn(view, node_id, classification, readiness, stamp, || {
            (
                crate::coordinator::enrichment::assistant_report(&agent)
                    .map(|report| report.revision),
                crate::agent::process::PROCESS_REGISTRY.is_alive(&agent_node_id),
            )
        });
    if classified.classification.is_none() {
        classified.classifier_error = classifier_error.into_inner();
    }
    tracing::info!("circuits: bound report classification for run {} step {node_id} agent {agent_node_id}: {:?}", active.run.id, classified.classification);
    Some(classified)
}

pub(super) fn awaits_review_turn(view: &RunView, node_id: &str) -> bool {
    if view.context.get("source.review_preset") != Some("1")
        && view.context.get("recovery.from_run_id").is_none()
    {
        return false;
    }
    match view.graph.node(node_id).map(|n| &n.kind) {
        // Older saved presets used a task classifier after feedback. Apply
        // the turn handoff policy without rewriting an active graph. Custom
        // task-completion gates and owned-agent gates retain their contract.
        Some(
            CircuitNodeKind::AwaitAgentTurn { target_node_id }
            | CircuitNodeKind::LlmTurnClassifier { target_node_id },
        ) => target_node_id.as_deref() == Some("$source"),
        _ => false,
    }
}

pub(super) fn review_turn_is_complete(
    view: &RunView,
    node_id: &str,
    status: SessionStatus,
) -> bool {
    awaits_review_turn(view, node_id)
        && matches!(status, SessionStatus::Ready | SessionStatus::Completed)
}

pub(super) fn classify_gate_report(
    view: &RunView,
    node_id: &str,
    status: SessionStatus,
    output: &str,
    classify: impl FnOnce(&str) -> Option<crate::circuit::evaluator::Classification>,
) -> Option<crate::circuit::evaluator::Classification> {
    use crate::circuit::evaluator;
    if output.trim().is_empty()
        || !matches!(
            status,
            SessionStatus::Ready | SessionStatus::Completed | SessionStatus::AwaitingInput
        )
    {
        return None;
    }
    if spawn_hands_off_report(view, node_id) {
        return Some(evaluator::Classification::Completed);
    }
    if let Some(result) = report_contract::interpretation(view, node_id, output) {
        return Some(result);
    }
    // Ready is a clean lifecycle turn completion, not an LLM judgement of
    // task quality. Review must work even when the classifier is unavailable.
    if review_turn_is_complete(view, node_id, status) {
        return Some(evaluator::Classification::Completed);
    }
    // A clean reviewer turn (ready/completed with a non-empty report) first
    // consults the live classifier exactly as before. Only when that backend
    // is absent or fails does the gate fall back to reading the report's
    // explicit verdict deterministically (issue #1815), so a review completes
    // on meshes with no classifier CLI instead of parking. The fallback never
    // overrides a live verdict. AwaitingInput turns (permission prompts,
    // questions) are judged by the reviewer readiness question first and keep
    // the classifier below as the tie-breaker.
    if is_reviewer_verdict_gate(view, node_id)
        && matches!(status, SessionStatus::Ready | SessionStatus::Completed)
    {
        let prompt = evaluator::review_prompt(output);
        if let Some(classification) = classify(&prompt) {
            return Some(classification);
        }
        return Some(evaluator::review_verdict_from_report(output));
    }
    let prompt = if is_reviewer_verdict_gate(view, node_id) {
        evaluator::review_prompt(output)
    } else if awaits_review_turn(view, node_id) {
        evaluator::review_turn_prompt(output)
    } else {
        evaluator::circuit_classify_prompt(output)
    };
    classify(&prompt)
}

/// True when `node_id` is the gate whose classification *is* another agent's
/// review verdict, so its report must be a finished turn.
pub(super) fn is_reviewer_verdict_gate(view: &RunView, node_id: &str) -> bool {
    matches!(
        view.graph.node(node_id).map(|node| &node.kind),
        Some(CircuitNodeKind::ReviewVerdict { .. })
    )
}

pub(super) fn spawn_hands_off_report(view: &RunView, node_id: &str) -> bool {
    matches!(
        view.graph.node(node_id).map(|node| &node.kind),
        Some(CircuitNodeKind::SpawnAgentNode { .. })
    ) && every_successor_classifies(view, node_id, true)
}

/// A report is handed off when every successor judges it. A round join (the
/// re-prompted reviewer's fan-in) is transparent, one level deep.
fn every_successor_classifies(view: &RunView, node_id: &str, through_join: bool) -> bool {
    let mut successors = view
        .graph
        .edges
        .iter()
        .filter(|edge| edge.from == node_id)
        .peekable();
    successors.peek().is_some()
        && successors.all(
            |edge| match view.graph.node(&edge.to).map(|node| &node.kind) {
                Some(
                    CircuitNodeKind::LlmTurnClassifier { .. }
                    | CircuitNodeKind::ReviewVerdict { .. },
                ) => true,
                Some(CircuitNodeKind::AnyCompleted) if through_join => {
                    every_successor_classifies(view, &edge.to, false)
                }
                _ => false,
            },
        )
}

/// What a `verdict` gate may do with a yielded report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReviewerReadiness {
    /// A finished report, or a reviewer that will not continue without a person
    /// — both are outcomes the verdict prompt can speak to.
    Reportable,
    /// The reviewer is still working, so this report is not the gate's result.
    Working,
    /// The readiness classifier could not be reached: unknown, not unfinished.
    Unavailable,
}

impl ReviewerReadiness {
    /// True when the gate publishes `TurnParked` instead of a classification.
    ///
    /// Only a *working* reviewer parks. `Unavailable` must take the ordinary
    /// classification channel even though it carries no verdict, because that is
    /// what records the outage and so admits the cooldown retry: a finished
    /// reviewer produces no further output, so parking it would leave the
    /// unchanged report unobserved until the gate's wait deadline.
    pub(super) fn parks(self) -> bool {
        matches!(self, Self::Working)
    }
}

/// Decide what a `verdict` gate may do with this yielded report.
///
/// An `AwaitingInput` yield is not evidence that the reviewer finished: an
/// intermediate progress line, a background-task hand-off, and a permission
/// prompt all reach the lifecycle identically. This gate's classification *is*
/// the reviewer's verdict, so judging a progress line here fails the run with a
/// verdict about a progress message — run 163 recorded an mcode reviewer's
/// "…Let me retry…" line as its review and ended the run `BLOCKED`. The
/// reviewer-specific readiness question separates the two
/// (`evaluator::reviewer_turn_prompt`, which deliberately does not treat a
/// failure the reviewer is retrying as "needs a person").
///
/// `Unavailable` is deliberately *not* a wait. A reviewer that has finished
/// produces no further output, so an observation-only park would never be
/// re-observed: `should_classify_report` refuses an unchanged report, so the
/// gate would stall until its wait deadline and the promised cooldown retry
/// would never fire. The caller reports an outage through the existing channel
/// instead, which records `classification = "unavailable"` and therefore admits
/// the 60-second retry (and the bounded failure budget every gate shares).
pub(super) fn reviewer_readiness(
    view: &RunView,
    node_id: &str,
    status: SessionStatus,
    output: &str,
    readiness: impl Fn(&str) -> Option<crate::circuit::evaluator::Classification>,
) -> ReviewerReadiness {
    use crate::circuit::evaluator::{reviewer_turn_prompt, Classification};
    if !(is_reviewer_verdict_gate(view, node_id) || spawn_hands_off_report(view, node_id))
        || status != SessionStatus::AwaitingInput
    {
        return ReviewerReadiness::Reportable;
    }
    if report_contract::declares_completion(view, node_id, output) {
        return ReviewerReadiness::Reportable;
    }
    match readiness(&reviewer_turn_prompt(output)) {
        // Intermediate progress, a background hand-off, and a failure the
        // reviewer is retrying are one state from here: the reviewer has not
        // finished, so there is no verdict yet.
        Some(Classification::Working | Classification::Continue) => ReviewerReadiness::Working,
        // The classifier is unreachable, so an unfinished report cannot be told
        // from a finished one. No verdict is taken from it — that is the failure
        // this path exists to prevent — but the outage is recorded so the gate
        // is retried rather than stalling.
        None => {
            tracing::warn!(
                "circuits: step {node_id} turn-readiness classifier unavailable — \
                 retrying on the classifier cooldown instead of reading the report as a verdict"
            );
            ReviewerReadiness::Unavailable
        }
        Some(_) => ReviewerReadiness::Reportable,
    }
}

pub(super) fn select_turn_report(
    transcript: Option<crate::services::transcript_reader::AssistantReport>,
    previous_revision: Option<&str>,
    has_turn_start: bool,
    current_tail: impl FnOnce() -> String,
) -> Option<String> {
    if let Some(report) = transcript.filter(|report| {
        !report.text.trim().is_empty()
            && previous_revision.is_none_or(|previous| {
                !crate::services::transcript_reader::same_assistant_revision(
                    &report.revision,
                    previous,
                )
            })
    }) {
        return Some(report.text);
    }
    // A resumed terminal can redraw the entire preceding turn. Without a
    // boundary recorded in this process, the PTY tail is unanchored even when
    // there is no durable previous revision to compare against.
    if !has_turn_start {
        return None;
    }
    Some(current_tail()).filter(|tail| !tail.trim().is_empty())
}

pub(super) fn has_unconsumed_classifier_evidence(view: &RunView, node_id: &str) -> bool {
    view.classifier_evidence(node_id).is_some_and(|evidence| {
        evidence.report.is_some()
            && evidence.identity.as_ref().is_some_and(|owner| {
                let encoded = serde_json::to_string(owner).ok();
                // Issue #2138: a refused classification records the owner it
                // observed in `evaluated_evidence_owner`. That report has been
                // judged and rejected, so treating it as unconsumed made an
                // unchanged parked report re-classify on every observation tick.
                //
                // A refused verdict is only valid for the blocker state that
                // refused it. Acceptance also reads live state — open owned
                // work, lifecycle blockers, evidence conflicts — which moves
                // neither the revision nor the owner, so that state is keyed
                // separately and re-admits the gate when it changes.
                let observed_owner_suppressed = view
                    .context
                    .get(&format!("node.{node_id}.evaluated_evidence_owner"))
                    == encoded.as_deref();
                let blocker = view
                    .report_blocker(node_id)
                    .and_then(|blocker| serde_json::to_string(&blocker).ok())
                    .unwrap_or_default();
                let blocker_matches = view
                    .context
                    .get(&format!("node.{node_id}.evaluated_evidence_blocker"))
                    .map_or(true, |recorded| recorded == blocker);
                view.context
                    .get(&format!("node.{node_id}.classified_evidence_owner"))
                    != encoded.as_deref()
                    && (!observed_owner_suppressed || !blocker_matches)
            })
    })
}

pub(super) fn should_classify_report(
    view: &RunView,
    node_id: &str,
    status: SessionStatus,
    output: &str,
    since_evaluation_ms: Option<u128>,
) -> bool {
    let Some(step) = view.step(node_id) else {
        return false;
    };
    if view.state != RunState::Running
        || !matches!(step.status, StepStatus::Running | StepStatus::Unverified)
    {
        return false;
    }
    if classifier_budget_exhausted(view, node_id) {
        return false;
    }
    // The same report may first be observed on a watchdog/permission yield,
    // then on the real completion hook. That new lifecycle fact must win
    // over a previously parked WORKING verdict, including after restart.
    if review_turn_is_complete(view, node_id, status) {
        return true;
    }
    let prefix = format!("node.{node_id}");
    let same_attempt = view
        .context
        .get(&format!("{prefix}.evaluated_attempt"))
        .and_then(|attempt| attempt.parse::<i32>().ok())
        == Some(step.attempt);
    let same_output = view.context.get(&format!("{prefix}.evaluated_output")) == Some(output);
    if same_attempt && same_output {
        // An unavailable interpretation did not consume the native owner as a
        // verdict. That is not new evidence: retry the same report on the
        // outage budget. Changed report revisions are admitted by the caller.
        if view.context.get(&format!("{prefix}.classification")) == Some("unavailable") {
            return since_evaluation_ms.is_none_or(|elapsed| elapsed >= 60_000);
        }
        if step.status == StepStatus::Unverified
            && view
                .context
                .get(&format!("{prefix}.evaluated_report_revision"))
                .is_none_or(str::is_empty)
            && view
                .context
                .get(&format!("{prefix}.classified_evidence_owner"))
                .is_none_or(str::is_empty)
        {
            return true;
        }
        if has_unconsumed_classifier_evidence(view, node_id) {
            return true;
        }
        return false;
    }
    true
}

pub(super) fn classifier_budget_exhausted(view: &RunView, node_id: &str) -> bool {
    view.step(node_id).is_some_and(|step| {
        view.context
            .get(&format!(
                "node.{node_id}.classifier_failures.{}",
                step.attempt
            ))
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0)
            >= crate::circuit::stepper::MAX_CLASSIFIER_FAILURES
    })
}

// ---------------------------------------------------------------------------
// Lost-turn watchdog (milestone 3, issue #1208).
//
// The turn webhook (`/api/attention/{id}`) is best-effort HTTP — a lost
// POST leaves a FINISHED piloted agent looking `running` forever, and
// the run wedges with it (the legacy pipeline learned this as #874).
// Quiet PTY output only makes the node eligible for a readiness check.
// Background tests can be silent for minutes; a false turn can cause a
// review gate to fail and kill the agent before autoclear can recover it.
// Publish through the normal lifecycle seam only after a finished turn or
// explicit input request, and only while the observation is still current.
// ---------------------------------------------------------------------------

/// Quiet window before checking for a missed turn, also the retry interval
/// for background or unavailable readiness evidence.
pub(crate) const LOST_TURN_QUIET_MS: u128 = 60_000;

/// Pure eligibility predicate: alive process, quiet past the window,
/// status still claiming to be mid-turn.
pub(super) fn should_check_quiet_turn(
    is_alive: bool,
    quiet_ms: Option<u128>,
    status: SessionStatus,
) -> bool {
    is_alive
        && quiet_ms.is_some_and(|q| q >= LOST_TURN_QUIET_MS)
        && status == SessionStatus::Running
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct QuietTurnEvidence {
    pub(super) lifecycle: Option<String>,
    pub(super) input: Option<String>,
    pub(super) report: Option<String>,
}

pub(super) fn quiet_turn_is_current(
    before: &QuietTurnEvidence,
    after: &QuietTurnEvidence,
    is_alive: bool,
    quiet_ms: Option<u128>,
    status: SessionStatus,
) -> bool {
    before == after && should_check_quiet_turn(is_alive, quiet_ms, status)
}

pub(super) fn native_completion_is_current(
    completion: &crate::services::transcript_reader::NativeTurnCompletion,
    lifecycle_stamp: Option<&str>,
) -> bool {
    lifecycle_stamp
        .is_some_and(|stamp| db::agent_turn_stamp_precedes(stamp, completion.completed_at_ms))
}

/// Recheck all observations at publication, including native turn evidence:
/// an unchanged assistant report does not exclude a newly submitted prompt.
pub(super) fn recover_native_turn(
    completion: Option<crate::services::transcript_reader::NativeTurnCompletion>,
    lifecycle_stamp: Option<&str>,
    still_current: impl FnOnce(&crate::services::transcript_reader::NativeTurnCompletion) -> bool,
    publish: impl FnOnce(),
) -> bool {
    let Some(completion) = completion.filter(|c| native_completion_is_current(c, lifecycle_stamp))
    else {
        return false;
    };
    if !still_current(&completion) {
        return false;
    }
    publish();
    true
}

pub(super) fn recover_quiet_turn(
    output: &str,
    classify: impl FnOnce(&str) -> Option<crate::circuit::evaluator::Classification>,
    still_current: impl FnOnce() -> bool,
    publish: impl FnOnce(),
) {
    use crate::circuit::evaluator::{quiet_turn_prompt, Classification};
    // Kept at the publication boundary: review verdicts interpret WORKING as
    // changes requested, so background progress must never reach that gate.
    if !output.trim().is_empty()
        && matches!(
            classify(&quiet_turn_prompt(output)),
            Some(Classification::Completed | Classification::Blocked)
        )
        && still_current()
    {
        publish();
    }
}

/// Fast-tick pass: recover every quiet piloted node bound to a running
/// circuit run. Readiness classification is throttled per agent and runs on
/// this dedicated worker thread, outside any DB connection.
pub(super) struct QuietClassifierFailure {
    pub(super) active: db::ActiveCircuitRun,
    pub(super) target: jobs::RecoveryTarget,
    pub(super) agent: i64,
    pub(super) step: String,
    pub(super) attempt: i32,
    pub(super) evidence: QuietTurnEvidence,
    pub(super) error: String,
}

pub(super) fn lost_turn_watchdog_pass(app: &AppHandle) -> Vec<QuietClassifierFailure> {
    let mut failures = Vec::new();
    let runs = match db::list_active_circuit_runs() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("circuits: watchdog could not list active runs: {}", e);
            return failures;
        }
    };
    for active in runs {
        // Watchdog recovers in-flight running runs even on disabled
        // circuits — a lost turn wedges a capacity slot either way.
        if active.run.state != "running" {
            continue;
        }
        let steps = match load_steps(active.run.id) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let (Ok(graph), Ok(context)) = (
            CircuitGraph::from_json(&active.circuit_graph_json),
            CircuitContext::from_json(&active.run.context_json),
        ) else {
            continue;
        };
        let view = RunView {
            run_id: active.run.id,
            graph,
            context,
            steps,
            state: RunState::Running,
        };
        let mut observed = HashSet::new();
        for step in view
            .steps
            .iter()
            .filter(|s| matches!(s.status, StepStatus::Running | StepStatus::Unverified))
        {
            let Some(agent_node_id) = observed_agent_for_step(
                step,
                &view.graph,
                &view.steps,
                view.context.source_agent_id(),
            ) else {
                continue;
            };
            if !observed.insert(agent_node_id) {
                continue;
            }
            let recovery_permit = begin_circuit_effect_batch(active.run.id);
            if recovery_permit.is_cancelled() {
                continue;
            }
            let recovery_target = jobs::RecoveryTarget::new(&view, step, agent_node_id);
            let recovery_fence = recovery_target.database_fence(&view.graph);
            let Ok(node) = db::get_agent_node_by_id(agent_node_id) else {
                continue;
            };
            use crate::circuit::evaluator;
            // Native completion is independent of PTY animation/quietness and
            // classifier availability. Probe it even when a Stop hook was lost.
            let native_key = format!("native-turn:{}", active.run.id);
            if let Some(generation) = (node.status == SessionStatus::Running
                && crate::agent::process::PROCESS_REGISTRY.is_alive(&agent_node_id))
            .then(|| evaluator::begin_circuit_wait_probe(agent_node_id, &native_key))
            .flatten()
            {
                evaluator::note_circuit_probe(agent_node_id, &native_key, generation);
                let stamp = db::agent_turn_stamp(agent_node_id).ok().flatten();
                let input = crate::agent::process::PROCESS_REGISTRY.input_stamp(agent_node_id);
                // A draft/paste is not an unchanged empty prompt. Native
                // recovery waits until it has a usable input ownership stamp.
                let (Some(stamp), Some(input)) = (stamp, input) else {
                    continue;
                };
                let snapshot = crate::coordinator::enrichment::native_turn_completion(&node);
                let completion = snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.completion.clone());
                let completed_at_ms = completion
                    .as_ref()
                    .map(|c| c.completed_at_ms)
                    .unwrap_or_default();
                if recover_native_turn(
                    completion,
                    Some(&stamp),
                    |_| {
                        if recovery_permit.is_cancelled() {
                            return false;
                        }
                        let Ok(current) = db::get_agent_node_by_id(agent_node_id) else {
                            return false;
                        };
                        current.status == SessionStatus::Running
                            && crate::agent::process::PROCESS_REGISTRY.is_alive(&agent_node_id)
                            && Some(&stamp)
                                == db::agent_turn_stamp(agent_node_id).ok().flatten().as_ref()
                            && Some(&input)
                                == crate::agent::process::PROCESS_REGISTRY
                                    .input_stamp(agent_node_id)
                                    .as_ref()
                            && current.cli_session_id == node.cli_session_id
                            && snapshot
                                .as_ref()
                                .is_some_and(|snapshot| snapshot.is_current())
                    },
                    || {
                        recovery_target.publish(&recovery_permit, &active, || {
                            crate::node_turn::recover_ready(
                                agent_node_id,
                                app,
                                crate::agent::session_lifecycle::HookSignalDetail {
                                    provider: Some(node.provider.clone()),
                                    provider_event: Some("transcript:turn_complete".into()),
                                    provider_session_id: node.cli_session_id.clone(),
                                    ..Default::default()
                                },
                                &crate::agent::session_lifecycle::CircuitTurnRecovery {
                                    stamp: &stamp,
                                    input: &input,
                                    observed_at_ms: completed_at_ms,
                                    fence: &recovery_fence,
                                    cancelled: &recovery_permit.cancelled,
                                },
                            );
                        });
                    },
                ) {
                    continue;
                }
            }
            // Native observation may recover a lifecycle, but an exhausted gate
            // must not restart inference through the quiet-turn fallback.
            if classifier_budget_exhausted(&view, &step.node_id) {
                continue;
            }
            let quiet_ms = crate::circuit::evaluator::millis_since_last_output(agent_node_id);
            if !should_check_quiet_turn(
                crate::agent::process::PROCESS_REGISTRY.is_alive(&agent_node_id),
                quiet_ms,
                node.status,
            ) {
                continue;
            }
            // Unknown/background evidence is retried at most once per minute,
            // including when a transcript changes without any PTY output.
            if evaluator::millis_since_last_evaluation(agent_node_id)
                .is_some_and(|elapsed| elapsed < LOST_TURN_QUIET_MS)
            {
                continue;
            }
            evaluator::note_evaluation(agent_node_id);
            let stamp = db::agent_turn_stamp(agent_node_id).ok().flatten();
            let input_stamp = crate::agent::process::PROCESS_REGISTRY.input_stamp(agent_node_id);
            let report = crate::coordinator::enrichment::assistant_report(&node);
            let evidence = QuietTurnEvidence {
                lifecycle: stamp,
                input: input_stamp,
                report: report.as_ref().map(|report| report.revision.clone()),
            };
            let observed_at_ms = chrono::Utc::now().timestamp_millis();
            let Some(output) = select_turn_report(
                report,
                view.context
                    .get(&format!("agent.{agent_node_id}.previous_report_revision")),
                evaluator::has_turn_start(agent_node_id),
                || evaluator::cleaned_turn_tail(agent_node_id),
            ) else {
                continue;
            };
            let classifier_error = std::cell::RefCell::new(None);
            recover_quiet_turn(
                &output,
                |prompt| {
                    if recovery_permit.is_cancelled() {
                        return None;
                    }
                    let result = (|| {
                        let preferences = crate::preferences::load()?;
                        let provider = classifier_provider(&preferences);
                        let launch = crate::circuit::classifier::resolve(provider)
                            .map_err(|error| format!("{provider}: {error}"))?;
                        evaluator::classify_with_prompt(agent_node_id, &launch, prompt)
                            .map_err(|error| format!("{provider}: {error}"))
                    })();
                    match result {
                        Ok(verdict) => Some(verdict),
                        Err(error) => {
                            *classifier_error.borrow_mut() = Some(error);
                            None
                        }
                    }
                },
                || {
                    // Classification can take 30s. A hook, user input, or resumed
                    // output during that interval invalidates the quiet observation.
                    if recovery_permit.is_cancelled() {
                        return false;
                    }
                    let Ok(current) = db::get_agent_node_by_id(agent_node_id) else {
                        return false;
                    };
                    quiet_turn_is_current(
                        &evidence,
                        &QuietTurnEvidence {
                            lifecycle: db::agent_turn_stamp(agent_node_id).ok().flatten(),
                            input: crate::agent::process::PROCESS_REGISTRY
                                .input_stamp(agent_node_id),
                            report: crate::coordinator::enrichment::assistant_report(&current)
                                .map(|report| report.revision),
                        },
                        crate::agent::process::PROCESS_REGISTRY.is_alive(&agent_node_id),
                        evaluator::millis_since_last_output(agent_node_id),
                        current.status,
                    )
                },
                || {
                    recovery_target.publish(&recovery_permit, &active, || {
                    tracing::warn!("circuits: run {} agent {} has a confirmed quiet turn; recovering missed webhook", active.run.id, agent_node_id);
                    if let (Some(stamp), Some(input)) = (&evidence.lifecycle, &evidence.input) {
                        crate::commands::attention::recover_attention(agent_node_id, app,
                            &crate::agent::session_lifecycle::CircuitTurnRecovery {
                                stamp, input, observed_at_ms, fence: &recovery_fence, cancelled: &recovery_permit.cancelled,
                            });
                    }
                });
                },
            );
            if let Some(error) = classifier_error.into_inner() {
                failures.push(QuietClassifierFailure {
                    active: active.clone(),
                    target: recovery_target,
                    agent: agent_node_id,
                    step: step.node_id.clone(),
                    attempt: step.attempt,
                    evidence,
                    error,
                });
            }
        }
    }
    failures
}

pub(super) fn classifier_provider(preferences: &crate::preferences::AppPreferences) -> &str {
    preferences
        .circuit_classifier_provider
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("claude")
}
/// Gate observation (#1207): for each Running gate step, perform the
/// impure part of the gate (LLM classification / deterministic command)
/// on bounded background jobs. Pending work is not a classifier failure;
/// completed results return through the ordinary single-writer transition.
pub(super) fn observe_gates_with(
    view: &RunView,
    events: &mut Vec<CircuitEvent>,
    source: &mut impl Observations,
) {
    for step in &view.steps {
        if !matches!(step.status, StepStatus::Running | StepStatus::Unverified) {
            continue;
        }
        match view.graph.node(&step.node_id).map(|n| &n.kind) {
            Some(
                CircuitNodeKind::AwaitAgentTurn { .. }
                | CircuitNodeKind::LlmTurnClassifier { .. }
                | CircuitNodeKind::ReviewVerdict { .. }
                | CircuitNodeKind::SpawnAgentNode { .. },
            ) => {
                let continuation_delivery = view
                    .context
                    .get(&format!("node.{}.continuation.delivery", step.node_id));
                let continuation_attempt = view
                    .context
                    .get(&format!("node.{}.continuation.attempt", step.node_id))
                    .and_then(|s| s.parse::<i32>().ok());
                if continuation_delivery == Some("claimed")
                    && continuation_attempt == Some(step.attempt)
                {
                    events.push(CircuitEvent::ContinuationUncertain {
                        node_id: step.node_id.clone(),
                        attempt: step.attempt,
                        error: "Continuation delivery was interrupted after claiming input; waiting for fresh agent progress.".into(),
                    });
                    continue;
                }
                if continuation_delivery == Some("pending")
                    && continuation_attempt == Some(step.attempt)
                    && view
                        .context
                        .get(&format!("node.{}.recheck_only", step.node_id))
                        != Some("1")
                {
                    events.push(CircuitEvent::ContinuationRetry {
                        node_id: step.node_id.clone(),
                        attempt: step.attempt,
                    });
                    continue;
                }
                if let Some(ClassifiedTurn {
                    agent_node_id,
                    classification,
                    output,
                    continuation,
                    waiting_for_a_finished_turn,
                    binding,
                    observation_blocker,
                    classifier_error,
                    missing_result,
                }) = source.classify(view, step)
                {
                    if let Some(blocker) = observation_blocker {
                        if matches!(blocker, crate::circuit::observation::CircuitObservationBlocker::ResultFileUnavailable { .. })
                            && view.result_read_failures(&step.node_id, step.attempt, agent_node_id) == Some(2)
                        {
                            let issue = view.context.get("issue.number")
                                .and_then(|number| number.parse::<i64>().ok()).unwrap_or(0);
                            source.blocked(agent_node_id, issue);
                        }
                        events.push(CircuitEvent::ObservationDeferred {
                            node_id: step.node_id.clone(),
                            attempt: step.attempt,
                            agent_node_id,
                            blocker,
                        });
                        continue;
                    }
                    if waiting_for_a_finished_turn {
                        events.push(CircuitEvent::TurnParked {
                            report_revision: binding
                                .as_ref()
                                .map(|binding| binding.report_revision.clone()),
                            node_id: step.node_id.clone(),
                            output,
                        });
                        continue;
                    }
                    if let Some(missing) = missing_result {
                        // Attention is raised once per observed turn, on the event
                        // the stepper exhausts (or cannot remind), not on every tick.
                        // The stepper unverifies the step on this same event, so the
                        // next tick is `Ignore` and this does not repeat.
                        let decision = view.result_reminder_decision(
                            &step.node_id,
                            step.attempt,
                            &missing.revision,
                            &missing.stamp,
                        );
                        if matches!(
                            decision,
                            crate::circuit::stepper::ResultReminderDecision::Exhausted
                                | crate::circuit::stepper::ResultReminderDecision::NotOwned
                        ) {
                            let issue = view
                                .context
                                .get("issue.number")
                                .and_then(|number| number.parse::<i64>().ok())
                                .unwrap_or(0);
                            source.blocked(agent_node_id, issue);
                        }
                        events.push(CircuitEvent::ResultFileMissing {
                            node_id: step.node_id.clone(),
                            attempt: step.attempt,
                            result_path: missing.path_for_agent,
                            stamp: missing.stamp,
                            revision: missing.revision,
                            input_stamp: missing.input_stamp,
                        });
                        continue;
                    }
                    if let Some((stamp, revision, input_stamp)) = continuation {
                        events.push(CircuitEvent::ContinuationObserved {
                            node_id: step.node_id.clone(),
                            attempt: step.attempt,
                            stamp,
                            revision,
                            input_stamp,
                        });
                    }
                    if matches!(
                        classification,
                        Some(crate::circuit::evaluator::Classification::Blocked)
                    ) {
                        let issue = view
                            .context
                            .get("issue.number")
                            .and_then(|number| number.parse::<i64>().ok())
                            .unwrap_or(0);
                        source.blocked(agent_node_id, issue);
                    }
                    if let Some(error) = classifier_error {
                        events.push(CircuitEvent::ClassifierErrorObserved {
                            node_id: step.node_id.clone(),
                            attempt: step.attempt,
                            error,
                        });
                    }
                    events.push(CircuitEvent::TurnClassified {
                        binding,
                        node_id: step.node_id.clone(),
                        classification,
                        output: Some(output),
                    });
                }
            }
            Some(CircuitNodeKind::DeterministicVerification { command })
                if step.status == StepStatus::Running =>
            {
                if let Some(green) = source.verify(view, step, command) {
                    events.push(CircuitEvent::VerificationResult {
                        node_id: step.node_id.clone(),
                        green,
                    });
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod result_continuation_tests {
    use super::*;
    use crate::circuit::evaluator::Classification;
    use crate::services::circuit_worker::observe_parity_tests::{spawn, view, Script};
    use crate::services::transcript_reader::report_snapshot::ReportReadError;

    #[test]
    fn circuit_result_file_continue_emits_the_assistant_revision_and_rejects_stale_turns() {
        let directory = tempfile::tempdir().unwrap();
        let result = directory.path().join("publish.result.md");
        let report = "The task still needs one change; continue implementing it.";
        std::fs::write(&result, report).unwrap();
        for kind in [
            CircuitNodeKind::AwaitAgentTurn {
                target_node_id: None,
            },
            CircuitNodeKind::LlmTurnClassifier {
                target_node_id: None,
            },
        ] {
            let mut run = view(kind, RunState::Running, StepStatus::Running, Some(42));
            run.graph.nodes.push(crate::circuit::model::CircuitNode {
                id: "producer".into(),
                kind: spawn(),
            });
            run.graph.edges.push(crate::circuit::model::CircuitEdge {
                from: "producer".into(),
                to: "step".into(),
                condition: Default::default(),
            });
            run.steps.push(StepView {
                node_id: "producer".into(),
                status: StepStatus::Completed,
                attempt: 1,
                agent_node_id: Some(42),
                outcome: None,
                error: None,
            });
            let native = crate::circuit::test_support::record_report_evidence(
                &mut run,
                "step",
                "The assistant yielded a different summary.",
            );
            let agent = crate::models::AgentNode {
                id: 42,
                status: SessionStatus::Ready,
                cli_session_id: native.owner.session_id.clone(),
                ..Default::default()
            };
            std::fs::remove_file(&result).unwrap();
            std::fs::create_dir(&result).unwrap();
            let candidate = readiness::prepare(
                &run,
                "step",
                &agent,
                Some("100:ready"),
                Ok(native.input_guard.input_stamp.clone()),
                Err(ReportReadError::Unsupported),
            )
            .unwrap()
            .unwrap();
            let Err(blocker) = candidate.with_result_file(&result, run.step("step").unwrap())
            else {
                panic!("directory result must defer");
            };
            let mut source = Script::default();
            source.turn = Some(ClassifiedTurn::deferred(42, blocker));
            let mut deferred = Vec::new();
            observe_gates_with(&run, &mut deferred, &mut source);
            assert_eq!(deferred.len(), 1);
            assert!(advance(&mut run, &deferred[0]).effects.is_empty());
            assert_eq!(run.step("step").unwrap().status, StepStatus::Unverified);
            std::fs::remove_dir(&result).unwrap();
            std::fs::write(&result, report).unwrap();
            let prepare = || {
                readiness::prepare(
                    &run,
                    "step",
                    &agent,
                    Some("100:ready"),
                    Ok(native.input_guard.input_stamp.clone()),
                    Err(ReportReadError::Unsupported),
                )
                .unwrap()
                .unwrap()
                .with_result_file(&result, run.step("step").unwrap())
                .unwrap()
                .unwrap()
            };
            let (bound, missing) = prepare();
            assert!(!missing);
            assert_ne!(bound.binding.report_revision, native.report_revision);
            for (revision, alive, expected) in [
                (
                    Some(native.report_revision.clone()),
                    true,
                    Classification::Continue,
                ),
                (
                    Some("newer-assistant-report".into()),
                    true,
                    Classification::Working,
                ),
                (None, true, Classification::Working),
                (
                    Some(native.report_revision.clone()),
                    false,
                    Classification::Working,
                ),
                (
                    Some(bound.binding.report_revision.clone()),
                    true,
                    Classification::Working,
                ),
            ] {
                let (candidate, missing) = prepare();
                assert!(!missing);
                let classification = classify_gate_report(
                    &run,
                    "step",
                    candidate.status,
                    &candidate.output,
                    |prompt| {
                        assert!(prompt.contains(report));
                        Some(Classification::Continue)
                    },
                );
                let turn = candidate.into_classified_turn(
                    &run,
                    "step",
                    classification,
                    ReviewerReadiness::Reportable,
                    Some("100:ready".into()),
                    || (revision, alive),
                );
                let mut source = Script::default();
                source.turn = Some(turn);
                let mut events = Vec::new();
                observe_gates_with(&run, &mut events, &mut source);
                if expected == Classification::Continue {
                    assert_eq!(events.len(), 2);
                    let mut continued = run.clone();
                    let effects: Vec<_> = events
                        .iter()
                        .flat_map(|event| advance(&mut continued, event).effects)
                        .collect();
                    let can_prompt = matches!(
                        run.graph.node("step").unwrap().kind,
                        CircuitNodeKind::LlmTurnClassifier { .. }
                    );
                    assert_eq!(
                        effects.iter().any(|effect| matches!(
                            effect,
                            crate::circuit::stepper::Effect::ContinueAgentTurn {
                                target_agent_id: 42,
                                ..
                            }
                        )),
                        can_prompt,
                        "only classifier gates automatically prompt owned agents"
                    );
                    assert!(continuation_is_current(
                        &continued,
                        "step",
                        42,
                        SessionStatus::Ready,
                        true,
                        Some("100:ready"),
                        Some(&native.report_revision)
                    ));
                    assert!(!continuation_is_current(
                        &continued,
                        "step",
                        42,
                        SessionStatus::Ready,
                        true,
                        Some("100:ready"),
                        Some(&bound.binding.report_revision)
                    ));
                    assert!(matches!(&events[0], CircuitEvent::ContinuationObserved {
                        node_id, attempt: 1, stamp, revision, input_stamp,
                    } if node_id == "step" && stamp == "100:ready"
                        && revision == &native.report_revision
                        && input_stamp == &native.input_guard.input_stamp));
                } else {
                    assert_eq!(events.len(), 1, "stale or dead turns cannot continue");
                }
                assert!(
                    matches!(events.last().unwrap(), CircuitEvent::TurnClassified {
                    node_id, classification: Some(value), output: Some(output), binding: Some(binding),
                } if node_id == "step" && *value == expected && output == report
                    && binding.report_revision == bound.binding.report_revision)
                );
            }
        }
    }
}

#[cfg(test)]
mod classifier_selection_tests {
    use super::classifier_provider;

    #[test]
    fn circuit_classifier_is_independent_of_codex_spawn_and_naming_defaults() {
        let mut preferences = crate::preferences::AppPreferences {
            default_provider: Some("codex".into()),
            naming_provider: Some("claude:minimax".into()),
            ..Default::default()
        };
        assert_eq!(classifier_provider(&preferences), "claude");
        let directory = tempfile::tempdir().unwrap();
        crate::preferences::storage::init_for_tests(directory.path().to_path_buf());
        crate::preferences::save(preferences.clone()).unwrap();
        let backend = crate::session_naming::naming_backend_env(classifier_provider(&preferences));
        assert!(
            backend.is_ok(),
            "Codex spawn defaults must not disable the Circuit classifier"
        );

        preferences.circuit_classifier_provider = Some("claude:minimax".into());
        assert_eq!(classifier_provider(&preferences), "claude:minimax");
        preferences.default_provider = Some("opencode".into());
        assert_eq!(classifier_provider(&preferences), "claude:minimax");
    }
}
