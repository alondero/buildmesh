//! The Autopilot Circuits worker (spec #1205 / walking skeleton #1206):
//! the impure seam around the pure stepper
//! (`autopilot::circuit::stepper`).
//!
//! ## Shape: dedicated OS thread + hybrid wakeups
//!
//! A dedicated `std::thread` (not a tokio task) owns the pass loop, per
//! the spec's runtime decision — it keeps blocking SQLite/git work off
//! the async runtime. Wakeups are hybrid:
//! - **Fast tick** every 2s: interval pacing, capacity unblocking, and
//!   piloted-node observation.
//! - **Condition-variable wake**: direct IPC dispatch — Trigger Now
//!   bumps the wake counter so a manual run starts within milliseconds.
//!   (GitHub poll passes and attention-webhook wakes arrive in later
//!   milestones; they plug into the same condvar.)
//!
//! ## One pass = observe → step → commit → execute
//!
//! For each active run the pass:
//! 1. **Observes** live state (agent-node status, process liveness,
//!    capacity counters) and turns it into pure [`CircuitEvent`]s;
//! 2. **Steps** via [`advance`](autopilot::circuit::stepper::advance) —
//!    no DB, no I/O;
//! 3. **Commits** the decided writes atomically through
//!    [`db::commit_circuit_advance`] (one transaction);
//! 4. **Executes** effects against the real world (spawn agent node,
//!    inject PTY prompt, set node status, notify UI).
//!
//! A crash between commit and effect execution is recovered by
//! observation on the next pass (e.g. an orphaned running spawn step
//! whose agent node vanished maps to `AgentLost`), which is the
//! milestone-1 slice of the startup-reconciliation story.

use std::sync::{Condvar, Mutex};
use std::time::Duration;

use once_cell::sync::Lazy;
use tauri::{AppHandle, Emitter};

use crate::autopilot::circuit::context::CircuitContext;
use crate::autopilot::circuit::model::{
    CircuitGraph, CircuitNodeKind, StepOutcome as GraphStepOutcome,
};
use crate::autopilot::circuit::stepper::{
    advance, Capacity, CircuitEvent, RunState, RunView, StepStatus, StepView,
};
use crate::db;
use crate::models::SessionStatus;

/// Fast tick — covers interval pacing headroom, slot unblocking latency,
/// and piloted-agent observation lag.
const TICK_INTERVAL: Duration = Duration::from_secs(2);

/// Startup delay so boot-time DB migration finishes before the first
/// pass (mirrors the legacy autopilot worker).
const STARTUP_DELAY: Duration = Duration::from_secs(5);

/// Wake condvar. Trigger Now notifies; the worker otherwise wakes on
/// its fast tick.
static WAKE: Lazy<(Mutex<()>, Condvar)> = Lazy::new(|| (Mutex::new(()), Condvar::new()));

/// Wake the circuit worker immediately (manual trigger dispatch).
pub fn wake_circuit_worker() {
    let (_lock, cvar) = &*WAKE;
    cvar.notify_all();
}

/// Start the dedicated circuits worker thread. Called once from Tauri
/// `setup`, alongside the legacy autopilot worker.
pub fn start_circuit_worker(app: AppHandle) {
    std::thread::Builder::new()
        .name("circuit-worker".to_string())
        .spawn(move || {
            std::thread::sleep(STARTUP_DELAY);
            let (lock, cvar) = &*WAKE;
            loop {
                run_pass(&app);
                // Wait for the next tick OR an immediate wake, whichever
                // first (`wait_timeout` returns either way).
                let guard = lock.lock().unwrap();
                let _ = cvar.wait_timeout(guard, TICK_INTERVAL).unwrap();
            }
        })
        .expect("circuit-worker thread spawn failed");
}

/// One full pass over every active circuit run. Per-run failures are
/// logged and isolated — one broken run must not starve the others.
fn run_pass(app: &AppHandle) {
    let runs = match db::list_active_circuit_runs() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("circuits: could not list active runs: {}", e);
            return;
        }
    };
    for active in runs {
        if !active.circuit_enabled {
            continue; // disabled mid-flight: park the run until re-enabled
        }
        if let Err(e) = drive_run(app, &active) {
            tracing::warn!("circuits: run {} pass failed: {}", active.run.id, e);
        }
    }
}

fn drive_run(
    app: &AppHandle,
    active: &db::ActiveCircuitRun,
) -> Result<(), String> {
    let graph = CircuitGraph::from_json(&active.circuit_graph_json)?;
    let context = CircuitContext::from_json(&active.run.context_json)?;
    let mut view = RunView {
        run_id: active.run.id,
        graph,
        state: RunState::from_db_str(&active.run.state),
        context,
        steps: load_steps(active.run.id)?,
    };

    for event in observe(app, active, &view) {
        let transition = advance(&mut view, &event);

        // Commit FIRST (atomically), then execute effects — a crash
        // after the commit is repaired by observation next pass.
        if !transition.step_writes.is_empty() || transition.run_state_changed {
            let ops = transition
                .step_writes
                .iter()
                .map(|w| db::CircuitStepOp {
                    node_id: w.node_id.clone(),
                    status: w.status.as_db_str().to_string(),
                    outcome: w.outcome.map(|o| o.map(|v| v.as_db_str().to_string())),
                    error: w.error.clone(),
                    agent_node_id: None,
                })
                .collect::<Vec<_>>();
            let run_state = if transition.run_state_changed {
                Some(view.state.as_db_str())
            } else {
                None
            };
            db::commit_circuit_advance(active.run.id, run_state, None, &ops)
                .map_err(|e| format!("commit failed: {}", e))?;
        }

        execute_effects(app, active.run.id, active.run.mesh_id, &view, &transition.effects)?;

        if transition.run_state_changed && matches!(view.state, RunState::Completed | RunState::Failed)
        {
            let _ = app.emit(
                "circuit-run-updated",
                CircuitRunUpdatedPayload {
                    run_id: active.run.id,
                    state: view.state.as_db_str().to_string(),
                },
            );
        }
    }
    Ok(())
}

/// Load this run's committed steps into the stepper's view shape.
fn load_steps(run_id: i64) -> Result<Vec<StepView>, String> {
    let rows = db::list_circuit_run_steps(run_id).map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|row| StepView {
            node_id: row.node_id,
            status: StepStatus::from_db_str(&row.status),
            outcome: row.outcome.as_deref().and_then(GraphStepOutcome::from_db_str),
            error: row.error_message,
            agent_node_id: row.agent_node_id,
        })
        .collect())
}

/// Observe the world and turn it into pure events for this run.
fn observe(app: &AppHandle, active: &db::ActiveCircuitRun, view: &RunView) -> Vec<CircuitEvent> {
    let _ = app; // reserved for later milestones' webhook-driven wakes
    let mut events = Vec::new();

    // A pending manual run fires now (direct IPC dispatch path).
    if view.state == RunState::Pending && active.run.trigger_identity.starts_with("manual:") {
        events.push(CircuitEvent::ManualTriggered);
    }

    // Piloted-agent observation for running steps bound to agent nodes.
    for step in &view.steps {
        let Some(agent_node_id) = step.agent_node_id else {
            continue;
        };
        if step.status != StepStatus::Running {
            continue;
        }
        let node = db::get_agent_node_by_id(agent_node_id).ok();
        match node {
            // Closed/deleted mid-run → clean cancel.
            None => {
                events.push(CircuitEvent::AgentLost { agent_node_id });
            }
            Some(n) => match n.status {
                SessionStatus::Archived => {
                    events.push(CircuitEvent::AgentLost { agent_node_id });
                }
                SessionStatus::Error => {
                    events.push(CircuitEvent::AgentFinished {
                        agent_node_id,
                        success: false,
                    });
                }
                // Milestone-1 completion heuristic: the turn detector's
                // `awaiting_input` write (or an explicit `completed`)
                // means the piloted agent finished its work. Keystrokes
                // never write these statuses, so manual PTY interaction
                // cannot produce a false completion. Known limitation: an
                // author-authored SetNodeStatus(completed) effect on the
                // piloted node would also read as completion here — the
                // milestone-2 LLM classifier gate replaces this heuristic.
                SessionStatus::AwaitingInput | SessionStatus::Completed => {
                    events.push(CircuitEvent::AgentFinished {
                        agent_node_id,
                        success: true,
                    });
                }
                _ => {}
            },
        }
    }

    // Injection readiness: any running InjectPty step whose target
    // process is now live fires its AgentReady event.
    let inject_waiting = view.steps.iter().any(|s| {
        s.status == StepStatus::Running
            && matches!(
                view.graph.node(&s.node_id).map(|n| &n.kind),
                Some(CircuitNodeKind::InjectPty { .. })
            )
    });
    if inject_waiting {
        if let Some(agent_node_id) = view.latest_agent_node_id() {
            if crate::agent::process::PROCESS_REGISTRY.is_alive(&agent_node_id) {
                for step in &view.steps {
                    if step.status == StepStatus::Running
                        && matches!(
                            view.graph.node(&step.node_id).map(|n| &n.kind),
                            Some(CircuitNodeKind::InjectPty { .. })
                        )
                    {
                        events.push(CircuitEvent::AgentReady {
                            node_id: step.node_id.clone(),
                        });
                    }
                }
            }
        }
    }

    // Capacity snapshot for scheduling.
    let mesh_limit = db::get_mesh_by_id(active.run.mesh_id)
        .map(|m| i64::from(m.autopilot_concurrency_limit))
        .unwrap_or(0);
    let circuit_running =
        db::count_running_circuit_steps(active.run.circuit_id).unwrap_or(i64::MAX);
    let mesh_active =
        db::count_active_circuit_agent_nodes(active.run.mesh_id).unwrap_or(i64::MAX);
    events.push(CircuitEvent::Tick(Capacity {
        circuit_free_slots: active.circuit_concurrency_limit - circuit_running,
        mesh_agent_free_slots: mesh_limit - mesh_active,
    }));

    events
}

/// Execute one transition's effects against the real world.
fn execute_effects(
    app: &AppHandle,
    run_id: i64,
    mesh_id: i64,
    view: &RunView,
    effects: &[crate::autopilot::circuit::stepper::Effect],
) -> Result<(), String> {
    use crate::autopilot::circuit::stepper::Effect;
    for effect in effects {
        match effect {
            Effect::SpawnAgentNode { node_id } => {
                spawn_step_agent(app, run_id, mesh_id, view, node_id)?;
            }
            Effect::InjectPty { prompt, .. } => {
                // Target = the run's most recent piloted agent (the
                // linear walking-skeleton contract).
                let target = latest_committed_agent_node_id(run_id)?;
                match target {
                    Some(target) => {
                        crate::autopilot::pipeline::write_prompt_to_pty(target, prompt, app)
                            .map_err(|e| format!("PTY injection failed: {}", e))?;
                        tracing::info!(
                            "circuits: injected prompt into agent {} for run {}",
                            target,
                            run_id
                        );
                    }
                    None => tracing::warn!(
                        "circuits: run {} had no piloted agent to inject into",
                        run_id
                    ),
                }
            }
            Effect::SetNodeStatus { status, .. } => {
                if let Some(agent_node_id) = latest_committed_agent_node_id(run_id)? {
                    let kind = SessionStatus::from_db_str(status);
                    db::update_agent_node_status(agent_node_id, kind)
                        .map_err(|e| format!("status write failed: {}", e))?;
                }
            }
            Effect::Notify { message } => {
                let _ = app.emit(
                    "circuit-notification",
                    CircuitNotificationPayload {
                        run_id,
                        message: message.clone(),
                    },
                );
            }
        }
    }
    Ok(())
}

/// The SpawnAgentNode effect: create the pending row (stage-1), wire it
/// to the step, then schedule stage-2 in the background — mirroring the
/// autopilot launch order minus the GitHub ledger.
fn spawn_step_agent(
    app: &AppHandle,
    run_id: i64,
    mesh_id: i64,
    view: &RunView,
    node_id: &str,
) -> Result<(), String> {
    use crate::agent::spawn::{SpawnIntent, SpawnRequest};
    let kind = view
        .graph
        .node(node_id)
        .map(|n| n.kind.clone())
        .ok_or_else(|| format!("node {} not in blueprint", node_id))?;
    let (prompt, name) = match kind {
        CircuitNodeKind::SpawnAgentNode { prompt, name } => (prompt, name),
        _ => return Err(format!("node {} is not a spawn node", node_id)),
    };

    let mesh = db::get_mesh_by_id(mesh_id).map_err(|e| e.to_string())?;
    let branch = crate::commands::git::get_default_branch_blocking(mesh.path.clone())
        .unwrap_or_else(|_| "main".to_string());
    // Prompt resolution happened in the stepper; resolve again here only
    // if the stored template still carries placeholders (idempotent).
    let resolved_prompt = view.context.resolve(&prompt);

    let node = crate::services::agent_node::create_pending(
        mesh.id,
        &mesh.path,
        &branch,
        None, // provider falls through the mesh/app cascade
        None,
        None,
        None,
        name.as_deref(),
    )
    .map_err(|e| e.to_string())?;

    crate::agent::session_lifecycle::on_created(
        &crate::agent::session_lifecycle::AppSessionLifecycleSink { app },
        node.id,
    )
    .map_err(|e| e.to_string())?;

    db::set_circuit_step_agent_node(run_id, node_id, node.id)
        .map_err(|e| format!("could not attach agent to step: {}", e))?;

    // Track output times for this piloted node (the PTY submit watcher
    // and future classifiers read them).
    crate::autopilot::evaluator::register(node.id);

    let _ = app.emit(
        "node-created",
        crate::commands::agent::NodeCreatedPayload { id: node.id },
    );
    tracing::info!(
        "circuits: spawned agent node {} for run {} (step {})",
        node.id,
        run_id,
        node_id
    );

    // Stage-2 in the background — same two-stage contract as every
    // other spawn path. The walking-skeleton blueprint spawns fresh
    // (empty prompt) and delivers the configured text via its InjectPty
    // step; a hand-authored graph with a spawn prompt stages it as
    // prefill instead (the Loop intent).
    let app_for_spawn = app.clone();
    tauri::async_runtime::spawn(async move {
        let intent = if resolved_prompt.trim().is_empty() {
            SpawnIntent::Fresh
        } else {
            SpawnIntent::Loop {
                initial_prompt: resolved_prompt.clone(),
            }
        };
        if let Err(error) = crate::agent::spawn::spawn_with_intent(
            &app_for_spawn,
            SpawnRequest::new(node.id, intent, Default::default()),
        )
        .await
        {
            tracing::error!("circuits: agent node {} failed: {}", node.id, error);
        }
    });

    Ok(())
}

/// The most recent piloted agent across this run's committed steps.
fn latest_committed_agent_node_id(run_id: i64) -> Result<Option<i64>, String> {
    db::list_circuit_run_steps(run_id)
        .map_err(|e| e.to_string())
        .map(|steps| steps.into_iter().rev().find_map(|s| s.agent_node_id))
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitEvents.ts")]
pub struct CircuitRunUpdatedPayload {
    #[ts(as = "i32")]
    pub run_id: i64,
    pub state: String,
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "CircuitEvents.ts")]
pub struct CircuitNotificationPayload {
    #[ts(as = "i32")]
    pub run_id: i64,
    pub message: String,
}
