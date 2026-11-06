//! The manual "Trigger Now" path for an Autopilot Circuit.
//!
//! Triggering a run is business logic between the command boundary and the
//! database, so it lives here rather than in `commands/circuit.rs`: the
//! command validates its input, calls [`trigger_circuit_now`], and maps the
//! error to `String` (issue #2527).
//!
//! The read of the circuit and the write of the run happen under **one**
//! writer connection — the run must be created against the graph that was
//! just read, and the writer is the only connection that may write. The
//! connection is released before the caller signals the worker, so the
//! wake's condvar signal cannot deadlock a reader-pool checkout that races
//! the next commit (the three-phase pattern).
//!
//! The automatic triggers — the GitHub poll pass and the interval pass —
//! live in [`crate::services::circuit_triggers`].

use crate::circuit::context::CircuitContext;
use crate::circuit::model::CircuitGraph;
use crate::db;
use crate::preferences::AppPreferences;

/// Trigger a run of `circuit_id` now.
///
/// The writer is held for the read and the write, and released before this
/// returns: the caller's worker wake happens after the guard is gone.
pub fn trigger_circuit_now(circuit_id: i64, preferences: &AppPreferences) -> Result<i64, String> {
    let mut conn = db::write_conn();
    trigger_circuit_now_prepared_locked(&mut conn, circuit_id, preferences)
}

/// The trigger's core, on a caller-supplied connection.
///
/// Production callers go through [`trigger_circuit_now`], which owns the
/// writer; the per-test isolated variant passes its own connection so
/// parallel tests can each operate against their own in-memory DB
/// (issue #1691).
pub(crate) fn trigger_circuit_now_prepared_locked(
    conn: &mut rusqlite::Connection,
    circuit_id: i64,
    preferences: &AppPreferences,
) -> Result<i64, String> {
    let circuit = db::circuit::get_autopilot_circuit_inner(conn, circuit_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("circuit {} does not exist", circuit_id))?;
    let graph = CircuitGraph::from_json(&circuit.graph_json)?;
    if graph.requires_source_agent() {
        return Err("Start this Circuit from the source agent's title bar.".into());
    }
    if graph.is_issue_driven_autopilot_review() {
        return Err(
            "issue-driven Circuit review runs are triggered by labelled GitHub issues; Trigger Now requires issue context"
                .to_string(),
        );
    }
    // Draft-first (issue #1356): Trigger Now is the dry-run seam and
    // must work while the circuit is still disabled. Background
    // pollers (`list_enabled_circuits`) stay gated on `enabled`.
    let identity = format!(
        "manual:{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    let mut context = CircuitContext::new();
    context.with_circuit(circuit.id, &circuit.name, circuit.mesh_id);
    context.set(
        "review.provider",
        preferences.reviewer_provider.as_deref().unwrap_or(""),
    );
    let action = "draft_pr".to_string();
    context.with_circuit_finish_prompt(None, Some(action.as_str()));
    let run_id = db::circuit::create_circuit_run_prepared_locked(
        conn,
        circuit.id,
        circuit.mesh_id,
        &identity,
        &context.to_json()?,
        preferences,
    )
    .map_err(|e| e.to_string())?;
    Ok(run_id)
}
