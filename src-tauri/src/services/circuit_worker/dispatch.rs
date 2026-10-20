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
///
/// A busy database also lands on `Ok(None)`: the claim is atomic, so nothing
/// was claimed and the effect stays dispatchable — the next tick retries it.
/// Failing the run there would terminalise healthy work over a transient
/// pool shortage (issue #1533).
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
    let revision = match db::circuit::evidence::claim_effect(run_id, &intent) {
        Ok(Some(revision)) => revision,
        Ok(None) => return Ok(None),
        Err(error) if crate::db::error::is_busy_error(&error) => {
            tracing::warn!(
                "circuits: run {} could not claim the effect for {}, deferring: {}",
                run_id,
                node_id,
                error
            );
            return Ok(None);
        }
        Err(error) => return Err(error.to_string()),
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

/// Release the claim the stepper gave an effect, so a later pass can
/// dispatch it again.
///
/// The stepper persists the claim (`continuation.delivery` /
/// `nudge.delivery`) *before* the effect reaches the dispatcher. A batch that
/// is abandoned without an outcome leaves that claim at `claimed`, and the
/// observation ignores a claimed delivery — so the effect would be stranded
/// instead of retried, and the observation would eventually report it as
/// uncertain, a false verdict about work that was never attempted (issue
/// #1533).
fn defer_effect(view: &RunView, effect: &Effect, outcomes: &mut Vec<CircuitEvent>) {
    let event = match effect {
        Effect::ContinueAgentTurn { node_id, .. } => CircuitEvent::ContinuationDeferred {
            node_id: node_id.clone(),
            attempt: view
                .step(node_id)
                .map(|step| step.attempt)
                .unwrap_or_default(),
        },
        Effect::NudgeIdleAgent { node_id, .. } => CircuitEvent::NudgeDeferred {
            node_id: node_id.clone(),
            attempt: view
                .step(node_id)
                .map(|step| step.attempt)
                .unwrap_or_default(),
        },
        _ => return,
    };
    outcomes.push(event);
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
    let finish = |outcomes: Vec<CircuitEvent>| DispatchedEffects {
        outcomes,
        batch_cancelled: effect_batch.is_cancelled(),
    };
    let run_state = match db::get_circuit_run(active.run.id) {
        Ok(run) => run.map(|run| run.state),
        Err(error) => {
            // A failed read is *unknown*, not "the run is gone" (issue
            // #1533). The batch cannot be dispatched — but the stepper has
            // already claimed the effects it holds, and a claim left at
            // `claimed` is one the observation never settles: the effect
            // would be stranded rather than retried. So the claims are
            // released here, and the next pass dispatches the batch.
            tracing::warn!(
                "circuits: run {} state read failed, deferring the batch: {}",
                active.run.id,
                error
            );
            for effect in effects {
                defer_effect(view, effect, &mut outcome_events);
            }
            return Ok(finish(outcome_events));
        }
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
                        //
                        // A read that *failed* is not a missing row: it is
                        // unknown (issue #1533). Failing the run there would
                        // terminalise healthy work over a transient pool
                        // shortage. Skip the effect instead — it stays
                        // unclaimed, so the next tick dispatches it.
                        let target_node = match db::get_agent_node_by_id(target) {
                            Ok(node) if node.status != SessionStatus::Archived => node,
                            Ok(_) | Err(rusqlite::Error::QueryReturnedNoRows) => {
                                let reason = format!(
                                    "target agent {} for step {} was lost before prompt injection",
                                    target, node_id
                                );
                                tracing::warn!("circuits: run {}: {}", active.run.id, reason);
                                return Err(reason);
                            }
                            Err(error) => {
                                tracing::warn!(
                                    "circuits: run {}: target agent {} for step {} could not be read, deferring the effect: {}",
                                    active.run.id,
                                    target,
                                    node_id,
                                    error
                                );
                                continue;
                            }
                        };
                        if effect_batch.is_cancelled() {
                            return Ok(finish(outcome_events));
                        }
                        // The run must still be running for the write below.
                        // A read that failed is unknown, not "the run is not
                        // running" (issue #1533): skip the effect rather than
                        // inject a prompt into a run that may have been
                        // cancelled while the database was unavailable.
                        match db::get_circuit_run(active.run.id) {
                            Ok(Some(run)) if run.state == "running" => {}
                            Ok(_) => return Ok(finish(outcome_events)),
                            Err(error) => {
                                tracing::warn!(
                                    "circuits: run {} state read failed before prompt injection, deferring the effect: {}",
                                    active.run.id,
                                    error
                                );
                                continue;
                            }
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
                // A read that failed is unknown, not "the agent is gone"
                // (issue #1533): the claim is released so the continuation is
                // dispatched on a later pass, instead of the run failing over
                // a transient pool shortage.
                let node = match db::get_agent_node_by_id(*target_agent_id) {
                    Ok(node) => node,
                    Err(rusqlite::Error::QueryReturnedNoRows) => {
                        let reason = format!(
                            "target agent {} for step {} was lost before prompt injection",
                            target_agent_id, node_id
                        );
                        tracing::warn!("circuits: run {}: {}", active.run.id, reason);
                        return Err(reason);
                    }
                    Err(error) => {
                        tracing::warn!(
                            "circuits: run {}: target agent {} for step {} could not be read, deferring the continuation: {}",
                            active.run.id,
                            target_agent_id,
                            node_id,
                            error
                        );
                        outcome_events.push(CircuitEvent::ContinuationDeferred {
                            node_id: node_id.clone(),
                            attempt: view
                                .step(node_id)
                                .map(|step| step.attempt)
                                .unwrap_or_default(),
                        });
                        continue;
                    }
                };
                let stamp = match db::agent_turn_stamp(*target_agent_id) {
                    Ok(stamp) => stamp,
                    Err(error) => {
                        tracing::warn!(
                            "circuits: run {}: turn stamp of agent {} for step {} could not be read, deferring the continuation: {}",
                            active.run.id,
                            target_agent_id,
                            node_id,
                            error
                        );
                        outcome_events.push(CircuitEvent::ContinuationDeferred {
                            node_id: node_id.clone(),
                            attempt: view
                                .step(node_id)
                                .map(|step| step.attempt)
                                .unwrap_or_default(),
                        });
                        continue;
                    }
                };
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
                if effect_batch.is_cancelled() {
                    return Ok(finish(outcome_events));
                }
                // The run must still be running for the write below. A read
                // that failed is unknown, not "the run is not running" (issue
                // #1533): the claim is released so the continuation is
                // dispatched on a later pass.
                match db::get_circuit_run(active.run.id) {
                    Ok(Some(run)) if run.state == "running" => {}
                    Ok(_) => return Ok(finish(outcome_events)),
                    Err(error) => {
                        tracing::warn!(
                            "circuits: run {} state read failed before continuation injection, deferring the continuation: {}",
                            active.run.id,
                            error
                        );
                        outcome_events.push(CircuitEvent::ContinuationDeferred {
                            node_id: node_id.clone(),
                            attempt: view
                                .step(node_id)
                                .map(|step| step.attempt)
                                .unwrap_or_default(),
                        });
                        continue;
                    }
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
                let attempt = view
                    .step(node_id)
                    .map(|step| step.attempt)
                    .unwrap_or_default();
                // A read that failed is unknown, not "the agent is gone"
                // (issue #1533): the claim is released so the wake-up is
                // decided again on a later pass, instead of the run failing
                // over a transient pool shortage.
                let node = match db::get_agent_node_by_id(*target_agent_id) {
                    Ok(node) => node,
                    Err(rusqlite::Error::QueryReturnedNoRows) => {
                        let reason = format!(
                            "target agent {} for step {} was lost before wake-up injection",
                            target_agent_id, node_id
                        );
                        tracing::warn!("circuits: run {}: {}", active.run.id, reason);
                        return Err(reason);
                    }
                    Err(error) => {
                        tracing::warn!(
                            "circuits: run {}: target agent {} for step {} could not be read, deferring the wake-up: {}",
                            active.run.id,
                            target_agent_id,
                            node_id,
                            error
                        );
                        outcome_events.push(CircuitEvent::NudgeDeferred {
                            node_id: node_id.clone(),
                            attempt,
                        });
                        continue;
                    }
                };
                // Re-read native completion before the identity/status fences:
                // an agent may have resumed since observation claimed the wake.
                let task_id = host.stalled_background_task(&node);
                let node = match db::get_agent_node_by_id(*target_agent_id) {
                    Ok(node) => node,
                    Err(error) => {
                        tracing::warn!(
                            "circuits: run {}: target agent {} for step {} could not be re-read, deferring the wake-up: {}",
                            active.run.id,
                            target_agent_id,
                            node_id,
                            error
                        );
                        outcome_events.push(CircuitEvent::NudgeDeferred {
                            node_id: node_id.clone(),
                            attempt,
                        });
                        continue;
                    }
                };
                let stamp = match db::agent_turn_stamp(*target_agent_id) {
                    Ok(stamp) => stamp,
                    Err(error) => {
                        tracing::warn!(
                            "circuits: run {}: turn stamp of agent {} for step {} could not be read, deferring the wake-up: {}",
                            active.run.id,
                            target_agent_id,
                            node_id,
                            error
                        );
                        outcome_events.push(CircuitEvent::NudgeDeferred {
                            node_id: node_id.clone(),
                            attempt,
                        });
                        continue;
                    }
                };
                let revision = host.assistant_revision(&node);
                // The lifecycle check is a read too, and a failed read is
                // unknown — not "the agent may be woken". Reading it as "not
                // allowed" recorded a *terminal* obsolete verdict about a
                // wake-up that was never attempted (issue #1533).
                let lifecycle_allows_wake =
                    match db::agent_node::circuit_lifecycle_blocker(*target_agent_id) {
                        Ok(None
                        | Some(
                            crate::circuit::observation::CircuitObservationBlocker::KnownWorkOutstanding,
                        )) => true,
                        Ok(Some(_)) => false,
                        Err(error) => {
                            tracing::warn!(
                                "circuits: run {}: lifecycle of agent {} for step {} could not be read, deferring the wake-up: {}",
                                active.run.id,
                                target_agent_id,
                                node_id,
                                error
                            );
                            outcome_events.push(CircuitEvent::NudgeDeferred {
                                node_id: node_id.clone(),
                                attempt,
                            });
                            continue;
                        }
                    };
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
                        lifecycle_allows_wake,
                    },
                );
                if !valid {
                    outcome_events.push(CircuitEvent::NudgeObsolete {
                        node_id: node_id.clone(),
                        attempt,
                    });
                    continue;
                }
                if effect_batch.is_cancelled() {
                    return Ok(finish(outcome_events));
                }
                // The run must still be running for the write below. A read
                // that failed is unknown, not "the run is not running" (issue
                // #1533): the claim is released so the wake-up is decided
                // again on a later pass.
                match db::get_circuit_run(active.run.id) {
                    Ok(Some(run)) if run.state == "running" => {}
                    Ok(_) => return Ok(finish(outcome_events)),
                    Err(error) => {
                        tracing::warn!(
                            "circuits: run {} state read failed before wake-up injection, deferring the wake-up: {}",
                            active.run.id,
                            error
                        );
                        outcome_events.push(CircuitEvent::NudgeDeferred {
                            node_id: node_id.clone(),
                            attempt,
                        });
                        continue;
                    }
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
#[path = "dispatch_tests.rs"]
mod tests;
