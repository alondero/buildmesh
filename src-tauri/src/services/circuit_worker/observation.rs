//! Ordered observation fan-out over RunView. The lazy live adapter preserves
//! read and job-poll order; scripted observations exercise the same decisions.
use super::admission::observe_capacity;
use super::turn_classify::ClassifiedTurn;
use super::*;

/// Resolve the agent node id the per-tick observer should check existence
/// for. Returns the step's direct `agent_node_id` when set (the
/// `SpawnAgentNode` case — spawn owns the agent it created), or — for
/// steps that act on an upstream spawned agent (`InjectPty`,
/// `LlmTurnClassifier`, `SetNodeStatus`, `CloseAgentNode`) — the resolved
/// lineage agent via [`resolve_target_agent`] (or the borrowed `$source`
/// context binding). Returns `None` when
/// neither applies (not a piloted step, or no spawn in lineage).
///
/// Used by [`observe`] and the matching startup-recovery pass to detect
/// orphaned injection/classifier steps whose piloted agent row has been
/// deleted. Without this seam, a `running` `InjectPty` whose target
/// agent_node vanished mid-run would stay `running` forever, holding a
/// `circuit_run_capacity` slot indefinitely.
pub(super) fn observed_agent_for_step(
    step: &StepView,
    graph: &CircuitGraph,
    steps: &[StepView],
    source_agent_id: Option<i64>,
) -> Option<i64> {
    let piloted = matches!(
        graph.node(&step.node_id).map(|node| &node.kind),
        Some(
            CircuitNodeKind::SpawnAgentNode { .. }
                | CircuitNodeKind::InjectPty { .. }
                | CircuitNodeKind::LlmTurnClassifier { .. }
                | CircuitNodeKind::AwaitAgentTurn { .. }
                | CircuitNodeKind::ReviewVerdict { .. }
                | CircuitNodeKind::SetNodeStatus { .. }
                | CircuitNodeKind::CloseAgentNode { .. }
        )
    );
    if !piloted {
        return None;
    }
    let source_bound = match graph.node(&step.node_id).map(|node| &node.kind) {
        Some(CircuitNodeKind::InjectPty { target_node_id, .. })
        | Some(CircuitNodeKind::LlmTurnClassifier { target_node_id })
        | Some(CircuitNodeKind::AwaitAgentTurn { target_node_id })
        | Some(CircuitNodeKind::ReviewVerdict { target_node_id }) => {
            target_node_id.as_deref() == Some("$source")
        }
        _ => false,
    };
    if source_bound {
        return source_agent_id;
    }
    step.agent_node_id
        .or_else(|| crate::circuit::stepper::resolve_target_agent(graph, steps, &step.node_id))
}

/// Observe the world and turn it into pure events for this run.
pub(super) fn observe_agent_projection(
    view: &RunView,
    step: &crate::circuit::stepper::StepView,
    agent: &crate::models::AgentNode,
    events: &mut Vec<CircuitEvent>,
) {
    let Ok(snapshot) = db::agent_node::agent_status_observation(agent.id) else {
        return;
    };
    observe_agent_projection_with(
        view,
        step,
        agent,
        snapshot,
        || crate::agent::process::PROCESS_REGISTRY.is_alive(&agent.id),
        events,
    );
}

pub(super) fn observe_agent_projection_with(
    view: &RunView,
    step: &StepView,
    agent: &crate::models::AgentNode,
    snapshot: db::agent_node::AgentStatusObservation,
    alive: impl FnOnce() -> bool,
    events: &mut Vec<CircuitEvent>,
) {
    use crate::circuit::observation::{CircuitObservation, ObservationIdentity, ObservedWorkFact};
    let identity = ObservationIdentity {
        run_id: view.run_id,
        step_id: step.node_id.clone(),
        attempt: step.attempt,
        agent_node_id: agent.id,
        session_incarnation: snapshot.session_incarnation,
        session_id: snapshot.session_id,
        turn_id: None,
        report_revision: None,
    };
    let fact = match snapshot.status {
        SessionStatus::AwaitingInput => ObservedWorkFact::NeedsInput,
        SessionStatus::Running => ObservedWorkFact::Working,
        _ => ObservedWorkFact::Yielded,
    };
    let observation = CircuitObservation {
        identity: identity.clone(),
        source: "agent_status_projection".into(),
        source_id: Some(snapshot.source_revision),
        observed_at_ms: snapshot.observed_at_ms,
        authoritative: false,
        fact,
    };
    let previous = view
        .context
        .get(&format!("node.{}.evidence.{}", step.node_id, step.attempt))
        .and_then(|s| serde_json::from_str::<crate::circuit::observation::WorkEvidence>(s).ok())
        .and_then(|e| e.latest);
    if previous.as_ref().is_none_or(|old| {
        old.source != observation.source || old.source_id != observation.source_id
    }) {
        events.push(CircuitEvent::Observed {
            expected: identity,
            observation: Box::new(observation),
        });
    }
    // An empty-prompt spawn only creates a process for a downstream prompt.
    // Its readiness acknowledges that dispatch; it asserts nothing about
    // the work that the later prompt will assign.
    if agent.status == SessionStatus::Ready
        && alive()
        && matches!(view.graph.node(&step.node_id).map(|n| &n.kind),
            Some(CircuitNodeKind::SpawnAgentNode { prompt, .. }) if view.context.resolve(prompt).trim().is_empty())
    {
        events.push(CircuitEvent::AgentFinished {
            agent_node_id: agent.id,
            success: true,
            output: None,
        });
    }
}

pub(super) fn agent_lookup_for_observation<T>(
    agent_node_id: i64,
    lookup: db::SqlResult<T>,
) -> db::SqlResult<Option<T>> {
    match lookup {
        Ok(node) => Ok(Some(node)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => {
            tracing::warn!(
                "circuits: agent {agent_node_id} lookup failed; loss is unconfirmed: {error}"
            );
            Err(error)
        }
    }
}

/// Observe the world and turn it into pure events for this run.
pub(super) fn observe(
    app: &AppHandle,
    active: &db::ActiveCircuitRun,
    view: &RunView,
) -> Vec<CircuitEvent> {
    observe_with(view, &mut LiveObservations { app, active })
}

pub(super) fn observe_with(view: &RunView, source: &mut impl Observations) -> Vec<CircuitEvent> {
    let mut events = source.native_events(view);

    // A pending run fires now. Runs exist in Pending only because an
    // actual trigger dispatch minted them — manual Trigger Now (milestone
    // 1) or a freshly-ingested GitHub/interval trigger (milestone 3,
    // `circuit_triggers`) — so the event is trigger-kind agnostic.
    if view.state == RunState::Pending {
        events.push(CircuitEvent::Triggered);
    }

    // Piloted-agent observation for running steps bound to agent nodes.
    //
    // Two observation sources per step:
    //   * **direct** — `SpawnAgentNode` carries the agent it owns on
    //     `step.agent_node_id`. The original code path.
    //   * **lineage** — `InjectPty` / `LlmTurnClassifier` / `SetNodeStatus` /
    //     `CloseAgentNode` act on an upstream spawned agent; their own
    //     `step.agent_node_id` is NULL. If the upstream spawn's agent is
    //     deleted mid-run, the dependent step stays `running` forever
    //     (the legacy draft-only `SpawnAgentNode` filter let this leak —
    //     issue surfaced via `buildmesh` mesh 65, runs 3/5/6).
    //
    // Both paths converge on the same AgentLost event so the stepper
    // cancels the step via `cancel_step` and the run reaches a terminal
    // state through the normal cascade.
    for step in &view.steps {
        if !matches!(step.status, StepStatus::Running | StepStatus::Unverified) {
            continue;
        }
        if step.agent_node_id.is_none()
            && matches!(
                view.graph.node(&step.node_id).map(|node| &node.kind),
                Some(CircuitNodeKind::SpawnAgentNode { .. })
            )
        {
            events.push(CircuitEvent::EffectUncertain { node_id: step.node_id.clone(), attempt: step.attempt,
                reason: "Agent dispatch has no durable attachment acknowledgement. Inspect retained agents; this attempt will not spawn again automatically.".into() });
            continue;
        }
        let Some(agent_node_id) = observed_agent_for_step(
            step,
            &view.graph,
            &view.steps,
            view.context.source_agent_id(),
        ) else {
            continue;
        };
        match agent_lookup_for_observation(agent_node_id, source.agent(agent_node_id)) {
            Ok(None) => events.push(CircuitEvent::AgentLost { agent_node_id }),
            Err(_) => {}
            Ok(Some(n)) => match n.status {
                SessionStatus::Archived => {
                    events.push(CircuitEvent::AgentLost { agent_node_id });
                }
                // Issue #1793: the reaper transitioned a never-observed
                // piloted node to the terminal `Lost` state. It cannot be
                // re-observed, so cancel the step now rather than waiting
                // out the first-observation window.
                SessionStatus::Lost => {
                    events.push(CircuitEvent::AgentLost { agent_node_id });
                }
                SessionStatus::Error => {
                    let tail = source.error_tail(agent_node_id);
                    events.push(CircuitEvent::AgentFinished {
                        agent_node_id,
                        success: false,
                        output: Some(tail),
                    });
                }
                SessionStatus::Running
                | SessionStatus::AwaitingInput
                | SessionStatus::Ready
                | SessionStatus::Completed => {
                    source.project_agent(view, step, &n, &mut events);
                    if let Some(event) = source.pull(view, step, &n) {
                        events.push(event);
                    }
                }
                _ => {}
            },
        }
    }

    // A missing result cannot establish whether a remote mutation happened.
    // Preserve the attempt as Unverified instead of replaying the effect.
    for step in &view.steps {
        if step.status == StepStatus::Running
            && matches!(
                view.graph.node(&step.node_id).map(|node| &node.kind),
                Some(CircuitNodeKind::GithubAction { .. })
            )
        {
            events.push(CircuitEvent::GithubActionRetry {
                node_id: step.node_id.clone(),
            });
        } else if step.status == StepStatus::Unverified
            && matches!(
                view.graph.node(&step.node_id).map(|node| &node.kind),
                Some(CircuitNodeKind::GithubAction {
                    action: crate::circuit::model::GithubActionKind::OpenPr,
                    ..
                })
            )
            && source.has_github_target(view, step)
        {
            events.push(CircuitEvent::GithubRecheckDue {
                node_id: step.node_id.clone(),
                attempt: step.attempt,
                now_ms: source.now_ms(),
            });
        }
    }

    // CloseAgentNode is committed complete before its destructive effect is
    // executed. Replay it while the completed step still points at an agent;
    // this closes the crash window where a reviewer row could survive a
    // restart after the step commit but before deletion.
    observe_close_agent_retries(view, &mut events);

    // Injection readiness: any running InjectPty step whose target
    // process is now live fires its AgentReady event.
    for step in &view.steps {
        if step.status == StepStatus::Running {
            if let Some(CircuitNodeKind::InjectPty { .. }) =
                view.graph.node(&step.node_id).map(|n| &n.kind)
            {
                if view.context.get(&format!(
                    "node.{}.prompt_delivery.{}",
                    step.node_id, step.attempt
                )) == Some("intent")
                {
                    // A concurrent receipt may have rejected the step commit
                    // after delivery. Replay only its durable acknowledgement.
                    match source.prompt_acknowledged(view, step) {
                        Ok(true) => {
                            events.push(CircuitEvent::PromptDelivered {
                                node_id: step.node_id.clone(),
                                attempt: step.attempt,
                            });
                            continue;
                        }
                        Err(error) => {
                            tracing::warn!(
                                "circuits: prompt acknowledgement lookup failed: {error}"
                            );
                            continue;
                        }
                        Ok(false) => {}
                    }
                    events.push(CircuitEvent::EffectUncertain {
                        node_id: step.node_id.clone(), attempt: step.attempt,
                        reason: "Prompt dispatch has no durable acknowledgement. Inspect the agent before recording an outcome; the prompt will not be replayed.".into(),
                    });
                    continue;
                }
                if let Some(agent_node_id) = view.resolve_target_agent(&step.node_id) {
                    if source.alive(agent_node_id) {
                        events.push(CircuitEvent::AgentReady {
                            node_id: step.node_id.clone(),
                        });
                    }
                }
            }
        }
    }

    // Milestone-2 gate observation (#1207). Skipped while paused — a
    // parked run must not burn classifier calls or run verification
    // commands; the gates re-evaluate after Resume.
    if view.state == RunState::Running {
        turn_classify::observe_gates_with(view, &mut events, source);
    }

    // Issue-triggered review blueprints always include a CollaboratorCheck so
    // an untrusted issue can park visibly for approval. The trigger pass has
    // already made the network-backed trust decision and records `auto` in
    // the run context for trusted authors; turn that durable decision into
    // the same event the Probe's Approve button emits.
    if view.state == RunState::Running
        && view.context.get("autopilot.collaborator_gate") == Some("auto")
    {
        for step in &view.steps {
            if step.status == StepStatus::Blocked
                && matches!(
                    view.graph.node(&step.node_id).map(|node| &node.kind),
                    Some(CircuitNodeKind::CollaboratorCheck {
                        require_approval: true
                    })
                )
            {
                events.push(CircuitEvent::CollaboratorApproved {
                    node_id: step.node_id.clone(),
                });
            }
        }
    }

    // Collaborator approvals queued since the last pass.
    for node_id in source.approvals(view.run_id) {
        events.push(CircuitEvent::CollaboratorApproved { node_id });
    }

    // Capacity snapshot for scheduling. Every failure here fails CLOSED
    // (zero capacity — the run parks in Queued until the next
    // pass), but loudly: a silent permanent queue would look exactly
    // like a busy mesh. Circuit-owned agents use their durable run lease;
    // the optional app-wide pool is the only external agent backstop.
    events.push(source.capacity());

    if view.state == RunState::Running {
        let now_ms = source.now_ms();
        observe_waits_with(view, &mut events, now_ms, |step, id| {
            source.wait(view, step, id)
        });
    }
    events
}

pub(super) const YIELDED_WAIT_MS: i64 = 15 * 60_000;
pub(super) const ACTIVE_WAIT_MS: i64 = 2 * 60 * 60_000;
const APPROVAL_WAIT_MS: i64 = 60 * 60_000;

/// Per-step wall-clock budget authored on `SpawnAgentNode.timeout_seconds`
/// (#1219), in the milliseconds the watchdog seam speaks. Returns `None`
/// when the graph node carries no explicit budget so the caller keeps its
/// fixed default (`YIELDED_WAIT_MS` / `ACTIVE_WAIT_MS` / `APPROVAL_WAIT_MS`).
/// `Some(0)` collapses to absent — the same rule as the spawn-input resolver
/// (`resolve_circuit_spawn_inputs`) — so a zero-int overflow at save time
/// can't request an instant expiry.
pub(super) fn spawn_step_timeout_ms(kind: Option<&CircuitNodeKind>) -> Option<i64> {
    match kind {
        Some(CircuitNodeKind::SpawnAgentNode {
            timeout_seconds: Some(t),
            ..
        }) if *t > 0 => Some(*t as i64 * 1000),
        _ => None,
    }
}

/// Whether the classifier's latest verdict for this attempt says the agent
/// still has work (`working`) or was sent a continuation (`continue`).
fn classifier_reported_work_remaining(view: &RunView, step: &StepView) -> bool {
    let prefix = format!("node.{}", step.node_id);
    view.context.get(&format!("{prefix}.evaluated_attempt"))
        == Some(step.attempt.to_string().as_str())
        && matches!(
            view.context.get(&format!("{prefix}.classification")),
            Some("working" | "continue")
        )
}

#[cfg(test)]
pub(super) fn observe_waits(view: &RunView, events: &mut Vec<CircuitEvent>) {
    observe_waits_with(
        view,
        events,
        chrono::Utc::now().timestamp_millis(),
        |step, id| live_wait(view, step, id),
    );
}

pub(super) fn observe_waits_with(
    view: &RunView,
    events: &mut Vec<CircuitEvent>,
    now_ms: i64,
    mut wait: impl FnMut(&StepView, i64) -> Option<WaitObservation>,
) {
    if view.state != RunState::Running {
        return;
    }
    for step in &view.steps {
        if !matches!(step.status, StepStatus::Running | StepStatus::Blocked) {
            continue;
        }
        if events.iter().any(|e| matches!(e, CircuitEvent::TurnClassified { node_id, .. } if node_id == &step.node_id)) { continue; }
        let mut progress = None;
        let mut observed = None;
        let mut stalled_task_id = None;
        let (reason, timeout_ms) = if step.status == StepStatus::Blocked {
            (
                "Waiting for your approval. This gate does not expire while you are away."
                    .to_string(),
                APPROVAL_WAIT_MS,
            )
        } else if let Some(id) = step
            .agent_node_id
            .or_else(|| view.resolve_target_agent(&step.node_id))
        {
            let Some(wait) = wait(step, id) else {
                continue;
            };
            progress = wait.progress;
            let observed_here = wait.observed;
            observed = Some(observed_here);
            stalled_task_id = wait.stalled_task_id;
            let yielded = wait.yielded;
            let reason = if !observed_here {
                "Waiting for session identity and a readable report; retrying discovery every 10 seconds.".to_string()
            } else if !yielded {
                String::new()
            } else {
                step.error.clone().unwrap_or_else(|| {
                    "Waiting for a fresh agent report; retrying observation every 10 seconds."
                        .into()
                })
            };
            let budget = if !yielded {
                wait.active_budget_ms
            } else if classifier_reported_work_remaining(view, step) {
                // The harness budget bounds reconciling a yield that has no
                // verdict yet. After the classifier says work remains, the
                // awaited report is the agent's own, which can take minutes.
                wait.yielded_budget_ms.max(YIELDED_WAIT_MS)
            } else {
                wait.yielded_budget_ms
            };
            (reason, budget)
        } else {
            (
                "Waiting for step prerequisites; no agent is attached.".into(),
                YIELDED_WAIT_MS,
            )
        };
        // #1219: an explicit per-step budget on the graph node overrides the
        // fixed defaults above. The stepper enforces whatever `timeout_ms`
        // this event carries, so this override is what makes the
        // user-visible timeout setting take effect on circuit execution — and
        // why it also takes precedence over the unobserved fast fail.
        let explicit_budget =
            spawn_step_timeout_ms(view.graph.node(&step.node_id).map(|n| &n.kind));
        let timeout_ms = explicit_budget.unwrap_or(timeout_ms);
        // A step with no agent has nothing to observe; only a bound agent can
        // be "unobserved".
        events.push(CircuitEvent::WaitObserved {
            node_id: step.node_id.clone(),
            attempt: step.attempt,
            now_ms,
            progress,
            observed: observed.unwrap_or(true),
            explicit_budget: explicit_budget.is_some(),
            reason,
            timeout_ms,
        });
        // Issue #2105: a yielded session with a finished background task it
        // cannot learn about on its own. The prompt this produces is the only
        // thing that starts the turn in which the CLI would have announced the
        // task, so it is fenced to this turn's stamp and revision and spent
        // after one use.
        if let Some(task_id) = stalled_task_id {
            let Some(agent_id) = step
                .agent_node_id
                .or_else(|| view.resolve_target_agent(&step.node_id))
            else {
                continue;
            };
            // Same identity the continuation fence uses: the lifecycle stamp and
            // the assistant-report revision describe the turn being observed.
            let stamp = db::agent_turn_stamp(agent_id)
                .ok()
                .flatten()
                .unwrap_or_default();
            let revision = db::get_agent_node_by_id(agent_id)
                .ok()
                .and_then(|node| {
                    crate::coordinator::enrichment::assistant_report(&node)
                        .map(|report| report.revision)
                })
                .unwrap_or_default();
            let input_stamp = crate::agent::process::PROCESS_REGISTRY.input_stamp(agent_id);
            // Without a real input-ownership stamp the nudge cannot be
            // delivered safely: a terminal whose input is ambiguous (a staged
            // draft, a pending key sequence, no live process) must not be
            // written to. Staying silent here is the safe reading — the normal
            // wait budget still owns the step.
            let Some(input_stamp) = input_stamp else {
                continue;
            };
            events.push(CircuitEvent::NudgeIdleSession {
                node_id: step.node_id.clone(),
                attempt: step.attempt,
                stamp,
                revision: revision.to_string(),
                input_stamp,
                task_id,
            });
        }
    }
}

/// Add replay events for close effects whose target association is still
/// present. Once the effect clears the target SpawnAgentNode association,
/// this returns no event on the next observation pass.
pub(super) fn observe_close_agent_retries(view: &RunView, events: &mut Vec<CircuitEvent>) {
    for step in &view.steps {
        if step.status == StepStatus::Completed {
            if let Some(CircuitNodeKind::CloseAgentNode { .. }) =
                view.graph.node(&step.node_id).map(|node| &node.kind)
            {
                // A completed close belongs to its review round, not a
                // later reviewer attached to the same spawn step.
                if view.resolve_target_agent(&step.node_id).is_some_and(|id| {
                    view.steps.iter().any(|target| {
                        target.agent_node_id == Some(id) && target.attempt > step.attempt
                    })
                }) {
                    continue;
                }
                if view.resolve_target_agent(&step.node_id).is_some() {
                    events.push(CircuitEvent::CloseAgentRetry {
                        node_id: step.node_id.clone(),
                    });
                }
            }
        }
    }
}

pub(super) struct WaitObservation {
    pub progress: Option<String>,
    pub observed: bool,
    pub yielded: bool,
    pub yielded_budget_ms: i64,
    pub active_budget_ms: i64,
    /// A finished background task this harness will never learn about without
    /// a new turn (issue #2105). `None` for every harness that does not
    /// delegate background work to the session, and while the session is still
    /// producing output.
    pub stalled_task_id: Option<String>,
}

pub(super) trait Observations {
    fn native_events(&mut self, view: &RunView) -> Vec<CircuitEvent>;
    fn agent(&mut self, id: i64) -> db::SqlResult<crate::models::AgentNode>;
    fn project_agent(
        &mut self,
        view: &RunView,
        step: &StepView,
        agent: &crate::models::AgentNode,
        events: &mut Vec<CircuitEvent>,
    );
    fn pull(
        &mut self,
        view: &RunView,
        step: &StepView,
        agent: &crate::models::AgentNode,
    ) -> Option<CircuitEvent>;
    fn error_tail(&mut self, id: i64) -> String;
    fn has_github_target(&mut self, view: &RunView, step: &StepView) -> bool;
    fn prompt_acknowledged(&mut self, view: &RunView, step: &StepView) -> db::SqlResult<bool>;
    fn alive(&mut self, id: i64) -> bool;
    fn classify(&mut self, view: &RunView, step: &StepView) -> Option<ClassifiedTurn>;
    fn verify(&mut self, view: &RunView, step: &StepView, command: &str) -> Option<bool>;
    fn blocked(&mut self, agent: i64, issue: i64);
    fn approvals(&mut self, run_id: i64) -> Vec<String>;
    fn capacity(&mut self) -> CircuitEvent;
    fn now_ms(&mut self) -> i64;
    fn wait(&mut self, view: &RunView, step: &StepView, id: i64) -> Option<WaitObservation>;
}

pub(super) struct LiveObservations<'a> {
    pub app: &'a AppHandle,
    pub active: &'a db::ActiveCircuitRun,
}

impl Observations for LiveObservations<'_> {
    fn native_events(&mut self, view: &RunView) -> Vec<CircuitEvent> {
        native_hooks::pending(view).unwrap_or_else(|error| {
            tracing::warn!("Circuit native receipt read failed: {error}");
            Vec::new()
        })
    }

    fn agent(&mut self, id: i64) -> db::SqlResult<crate::models::AgentNode> {
        db::get_agent_node_by_id(id)
    }

    fn project_agent(
        &mut self,
        view: &RunView,
        step: &StepView,
        agent: &crate::models::AgentNode,
        events: &mut Vec<CircuitEvent>,
    ) {
        observe_agent_projection(view, step, agent, events);
    }

    fn pull(
        &mut self,
        view: &RunView,
        step: &StepView,
        agent: &crate::models::AgentNode,
    ) -> Option<CircuitEvent> {
        native_pull::observe(view, step, agent)
    }

    fn error_tail(&mut self, id: i64) -> String {
        crate::circuit::evaluator::cleaned_turn_tail(id)
    }

    fn has_github_target(&mut self, view: &RunView, step: &StepView) -> bool {
        db::circuit::evidence::latest_effect_target(view.run_id, &step.node_id, step.attempt)
            .ok()
            .flatten()
            .is_some()
    }

    fn prompt_acknowledged(&mut self, view: &RunView, step: &StepView) -> db::SqlResult<bool> {
        db::circuit::evidence::prompt_delivery_acknowledged(
            view.run_id,
            &step.node_id,
            step.attempt,
        )
    }

    fn alive(&mut self, id: i64) -> bool {
        crate::agent::process::PROCESS_REGISTRY.is_alive(&id)
    }

    fn classify(&mut self, view: &RunView, step: &StepView) -> Option<ClassifiedTurn> {
        jobs::classify(self.active, view, step)
    }

    fn verify(&mut self, view: &RunView, step: &StepView, command: &str) -> Option<bool> {
        jobs::verify(self.active, view, step, command)
    }

    fn blocked(&mut self, agent: i64, issue: i64) {
        let _ = self.app.emit(
            "circuit-agent-blocked",
            CircuitAgentBlockedPayload {
                node_id: agent,
                issue,
            },
        );
    }

    fn approvals(&mut self, run_id: i64) -> Vec<String> {
        drain_approvals_for(run_id)
    }

    fn capacity(&mut self) -> CircuitEvent {
        observe_capacity(self.active, crate::preferences::circuit_agent_pool_size())
    }

    fn now_ms(&mut self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    fn wait(&mut self, view: &RunView, step: &StepView, id: i64) -> Option<WaitObservation> {
        live_wait(view, step, id)
    }
}

pub(super) fn live_wait(view: &RunView, step: &StepView, id: i64) -> Option<WaitObservation> {
    let probe = format!("run:{}:wait:{}:{}", view.run_id, step.node_id, step.attempt);
    let generation = crate::circuit::evaluator::begin_circuit_wait_probe(id, &probe)?;
    crate::circuit::evaluator::note_circuit_probe(id, &probe, generation);
    let node = db::get_agent_node_by_id(id).ok()?;
    let progress =
        crate::coordinator::enrichment::assistant_report(&node).map(|report| report.revision);
    let policy = observer_policy::for_agent(&node);
    // A session still producing output is never stalled, whatever its
    // transcript says; only a yielded one can be missing a wake-up.
    let yielded = matches!(
        node.status,
        SessionStatus::AwaitingInput | SessionStatus::Ready | SessionStatus::Completed
    );
    let stalled_task_id = yielded.then(|| stalled_background_task(&node)).flatten();
    Some(WaitObservation {
        observed: node
            .cli_session_id
            .as_deref()
            .is_some_and(|id| !id.is_empty())
            || progress.is_some(),
        progress,
        yielded,
        yielded_budget_ms: i64::from(policy.yielded_budget_ms),
        active_budget_ms: i64::from(policy.active_budget_ms),
        stalled_task_id,
    })
}

/// The finished background task a yielded session has not read, when its
/// harness's own transcript says so.
///
/// Pure file I/O against the session's transcript — never a database read, so
/// it cannot hold a connection across disk. Any failure (unknown harness,
/// missing transcript, unparsable record) yields `None`: an unreadable session
/// is not evidence of a stall.
fn stalled_background_task(node: &crate::models::AgentNode) -> Option<String> {
    let harness = crate::circuit::strategy::selector_for_agent(node)
        .provider()
        .map(|provider| provider.adapter().id())?;
    let format = crate::services::transcript_reader::TranscriptFormat::for_harness(harness)?;
    crate::services::transcript_reader::stalled_background_task(
        format,
        node.cli_session_id.as_deref(),
        node.worktree_path.as_deref().unwrap_or_default(),
    )
}
