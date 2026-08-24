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
//! observation on the next pass: a spawn step whose agent node has
//! since vanished maps to `AgentLost`, and an effect that fails
//! synchronously fails its step immediately (a Running step with no
//! attached agent would otherwise wedge the run — nothing observes it).
//! The one remaining gap, a process crash inside the milliseconds
//! between commit and stage-1 attach, is startup-reconciliation scope
//! (later milestone).

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
/// its fast tick. Milestone 2 (#1207): PTY yields also notify (reactive
/// gate evaluation), as do collaborator approvals.
static WAKE: Lazy<(Mutex<()>, Condvar)> = Lazy::new(|| (Mutex::new(()), Condvar::new()));

/// Pending collaborator approvals (#1207): `(run_id, node_id)` pairs the
/// user approved via IPC while the gate step parks in `blocked`. Drained
/// by the owning run's next pass into pure `CollaboratorApproved` events.
/// Deliberately in-memory: approvals are click-moments, not durable
/// state — after an app restart the user simply approves again.
static APPROVALS: Lazy<Mutex<Vec<(i64, String)>>> = Lazy::new(|| Mutex::new(Vec::new()));

/// Queue a collaborator approval and wake the worker immediately so the
/// parked run advances within milliseconds.
pub fn request_circuit_approval(run_id: i64, node_id: String) {
    APPROVALS.lock().unwrap().push((run_id, node_id));
    wake_circuit_worker();
}

/// Take this run's queued approvals (leaving other runs' entries alone).
fn drain_approvals_for(run_id: i64) -> Vec<String> {
    let mut queue = APPROVALS.lock().unwrap();
    let (mine, rest): (Vec<_>, Vec<_>) =
        queue.drain(..).partition(|(r, _)| *r == run_id);
    *queue = rest;
    mine.into_iter().map(|(_, node_id)| node_id).collect()
}

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
    let mut context = CircuitContext::from_json(&active.run.context_json)?;
    // Older runs (and the pre-seeding window) may lack `circuit.run_id`;
    // top it up on the first pass and persist through the normal commit.
    if context.get("circuit.run_id") != Some(active.run.id.to_string().as_str()) {
        context.with_run(active.run.id);
    }
    let mut view = RunView {
        run_id: active.run.id,
        graph,
        state: RunState::from_db_str(&active.run.state),
        context: context.clone(),
        steps: load_steps(active.run.id)?,
    };

    for event in observe(app, active, &view) {
        let transition = advance(&mut view, &event);

        // Commit FIRST (atomically), then execute effects — a crash
        // after the commit is repaired by observation next pass. The
        // (possibly run_id-topped-up) context rides along with every
        // commit so the seeding above lands whenever anything else
        // writes; a pass with no commits simply re-seeds next time.
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
                    attempt: w.attempt,
                    fresh_attempt: w.fresh_attempt,
                })
                .collect::<Vec<_>>();
            let run_state = if transition.run_state_changed {
                Some(view.state.as_db_str())
            } else {
                None
            };
            db::commit_circuit_advance(
                active.run.id,
                run_state,
                Some(&view.context.to_json()?),
                &ops,
            )
            .map_err(|e| format!("commit failed: {}", e))?;
        }

        if let Err(e) = execute_effects(app, active.run.id, active.run.mesh_id, &mut view, &transition.effects) {
            // An effect that fails synchronously (e.g. the spawn row
            // creation) must not leave its step Running forever — the
            // observation loop has nothing to observe and would wedge
            // the run. Fail the offending step directly; the next pass's
            // sweep cancels the siblings.
            tracing::warn!("circuits: run {} effect failed: {}", active.run.id, e);
            let failed: Vec<String> = transition
                .effects
                .iter()
                .filter_map(|eff| match eff {
                    crate::autopilot::circuit::stepper::Effect::SpawnAgentNode { node_id }
                    | crate::autopilot::circuit::stepper::Effect::InjectPty { node_id, .. }
                    | crate::autopilot::circuit::stepper::Effect::SetNodeStatus { node_id, .. } => {
                        Some(node_id.clone())
                    }
                    _ => None,
                })
                .collect();
let ops = failed
                .iter()
                .map(|node_id| db::CircuitStepOp {
                    node_id: node_id.clone(),
                    status: "failed".to_string(),
                    outcome: Some(Some("failed".to_string())),
                    error: Some(e.clone()),
                    agent_node_id: None,
                    attempt: 1,
                    fresh_attempt: false,
                })
                .collect::<Vec<_>>();
            db::commit_circuit_advance(
                active.run.id,
                Some(crate::autopilot::circuit::stepper::RunState::Failed.as_db_str()),
                None,
                &ops,
            )
            .map_err(|commit_err| format!("effect-failure commit also failed: {}", commit_err))?;
            view.state = RunState::Failed;
            let _ = app.emit(
                "circuit-run-updated",
                CircuitRunUpdatedPayload {
                    run_id: active.run.id,
                    state: "failed".to_string(),
                },
            );
        }

        // Live ledger: every step transition or state change refreshes
        // the Probe tab, not just terminal ones — otherwise a long agent
        // run renders as a frozen list until it finishes.
        if !transition.step_writes.is_empty() || transition.run_state_changed {
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
            attempt: row.attempt,
        })
        .collect())
}

/// Observe the world and turn it into pure events for this run.
fn observe(_app: &AppHandle, active: &db::ActiveCircuitRun, view: &RunView) -> Vec<CircuitEvent> {
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

    // Milestone-2 gate observation (#1207). Skipped while paused — a
    // parked run must not burn classifier calls or run verification
    // commands; the gates re-evaluate after Resume.
    if view.state == RunState::Running {
        observe_gates(active, view, &mut events);
    }

    // Collaborator approvals queued since the last pass.
    for node_id in drain_approvals_for(active.run.id) {
        events.push(CircuitEvent::CollaboratorApproved { node_id });
    }

    // Capacity snapshot for scheduling. Every failure here fails CLOSED
    // (zero capacity — the run parks in pending_slot until the next
    // pass), but loudly: a silent permanent queue would look exactly
    // like a busy mesh.
    let mesh_limit = match db::get_mesh_by_id(active.run.mesh_id) {
        Ok(m) => i64::from(m.autopilot_concurrency_limit),
        Err(e) => {
            tracing::warn!(
                "circuits: could not read mesh {} for capacity snapshot, failing closed: {}",
                active.run.mesh_id,
                e
            );
            0
        }
    };
    let circuit_running =
        db::count_running_circuit_steps(active.run.circuit_id).unwrap_or_else(|e| {
            tracing::warn!("circuits: running-step count failed, failing closed: {}", e);
            i64::MAX
        });
    let mesh_active =
        db::count_active_circuit_agent_nodes(active.run.mesh_id).unwrap_or_else(|e| {
            tracing::warn!("circuits: active-agent count failed, failing closed: {}", e);
            i64::MAX
        });
    events.push(CircuitEvent::Tick(Capacity {
        circuit_free_slots: active.circuit_concurrency_limit - circuit_running,
        mesh_agent_free_slots: mesh_limit - mesh_active,
    }));

    events
}

/// Gate observation (#1207): for each Running gate step, perform the
/// impure part of the gate (LLM classification / deterministic command)
/// and feed the result back as a pure event in THIS pass — so a gate
/// decision lands without waiting another tick.
fn observe_gates(active: &db::ActiveCircuitRun, view: &RunView, events: &mut Vec<CircuitEvent>) {
    for step in &view.steps {
        if step.status != StepStatus::Running {
            continue;
        }
        match view.graph.node(&step.node_id).map(|n| &n.kind) {
            Some(CircuitNodeKind::LlmTurnClassifier) => {
                if let Some(classification) = classify_latest_turn(active, view) {
                    events.push(CircuitEvent::TurnClassified {
                        node_id: step.node_id.clone(),
                        classification,
                    });
                }
            }
            Some(CircuitNodeKind::DeterministicVerification { command }) => {
                let resolved = view.context.resolve(command);
                let mesh_path = db::get_mesh_by_id(active.run.mesh_id)
                    .map(|m| m.path)
                    .unwrap_or_default();
                let green = run_verification_command(&mesh_path, &resolved);
                tracing::info!(
                    "circuits: verification '{}' → {} for run {}",
                    resolved,
                    if green { "green" } else { "red" },
                    active.run.id
                );
                events.push(CircuitEvent::VerificationResult {
                    node_id: step.node_id.clone(),
                    green,
                });
            }
            _ => {}
        }
    }
}

/// Classify the run's piloted agent's latest turn, or `None` when there
/// is nothing to classify yet. Fires only on a FRESH yield: the agent
/// must be sitting at its input prompt (`awaiting_input`/`completed`)
/// AND have produced PTY output more recently than the last evaluation
/// started — the same lost-turn guard the legacy pipeline uses.
/// Reactive latency comes from `evaluator::on_output` waking this
/// worker; correctness comes from these guards.
fn classify_latest_turn(
    active: &db::ActiveCircuitRun,
    view: &RunView,
) -> Option<Option<crate::autopilot::evaluator::Classification>> {
    use crate::autopilot::evaluator;
    let agent_node_id = view.latest_agent_node_id()?;
    if !crate::agent::process::PROCESS_REGISTRY.is_alive(&agent_node_id) {
        return None;
    }
    let yielded = matches!(
        db::get_agent_node_by_id(agent_node_id).map(|n| n.status),
        Ok(SessionStatus::AwaitingInput) | Ok(SessionStatus::Completed)
    );
    if !yielded {
        return None;
    }
    let fresh_output = match (
        evaluator::millis_since_last_output(agent_node_id),
        evaluator::millis_since_last_evaluation(agent_node_id),
    ) {
        (Some(output), Some(eval)) => output < eval,
        (Some(_), None) => true,
        _ => false,
    };
    if !fresh_output {
        return None;
    }
    // Evaluator backend env: the mesh's Autopilot provider side-channel
    // (never the node's own model — the #824 lesson).
    let provider = db::get_mesh_by_id(active.run.mesh_id)
        .ok()
        .and_then(|m| m.autopilot_provider);
    let backend_env =
        crate::session_naming::naming_backend_env(provider.as_deref().unwrap_or("anthropic"));
    evaluator::note_evaluation(agent_node_id);
    let classification = evaluator::classify(agent_node_id, &backend_env);
    tracing::info!(
        "circuits: turn classifier for run {} agent {} → {:?}",
        active.run.id,
        agent_node_id,
        classification
    );
    Some(classification)
}

/// Run a DeterministicVerification command in the mesh directory and
/// report green (exit 0) / red. Bounded wait (2 minutes), then kill and
/// call it red — a hung check must not wedge the worker thread.
fn run_verification_command(mesh_path: &str, command: &str) -> bool {
    let (program, prefix): (&str, &[&str]) =
        if cfg!(windows) { ("cmd", &["/C"]) } else { ("sh", &["-c"]) };
    let mut cmd = crate::process_util::command_no_window(program);
    cmd.args(prefix).arg(command).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    if !mesh_path.is_empty() {
        cmd.current_dir(mesh_path);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("circuits: verification '{command}' failed to spawn: {}", e);
            return false;
        }
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    tracing::warn!("circuits: verification '{command}' timed out after 120s");
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Err(e) => {
                tracing::warn!("circuits: verification '{command}' wait failed: {}", e);
                let _ = child.kill();
                return false;
            }
        }
    }
}

/// Execute one transition's effects against the real world. Takes the
/// view mutably: a spawn attaches its new agent node id to the in-memory
/// step (and the DB) so later effects in the same pass — and the
/// observation loop next pass — resolve targets from the view instead of
/// re-querying SQLite.
fn execute_effects(
    app: &AppHandle,
    run_id: i64,
    mesh_id: i64,
    view: &mut RunView,
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
                match view.latest_agent_node_id() {
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
                if let Some(agent_node_id) = view.latest_agent_node_id() {
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
    view: &mut RunView,
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
    // Keep the in-memory view in sync with the committed row so later
    // effects this pass (and the observation loop next pass) resolve the
    // injection/status target from the view.
    view.attach_agent_node(node_id, node.id);

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
