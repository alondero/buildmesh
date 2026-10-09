//! Startup reconciliation and passive observer recovery. External reads and
//! watcher attachment stay outside the injected reconciliation decisions.
use super::observation::{agent_lookup_for_observation, observed_agent_for_step};
use super::*;
use crate::circuit::strategy::PassiveWatcher;

pub(super) fn restore_run_evaluators(view: &RunView) {
    // Spawn steps complete at the first yield, but keep owning their agent
    // throughout finish/review. Restoring only Running steps loses those tails.
    for id in view.steps.iter().filter_map(|step| step.agent_node_id) {
        crate::circuit::evaluator::register_circuit(id);
    }
    if let Some(source) = view.context.source_agent_id() {
        crate::circuit::evaluator::register_circuit(source);
    }
}

/// A terminal run stops piloting every agent it still references, including
/// agents a completed run hands back open. Another active run that borrows the
/// same agent restores its own ownership on its next drive.
pub(super) fn release_run_evaluators(view: &RunView) {
    for id in view.steps.iter().filter_map(|step| step.agent_node_id) {
        crate::circuit::evaluator::unregister(id);
    }
    if let Some(source) = view.context.source_agent_id() {
        crate::circuit::evaluator::unregister(source);
    }
}

/// Which passive observer a recovered node needs, or `None` when its harness
/// has no transcript watcher. Command Code (issue #1407) and Muse (issue #1709)
/// deliver their turn signal through a transcript watcher rather than an
/// attention hook, so a restarted circuit must reattach the matching watcher —
/// recovering the identity alone leaves the node unobservable. The harness
/// declares its watcher in its observation strategy; this is pure so the
/// dispatch is unit-testable without an `AppHandle`.
pub(super) fn observer_restart(node: &crate::models::AgentNode) -> Option<PassiveWatcher> {
    node.cli_session_id.as_deref().filter(|id| !id.is_empty())?;
    crate::circuit::strategy::for_agent(node).passive_watcher
}

/// Reattach the passive observer a recovered node needs. Split from the
/// watcher backends so the dispatch (which harness gets which watcher) is
/// testable without an `AppHandle` or a live backend.
pub(super) fn restart_passive_observer_with(
    node: &crate::models::AgentNode,
    start_commandcode: impl FnOnce(&str, &str) -> Result<(), String>,
    start_muse: impl FnOnce(&str, &str) -> Result<(), String>,
) -> Result<(), String> {
    let Some(restart) = observer_restart(node) else {
        return Ok(());
    };
    let session_id = node.cli_session_id.as_deref().unwrap_or_default();
    let path = crate::env::node_working_path(node).spawn_path;
    match restart {
        PassiveWatcher::CommandCode => start_commandcode(session_id, &path),
        PassiveWatcher::Muse => start_muse(session_id, &path),
    }
}

/// Reattach the real watcher backend for a recovered node. Both watcher
/// registries are idempotent, so re-running recovery on a node that already
/// observes its session is a no-op.
pub(super) fn restart_passive_observer(
    node: &crate::models::AgentNode,
    app: &AppHandle,
) -> Result<(), String> {
    restart_passive_observer_with(
        node,
        |session_id, path| {
            crate::services::commandcode_watcher::start_for_session(
                node.id, session_id, path, node.env, app,
            )
        },
        |session_id, path| {
            crate::services::muse_watcher::start_for_session(node.id, session_id, path, app)
        },
    )
}

pub(super) fn recover_run_observers(app: &AppHandle, view: &RunView) {
    recover_run_observers_with(
        view,
        |id| crate::agent::process::PROCESS_REGISTRY.is_alive(&id),
        |id| {
            crate::services::session_recovery::recover_live_node(id)?;
            let node = db::get_agent_node_by_id(id).map_err(|e| e.to_string())?;
            restart_passive_observer(&node, app)
        },
    );
}

pub(super) fn recover_run_observers_with(
    view: &RunView,
    is_alive: impl Fn(i64) -> bool,
    recover: impl FnMut(i64) -> Result<(), String>,
) {
    recover_run_observers_with_probe(
        view,
        is_alive,
        |id| {
            let key = "circuit:recover-observer";
            let Some(probe) = crate::circuit::evaluator::begin_circuit_wait_probe(id, key) else {
                return false;
            };
            crate::circuit::evaluator::note_circuit_probe(id, key, probe);
            true
        },
        recover,
    );
}

pub(super) fn recover_run_observers_with_probe(
    view: &RunView,
    is_alive: impl Fn(i64) -> bool,
    mut probe_due: impl FnMut(i64) -> bool,
    mut recover: impl FnMut(i64) -> Result<(), String>,
) {
    if view.state != RunState::Running {
        return;
    }
    let agents: HashSet<_> = view
        .steps
        .iter()
        .filter_map(|step| step.agent_node_id)
        .chain(view.context.source_agent_id())
        .collect();
    for id in agents {
        if !is_alive(id) {
            continue;
        }
        // Identity is needed to observe a yield for transcript-driven
        // harnesses. Recovery cannot itself depend on an observed yield.
        if !probe_due(id) {
            continue;
        }
        if let Err(error) = recover(id) {
            tracing::warn!("circuits: observer recovery for agent {id}: {error}");
        }
    }
}

// ---------------------------------------------------------------------------
// Startup reconciliation (milestone 3, issue #1208).
// ---------------------------------------------------------------------------

/// What startup reconciliation decides for one Running spawn step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SpawnReconciliation {
    Leave,
    Lost,
    NeverAttached,
}

/// The slice of agent-node state the pure decision needs. Built by the
/// impure wrapper so tests need no DB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ReconcileNodeState {
    pub(super) archived: bool,
    pub(super) worktree_dir_exists: Option<bool>,
}

/// Pure core of [`startup_reconcile_pass`]: classify one Running spawn
/// step observed at app launch.
pub(super) fn reconcile_spawn_step(
    attached_agent: Option<i64>,
    node: Option<ReconcileNodeState>,
) -> SpawnReconciliation {
    let Some(_agent_node_id) = attached_agent else {
        return SpawnReconciliation::NeverAttached;
    };
    match node {
        None => SpawnReconciliation::Lost,
        Some(n) if n.archived => SpawnReconciliation::Lost,
        Some(n) => match n.worktree_dir_exists {
            Some(false) => SpawnReconciliation::Lost,
            _ => SpawnReconciliation::Leave,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LineageReconciliation {
    Leave,
    Lost,
    PreserveOnLookupError,
}

pub(super) fn reconcile_lineage_target(
    target_agent_id: Option<i64>,
    lookup: impl FnOnce(i64) -> db::SqlResult<SessionStatus>,
) -> LineageReconciliation {
    let Some(target_agent_id) = target_agent_id else {
        return LineageReconciliation::Leave;
    };
    match agent_lookup_for_observation(target_agent_id, lookup(target_agent_id)) {
        Err(_) => LineageReconciliation::PreserveOnLookupError,
        Ok(None) | Ok(Some(SessionStatus::Archived)) => LineageReconciliation::Lost,
        Ok(Some(_)) => LineageReconciliation::Leave,
    }
}

/// One-shot per-launch sweep over `running` circuit runs. Maps the
/// spec's three verdicts (issue #1208) onto what observation leaves
/// behind: `Leave` = **resume** (the node row is intact and auto-resume
/// re-spawns it; observation then carries the run forward), `Lost` /
/// `NeverAttached` = **fail**. There is deliberately no in-place
/// "retry": re-running a half-attached spawn would double-spawn into a
/// worktree whose state we can't verify, so an unrecoverable step fails
/// loudly instead.
pub fn startup_reconcile_pass(app: &AppHandle) {
    let runs = match db::list_active_circuit_runs() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                "circuits: startup reconcile could not list active runs: {}",
                e
            );
            return;
        }
    };
    for active in runs {
        if active.run.state != "running" {
            continue; // pending runs start via the normal trigger path
        }
        // Reconcile in-flight runs even on disabled circuits: the orphan
        // sweep must free lost-agent slots regardless of the enabled flag.
        // (run_pass drives disabled running/paused to completion for the
        // same reason — disabling parks NEW work, not in-flight work.)
        let graph = match CircuitGraph::from_json(&active.circuit_graph_json) {
            Ok(g) => g,
            Err(e) => {
                let reason = format!("unreadable graph_json, failing: {}", e);
                tracing::warn!("circuits: run {} {}", active.run.id, reason);
                let _ = db::commit_circuit_advance(
                    active.run.id,
                    Some(crate::circuit::stepper::RunState::Failed.as_db_str()),
                    None,
                    &[corrupt_payload_step_op("__graph__", &reason)],
                );
                let _ = app.emit(
                    "circuit-run-updated",
                    CircuitRunUpdatedPayload {
                        run_id: active.run.id,
                        state: "failed".into(),
                    },
                );
                continue;
            }
        };
        let steps = match load_steps(active.run.id) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("circuits: run {} step load failed: {}", active.run.id, e);
                continue;
            }
        };
        let context = match CircuitContext::from_json(&active.run.context_json) {
            Ok(context) => context,
            Err(e) => {
                let reason = format!("unreadable context_json, failing: {}", e);
                tracing::warn!("circuits: run {} {}", active.run.id, reason);
                let _ = db::commit_circuit_advance(
                    active.run.id,
                    Some(crate::circuit::stepper::RunState::Failed.as_db_str()),
                    None,
                    &[corrupt_payload_step_op("__context__", &reason)],
                );
                let _ = app.emit(
                    "circuit-run-updated",
                    CircuitRunUpdatedPayload {
                        run_id: active.run.id,
                        state: "failed".into(),
                    },
                );
                continue;
            }
        };
        let view = RunView {
            run_id: active.run.id,
            graph,
            context,
            steps,
            state: RunState::from_db_str(&active.run.state),
        };
        for step in view
            .steps
            .iter()
            .filter(|s| s.status == StepStatus::Running)
        {
            let Some(node) = view.graph.node(&step.node_id) else {
                continue;
            };
            match &node.kind {
                CircuitNodeKind::SpawnAgentNode { .. } => {
                    let lookup = step
                        .agent_node_id
                        .map(|id| agent_lookup_for_observation(id, db::get_agent_node_by_id(id)))
                        .transpose();
                    let agent = match lookup {
                        Ok(agent) => agent.flatten(),
                        Err(_) => continue,
                    };
                    let node_state = agent.map(|n| ReconcileNodeState {
                        archived: n.status == SessionStatus::Archived,
                        worktree_dir_exists: if n.use_worktree {
                            Some(std::path::Path::new(&n.path).exists())
                        } else {
                            None
                        },
                    });
                    match reconcile_spawn_step(step.agent_node_id, node_state) {
                        SpawnReconciliation::Leave => {}
                        SpawnReconciliation::Lost => {
                            let reason = step.cancellation_reason(
                                "piloted agent was lost while the app was offline",
                            );
                            tracing::warn!("circuits: run {}: {}", active.run.id, reason);
                            let _ = fail_run_step(
                                app,
                                &active.run.id,
                                &step.node_id,
                                "cancelled",
                                &reason,
                            );
                        }
                        SpawnReconciliation::NeverAttached => {
                            let mut recovered = view.clone();
                            let transition = advance(&mut recovered, &CircuitEvent::EffectUncertain {
                                node_id: step.node_id.clone(), attempt: step.attempt,
                                reason: "Agent dispatch has no durable attachment acknowledgement. Inspect retained agents; this attempt will not spawn again automatically.".into(),
                            });
                            if let Err(error) =
                                persist_transition(active.run.id, &mut recovered, &transition)
                            {
                                tracing::warn!(
                                    "Circuit spawn recovery could not persist uncertainty: {error}"
                                );
                            }
                        }
                    }
                }
                // Orphan-detection for non-spawn steps whose target agent
                // vanished while the app was offline. The legacy pass
                // skipped these with a "Nothing to decide" comment — that
                // was wrong when the lineage agent is gone: auto-resume
                // has nothing to respawn, and the step stays `running`
                // holding a `circuit_run_capacity` slot until the user
                // cancels the run by hand.
                //
                // Step types without a target lineage (`Notify`, `Join`,
                // `RetryLimit`, `AnyCompleted`, classifier-only markers,
                // `GithubAction`) are NOT piloted — there is no agent to
                // lose, so the wildcard arm must skip them. The helper
                // returns `None` for those step kinds and we leave the
                // step alone.
                CircuitNodeKind::InjectPty { .. }
                | CircuitNodeKind::LlmTurnClassifier { .. }
                | CircuitNodeKind::AwaitAgentTurn { .. }
                | CircuitNodeKind::ReviewVerdict { .. }
                | CircuitNodeKind::SetNodeStatus { .. }
                | CircuitNodeKind::CloseAgentNode { .. } => {
                    let target = observed_agent_for_step(
                        step,
                        &view.graph,
                        &view.steps,
                        view.context.source_agent_id(),
                    );
                    if reconcile_lineage_target(target, |target| {
                        db::get_agent_node_by_id(target).map(|node| node.status)
                    }) == LineageReconciliation::Lost
                    {
                        let reason = step.cancellation_reason(&format!(
                            "target agent lineage for step {} was lost while the app was offline",
                            step.node_id
                        ));
                        tracing::warn!("circuits: run {}: {}", active.run.id, reason);
                        let _ =
                            fail_run_step(app, &active.run.id, &step.node_id, "cancelled", &reason);
                    }
                }
                _ => {
                    // Non-piloted step type (`Notify`, `Join`, `RetryLimit`,
                    // `AnyCompleted`, `GithubAction`, etc.) — nothing to
                    // orphan-detect. The legacy comment said "Nothing to
                    // decide"; that's still true here.
                }
            }
        }
    }
}
