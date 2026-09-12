//! Circuit run queue: pending-list, neighbour swaps, and explicit reorder.

use rusqlite::{Connection, OptionalExtension, params};

use crate::db::SqlResult;
use crate::models::AutopilotCircuitRun;

/// Pending Circuit Runs on one mesh in worker-admission order. The circuit
/// name rides beside the canonical run row for the Probe's global queue.
pub fn list_queued_circuit_runs(
    mesh_id: i64,
) -> SqlResult<Vec<(AutopilotCircuitRun, String)>> {
    let db = crate::db::read_conn();
    list_queued_circuit_runs_inner(&db, mesh_id)
}

pub(crate) fn list_queued_circuit_runs_inner(
    db: &Connection,
    mesh_id: i64,
) -> SqlResult<Vec<(AutopilotCircuitRun, String)>> {
    let mut stmt = db.prepare(
        "SELECT r.id, r.circuit_id, r.mesh_id, r.trigger_identity, r.state, \
                r.context_json, r.source_agent_node_id, r.created_at, r.updated_at, c.name \
         FROM autopilot_circuit_runs r \
         JOIN autopilot_circuits c ON c.id = r.circuit_id \
         WHERE r.mesh_id = ?1 AND r.state = 'pending' \
         ORDER BY r.queue_position, r.id",
    )?;
    let rows = stmt.query_map(params![mesh_id], |row| {
        Ok((
            AutopilotCircuitRun {
                id: row.get(0)?,
                circuit_id: row.get(1)?,
                mesh_id: row.get(2)?,
                trigger_identity: row.get(3)?,
                state: row.get(4)?,
                context_json: row.get(5)?,
                source_agent_node_id: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            },
            row.get(9)?,
        ))
    })?;
    rows.collect()
}
/// Swap one pending run with its adjacent queue neighbour. Returns false at
/// the front/back boundary. Running and terminal rows cannot be reordered.
pub fn move_queued_circuit_run(run_id: i64, toward_front: bool) -> SqlResult<bool> {
    let mut db = crate::db::write_conn();
    move_queued_circuit_run_locked(&mut db, run_id, toward_front)
}

/// Per-test isolated variant of [`move_queued_circuit_run`] (issue #1691).
/// Opens its own transaction on `db`; the `_locked` suffix distinguishes
/// this from the `_inner` helpers that operate inside an externally-managed
/// transaction.
pub(crate) fn move_queued_circuit_run_locked(
    db: &mut Connection,
    run_id: i64,
    toward_front: bool,
) -> SqlResult<bool> {
    let tx = db.transaction()?;
    let Some((mesh_id, position)): Option<(i64, i64)> = tx.query_row(
        "SELECT mesh_id, queue_position FROM autopilot_circuit_runs \
         WHERE id = ?1 AND state = 'pending'",
        params![run_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()? else {
        // The worker may promote or cancel the row between the UI render and
        // this command. A stale reorder is a harmless no-op, not a raw
        // QueryReturnedNoRows error at the IPC boundary.
        return Ok(false);
    };
    let neighbour = if toward_front {
        tx.query_row(
            "SELECT id, queue_position FROM autopilot_circuit_runs \
             WHERE mesh_id = ?1 AND state = 'pending' AND queue_position < ?2 \
             ORDER BY queue_position DESC, id DESC LIMIT 1",
            params![mesh_id, position],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?
    } else {
        tx.query_row(
            "SELECT id, queue_position FROM autopilot_circuit_runs \
             WHERE mesh_id = ?1 AND state = 'pending' AND queue_position > ?2 \
             ORDER BY queue_position, id LIMIT 1",
            params![mesh_id, position],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?
    };
    let Some((neighbour_id, neighbour_position)) = neighbour else {
        return Ok(false);
    };
    tx.execute(
        "UPDATE autopilot_circuit_runs \
         SET queue_position = CASE id WHEN ?1 THEN ?4 WHEN ?2 THEN ?3 END \
         WHERE id IN (?1, ?2)",
        params![run_id, neighbour_id, position, neighbour_position],
    )?;
    tx.commit()?;
    Ok(true)
}

/// Jump one pending run to the front or back of its mesh queue. Returns
/// false when already at that edge or when the row is no longer pending
/// (worker promoted/cancelled it between render and command).
pub fn move_queued_circuit_run_to_edge(run_id: i64, to_front: bool) -> SqlResult<bool> {
    let mut db = crate::db::write_conn();
    move_queued_circuit_run_to_edge_locked(&mut db, run_id, to_front)
}

/// Per-test isolated variant of [`move_queued_circuit_run_to_edge`] (issue #1691).
/// Opens its own transaction on `db`; the `_locked` suffix distinguishes
/// this from the `_inner` helpers that operate inside an externally-managed
/// transaction.
pub(crate) fn move_queued_circuit_run_to_edge_locked(
    db: &mut Connection,
    run_id: i64,
    to_front: bool,
) -> SqlResult<bool> {
    let tx = db.transaction()?;
    let Some((mesh_id, position)): Option<(i64, i64)> = tx.query_row(
        "SELECT mesh_id, queue_position FROM autopilot_circuit_runs \
         WHERE id = ?1 AND state = 'pending'",
        params![run_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()? else {
        return Ok(false);
    };
    let edge: Option<i64> = tx.query_row(
        if to_front {
            "SELECT MIN(queue_position) FROM autopilot_circuit_runs \
             WHERE mesh_id = ?1 AND state = 'pending'"
        } else {
            "SELECT MAX(queue_position) FROM autopilot_circuit_runs \
             WHERE mesh_id = ?1 AND state = 'pending'"
        },
        params![mesh_id],
        |row| row.get(0),
    ).optional()?.flatten();
    let Some(edge_position) = edge else {
        return Ok(false);
    };
    if (to_front && position <= edge_position) || (!to_front && position >= edge_position) {
        return Ok(false);
    }
    let new_position = if to_front { edge_position - 1 } else { edge_position + 1 };
    tx.execute(
        "UPDATE autopilot_circuit_runs SET queue_position = ?2 WHERE id = ?1",
        params![run_id, new_position],
    )?;
    tx.commit()?;
    Ok(true)
}

/// Rewrite a mesh queue to an explicit front-to-back run-id order (drag-drop
/// / keyboard reorder seam). The payload must contain exactly the mesh's
/// current pending set in the desired order — a subset would collide
/// `queue_position` values with unpassed rows and break adjacent Up/Down
/// moves, so subsets and stale ids abort with a domain error asking the
/// caller to refresh. Returns the number of rows repositioned.
pub fn reorder_queued_circuit_runs(mesh_id: i64, ordered_run_ids: &[i64]) -> Result<usize, String> {
    let mut db = crate::db::write_conn();
    reorder_queued_circuit_runs_locked(&mut db, mesh_id, ordered_run_ids)
}

/// Per-test isolated variant of [`reorder_queued_circuit_runs`] (issue #1691).
/// Opens its own transaction on `db`; the `_locked` suffix distinguishes
/// this from the `_inner` helpers that operate inside an externally-managed
/// transaction.
pub(crate) fn reorder_queued_circuit_runs_locked(
    db: &mut Connection,
    mesh_id: i64,
    ordered_run_ids: &[i64],
) -> Result<usize, String> {
    if ordered_run_ids.is_empty() {
        return Ok(0);
    }
    // Reject duplicate ids up front: they would assign two rows the same
    // position even when the set otherwise matches.
    {
        let mut seen = std::collections::HashSet::with_capacity(ordered_run_ids.len());
        for id in ordered_run_ids {
            if !seen.insert(id) {
                return Err("queue order contains a duplicate run id - refresh and retry".to_string());
            }
        }
    }
    let tx = db.transaction().map_err(|e| e.to_string())?;
    // The full pending set for this mesh: the payload must match it exactly
    // or positions would collide with rows the caller did not pass.
    let current: Vec<i64> = {
        let mut stmt = tx.prepare(
            "SELECT id FROM autopilot_circuit_runs \
             WHERE mesh_id = ?1 AND state = 'pending' ORDER BY queue_position, id",
        ).map_err(|e| e.to_string())?;
        let rows = stmt.query_map(params![mesh_id], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        let collected: SqlResult<Vec<i64>> = rows.collect();
        collected.map_err(|e| e.to_string())?
    };
    if current.len() != ordered_run_ids.len() {
        return Err(format!(
            "queue changed while reordering (expected {} pending runs, got {}) - refresh and retry",
            current.len(),
            ordered_run_ids.len()
        ));
    }
    {
        let mut current_sorted = current.clone();
        current_sorted.sort_unstable();
        let mut ordered_sorted = ordered_run_ids.to_vec();
        ordered_sorted.sort_unstable();
        if current_sorted != ordered_sorted {
            return Err("queue changed while reordering (stale or foreign run ids) - refresh and retry".to_string());
        }
    }
    // Anchor on the current minimum so the rewrite never collides with
    // concurrent MAX+1 mints racing this transaction.
    let base: i64 = tx.query_row(
        "SELECT COALESCE(MIN(queue_position), 0) FROM autopilot_circuit_runs \
         WHERE mesh_id = ?1 AND state = 'pending'",
        params![mesh_id],
        |row| row.get(0),
    ).map_err(|e| e.to_string())?;
    for (index, run_id) in ordered_run_ids.iter().enumerate() {
        tx.execute(
            "UPDATE autopilot_circuit_runs SET queue_position = ?2 \
             WHERE id = ?1 AND state = 'pending'",
            params![run_id, base + index as i64],
        ).map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(ordered_run_ids.len())
}
