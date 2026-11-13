//! Cancelling a Circuit Run, cancelling a batch of them, and deleting the
//! Circuit that owns them.
//!
//! This is the choreography that used to sit in the command layer: the
//! cancellation marker, the spawn quiescence barrier, the durable ledger
//! write, the source release, the helper-agent retirement, and the update the
//! windows need. Both transports call it here, so the ordering that keeps a
//! cancelled run from continuing its work is one implementation, not two
//! (issue #2673).
//!
//! The ordering is the contract:
//!
//! 1. Invalidate the worker's current effect batch **before** waiting for
//!    spawn teardown, so a slow external effect cannot continue after the
//!    person pressed Cancel.
//! 2. Wait for in-flight spawns to quiesce (a bounded wait).
//! 3. Terminalise the run in the database and release its source.
//! 4. Retire the helper agents, and report the cleanup failure without
//!    undoing the cancellation that already committed.

use tauri::AppHandle;

use crate::db::DbError;

/// Cancel a run and retire its helper agents.
///
/// `app` only decides whether a window is told; `None` is a process whose
/// application handle is not installed yet.
///
/// The failure stays typed: the mobile route answers `503` for a transient
/// pool shortage (issue #1533) rather than the `500` a flattened string
/// would produce.
pub fn cancel_run(app: Option<&AppHandle>, run_id: i64) -> Result<(), DbError> {
    cancel_run_with_closing_agent(app, run_id, None)
}

/// Cancel the run an Agent Node belongs to, because that node is closing.
///
/// The closing agent is the one that survives the cancellation: it is the
/// agent the person asked to close, so it must not be retired as a helper.
pub fn cancel_run_for_agent_close(
    app: Option<&AppHandle>,
    run_id: i64,
    agent_id: i64,
) -> Result<(), DbError> {
    cancel_run_with_closing_agent(app, run_id, Some(agent_id))
}

/// Bulk cancel for queue / activity hygiene in ONE write transaction:
/// every listed run is terminalised together, the worker is woken once,
/// and a single `circuit-run-updated` event refreshes the Probe once.
/// Missing rows are already gone and skipped without failing the batch.
pub fn cancel_runs(app: Option<&AppHandle>, run_ids: Vec<i64>) -> Result<(), String> {
    if run_ids.is_empty() {
        return Ok(());
    }
    let mut deduped = run_ids;
    deduped.sort_unstable();
    deduped.dedup();
    // Stop future effects first so slow spawns cannot continue after Cancel.
    for run_id in &deduped {
        crate::services::circuit_worker::mark_circuit_run_cancelled(*run_id);
    }
    // Settle in-flight spawns per run (bounded waits, no wake/emit inside).
    // A timeout does not fail the batch: the DB txn below terminalises the
    // runs anyway and late spawns self-compensate.
    for run_id in &deduped {
        if let Err(wait_error) =
            crate::services::circuit_worker::with_circuit_run_spawns_quiesced(*run_id, || {
                Ok::<(), String>(())
            })
        {
            tracing::warn!(
                "circuits: batch cancel quiescence for run {}: {}",
                run_id,
                wait_error
            );
        }
    }
    let batch = crate::db::cancel_circuit_runs(&deduped).map_err(|error| error.to_string())?;
    for run_id in &deduped {
        crate::services::circuit_worker::finish_circuit_run_cancellation(*run_id);
        // Source unregister is idempotent; the batch txn already collected
        // sources, but per-run release covers rows whose source was set
        // outside the txn window.
        release_circuit_source(*run_id);
    }
    for source in &batch.sources {
        crate::circuit::evaluator::unregister(*source);
    }
    crate::services::circuit_worker::wake_circuit_worker();
    let cleanup = retire_cancelled_agents(batch.agents);
    // One event refreshes the Probe once; the listener ignores the payload
    // and reloads the snapshot.
    let signal_run = batch
        .cancelled
        .first()
        .copied()
        .or(deduped.first().copied())
        .unwrap_or(0);
    crate::services::circuit_worker::emit_run_updated(app, signal_run, "cancelled");
    cleanup
}

/// Delete a circuit and the runs it owns.
///
/// The trigger is disabled first, then the spawn barrier holds while every
/// run is terminalised and the ledger is deleted. A cleanup that fails keeps
/// the ledger as the retry anchor: the agent ids a retry needs would be gone
/// otherwise, and a retained PTY or worktree would be orphaned permanently.
pub fn delete_circuit(app: Option<&AppHandle>, circuit_id: i64) -> Result<(), String> {
    let circuit = crate::db::get_autopilot_circuit(circuit_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("circuit {} does not exist", circuit_id))?;

    if circuit.is_preset {
        return Err("Built-in Review Blueprints are read-only".into());
    }

    // Stop fresh trigger ingestion before terminalising existing runs. The
    // worker re-checks terminal state before spawning, closing the race with a
    // pass that already loaded this circuit.
    crate::db::set_autopilot_circuit_enabled(circuit_id, false)
        .map_err(|error| error.to_string())?;
    // Invalidate effect batches before waiting for spawn quiescence. Fresh
    // triggers are disabled above, so this snapshot covers every run that
    // the deletion barrier will subsequently clean up.
    if let Ok(run_ids) = crate::db::list_circuit_run_ids_for_cleanup(circuit_id) {
        for run_id in run_ids {
            crate::services::circuit_worker::mark_circuit_run_cancelled(run_id);
        }
    }
    let result = crate::services::circuit_worker::with_circuit_spawns_quiesced(circuit_id, || {
        let run_ids = crate::db::list_circuit_run_ids_for_cleanup(circuit_id)
            .map_err(|error| error.to_string())?;
        let mut cleanup_errors = Vec::new();
        for run_id in run_ids {
            if let Err(error) = cancel_run_and_cleanup_inner(app, run_id, None) {
                cleanup_errors.push(format!("run {}: {}", run_id, error));
            }
        }
        // Keep the ledger as the retry anchor when any external retirement
        // fails. Deleting it here would erase the agent IDs needed by a
        // subsequent retry and could orphan a retained PTY/worktree
        // permanently. The circuit is already disabled and every run has
        // been terminalised, so retrying is safe and exhaustive.
        if !cleanup_errors.is_empty() {
            return Err(format!(
                "circuit cleanup incomplete; ledger retained for retry: {}",
                cleanup_errors.join("; ")
            ));
        }
        // Ledger deletion stays inside the barrier: a worker pass that had
        // already loaded this circuit cannot begin a late stage-1 spawn after
        // the final cleanup snapshot.
        crate::db::delete_autopilot_circuit(circuit_id).map_err(|e| e.to_string())
    });
    match result {
        Ok(result) => result,
        Err(wait_error) => {
            // Terminalise every known run even when a spawn refuses to quiesce
            // within the bounded wait. Keep the disabled ledger for the next
            // retry, which will re-snapshot all attached agents.
            if let Ok(run_ids) = crate::db::list_circuit_run_ids_for_cleanup(circuit_id) {
                for run_id in run_ids {
                    crate::services::circuit_worker::mark_circuit_run_cancelled(run_id);
                    if crate::db::cancel_circuit_run(run_id).is_ok() {
                        crate::services::circuit_worker::finish_circuit_run_cancellation(run_id);
                    }
                    release_circuit_source(run_id);
                }
            }
            crate::services::circuit_worker::wake_circuit_worker();
            Err(format!(
                "circuit cleanup incomplete; ledger retained for retry: {}",
                wait_error
            ))
        }
    }
}

fn cancel_run_with_closing_agent(
    app: Option<&AppHandle>,
    run_id: i64,
    closing_agent_id: Option<i64>,
) -> Result<(), DbError> {
    // Invalidate the worker's current effect batch before waiting for spawn
    // teardown. This closes the race where a slow external effect would
    // otherwise continue after the user pressed Cancel.
    crate::services::circuit_worker::mark_circuit_run_cancelled(run_id);
    match crate::services::circuit_worker::with_circuit_run_spawns_quiesced(run_id, || {
        cancel_run_and_cleanup_inner(app, run_id, closing_agent_id)
    }) {
        Ok(result) => result,
        Err(wait_error) => {
            // Stop future effects even if a stage-2 spawn exceeded the
            // quiescence window. The spawn's post-launch compensation will
            // retire itself; retaining the terminal ledger makes a retry
            // possible if that OS cleanup is transiently locked.
            if cancel_run_ledger(run_id, closing_agent_id).is_ok() {
                crate::services::circuit_worker::finish_circuit_run_cancellation(run_id);
            }
            release_circuit_source(run_id);
            crate::services::circuit_worker::wake_circuit_worker();
            Err(DbError::Rendered(wait_error))
        }
    }
}

fn cancel_run_and_cleanup_inner(
    app: Option<&AppHandle>,
    run_id: i64,
    closing_agent_id: Option<i64>,
) -> Result<(), DbError> {
    crate::services::circuit_worker::mark_circuit_run_cancelled(run_id);
    let agent_ids = cancel_run_ledger(run_id, closing_agent_id)?;
    crate::services::circuit_worker::finish_circuit_run_cancellation(run_id);
    release_circuit_source(run_id);
    crate::services::circuit_worker::wake_circuit_worker();
    let cleanup = retire_cancelled_agents(agent_ids);
    let state = crate::db::get_circuit_run(run_id)
        .map_err(DbError::from)?
        .map(|run| run.state)
        .unwrap_or_else(|| "cancelled".to_string());
    crate::services::circuit_worker::emit_run_updated(app, run_id, &state);
    cleanup.map_err(DbError::Rendered)
}

fn cancel_run_ledger(run_id: i64, closing_agent_id: Option<i64>) -> Result<Vec<i64>, DbError> {
    match closing_agent_id {
        Some(agent_id) => crate::db::cancel_circuit_run_for_agent_close(run_id, agent_id),
        None => crate::db::cancel_circuit_run(run_id),
    }
    .map_err(DbError::from)
}

fn retire_cancelled_agents_with(
    agent_ids: Vec<i64>,
    mut retire: impl FnMut(i64) -> Result<(), String>,
) -> Result<(), String> {
    let mut failures = Vec::new();
    for agent_id in agent_ids {
        if let Err(error) = retire(agent_id) {
            failures.push(format!("agent node {}: {}", agent_id, error));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "run was cancelled, but cleanup failed for {}",
            failures.join("; ")
        ))
    }
}

fn retire_cancelled_agents(agent_ids: Vec<i64>) -> Result<(), String> {
    retire_cancelled_agents_with(agent_ids, |agent_id| match crate::db::get_agent_node_by_id(
        agent_id,
    ) {
        Ok(_) => {
            crate::services::agent_node::delete(agent_id, true).map_err(|error| error.to_string())
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(()),
        Err(error) => Err(format!("lookup failed: {}", error)),
    })
}

fn release_circuit_source(run_id: i64) {
    if let Some(source) = crate::db::get_circuit_run(run_id)
        .ok()
        .flatten()
        .and_then(|run| run.source_agent_node_id)
    {
        crate::circuit::evaluator::unregister(source);
    }
}

#[cfg(test)]
#[path = "cancellation_tests.rs"]
mod tests;
