//! Gate, claim, and dispatch for one committed effect batch.
//!
//! The Tauri handle stays behind [`EffectHost`]. Production wraps the live
//! app; tests pass a fake host and assert the durable journal together with
//! the external call. Commit-before-effect stays in the worker pass: this
//! module only runs effects the transition has already committed.

#[cfg(test)]
use std::cell::{Cell, RefCell};

use tauri::{AppHandle, Emitter};

use crate::circuit::delivery::PreparedPrompt;
use crate::circuit::stepper::{CircuitEvent, Effect, EffectJournal, RunState, RunView};
use crate::db;
use crate::models::{AgentNode, SessionStatus};

use super::{
    background_wake_is_current, begin_circuit_effect_batch, close_blocker,
    close_target_spawn_step_id, continuation_is_current, dispatch_prompt_and_acknowledge,
    notification_severity, BackgroundWakeEvidence, CircuitNotificationPayload,
    CircuitPrReadyPayload,
};

/// What one effect batch did, including whether cancellation was visible
/// before the batch permit dropped. The permit's `Drop` removes a finished
/// marker, so the flag has to be read while the permit is still alive.
pub(super) struct DispatchedEffects {
    pub outcomes: Vec<CircuitEvent>,
    /// Captured while the batch permit is still alive. Recovery tests read it;
    /// the production pass only uses [`Self::outcomes`]. The field must be
    /// written in the production pass too — it is read while the permit is
    /// alive — so the lint is allowed here rather than the write removed
    /// (issue #2450).
    #[allow(dead_code)]
    pub batch_cancelled: bool,
}

/// External work and the observations a prompt effect re-checks before it
/// writes. Spawn, PTY delivery, and UI events go through here so a test can
/// supply them without a Tauri handle or a live process.
pub(super) trait EffectHost {
    fn emit_notification(&self, run_id: i64, message: String, severity: String);
    fn emit_pr_ready(&self, run_id: i64, node_id: i64);
    fn mark_attention(&self, node_id: i64);
    fn spawn_step_agent(
        &self,
        run_id: i64,
        mesh_id: i64,
        view: &mut RunView,
        node_id: &str,
    ) -> Result<(), String>;
    fn input_stamp(&self, agent_id: i64) -> Option<String>;
    fn write_prompt(
        &self,
        prepared: &PreparedPrompt,
        expected_input: Option<&str>,
    ) -> Result<bool, String>;
    fn agent_alive(&self, agent_id: i64) -> bool;
    fn millis_since_last_output(&self, agent_id: i64) -> Option<u128>;
    fn stalled_background_task(&self, node: &AgentNode) -> Option<String>;
    fn assistant_revision(&self, node: &AgentNode) -> Option<String>;
}

pub(super) struct AppEffectHost<'a> {
    app: &'a AppHandle,
}

impl EffectHost for AppEffectHost<'_> {
    fn emit_notification(&self, run_id: i64, message: String, severity: String) {
        let _ = self.app.emit(
            "circuit-notification",
            CircuitNotificationPayload {
                run_id,
                message,
                severity,
            },
        );
    }

    fn emit_pr_ready(&self, run_id: i64, node_id: i64) {
        let _ = self.app.emit(
            "circuit-pr-ready",
            CircuitPrReadyPayload { run_id, node_id },
        );
    }

    fn mark_attention(&self, node_id: i64) {
        crate::commands::attention::mark_attention(node_id, self.app);
    }

    fn spawn_step_agent(
        &self,
        run_id: i64,
        mesh_id: i64,
        view: &mut RunView,
        node_id: &str,
    ) -> Result<(), String> {
        super::spawn::spawn_step_agent(self.app, run_id, mesh_id, view, node_id)
    }

    fn input_stamp(&self, agent_id: i64) -> Option<String> {
        crate::agent::process::PROCESS_REGISTRY.input_stamp(agent_id)
    }

    fn write_prompt(
        &self,
        prepared: &PreparedPrompt,
        expected_input: Option<&str>,
    ) -> Result<bool, String> {
        crate::circuit::delivery::write_prompt_to_pty_guarded(
            &crate::agent::process::PROCESS_REGISTRY,
            prepared,
            self.app,
            expected_input,
        )
    }

    fn agent_alive(&self, agent_id: i64) -> bool {
        crate::agent::process::PROCESS_REGISTRY.is_alive(&agent_id)
    }

    fn millis_since_last_output(&self, agent_id: i64) -> Option<u128> {
        crate::circuit::evaluator::millis_since_last_output(agent_id)
    }

    fn stalled_background_task(&self, node: &AgentNode) -> Option<String> {
        super::observation::stalled_background_task(node)
    }

    fn assistant_revision(&self, node: &AgentNode) -> Option<String> {
        crate::coordinator::enrichment::assistant_report(node).map(|report| report.revision)
    }
}

/// Recorded external calls. A missing write override means the prompt was
/// accepted (`Ok(true)`); `Ok(false)` is a real ownership miss and must not
/// be the default.
#[cfg(test)]
#[derive(Default)]
pub(super) struct FakeHost {
    pub spawns: RefCell<Vec<(i64, i64, String)>>,
    pub prompts: RefCell<Vec<RecordedPrompt>>,
    pub notifications: RefCell<Vec<(i64, String, String)>>,
    pub pr_ready: RefCell<Vec<(i64, i64)>>,
    pub attention: RefCell<Vec<i64>>,
    pub spawn_error: RefCell<Option<String>>,
    pub write_override: RefCell<Option<Result<bool, String>>>,
    pub alive: Cell<bool>,
    pub input_stamp_value: RefCell<Option<String>>,
    pub stalled_task: RefCell<Option<String>>,
    pub report_revision: RefCell<Option<String>>,
    pub quiet_ms: Cell<Option<u128>>,
    /// Invoked at the start of each external call so a test can read the
    /// journal at the moment dispatch begins.
    pub at_external: RefCell<Option<Box<dyn FnMut()>>>,
}

#[cfg(test)]
pub(super) struct RecordedPrompt {
    pub agent_id: i64,
    pub text: String,
    pub expected_input: Option<String>,
}

#[cfg(test)]
impl FakeHost {
    fn note_external(&self) {
        if let Some(hook) = self.at_external.borrow_mut().as_mut() {
            hook();
        }
    }
}

#[cfg(test)]
impl EffectHost for FakeHost {
    fn emit_notification(&self, run_id: i64, message: String, severity: String) {
        self.note_external();
        self.notifications
            .borrow_mut()
            .push((run_id, message, severity));
    }

    fn emit_pr_ready(&self, run_id: i64, node_id: i64) {
        self.note_external();
        self.pr_ready.borrow_mut().push((run_id, node_id));
    }

    fn mark_attention(&self, node_id: i64) {
        self.note_external();
        self.attention.borrow_mut().push(node_id);
    }

    fn spawn_step_agent(
        &self,
        run_id: i64,
        mesh_id: i64,
        _view: &mut RunView,
        node_id: &str,
    ) -> Result<(), String> {
        self.note_external();
        self.spawns
            .borrow_mut()
            .push((run_id, mesh_id, node_id.to_string()));
        match self.spawn_error.borrow().clone() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn input_stamp(&self, _agent_id: i64) -> Option<String> {
        self.input_stamp_value.borrow().clone()
    }

    fn write_prompt(
        &self,
        prepared: &PreparedPrompt,
        expected_input: Option<&str>,
    ) -> Result<bool, String> {
        self.note_external();
        self.prompts.borrow_mut().push(RecordedPrompt {
            agent_id: prepared.node_id(),
            text: prepared.text().to_string(),
            expected_input: expected_input.map(str::to_string),
        });
        self.write_override.borrow().clone().unwrap_or(Ok(true))
    }

    fn agent_alive(&self, _agent_id: i64) -> bool {
        self.alive.get()
    }

    fn millis_since_last_output(&self, _agent_id: i64) -> Option<u128> {
        self.quiet_ms.get()
    }

    fn stalled_background_task(&self, _node: &AgentNode) -> Option<String> {
        self.stalled_task.borrow().clone()
    }

    fn assistant_revision(&self, _node: &AgentNode) -> Option<String> {
        self.report_revision.borrow().clone()
    }
}

/// Claim one journalled effect and record the evidence revision on the view.
/// `Ok(None)` means the row was not `intent` on a running step, so the caller
/// skips dispatch. The attempt is the step's current attempt.
fn claim_journal_effect(
    view: &mut RunView,
    run_id: i64,
    node_id: &str,
    kind: EffectJournal,
) -> Result<Option<i32>, String> {
    let attempt = view.step(node_id).map_or(1, |step| step.attempt);
    let intent = db::circuit::evidence::EffectIntent {
        node_id: node_id.to_string(),
        attempt,
        kind: kind.into(),
    };
    let Some(revision) =
        db::circuit::evidence::claim_effect(run_id, &intent).map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    view.context.set("evidence.revision", revision.to_string());
    Ok(Some(attempt))
}

/// Dispatch one `CallGithub` effect. The recheck lookup and the idempotent
/// create path use only the DB and the GitHub client. Production passes
/// `None` and the live client is built inside the call, after the mesh origin
/// resolves.
pub(super) fn execute_call_github_effect(
    active: &db::ActiveCircuitRun,
    view: &mut RunView,
    node_id: &str,
    action: crate::circuit::model::GithubActionKind,
    label: Option<&str>,
    comment: Option<&str>,
    github_client: Option<&crate::services::github::GitHubClient>,
) -> Result<Vec<CircuitEvent>, String> {
    use crate::circuit::model::GithubActionKind;
    let attempt = view.step(node_id).map_or(1, |step| step.attempt);
    if view.context.get(&format!("node.{node_id}.recheck_only")) == Some("1") {
        if action == GithubActionKind::OpenPr {
            let event = match github_client {
                Some(client) => super::github::reconcile_open_pr_effect_for_worker_with_client(
                    active, view, node_id, client,
                ),
                None => super::github::reconcile_open_pr_effect_for_worker(active, view, node_id),
            };
            return Ok(vec![event]);
        }
        view.context
            .set(&format!("node.{node_id}.recheck_only"), "0");
        return Ok(vec![CircuitEvent::EffectUncertain {
            node_id: node_id.to_string(),
            attempt,
            reason: "Read-only external-action recheck is unavailable for this GitHub action."
                .into(),
        }]);
    }
    let Some(_) = claim_journal_effect(view, view.run_id, node_id, EffectJournal::Github)? else {
        return Ok(Vec::new());
    };
    Ok(vec![super::github::call_github_effect(
        active, view, node_id, action, label, comment,
    )
    .unwrap_or_else(|reason| CircuitEvent::EffectUncertain {
        node_id: node_id.to_string(),
        attempt,
        reason,
    })])
}

/// Run an already-committed effect batch. The cancellation flag is sampled
/// before the batch permit drops, including on the early returns that stop
/// the rest of the batch.
pub(super) fn dispatch_effects(
    host: &impl EffectHost,
    active: &db::ActiveCircuitRun,
    view: &mut RunView,
    effects: &[Effect],
    github_client: Option<&crate::services::github::GitHubClient>,
) -> Result<DispatchedEffects, String> {
    let mut outcome_events = Vec::new();
    let effect_batch = begin_circuit_effect_batch(active.run.id);
    let run_state = db::get_circuit_run(active.run.id)
        .map_err(|error| error.to_string())?
        .map(|run| run.state);
    let finish = |outcomes: Vec<CircuitEvent>| DispatchedEffects {
        outcomes,
        batch_cancelled: effect_batch.is_cancelled(),
    };
    for effect in effects {
        if effect_batch.is_cancelled() {
            tracing::info!(
                "circuits: cancellation invalidated remaining effects for run {}",
                active.run.id
            );
            return Ok(finish(outcome_events));
        }
        let completing = matches!(view.state, RunState::Completed | RunState::Failed);
        let accepts_effect = run_state
            .as_deref()
            .is_some_and(|state| effect_allowed_in_state(state, completing, effect));
        if !accepts_effect {
            tracing::info!(
                "circuits: stopped effects for terminal/deleted run {}",
                active.run.id
            );
            return Ok(finish(outcome_events));
        }
        match effect {
            Effect::SpawnAgentNode { node_id } => {
                let kind = effect.journal_kind().expect("spawn claims a journal row");
                let Some(attempt) = claim_journal_effect(view, active.run.id, node_id, kind)?
                else {
                    continue;
                };
                if let Err(error) =
                    host.spawn_step_agent(active.run.id, active.run.mesh_id, view, node_id)
                {
                    outcome_events.push(CircuitEvent::EffectUncertain {
                        node_id: node_id.clone(),
                        attempt,
                        reason: format!("Agent dispatch is unverified: {error}. Inspect retained agents before recording an outcome."),
                    });
                }
            }
            Effect::InjectPty {
                node_id, prompt, ..
            } => {
                let requests_result =
                    super::report_contract::requests_result(view, node_id, prompt);
                let prompt = super::report_contract::prompt(view, node_id, prompt);
                let attempt = view.step(node_id).map_or(1, |step| step.attempt);
                match view.resolve_target_agent(node_id) {
                    Some(target) => {
                        // Mirrors `observe`'s agent-existence check: treat
                        // both "row deleted" AND "row archived" as lost.
                        // An archived row can't accept a PTY write, and
                        // returning Err here makes `drive_run` persist
                        // `status: "failed"` directly via
                        // `commit_circuit_advance` — no AgentLost event,
                        // no stepper cascade. That's intentional: the
                        // stepper only emits AgentLost via observation,
                        // and a missing target here means the row is
                        // already gone, so a direct write is honest.
                        let Some(target_node) = db::get_agent_node_by_id(target)
                            .ok()
                            .filter(|node| node.status != SessionStatus::Archived)
                        else {
                            let reason = format!(
                                "target agent {} for step {} was lost before prompt injection",
                                target, node_id
                            );
                            tracing::warn!("circuits: run {}: {}", active.run.id, reason);
                            return Err(reason);
                        };
                        if effect_batch.is_cancelled()
                            || db::get_circuit_run(active.run.id)
                                .ok()
                                .flatten()
                                .is_none_or(|run| run.state != "running")
                        {
                            return Ok(finish(outcome_events));
                        }
                        let kind = effect
                            .journal_kind()
                            .expect("prompt injection claims a journal row");
                        let Some(_) = claim_journal_effect(view, active.run.id, node_id, kind)?
                        else {
                            continue;
                        };
                        // The full prompt goes to its handoff file; the terminal
                        // receives either the same text or a pointer to it. The
                        // digest and the PTY write both use what is delivered.
                        let staged = crate::circuit::handoff::stage_prompt(
                            active.run.id,
                            node_id,
                            attempt,
                            target_node.env,
                            &prompt,
                            requests_result,
                        );
                        let prepared = PreparedPrompt::prepare(target, &staged.delivered)?;
                        crate::circuit::handoff::set_agent_turn(
                            active.run.id,
                            target,
                            staged.turn.as_ref(),
                        );
                        // Record the submission before the PTY write. Claude
                        // echoes the prompt back within milliseconds of Enter,
                        // and that echo only proves *this* submission if the
                        // record already exists when the hook is handled
                        // (issue #1898).
                        match db::circuit::evidence::record_prompt_submission(
                            active.run.id,
                            node_id,
                            attempt,
                            &prepared,
                        ) {
                            Ok(revision) => {
                                view.context.set("evidence.revision", revision.to_string())
                            }
                            Err(error) => {
                                tracing::warn!(
                                    "circuits: run {}: could not record prompt submission: {}",
                                    active.run.id,
                                    error
                                );
                                outcome_events.push(CircuitEvent::EffectUncertain {
                                    node_id: node_id.clone(),
                                    attempt,
                                    reason: format!(
                                        "Prompt submission could not be recorded for correlation: {error}"
                                    ),
                                });
                                continue;
                            }
                        }
                        crate::circuit::evaluator::note_turn_start(target);
                        let delivered = dispatch_prompt_and_acknowledge(
                            || {
                                let input = host.input_stamp(target).ok_or(
                                    "Terminal input is already owned; prompt was not sent",
                                )?;
                                host.write_prompt(&prepared, Some(&input))
                            },
                            || {
                                db::circuit::evidence::acknowledge_prompt_delivery(
                                    active.run.id,
                                    node_id,
                                    attempt,
                                )
                                .map_err(|error| {
                                    format!(
                                        "Prompt was submitted but acknowledgement could not be recorded: {error}"
                                    )
                                })
                            },
                        );
                        match delivered {
                            Ok(Some(revision)) => {
                                view.context.set("evidence.revision", revision.to_string())
                            }
                            Ok(None) => continue,
                            Err(error) => {
                                outcome_events.push(CircuitEvent::EffectUncertain {
                                    node_id: node_id.clone(),
                                    attempt,
                                    reason: format!("Prompt delivery is unverified: {error}"),
                                });
                                continue;
                            }
                        }
                        let _ = db::update_agent_node_status(target, SessionStatus::Running);
                        outcome_events.push(CircuitEvent::PromptDelivered {
                            node_id: node_id.clone(),
                            attempt,
                        });
                        tracing::info!(
                            "circuits: injected prompt into agent {} for run {}",
                            target,
                            active.run.id
                        );
                    }
                    None => {
                        return Err(format!(
                            "circuits: run {} had no piloted agent in lineage to inject into (node {})",
                            active.run.id, node_id
                        ));
                    }
                }
            }
            Effect::ContinueAgentTurn {
                node_id,
                target_agent_id,
                prompt,
            } => {
                let key = format!("node.{node_id}.continuation.delivery");
                if view.context.get(&key) != Some("claimed") {
                    continue;
                }
                let node = db::get_agent_node_by_id(*target_agent_id)
                    .map_err(|error| error.to_string())?;
                let stamp =
                    db::agent_turn_stamp(*target_agent_id).map_err(|error| error.to_string())?;
                let revision = host.assistant_revision(&node);
                let valid = continuation_is_current(
                    view,
                    node_id,
                    *target_agent_id,
                    node.status,
                    host.agent_alive(*target_agent_id),
                    stamp.as_deref(),
                    revision.as_deref(),
                );
                if !valid {
                    outcome_events.push(CircuitEvent::ContinuationObsolete {
                        node_id: node_id.clone(),
                        attempt: view
                            .step(node_id)
                            .map(|step| step.attempt)
                            .unwrap_or_default(),
                    });
                    continue;
                }
                if effect_batch.is_cancelled()
                    || db::get_circuit_run(active.run.id)
                        .ok()
                        .flatten()
                        .is_none_or(|run| run.state != "running")
                {
                    return Ok(finish(outcome_events));
                }
                let expected = view
                    .context
                    .get(&format!("node.{node_id}.continuation.input"))
                    .ok_or_else(|| "Continuation lacks an input ownership stamp".to_string())?;
                // The agent may owe a result file from an earlier turn. Archive it so the
                // continuation cannot be judged from that stale report.
                let prompt = crate::circuit::handoff::prepare_continuation(
                    active.run.id,
                    *target_agent_id,
                    node.env,
                    prompt,
                );
                let prepared = PreparedPrompt::prepare(*target_agent_id, &prompt)?;
                match host.write_prompt(&prepared, Some(expected)) {
                    Ok(true) => {
                        let _ =
                            db::update_agent_node_status(*target_agent_id, SessionStatus::Running);
                        outcome_events.push(CircuitEvent::ContinuationDelivered {
                            node_id: node_id.clone(),
                            attempt: view
                                .step(node_id)
                                .map(|step| step.attempt)
                                .unwrap_or_default(),
                        });
                    }
                    Ok(false) => outcome_events.push(CircuitEvent::ContinuationObsolete {
                        node_id: node_id.clone(),
                        attempt: view
                            .step(node_id)
                            .map(|step| step.attempt)
                            .unwrap_or_default(),
                    }),
                    Err(error) => {
                        host.mark_attention(*target_agent_id);
                        outcome_events.push(CircuitEvent::ContinuationUncertain {
                            node_id: node_id.clone(),
                            attempt: view
                                .step(node_id)
                                .map(|step| step.attempt)
                                .unwrap_or_default(),
                            error,
                        });
                    }
                }
            }
            Effect::NudgeIdleAgent {
                node_id,
                target_agent_id,
                prompt,
            } => {
                let key = format!("node.{node_id}.nudge.delivery");
                if view.context.get(&key) != Some("claimed") {
                    continue;
                }
                let node = db::get_agent_node_by_id(*target_agent_id)
                    .map_err(|error| error.to_string())?;
                // Re-read native completion before the identity/status fences:
                // an agent may have resumed since observation claimed the wake.
                let task_id = host.stalled_background_task(&node);
                let node = db::get_agent_node_by_id(*target_agent_id)
                    .map_err(|error| error.to_string())?;
                let stamp =
                    db::agent_turn_stamp(*target_agent_id).map_err(|error| error.to_string())?;
                let revision = host.assistant_revision(&node);
                let attempt = view
                    .step(node_id)
                    .map(|step| step.attempt)
                    .unwrap_or_default();
                // The wake-up is only worth sending to the turn that was
                // observed as stalled. Reuse the continuation fence, bound to
                // this nudge's own claim keys: a turn that has since moved on
                // needs nothing.
                let valid = background_wake_is_current(
                    view,
                    node_id,
                    *target_agent_id,
                    BackgroundWakeEvidence {
                        status: node.status,
                        alive: host.agent_alive(*target_agent_id),
                        stamp: stamp.as_deref(),
                        revision: revision.as_deref(),
                        quiet_ms: host.millis_since_last_output(*target_agent_id),
                        task_id: task_id.as_deref(),
                        lifecycle_allows_wake: matches!(
                            db::agent_node::circuit_lifecycle_blocker(*target_agent_id),
                            Ok(None
                                | Some(
                                    crate::circuit::observation::CircuitObservationBlocker::KnownWorkOutstanding
                                ))
                        ),
                    },
                );
                if !valid {
                    outcome_events.push(CircuitEvent::NudgeObsolete {
                        node_id: node_id.clone(),
                        attempt,
                    });
                    continue;
                }
                if effect_batch.is_cancelled()
                    || db::get_circuit_run(active.run.id)
                        .ok()
                        .flatten()
                        .is_none_or(|run| run.state != "running")
                {
                    return Ok(finish(outcome_events));
                }
                let expected = view
                    .context
                    .get(&format!("node.{node_id}.nudge.input"))
                    .ok_or_else(|| "Wake-up lacks an input ownership stamp".to_string())?;
                let prepared = PreparedPrompt::prepare(*target_agent_id, prompt)?;
                // The nudge is a real prompt submission, so it is recorded the
                // same way an injected one is: the receipt it may produce has to
                // bind to a submission Buildmesh itself made.
                if let Err(error) = db::circuit::evidence::record_prompt_submission(
                    active.run.id,
                    node_id,
                    attempt,
                    &prepared,
                ) {
                    tracing::warn!(
                        "circuits: run {}: could not record wake-up submission: {}",
                        active.run.id,
                        error
                    );
                    outcome_events.push(CircuitEvent::NudgeUncertain {
                        node_id: node_id.clone(),
                        attempt,
                        error: format!(
                            "Wake-up submission could not be recorded for correlation: {error}"
                        ),
                    });
                    continue;
                }
                match host.write_prompt(&prepared, Some(expected)) {
                    Ok(true) => {
                        let _ =
                            db::update_agent_node_status(*target_agent_id, SessionStatus::Running);
                        outcome_events.push(CircuitEvent::NudgeDelivered {
                            node_id: node_id.clone(),
                            attempt,
                        });
                    }
                    Ok(false) => outcome_events.push(CircuitEvent::NudgeObsolete {
                        node_id: node_id.clone(),
                        attempt,
                    }),
                    Err(error) => {
                        host.mark_attention(*target_agent_id);
                        outcome_events.push(CircuitEvent::NudgeUncertain {
                            node_id: node_id.clone(),
                            attempt,
                            error,
                        });
                    }
                }
            }
            Effect::SetNodeStatus { .. } => {
                // persist_transition commits this local SQLite mutation with
                // the completed step, closing the crash gap before dispatch.
            }
            Effect::CloseAgentNode {
                node_id,
                target_node_id,
            } => {
                let target = view.resolve_target_agent(node_id).ok_or_else(|| {
                    format!(
                        "CloseAgentNode target agent not found in lineage for node {}",
                        node_id
                    )
                })?;
                // Closing is intentionally idempotent.  A previous worker
                // pass may have killed/deleted the row after committing the
                // close step but before the effect was retried.
                match db::get_agent_node_by_id(target) {
                    Ok(_) => {
                        // An agent no step identifies as a helper is treated as
                        // the user's work, the safer reading. A failed DB read
                        // here is the safer reading too: leave the agent open
                        // and let the next sweep retry.
                        let is_helper = match db::circuit::agent_is_circuit_helper(target) {
                            Ok(value) => value,
                            Err(error) => {
                                tracing::warn!(
                                    "circuits: run {} could not read helper status for agent {}: {}; leaving the node open",
                                    active.run.id,
                                    target,
                                    error
                                );
                                false
                            }
                        };
                        // Helpers are always closed without inspecting their
                        // worktree. For the implementation agent, a failed
                        // worktree inspection is reported as having changes so
                        // the node stays open until a person looks at it.
                        let mut safety = crate::git::worktree::WorktreeCloseSafety {
                            worktree_path: None,
                            has_uncommitted: false,
                            has_unpushed: false,
                            is_detached: false,
                        };
                        if !is_helper {
                            match crate::services::agent_node::get_worktree_close_safety(target) {
                                Ok(inspected) => safety = inspected,
                                Err(error) => {
                                    tracing::warn!(
                                        "circuits: run {} could not inspect the worktree of agent {}: {}; leaving the node open",
                                        active.run.id,
                                        target,
                                        error
                                    );
                                    safety.has_uncommitted = true;
                                }
                            }
                        }
                        if let Some(reason) = close_blocker(is_helper, &safety) {
                            tracing::warn!(
                                "circuits: run {} left agent {} open instead of closing it: {}",
                                active.run.id,
                                target,
                                reason
                            );
                            host.emit_notification(
                                active.run.id,
                                format!(
                                    "The implementation agent was left open instead of being closed: {reason}."
                                ),
                                "warning".into(),
                            );
                        } else {
                            crate::services::agent_node::delete(target, true)
                                .map_err(|error| format!("agent node close failed: {}", error))?;
                        }
                    }
                    Err(rusqlite::Error::QueryReturnedNoRows) => {}
                    Err(error) => {
                        return Err(format!("agent node lookup failed during close: {}", error));
                    }
                }
                let spawn_step_id =
                    close_target_spawn_step_id(view, node_id, target_node_id.as_deref(), target);
                db::clear_circuit_step_agent_node(active.run.id, &spawn_step_id).map_err(
                    |error| format!("circuit close association cleanup failed: {}", error),
                )?;
                if let Some(step) = view.step_mut(&spawn_step_id) {
                    step.agent_node_id = None;
                }
            }
            Effect::Notify { message } => {
                host.emit_notification(
                    active.run.id,
                    message.clone(),
                    notification_severity(message),
                );
            }
            Effect::CallGithub {
                node_id,
                action,
                label,
                comment,
            } => {
                let outcomes = execute_call_github_effect(
                    active,
                    view,
                    node_id,
                    *action,
                    label.as_deref(),
                    comment.as_deref(),
                    github_client,
                )?;
                let pr_is_available = *action == crate::circuit::model::GithubActionKind::OpenPr
                    && outcomes.iter().any(|event| {
                        matches!(
                            event,
                            CircuitEvent::GithubActionResult {
                                success: true,
                                pr_number: Some(_),
                                ..
                            }
                        )
                    });
                if pr_is_available {
                    if let Some(agent_node_id) = view.resolve_target_agent(node_id) {
                        host.emit_pr_ready(active.run.id, agent_node_id);
                    }
                }
                outcome_events.extend(outcomes);
            }
        }
    }
    Ok(finish(outcome_events))
}

pub(super) fn execute_effects(
    app: &AppHandle,
    active: &db::ActiveCircuitRun,
    view: &mut RunView,
    effects: &[Effect],
) -> Result<Vec<CircuitEvent>, String> {
    Ok(dispatch_effects(&AppEffectHost { app }, active, view, effects, None)?.outcomes)
}

/// Cancellation commits the terminal run state before retiring external
/// resources. The transition's effects take one durable-state snapshot before
/// execution and check an in-memory cancellation token before each effect;
/// terminal transitions retain only their synchronous cleanup and notification
/// effects, while InjectPty is never allowed after completion.
pub(super) fn effect_allowed_in_state(
    state: &str,
    completing_transition: bool,
    effect: &Effect,
) -> bool {
    RunState::is_live_db_str(state)
        // Synchronous terminal actions are emitted by the same transition
        // that completes the run, and must survive its commit-before-effects.
        // InjectPty is intentionally absent: it starts new work after the
        // run has durably finished and would leave an untracked command.
        // A DB state of "cancelled" blocks everything even when the
        // in-memory transition is completing: cancellation wins over a
        // concurrently-computed completion, whose Notify would otherwise
        // fire about a run the user just cancelled. Cancellation cleanup
        // runs through the lease/retire path, not stepper effects.
        || ((state == RunState::Completed.as_db_str() || state == RunState::Failed.as_db_str())
            && completing_transition
            && effect.allowed_while_completing())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circuit::context::CircuitContext;
    use crate::circuit::model::{
        CircuitEdge, CircuitGraph, CircuitNode, CircuitNodeKind, GithubActionKind,
        CIRCUIT_GRAPH_VERSION,
    };
    use crate::circuit::stepper::StepStatus;
    use crate::db::circuit::evidence::{EffectIntent, EffectKind, EvidenceWrite};
    use crate::db::CircuitStepOp;
    use crate::models::{EnvType, SessionStatus};
    use rusqlite::OptionalExtension;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(1);

    fn seq() -> u64 {
        SEQ.fetch_add(1, Ordering::Relaxed)
    }

    struct Scratch {
        db: crate::db::test_support::IsolatedDbGuard,
        dir: tempfile::TempDir,
        mesh_id: i64,
        path: String,
    }

    struct World {
        _db: crate::db::test_support::IsolatedDbGuard,
        _dir: tempfile::TempDir,
        active: db::ActiveCircuitRun,
        view: RunView,
        run_id: i64,
    }

    impl Scratch {
        fn new() -> Self {
            let db = crate::db::test_support::isolated();
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().to_str().unwrap().to_string();
            let mesh = db::create_mesh(
                &format!("effect-dispatch-{}-{}", std::process::id(), seq()),
                &path,
            )
            .unwrap();
            Self {
                db,
                dir,
                mesh_id: mesh.id,
                path,
            }
        }

        fn agent(&self) -> crate::models::AgentNode {
            db::create_agent_node(db::NewAgentNode {
                mesh_id: self.mesh_id,
                name: &format!("dispatch-agent-{}-{}", std::process::id(), seq()),
                path: &self.path,
                branch: "work",
                env: EnvType::Windows,
                provider: "grok",
                worktree_name: None,
                source_issue: None,
                source_pr: None,
                source_pr_pinned_sha: None,
                use_worktree: false,
                head_repo_owner: None,
                head_repo_clone_url: None,
                worktree_path: None,
                spawn_configuration: None,
            })
            .unwrap()
        }

        fn start(
            self,
            graph: CircuitGraph,
            steps: Vec<CircuitStepOp>,
            intents: &[EffectIntent],
            mut context: CircuitContext,
            db_state_after: Option<&str>,
        ) -> World {
            let json = graph.to_json().unwrap();
            let circuit = db::create_autopilot_circuit(
                self.mesh_id,
                &format!("dispatch-{}-{}", std::process::id(), seq()),
                "",
                &json,
            )
            .unwrap();
            let run_id = db::create_circuit_run(
                circuit.id,
                self.mesh_id,
                &format!("manual:dispatch-{}-{}", std::process::id(), seq()),
                "{}",
            )
            .unwrap();
            db::set_circuit_run_state(run_id, "running").unwrap();
            context.with_run(run_id);
            db::circuit::evidence::commit_transition(
                run_id,
                Some("running"),
                &context.to_json().unwrap(),
                &steps,
                EvidenceWrite {
                    intents,
                    ..Default::default()
                },
            )
            .unwrap();
            let active = db::list_active_circuit_runs()
                .unwrap()
                .into_iter()
                .find(|active| active.run.id == run_id)
                .expect("running run is active");
            if let Some(state) = db_state_after {
                db::set_circuit_run_state(run_id, state).unwrap();
            }
            let view = RunView {
                run_id,
                graph,
                state: RunState::Running,
                context,
                steps: steps
                    .into_iter()
                    .map(|step| crate::circuit::stepper::StepView {
                        node_id: step.node_id,
                        attempt: step.attempt,
                        status: StepStatus::Running,
                        outcome: None,
                        error: None,
                        agent_node_id: step.agent_node_id,
                    })
                    .collect(),
            };
            World {
                _db: self.db,
                _dir: self.dir,
                active,
                view,
                run_id,
            }
        }
    }

    fn spawn_kind() -> CircuitNodeKind {
        CircuitNodeKind::SpawnAgentNode {
            prompt: "work".into(),
            name: None,
            provider: None,
            model: None,
            effort: None,
            extra_args: None,
            timeout_seconds: None,
        }
    }

    fn one_node(id: &str, kind: CircuitNodeKind) -> CircuitGraph {
        CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![CircuitNode {
                id: id.into(),
                kind,
            }],
            edges: vec![],
        }
    }

    fn running_step(node_id: &str, agent_node_id: Option<i64>) -> CircuitStepOp {
        CircuitStepOp {
            node_id: node_id.into(),
            status: "running".into(),
            outcome: None,
            error: None,
            agent_node_id,
            attempt: 1,
            fresh_attempt: false,
        }
    }

    fn intent(node_id: &str, kind: EffectKind) -> EffectIntent {
        EffectIntent {
            node_id: node_id.into(),
            attempt: 1,
            kind,
        }
    }

    fn dispatch(world: &mut World, host: &FakeHost, effects: &[Effect]) -> DispatchedEffects {
        dispatch_effects(host, &world.active, &mut world.view, effects, None)
            .expect("dispatch runs")
    }

    fn effect_state(run_id: i64, node_id: &str, kind: &str) -> Option<String> {
        db::read_conn()
            .query_row(
                "SELECT state FROM circuit_effects WHERE run_id=?1 AND node_id=?2 AND kind=?3",
                rusqlite::params![run_id, node_id, kind],
                |row| row.get(0),
            )
            .optional()
            .unwrap()
    }

    fn effect_rows(run_id: i64) -> i64 {
        db::read_conn()
            .query_row(
                "SELECT COUNT(*) FROM circuit_effects WHERE run_id=?1",
                [run_id],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn history_kinds(run_id: i64) -> Vec<String> {
        let conn = db::read_conn();
        let mut stmt = conn
            .prepare("SELECT kind FROM circuit_run_history WHERE run_id=?1 ORDER BY id")
            .unwrap();
        stmt.query_map(rusqlite::params![run_id], |row| row.get(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect()
    }

    fn step_agent(run_id: i64, node_id: &str) -> Option<i64> {
        db::read_conn()
            .query_row(
                "SELECT agent_node_id FROM autopilot_circuit_run_steps WHERE run_id=?1 AND node_id=?2",
                rusqlite::params![run_id, node_id],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn pin_ready_turn(agent_id: i64) -> String {
        db::update_agent_node_status(agent_id, SessionStatus::Ready).unwrap();
        db::write_conn()
            .execute(
                "UPDATE agent_nodes SET session_started_at=42 WHERE id=?1",
                [agent_id],
            )
            .unwrap();
        db::agent_turn_stamp(agent_id)
            .unwrap()
            .expect("session start and status stamp")
    }

    fn probe_state(
        host: &FakeHost,
        run_id: i64,
        node_id: &'static str,
        kind: &'static str,
    ) -> Rc<RefCell<String>> {
        let seen = Rc::new(RefCell::new(String::new()));
        let probe = Rc::clone(&seen);
        *host.at_external.borrow_mut() = Some(Box::new(move || {
            *probe.borrow_mut() = effect_state(run_id, node_id, kind).unwrap_or_default();
        }));
        seen
    }

    #[test]
    fn spawn_effect_claims_then_dispatches_once_and_a_closed_gate_does_neither() {
        let closed = Scratch::new().start(
            one_node("spawn", spawn_kind()),
            vec![running_step("spawn", None)],
            &[intent("spawn", EffectKind::Spawn)],
            CircuitContext::new(),
            Some("cancelled"),
        );
        let mut closed = closed;
        let closed_host = FakeHost::default();
        let blocked = dispatch(
            &mut closed,
            &closed_host,
            &[Effect::SpawnAgentNode {
                node_id: "spawn".into(),
            }],
        );
        assert!(blocked.outcomes.is_empty());
        assert!(closed_host.spawns.borrow().is_empty());
        assert_eq!(
            effect_state(closed.run_id, "spawn", "spawn").as_deref(),
            Some("intent")
        );

        let mut open = Scratch::new().start(
            one_node("spawn", spawn_kind()),
            vec![running_step("spawn", None)],
            &[intent("spawn", EffectKind::Spawn)],
            CircuitContext::new(),
            None,
        );
        let host = FakeHost::default();
        *host.spawn_error.borrow_mut() = Some("boom".into());
        let seen = probe_state(&host, open.run_id, "spawn", "spawn");
        let effect = Effect::SpawnAgentNode {
            node_id: "spawn".into(),
        };
        let pass = dispatch(&mut open, &host, std::slice::from_ref(&effect));
        assert_eq!(seen.borrow().as_str(), "possible_dispatch");
        assert_eq!(host.spawns.borrow().len(), 1);
        assert!(matches!(
            &pass.outcomes[..],
            [CircuitEvent::EffectUncertain { node_id, attempt: 1, reason }]
                if node_id == "spawn" && reason.contains("boom") && reason.contains("unverified")
        ));
        assert_eq!(
            effect_state(open.run_id, "spawn", "spawn").as_deref(),
            Some("possible_dispatch")
        );
        let again = dispatch(&mut open, &host, &[effect]);
        assert!(again.outcomes.is_empty());
        assert_eq!(host.spawns.borrow().len(), 1);
    }

    #[test]
    fn inject_pty_effect_claims_then_writes_the_prompt() {
        let graph = one_node(
            "inject",
            CircuitNodeKind::InjectPty {
                prompt: "follow up".into(),
                target_node_id: Some("$source".into()),
            },
        );
        let closed_scratch = Scratch::new();
        let closed_agent = closed_scratch.agent();
        let mut closed_context = CircuitContext::new();
        closed_context.set("source.agent_id", closed_agent.id.to_string());
        let mut closed = closed_scratch.start(
            graph.clone(),
            vec![running_step("inject", None)],
            &[intent("inject", EffectKind::Prompt)],
            closed_context,
            Some("cancelled"),
        );
        let closed_host = FakeHost::default();
        *closed_host.input_stamp_value.borrow_mut() = Some("stamp-1".into());
        let blocked = dispatch(
            &mut closed,
            &closed_host,
            &[Effect::InjectPty {
                node_id: "inject".into(),
                prompt: "follow up".into(),
                target_node_id: Some("$source".into()),
            }],
        );
        assert!(blocked.outcomes.is_empty());
        assert!(closed_host.prompts.borrow().is_empty());
        assert_eq!(
            effect_state(closed.run_id, "inject", "prompt").as_deref(),
            Some("intent")
        );
        assert_ne!(
            db::get_agent_node_by_id(closed_agent.id).unwrap().status,
            SessionStatus::Running
        );

        let scratch = Scratch::new();
        let agent = scratch.agent();
        db::update_agent_node_status(agent.id, SessionStatus::Ready).unwrap();
        let mut context = CircuitContext::new();
        context.set("source.agent_id", agent.id.to_string());
        let mut open = scratch.start(
            graph,
            vec![running_step("inject", None)],
            &[intent("inject", EffectKind::Prompt)],
            context,
            None,
        );
        let host = FakeHost::default();
        *host.input_stamp_value.borrow_mut() = Some("stamp-1".into());
        let seen = probe_state(&host, open.run_id, "inject", "prompt");
        let pass = dispatch(
            &mut open,
            &host,
            &[Effect::InjectPty {
                node_id: "inject".into(),
                prompt: "follow up".into(),
                target_node_id: Some("$source".into()),
            }],
        );
        assert_eq!(seen.borrow().as_str(), "possible_dispatch");
        assert_eq!(host.prompts.borrow().len(), 1);
        assert_eq!(host.prompts.borrow()[0].agent_id, agent.id);
        assert!(host.prompts.borrow()[0].text.contains("follow up"));
        assert_eq!(
            host.prompts.borrow()[0].expected_input.as_deref(),
            Some("stamp-1")
        );
        assert!(matches!(
            &pass.outcomes[..],
            [CircuitEvent::PromptDelivered { node_id, attempt: 1 }] if node_id == "inject"
        ));
        assert_eq!(
            effect_state(open.run_id, "inject", "prompt").as_deref(),
            Some("acknowledged")
        );
        assert_eq!(
            db::get_agent_node_by_id(agent.id).unwrap().status,
            SessionStatus::Running
        );
        let kinds = history_kinds(open.run_id);
        let claimed = kinds
            .iter()
            .position(|kind| kind == "effect_possible_dispatch")
            .unwrap();
        let submitted = kinds
            .iter()
            .position(|kind| kind == "prompt_submitted")
            .unwrap();
        let acked = kinds
            .iter()
            .position(|kind| kind == "effect_result")
            .unwrap();
        assert!(claimed < submitted && submitted < acked);
    }

    fn claimed_delivery(agent_id: i64, prefix: &str, task_id: Option<&str>) -> CircuitContext {
        let stamp = pin_ready_turn(agent_id);
        let mut context = CircuitContext::new();
        context.set(&format!("node.spawn.{prefix}.delivery"), "claimed");
        context.set(&format!("node.spawn.{prefix}.stamp"), stamp);
        context.set(&format!("node.spawn.{prefix}.revision"), "rev-1");
        context.set(&format!("node.spawn.{prefix}.input"), "owned-input");
        if let Some(task_id) = task_id {
            context.set("node.spawn.nudge.task_id", task_id);
        }
        context
    }

    fn ready_host() -> FakeHost {
        let host = FakeHost::default();
        host.alive.set(true);
        *host.report_revision.borrow_mut() = Some("rev-1".into());
        host
    }

    #[test]
    fn continue_effect_requires_a_claimed_delivery_before_the_write() {
        let graph = one_node("spawn", spawn_kind());
        let unclaimed_scratch = Scratch::new();
        let unclaimed_agent = unclaimed_scratch.agent();
        pin_ready_turn(unclaimed_agent.id);
        let mut unclaimed = unclaimed_scratch.start(
            graph.clone(),
            vec![running_step("spawn", Some(unclaimed_agent.id))],
            &[],
            CircuitContext::new(),
            None,
        );
        let unclaimed_host = ready_host();
        let skipped = dispatch(
            &mut unclaimed,
            &unclaimed_host,
            &[Effect::ContinueAgentTurn {
                node_id: "spawn".into(),
                target_agent_id: unclaimed_agent.id,
                prompt: "go on".into(),
            }],
        );
        assert!(skipped.outcomes.is_empty());
        assert!(unclaimed_host.prompts.borrow().is_empty());
        assert_eq!(
            db::get_agent_node_by_id(unclaimed_agent.id).unwrap().status,
            SessionStatus::Ready
        );

        let cancelled_scratch = Scratch::new();
        let cancelled_agent = cancelled_scratch.agent();
        let mut cancelled = cancelled_scratch.start(
            graph.clone(),
            vec![running_step("spawn", Some(cancelled_agent.id))],
            &[],
            claimed_delivery(cancelled_agent.id, "continuation", None),
            Some("cancelled"),
        );
        let cancelled_host = ready_host();
        let blocked = dispatch(
            &mut cancelled,
            &cancelled_host,
            &[Effect::ContinueAgentTurn {
                node_id: "spawn".into(),
                target_agent_id: cancelled_agent.id,
                prompt: "go on".into(),
            }],
        );
        assert!(blocked.outcomes.is_empty());
        assert!(cancelled_host.prompts.borrow().is_empty());
        assert_eq!(
            db::get_agent_node_by_id(cancelled_agent.id).unwrap().status,
            SessionStatus::Ready
        );

        let scratch = Scratch::new();
        let agent = scratch.agent();
        let mut open = scratch.start(
            graph,
            vec![running_step("spawn", Some(agent.id))],
            &[],
            claimed_delivery(agent.id, "continuation", None),
            None,
        );
        let host = ready_host();
        let pass = dispatch(
            &mut open,
            &host,
            &[Effect::ContinueAgentTurn {
                node_id: "spawn".into(),
                target_agent_id: agent.id,
                prompt: "go on".into(),
            }],
        );
        assert!(matches!(
            &pass.outcomes[..],
            [CircuitEvent::ContinuationDelivered { node_id, attempt: 1 }] if node_id == "spawn"
        ));
        assert_eq!(host.prompts.borrow().len(), 1);
        assert_eq!(
            host.prompts.borrow()[0].expected_input.as_deref(),
            Some("owned-input")
        );
        assert!(host.prompts.borrow()[0].text.contains("go on"));
        assert_eq!(
            db::get_agent_node_by_id(agent.id).unwrap().status,
            SessionStatus::Running
        );
        assert_eq!(effect_rows(open.run_id), 0);
    }

    #[test]
    fn nudge_effect_requires_a_claimed_delivery_before_the_write() {
        let graph = one_node("spawn", spawn_kind());
        let unclaimed_scratch = Scratch::new();
        let unclaimed_agent = unclaimed_scratch.agent();
        pin_ready_turn(unclaimed_agent.id);
        let mut unclaimed = unclaimed_scratch.start(
            graph.clone(),
            vec![running_step("spawn", Some(unclaimed_agent.id))],
            &[],
            CircuitContext::new(),
            None,
        );
        let unclaimed_host = ready_host();
        *unclaimed_host.stalled_task.borrow_mut() = Some("task-7".into());
        let skipped = dispatch(
            &mut unclaimed,
            &unclaimed_host,
            &[Effect::NudgeIdleAgent {
                node_id: "spawn".into(),
                target_agent_id: unclaimed_agent.id,
                prompt: "wake".into(),
            }],
        );
        assert!(skipped.outcomes.is_empty());
        assert!(unclaimed_host.prompts.borrow().is_empty());

        let cancelled_scratch = Scratch::new();
        let cancelled_agent = cancelled_scratch.agent();
        let mut cancelled = cancelled_scratch.start(
            graph.clone(),
            vec![running_step("spawn", Some(cancelled_agent.id))],
            &[],
            claimed_delivery(cancelled_agent.id, "nudge", Some("task-7")),
            Some("cancelled"),
        );
        let cancelled_host = ready_host();
        *cancelled_host.stalled_task.borrow_mut() = Some("task-7".into());
        let blocked = dispatch(
            &mut cancelled,
            &cancelled_host,
            &[Effect::NudgeIdleAgent {
                node_id: "spawn".into(),
                target_agent_id: cancelled_agent.id,
                prompt: "wake".into(),
            }],
        );
        assert!(blocked.outcomes.is_empty());
        assert!(cancelled_host.prompts.borrow().is_empty());
        assert_eq!(
            db::get_agent_node_by_id(cancelled_agent.id).unwrap().status,
            SessionStatus::Ready
        );

        let scratch = Scratch::new();
        let agent = scratch.agent();
        let mut open = scratch.start(
            graph,
            vec![running_step("spawn", Some(agent.id))],
            &[],
            claimed_delivery(agent.id, "nudge", Some("task-7")),
            None,
        );
        let host = ready_host();
        *host.stalled_task.borrow_mut() = Some("task-7".into());
        let pass = dispatch(
            &mut open,
            &host,
            &[Effect::NudgeIdleAgent {
                node_id: "spawn".into(),
                target_agent_id: agent.id,
                prompt: "wake".into(),
            }],
        );
        assert!(matches!(
            &pass.outcomes[..],
            [CircuitEvent::NudgeDelivered { node_id, attempt: 1 }] if node_id == "spawn"
        ));
        assert_eq!(host.prompts.borrow().len(), 1);
        assert_eq!(
            host.prompts.borrow()[0].expected_input.as_deref(),
            Some("owned-input")
        );
        assert!(history_kinds(open.run_id)
            .iter()
            .any(|kind| kind == "prompt_submitted"));
        assert_eq!(effect_rows(open.run_id), 0);
        assert_eq!(
            db::get_agent_node_by_id(agent.id).unwrap().status,
            SessionStatus::Running
        );
    }

    #[test]
    fn set_node_status_effect_does_not_dispatch_and_the_gate_stops_the_batch() {
        let effects = [
            Effect::SetNodeStatus {
                node_id: "status".into(),
                status: "completed".into(),
                target_node_id: None,
            },
            Effect::Notify {
                message: "shipped".into(),
            },
        ];
        let graph = one_node(
            "status",
            CircuitNodeKind::SetNodeStatus {
                status: crate::circuit::model::SessionStatusKind::Completed,
                target_node_id: None,
            },
        );
        let closed_scratch = Scratch::new();
        let closed_agent = closed_scratch.agent();
        db::update_agent_node_status(closed_agent.id, SessionStatus::Ready).unwrap();
        let mut closed = closed_scratch.start(
            graph.clone(),
            vec![running_step("status", None)],
            &[],
            CircuitContext::new(),
            Some("cancelled"),
        );
        let closed_host = FakeHost::default();
        let blocked = dispatch(&mut closed, &closed_host, &effects);
        assert!(blocked.outcomes.is_empty());
        assert!(closed_host.notifications.borrow().is_empty());
        assert_eq!(effect_rows(closed.run_id), 0);
        assert_eq!(
            db::get_agent_node_by_id(closed_agent.id).unwrap().status,
            SessionStatus::Ready
        );

        let scratch = Scratch::new();
        let agent = scratch.agent();
        db::update_agent_node_status(agent.id, SessionStatus::Ready).unwrap();
        let mut open = scratch.start(
            graph,
            vec![running_step("status", None)],
            &[],
            CircuitContext::new(),
            Some("completed"),
        );
        open.view.state = RunState::Completed;
        let host = FakeHost::default();
        let pass = dispatch(&mut open, &host, &effects);
        assert!(pass.outcomes.is_empty());
        assert_eq!(host.notifications.borrow().len(), 1);
        assert_eq!(host.notifications.borrow()[0].1, "shipped");
        assert_eq!(host.notifications.borrow()[0].2, "info");
        assert_eq!(effect_rows(open.run_id), 0);
        assert_eq!(
            db::get_agent_node_by_id(agent.id).unwrap().status,
            SessionStatus::Ready
        );
    }

    #[test]
    fn close_agent_effect_deletes_the_agent_only_while_the_gate_is_open() {
        let graph = CircuitGraph {
            version: CIRCUIT_GRAPH_VERSION,
            blueprint: None,
            nodes: vec![
                CircuitNode {
                    id: "spawn".into(),
                    kind: spawn_kind(),
                },
                CircuitNode {
                    id: "close".into(),
                    kind: CircuitNodeKind::CloseAgentNode {
                        target_node_id: Some("spawn".into()),
                    },
                },
            ],
            edges: vec![CircuitEdge {
                from: "spawn".into(),
                to: "close".into(),
                condition: Default::default(),
            }],
        };
        let effect = Effect::CloseAgentNode {
            node_id: "close".into(),
            target_node_id: Some("spawn".into()),
        };
        let closed_scratch = Scratch::new();
        let closed_agent = closed_scratch.agent();
        let mut closed = closed_scratch.start(
            graph.clone(),
            vec![
                running_step("spawn", Some(closed_agent.id)),
                running_step("close", None),
            ],
            &[],
            CircuitContext::new(),
            Some("cancelled"),
        );
        let closed_host = FakeHost::default();
        let blocked = dispatch(&mut closed, &closed_host, std::slice::from_ref(&effect));
        assert!(blocked.outcomes.is_empty());
        assert!(closed_host.notifications.borrow().is_empty());
        assert!(db::get_agent_node_by_id(closed_agent.id).is_ok());
        assert_eq!(step_agent(closed.run_id, "spawn"), Some(closed_agent.id));

        let scratch = Scratch::new();
        let agent = scratch.agent();
        let mut open = scratch.start(
            graph,
            vec![
                running_step("spawn", Some(agent.id)),
                running_step("close", None),
            ],
            &[],
            CircuitContext::new(),
            None,
        );
        let host = FakeHost::default();
        let pass = dispatch(&mut open, &host, &[effect]);
        assert!(pass.outcomes.is_empty());
        assert!(
            host.notifications.borrow().is_empty(),
            "a clean close does not warn: {:?}",
            host.notifications.borrow()
        );
        assert!(db::get_agent_node_by_id(agent.id).is_err());
        assert_eq!(step_agent(open.run_id, "spawn"), None);
        assert!(open.view.step("spawn").unwrap().agent_node_id.is_none());
    }

    #[test]
    fn notify_effect_emits_only_while_the_gate_is_open() {
        let graph = one_node(
            "note",
            CircuitNodeKind::Notify {
                message: "shipped".into(),
            },
        );
        let effect = Effect::Notify {
            message: "shipped".into(),
        };
        let mut closed = Scratch::new().start(
            graph.clone(),
            vec![running_step("note", None)],
            &[],
            CircuitContext::new(),
            Some("cancelled"),
        );
        let closed_host = FakeHost::default();
        let blocked = dispatch(&mut closed, &closed_host, std::slice::from_ref(&effect));
        assert!(blocked.outcomes.is_empty());
        assert!(closed_host.notifications.borrow().is_empty());

        let mut open = Scratch::new().start(
            graph,
            vec![running_step("note", None)],
            &[],
            CircuitContext::new(),
            None,
        );
        let host = FakeHost::default();
        let pass = dispatch(&mut open, &host, &[effect]);
        assert!(pass.outcomes.is_empty());
        assert_eq!(
            host.notifications.borrow().as_slice(),
            &[(open.run_id, "shipped".into(), "info".into(),)]
        );
        assert_eq!(effect_rows(open.run_id), 0);
    }

    #[test]
    fn github_effect_claims_then_records_an_uncertain_dispatch() {
        let graph = one_node(
            "label",
            CircuitNodeKind::GithubAction {
                action: GithubActionKind::AddLabel,
                open_pr_policy: None,
                label: Some("triaged".into()),
                comment: None,
            },
        );
        let effect = Effect::CallGithub {
            node_id: "label".into(),
            action: GithubActionKind::AddLabel,
            label: Some("triaged".into()),
            comment: None,
        };
        let mut closed = Scratch::new().start(
            graph.clone(),
            vec![running_step("label", None)],
            &[intent("label", EffectKind::Github)],
            CircuitContext::new(),
            Some("cancelled"),
        );
        let closed_host = FakeHost::default();
        let blocked = dispatch(&mut closed, &closed_host, std::slice::from_ref(&effect));
        assert!(blocked.outcomes.is_empty());
        assert!(closed_host.pr_ready.borrow().is_empty());
        assert_eq!(
            effect_state(closed.run_id, "label", "github").as_deref(),
            Some("intent")
        );

        let mut open = Scratch::new().start(
            graph,
            vec![running_step("label", None)],
            &[intent("label", EffectKind::Github)],
            CircuitContext::new(),
            None,
        );
        let host = FakeHost::default();
        let pass = dispatch(&mut open, &host, &[effect]);
        assert!(host.pr_ready.borrow().is_empty());
        assert_eq!(
            effect_state(open.run_id, "label", "github").as_deref(),
            Some("possible_dispatch")
        );
        let [CircuitEvent::EffectUncertain {
            node_id,
            attempt: 1,
            reason,
        }] = &pass.outcomes[..]
        else {
            panic!("expected one uncertain outcome, got {:?}", pass.outcomes);
        };
        assert_eq!(node_id, "label");
        assert!(
            reason.contains("git error")
                || reason.contains("no `origin` remote")
                || reason.contains("isn't a GitHub URL"),
            "missing origin fails before a GitHub client is built: {reason}"
        );
    }
}
