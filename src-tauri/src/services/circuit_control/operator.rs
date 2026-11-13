//! Operator decisions on a Circuit Run: the outcome of a checkpoint, the
//! recovery of a failed run, an approval, a pushed step, and the run's pace.
//!
//! Every operation here owns the whole choreography — the durable write, the
//! worker wake, and the notification the windows need. The adapters on either
//! side of the transport (issue #2673) only convert a request and a typed
//! failure.

use tauri::AppHandle;

use crate::db::circuit::evidence::CheckpointRequest;
use crate::db::circuit::operator_push::CircuitStepPushRequest;
use crate::db::circuit::step_recovery::RecoveryRequest;
use crate::db::DbError;

/// Record the outcome of an operator checkpoint and wake the worker.
///
/// The failure stays typed: the mobile route answers `503` for a transient
/// pool shortage and `409` for a refused revision, and text cannot tell those
/// apart (issue #1533). The desktop command flattens it at the Tauri
/// boundary.
pub fn record_outcome(app: Option<&AppHandle>, request: &CheckpointRequest) -> Result<(), DbError> {
    crate::db::circuit::evidence::record_outcome(request)?;
    crate::services::circuit_worker::wake_circuit_worker();
    crate::services::circuit_worker::emit_run_updated(app, request.run_id, "running");
    Ok(())
}

/// Retry a failed run's failed step, or record that the person finished it,
/// and reopen the run. The worker is woken so it re-enters the queue promptly.
pub fn recover_run(app: Option<&AppHandle>, request: &RecoveryRequest) -> Result<(), DbError> {
    crate::db::circuit::step_recovery::recover_failed_run(request)?;
    crate::services::circuit_worker::wake_circuit_worker();
    crate::services::circuit_worker::emit_run_updated(app, request.run_id, "pending");
    Ok(())
}

/// Move a stuck agent wait or classifier step on to the outcome a person
/// chose, overriding stale work or input evidence, and wake the worker so the
/// next stage starts promptly.
pub fn push_step(app: Option<&AppHandle>, request: &CircuitStepPushRequest) -> Result<(), DbError> {
    crate::db::circuit::operator_push::push_step(request)?;
    crate::services::circuit_worker::wake_circuit_worker();
    crate::services::circuit_worker::emit_run_updated(app, request.run_id, "running");
    Ok(())
}

/// Approve a CollaboratorCheck gate parked in `blocked` on this run.
///
/// The failure stays typed: the mobile route needs to tell a *refused*
/// approval (`409`) from a transient pool shortage (`503`), and text cannot
/// (issue #1533).
pub fn approve_step(run_id: i64, node_id: String) -> Result<(), DbError> {
    let steps = crate::db::list_circuit_run_steps(run_id).map_err(DbError::from)?;
    let step = steps
        .iter()
        .find(|s| s.node_id == node_id)
        .ok_or_else(|| DbError::Rendered(format!("run {} has no step {}", run_id, node_id)))?;
    if step.status != "blocked" {
        return Err(DbError::Rendered(format!(
            "step {} is not waiting for approval (status {})",
            node_id, step.status
        )));
    }
    crate::services::circuit_worker::request_circuit_approval(run_id, node_id);
    Ok(())
}

/// Update only this run's review allowance; current work keeps running.
///
/// The write is offloaded: this is an `async` operation, and a SQLite write
/// on a tokio worker would park it (issue #1380).
pub async fn set_round_limit(
    app: Option<&AppHandle>,
    run_id: i64,
    max_rounds: i32,
) -> Result<(), String> {
    let state = crate::blocking::run_blocking("set_circuit_review_round_limit", move || {
        crate::db::circuit::review_rounds::set_review_round_limit(run_id, max_rounds)
            .map_err(String::from)
    })
    .await?;
    crate::services::circuit_worker::wake_circuit_worker();
    crate::services::circuit_worker::emit_run_updated(app, run_id, &state);
    Ok(())
}

/// Gracefully pause one active run: the graph stops advancing while the
/// current steps finish. Idempotent-friendly (pausing a paused run is a
/// no-op error only if the run isn't active).
pub fn pause_run(run_id: i64) -> Result<(), DbError> {
    let run = crate::db::get_circuit_run(run_id)
        .map_err(DbError::from)?
        .ok_or_else(|| DbError::Rendered(format!("run {} does not exist", run_id)))?;
    if run.state != "running" {
        return Err(DbError::Rendered(format!(
            "only running runs can be paused (run {} is {})",
            run_id, run.state
        )));
    }
    if !crate::db::transition_circuit_run_state(run_id, "running", "paused")
        .map_err(DbError::from)?
    {
        return Err(DbError::Rendered(format!(
            "run {} changed state before it could be paused",
            run_id
        )));
    }
    crate::services::circuit_worker::wake_circuit_worker();
    tracing::info!("circuits: run {} paused", run_id);
    Ok(())
}

/// Resume one paused run; automation picks up exactly where it stopped.
pub fn resume_run(run_id: i64) -> Result<(), DbError> {
    let run = crate::db::get_circuit_run(run_id)
        .map_err(DbError::from)?
        .ok_or_else(|| DbError::Rendered(format!("run {} does not exist", run_id)))?;
    if run.state != "paused" {
        return Err(DbError::Rendered(format!(
            "only paused runs can be resumed (run {} is {})",
            run_id, run.state
        )));
    }
    if !crate::db::transition_circuit_run_state(run_id, "paused", "running")
        .map_err(DbError::from)?
    {
        return Err(DbError::Rendered(format!(
            "run {} changed state before it could be resumed",
            run_id
        )));
    }
    crate::services::circuit_worker::wake_circuit_worker();
    tracing::info!("circuits: run {} resumed", run_id);
    Ok(())
}
