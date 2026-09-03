//! Autopilot Circuits persistence (spec #1205 / walking skeleton #1206):
//! accessors for the three ledger tables added in schema v34.
//!
//! - `autopilot_circuits` — blueprint rows (one per mesh workflow).
//! - `autopilot_circuit_runs` — one row per execution instance.
//! - `autopilot_circuit_run_steps` — per-circuit-node execution state.
//!
//! Locking discipline (see `db::mod` hard rules): every public fn takes
//! one reader or writer connection exactly once and never calls another public
//! accessor from inside. [`commit_circuit_advance`] is the engine's
//! single atomic commit point: run-state, context, and all step writes
//! of one stepper transition land in ONE transaction so a crash can
//! never leave a half-applied decision.

use super::{params, SqlResult};
use crate::models::{
    AutopilotCircuit, AutopilotCircuitRun, AutopilotCircuitRunStep,
};
use rusqlite::{Connection, OptionalExtension};

// ---------------------------------------------------------------------------
// Circuits — CRUD for the blueprint rows.
// ---------------------------------------------------------------------------

pub fn create_autopilot_circuit(
    mesh_id: i64,
    name: &str,
    description: &str,
    concurrency_limit: i64,
    graph_json: &str,
) -> SqlResult<AutopilotCircuit> {
    let db = super::write_conn();
    // Draft-first (issue #1356): new blueprints start disabled so the
    // GitHub/interval pollers cannot fire while the user is still
    // authoring. Trigger Now still mints a run against a disabled row.
    // `enabled` is written explicitly so existing v34 DBs whose column
    // default is still 1 cannot silently enable a fresh circuit.
    db.execute(
        "INSERT INTO autopilot_circuits \
             (mesh_id, name, description, enabled, concurrency_limit, graph_json) \
         VALUES (?1, ?2, ?3, 0, ?4, ?5)",
        params![mesh_id, name, description, concurrency_limit, graph_json],
    )?;
    get_autopilot_circuit_inner(&db, db.last_insert_rowid())?
        .ok_or_else(|| rusqlite::Error::QueryReturnedNoRows)
}

fn get_autopilot_circuit_inner(
    conn: &Connection,
    id: i64,
) -> SqlResult<Option<AutopilotCircuit>> {
    let mut stmt = conn.prepare(
        "SELECT id, mesh_id, name, description, enabled, concurrency_limit, \
                graph_json, created_at, updated_at \
         FROM autopilot_circuits WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map(params![id], map_circuit_row)?;
    rows.next().transpose()
}

pub fn get_autopilot_circuit(id: i64) -> SqlResult<Option<AutopilotCircuit>> {
    let db = super::read_conn();
    get_autopilot_circuit_inner(&db, id)
}

fn map_circuit_row(row: &rusqlite::Row<'_>) -> SqlResult<AutopilotCircuit> {
    Ok(AutopilotCircuit {
        id: row.get(0)?,
        mesh_id: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        enabled: row.get::<_, i64>(4)? != 0,
        concurrency_limit: row.get(5)?,
        graph_json: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

pub fn list_autopilot_circuits(mesh_id: i64) -> SqlResult<Vec<AutopilotCircuit>> {
    let db = super::read_conn();
    let mut stmt = db.prepare(
        "SELECT id, mesh_id, name, description, enabled, concurrency_limit, \
                graph_json, created_at, updated_at \
         FROM autopilot_circuits WHERE mesh_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map(params![mesh_id], map_circuit_row)?;
    rows.collect()
}

/// Every enabled circuit across ALL meshes — the GitHub poll and
/// interval trigger passes' input (issue #1208). Circuits are not
/// mesh-scoped at the trigger layer: a circuit carries its own mesh_id,
/// so one query serves the whole worker pass.
pub fn list_enabled_circuits() -> SqlResult<Vec<AutopilotCircuit>> {
    let db = super::read_conn();
    let mut stmt = db.prepare(
        "SELECT id, mesh_id, name, description, enabled, concurrency_limit, \
                graph_json, created_at, updated_at \
         FROM autopilot_circuits WHERE enabled = 1 ORDER BY id",
    )?;
    let rows = stmt.query_map([], map_circuit_row)?;
    rows.collect()
}

/// `created_at` of the circuit's newest run — the interval trigger's
/// cooldown anchor (issue #1208). `None` when the circuit never fired;
/// SQLite's datetime strings sort lexicographically, so MAX is correct.
/// Deliberately trigger-kind agnostic: ANY run (manual Trigger Now
/// included) restarts the cadence, because the user just intervened.
pub fn latest_circuit_run_created_at(circuit_id: i64) -> SqlResult<Option<String>> {
    let db = super::read_conn();
    db.query_row(
        "SELECT MAX(created_at) FROM autopilot_circuit_runs WHERE circuit_id = ?1",
        params![circuit_id],
        |row| row.get(0),
    )
}

/// Every `trigger_identity` ever recorded for this circuit — the GitHub
/// poll pass's pre-filter set (issue #1208). The schema's UNIQUE
/// constraint stays the authoritative backstop; this just keeps the pass
/// from rewriting identical rows every cycle.
pub fn list_circuit_trigger_identities(circuit_id: i64) -> SqlResult<Vec<String>> {
    let db = super::read_conn();
    let mut stmt = db.prepare(
        "SELECT trigger_identity FROM autopilot_circuit_runs WHERE circuit_id = ?1",
    )?;
    let rows = stmt.query_map(params![circuit_id], |row| row.get(0))?;
    rows.collect()
}

/// One run plus its step ledger, as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct CircuitRunLedger {
    pub run: AutopilotCircuitRun,
    pub steps: Vec<AutopilotCircuitRunStep>,
}

/// A mesh's circuits WITH every running/paused ledger plus bounded terminal
/// history, in ONE mutex acquisition. Pending runs have their own complete
/// mesh queue; excluding them here prevents the queue and ledger from
/// presenting the same run twice.
pub fn list_circuits_with_recent_runs(
    mesh_id: i64,
    runs_per_circuit: i64,
) -> SqlResult<Vec<(AutopilotCircuit, Vec<CircuitRunLedger>)>> {
    let db = super::read_conn();
    let mut stmt = db.prepare(
        "SELECT id, mesh_id, name, description, enabled, concurrency_limit, \
                graph_json, created_at, updated_at \
         FROM autopilot_circuits WHERE mesh_id = ?1 ORDER BY id",
    )?;
    let circuits: Vec<AutopilotCircuit> =
        stmt.query_map(params![mesh_id], map_circuit_row)?.collect::<SqlResult<_>>()?;
    if circuits.is_empty() {
        return Ok(vec![]);
    }
    let ids: Vec<String> = circuits.iter().map(|c| c.id.to_string()).collect();
    let mut stmt = db.prepare(&format!(
        "SELECT id, circuit_id, mesh_id, trigger_identity, state, \
                context_json, created_at, updated_at \
         FROM autopilot_circuit_runs WHERE circuit_id IN ({}) \
         ORDER BY circuit_id, id DESC",
        ids.join(",")
    ))?;
    let all_runs: Vec<AutopilotCircuitRun> = stmt
        .query_map([], |row| {
            Ok(AutopilotCircuitRun {
                id: row.get(0)?,
                circuit_id: row.get(1)?,
                mesh_id: row.get(2)?,
                trigger_identity: row.get(3)?,
                state: row.get(4)?,
                context_json: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })?
        .collect::<SqlResult<_>>()?;

    let mut out = Vec::with_capacity(circuits.len());
    for circuit in circuits {
        let history_limit = runs_per_circuit.max(0) as usize;
        let mut history_count = 0;
        let runs: Vec<AutopilotCircuitRun> = all_runs
            .iter()
            .filter(|run| run.circuit_id == circuit.id)
            .filter(|run| match run.state.as_str() {
                "running" | "paused" => true,
                "pending" => false,
                _ if history_count < history_limit => {
                    history_count += 1;
                    true
                }
                _ => false,
            })
            .cloned()
            .collect();
        let mut ledgers = Vec::with_capacity(runs.len());
        for run in runs {
            let mut stmt = db.prepare(
                "SELECT id, run_id, node_id, agent_node_id, status, attempt, \
                        outcome, error_message, started_at, completed_at \
                 FROM autopilot_circuit_run_steps WHERE run_id = ?1 ORDER BY id",
            )?;
            let steps = stmt
                .query_map(params![run.id], map_step_row)?
                .collect::<SqlResult<_>>()?;
            ledgers.push(CircuitRunLedger { run, steps });
        }
        out.push((circuit, ledgers));
    }
    Ok(out)
}

pub fn set_autopilot_circuit_enabled(id: i64, enabled: bool) -> SqlResult<()> {
    let db = super::write_conn();
    db.execute(
        "UPDATE autopilot_circuits SET enabled = ?2, updated_at = datetime('now') WHERE id = ?1",
        params![id, i64::from(enabled)],
    )?;
    Ok(())
}

/// Persist a new blueprint AST for one circuit — the canvas editor's
/// save seam (issue #1209). The IPC boundary validates the JSON parses
/// AND passes semantic checks; this accessor only writes. Errors when
/// the row doesn't exist (a stale editor must not silently no-op).
/// `updated_at` stamps so the Probe list shows fresh edit times.
pub fn update_autopilot_circuit_graph(id: i64, graph_json: &str) -> SqlResult<()> {
    let db = super::write_conn();
    let changed = db.execute(
        "UPDATE autopilot_circuits SET graph_json = ?2, updated_at = datetime('now') WHERE id = ?1",
        params![id, graph_json],
    )?;
    if changed == 0 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

/// Delete one circuit and ALL its descendants (runs, steps) in one
/// transaction. Explicit child deletes even though the schema declares
/// `ON DELETE CASCADE`: enforcement depends on the connection's
/// `foreign_keys` pragma, which is on for the bundled SQLite build but
/// off-by-default for a system-libsqlite link — the same defensive rule
/// `delete_mesh` follows for `warm_worktrees`.
pub fn delete_autopilot_circuit(id: i64) -> SqlResult<()> {
    let mut db = super::write_conn();
    let tx = db.transaction()?;
    tx.execute(
        "DELETE FROM autopilot_circuit_run_steps WHERE run_id IN \
             (SELECT id FROM autopilot_circuit_runs WHERE circuit_id = ?1)",
        params![id],
    )?;
    tx.execute("DELETE FROM autopilot_circuit_runs WHERE circuit_id = ?1", params![id])?;
    tx.execute("DELETE FROM autopilot_circuits WHERE id = ?1", params![id])?;
    tx.commit()
}

/// Delete every circuit (and its runs/steps) belonging to a mesh.
/// Called from [`super::delete_mesh`] inside ITS mutex acquisition —
/// `_inner(&Connection)` discipline, no second lock.
pub(crate) fn delete_circuits_for_mesh_inner(conn: &Connection, mesh_id: i64) -> SqlResult<()> {
    conn.execute(
        "DELETE FROM autopilot_circuit_run_steps WHERE run_id IN \
             (SELECT id FROM autopilot_circuit_runs WHERE mesh_id = ?1)",
        params![mesh_id],
    )?;
    conn.execute("DELETE FROM autopilot_circuit_runs WHERE mesh_id = ?1", params![mesh_id])?;
    conn.execute("DELETE FROM autopilot_circuits WHERE mesh_id = ?1", params![mesh_id])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Runs + steps — the execution ledger the circuit worker drives.
// ---------------------------------------------------------------------------

/// Create a fresh `pending` run seeded with its template context
/// (`circuit.*`, pre-populated by the caller; see
/// [`CircuitContext::with_circuit`]).
///
/// Deduplication is enforced by the schema: `UNIQUE (circuit_id,
/// trigger_identity)` means re-reporting the same trigger identity
/// returns the EXISTING run id instead of minting a duplicate (spec:
/// dedupe scoped per-circuit, so two circuits may process the same
/// source independently). Manual identities embed a millisecond
/// timestamp, so Trigger Now effectively always mints a fresh run.
pub fn create_circuit_run(
    circuit_id: i64,
    mesh_id: i64,
    trigger_identity: &str,
    context_json: &str,
) -> SqlResult<i64> {
    let mut db = super::write_conn();
    let tx = db.transaction()?;
    let next_position: i64 = tx.query_row(
        "SELECT COALESCE(MAX(queue_position), 0) + 1 FROM autopilot_circuit_runs WHERE mesh_id = ?1",
        params![mesh_id],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO autopilot_circuit_runs \
             (circuit_id, mesh_id, trigger_identity, context_json, queue_position) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![circuit_id, mesh_id, trigger_identity, context_json, next_position],
    )?;
    let id = tx.query_row(
        "SELECT id FROM autopilot_circuit_runs \
         WHERE circuit_id = ?1 AND trigger_identity = ?2",
        params![circuit_id, trigger_identity],
        |row| row.get(0),
    )?;
    tx.commit()?;
    Ok(id)
}

/// Pending Circuit Runs on one mesh in worker-admission order. The circuit
/// name rides beside the canonical run row for the Probe's global queue.
pub fn list_queued_circuit_runs(
    mesh_id: i64,
) -> SqlResult<Vec<(AutopilotCircuitRun, String)>> {
    let db = super::read_conn();
    let mut stmt = db.prepare(
        "SELECT r.id, r.circuit_id, r.mesh_id, r.trigger_identity, r.state, \
                r.context_json, r.created_at, r.updated_at, c.name \
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
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            },
            row.get(8)?,
        ))
    })?;
    rows.collect()
}

/// Swap one pending run with its adjacent queue neighbour. Returns false at
/// the front/back boundary. Running and terminal rows cannot be reordered.
pub fn move_queued_circuit_run(run_id: i64, toward_front: bool) -> SqlResult<bool> {
    let mut db = super::write_conn();
    let tx = db.transaction()?;
    let (mesh_id, position): (i64, i64) = tx.query_row(
        "SELECT mesh_id, queue_position FROM autopilot_circuit_runs \
         WHERE id = ?1 AND state = 'pending'",
        params![run_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
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

/// Atomically terminalise one active run and return its attached Agent Nodes
/// so the command layer can retire their processes/worktrees after the DB
/// stops the worker from driving the run.
pub fn cancel_circuit_run(run_id: i64) -> SqlResult<Vec<i64>> {
    let mut db = super::write_conn();
    let tx = db.transaction()?;
    let state: String = tx.query_row(
        "SELECT state FROM autopilot_circuit_runs WHERE id = ?1",
        params![run_id],
        |row| row.get(0),
    )?;
    if matches!(state.as_str(), "completed" | "failed") {
        return Ok(Vec::new());
    }
    let agents = {
        let mut stmt = tx.prepare(
            "SELECT DISTINCT agent_node_id FROM autopilot_circuit_run_steps \
             WHERE run_id = ?1 AND agent_node_id IS NOT NULL ORDER BY agent_node_id",
        )?;
        let rows = stmt
            .query_map(params![run_id], |row| row.get(0))?
            .collect::<SqlResult<Vec<i64>>>()?;
        rows
    };
    if state != "cancelled" {
        tx.execute(
            "UPDATE autopilot_circuit_runs SET state = 'cancelled', updated_at = datetime('now') \
             WHERE id = ?1 AND state IN ('pending', 'running', 'paused')",
            params![run_id],
        )?;
    }
    tx.commit()?;
    crate::services::circuit_worker::wake_circuit_worker();
    Ok(agents)
}

/// Runs whose attached agents may still need retiring while a circuit is
/// deleted. Cancelled rows are included so a deletion can be retried after a
/// transient process/worktree cleanup failure without orphaning the agents.
pub fn list_circuit_run_ids_for_cleanup(circuit_id: i64) -> SqlResult<Vec<i64>> {
    let db = super::read_conn();
    let mut stmt = db.prepare(
        "SELECT id FROM autopilot_circuit_runs \
         WHERE circuit_id = ?1 AND state IN ('pending', 'running', 'paused', 'cancelled') \
         ORDER BY id",
    )?;
    let ids = stmt
        .query_map(params![circuit_id], |row| row.get(0))?
        .collect();
    ids
}

/// One active (pending/running) run joined with the fields its worker
/// pass needs from the owning circuit.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveCircuitRun {
    pub run: AutopilotCircuitRun,
    pub circuit_enabled: bool,
    pub circuit_concurrency_limit: i64,
    pub circuit_graph_json: String,
    pub circuit_name: String,
}

pub fn list_active_circuit_runs() -> SqlResult<Vec<ActiveCircuitRun>> {
    let db = super::read_conn();
    let mut stmt = db.prepare(
        "SELECT r.id, r.circuit_id, r.mesh_id, r.trigger_identity, r.state, \
                r.context_json, r.created_at, r.updated_at, \
                c.enabled, c.concurrency_limit, c.graph_json, c.name \
         FROM autopilot_circuit_runs r \
         JOIN autopilot_circuits c ON c.id = r.circuit_id \
         WHERE r.state IN ('pending', 'running', 'paused') \
         ORDER BY CASE WHEN r.state = 'pending' THEN 1 ELSE 0 END, r.queue_position, r.id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ActiveCircuitRun {
            run: AutopilotCircuitRun {
                id: row.get(0)?,
                circuit_id: row.get(1)?,
                mesh_id: row.get(2)?,
                trigger_identity: row.get(3)?,
                state: row.get(4)?,
                context_json: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            },
            circuit_enabled: row.get::<_, i64>(8)? != 0,
            circuit_concurrency_limit: row.get(9)?,
            circuit_graph_json: row.get(10)?,
            circuit_name: row.get(11)?,
        })
    })?;
    rows.collect()
}

pub fn list_circuit_runs(circuit_id: i64, limit: i64) -> SqlResult<Vec<AutopilotCircuitRun>> {
    let db = super::read_conn();
    let mut stmt = db.prepare(
        "SELECT id, circuit_id, mesh_id, trigger_identity, state, \
                context_json, created_at, updated_at \
         FROM autopilot_circuit_runs WHERE circuit_id = ?1 \
         ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![circuit_id, limit], |row| {
        Ok(AutopilotCircuitRun {
            id: row.get(0)?,
            circuit_id: row.get(1)?,
            mesh_id: row.get(2)?,
            trigger_identity: row.get(3)?,
            state: row.get(4)?,
            context_json: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    })?;
    rows.collect()
}

/// Test helper for direct live-state setup. Production pause/resume uses the
/// compare-and-set transition below.
#[cfg(test)]
pub fn set_circuit_run_state(run_id: i64, state: &str) -> SqlResult<()> {
    let db = super::write_conn();
    db.execute(
        "UPDATE autopilot_circuit_runs SET state = ?2, updated_at = datetime('now') \
         WHERE id = ?1 AND state IN ('pending', 'running', 'paused')",
        params![run_id, state],
    )?;
    Ok(())
}

/// Compare-and-set a live run state. Pause/resume commands use this instead
/// of a read followed by an unconditional write, so cancellation cannot win
/// between those operations and then be overwritten by the stale command.
pub fn transition_circuit_run_state(
    run_id: i64,
    expected_state: &str,
    next_state: &str,
) -> SqlResult<bool> {
    let db = super::write_conn();
    let updated = db.execute(
        "UPDATE autopilot_circuit_runs SET state = ?3, updated_at = datetime('now') \
         WHERE id = ?1 AND state = ?2 AND state IN ('pending', 'running', 'paused')",
        params![run_id, expected_state, next_state],
    )?;
    Ok(updated > 0)
}

/// Is this DB string a terminal run state? The three terminal values
/// (`completed` / `failed` / `cancelled`) each release one
/// circuit-run-admission slot exactly once. `paused` is deliberately
/// NOT terminal: paused runs retain their slot (the user-chosen
/// semantics in #1467 planning).
pub fn is_terminal_run_state(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "cancelled")
}

/// One run row by id, or `None` when the id is unknown.
pub fn get_circuit_run(run_id: i64) -> SqlResult<Option<AutopilotCircuitRun>> {
    let db = super::read_conn();
    let mut stmt = db.prepare(
        "SELECT id, circuit_id, mesh_id, trigger_identity, state, \
                context_json, created_at, updated_at \
         FROM autopilot_circuit_runs WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map(params![run_id], |row| {
        Ok(AutopilotCircuitRun {
            id: row.get(0)?,
            circuit_id: row.get(1)?,
            mesh_id: row.get(2)?,
            trigger_identity: row.get(3)?,
            state: row.get(4)?,
            context_json: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    })?;
    rows.next().transpose()
}

pub fn list_circuit_run_steps(run_id: i64) -> SqlResult<Vec<AutopilotCircuitRunStep>> {
    let db = super::read_conn();
    let mut stmt = db.prepare(
        "SELECT id, run_id, node_id, agent_node_id, status, attempt, \
                outcome, error_message, started_at, completed_at \
         FROM autopilot_circuit_run_steps WHERE run_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map(params![run_id], map_step_row)?;
    rows.collect()
}

fn map_step_row(row: &rusqlite::Row<'_>) -> SqlResult<AutopilotCircuitRunStep> {
    Ok(AutopilotCircuitRunStep {
        id: row.get(0)?,
        run_id: row.get(1)?,
        node_id: row.get(2)?,
        agent_node_id: row.get(3)?,
        status: row.get(4)?,
        attempt: row.get(5)?,
        outcome: row.get(6)?,
        error_message: row.get(7)?,
        started_at: row.get(8)?,
        completed_at: row.get(9)?,
    })
}

/// One step mutation inside a [`commit_circuit_advance`] transaction.
/// Mirrors the stepper's `StepWrite`; `outcome: None` means "leave as-is".
#[derive(Debug, Clone, PartialEq)]
pub struct CircuitStepOp {
    pub node_id: String,
    pub status: String,
    pub outcome: Option<Option<String>>,
    pub error: Option<String>,
    pub agent_node_id: Option<i64>,
    /// The step's execution count after this write (#1207 retry
    /// bookkeeping). Written on both insert and update.
    pub attempt: i32,
    /// A retried execution: clear outcome/error, restamp started_at.
    pub fresh_attempt: bool,
}

/// The engine's atomic commit point. Applies an optional run-state and/or
/// context update plus any number of step upserts in ONE transaction on
/// ONE mutex acquisition, so a crash mid-apply can never leave a
/// half-applied stepper decision behind. A `context_json` without a
/// `run_state` still persists (the worker's run-id seeding rides any
/// other write).
///
/// Step rows are upserted by `(run_id, node_id)` (UNIQUE constraint);
/// insert stamps `started_at`, terminal statuses stamp `completed_at`,
/// and a `fresh_attempt` op clears the previous round's outcome/error
/// and restamps `started_at` for the retried execution.
///
/// **Terminal-state single-release idempotency (issue #1467, ADR-0028).**
/// When `run_state` is `Some(completed|failed|cancelled)` (per
/// [`is_terminal_run_state`]), the run-state UPDATE uses an extra
/// `WHERE state IN ('pending', 'running', 'paused')` clause. A row
/// already in a terminal state matches zero rows, so the update is a
/// no-op and **no** capacity is double-decremented. Three failure modes
/// this guards against:
///
/// 1. **Concurrent terminal writes** — the stepper's
///    `finish_run_if_done` flushing `completed` racing an effect-
///    failure path to `failed`. The first commit wins (terminal row
///    matches zero rows for the second, so no overwrite).
/// 2. **Crash after commit, before wake** — the next worker pass
///    retries the wake and re-evaluates pending runs cleanly.
/// 3. **Retry path** — a `RetryLimit` reseting a failed step keeps the
///    run's terminal state untouched (the WHERE filter blocks the
///    reschedule from clobbering it to `running`).
///
/// On a successful terminal-state commit (`rows_updated > 0`), this
/// function wakes the circuit worker so the next pass promotes the
/// next FIFO pending run into the freed slot. Wakes are idempotent
/// (condvar-only).
pub fn commit_circuit_advance(
    run_id: i64,
    run_state: Option<&str>,
    context_json: Option<&str>,
    step_ops: &[CircuitStepOp],
) -> SqlResult<()> {
    let mut db = super::write_conn();
    let tx = db.transaction()?;
    let durable_state = tx
        .query_row(
            "SELECT state FROM autopilot_circuit_runs WHERE id = ?1",
            params![run_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    // A worker may have loaded this run just before cancellation or circuit
    // deletion. The first transaction to acquire the writer lock wins: once
    // terminal (or deleted), stale context/step writes are discarded together
    // and can never resurrect the run.
    if durable_state
        .as_deref()
        .map(is_terminal_run_state)
        .unwrap_or(true)
    {
        return Ok(());
    }
    let mut terminal_woke = false;
    match (run_state, context_json) {
        (Some(state), ctx) => {
            let rows_updated = if is_terminal_run_state(state) {
                tx.execute(
                    "UPDATE autopilot_circuit_runs \
                     SET state = ?2, context_json = COALESCE(?3, context_json), updated_at = datetime('now') \
                     WHERE id = ?1 AND state IN ('pending', 'running', 'paused')",
                    params![run_id, state, ctx],
                )?
            } else {
                tx.execute(
                    "UPDATE autopilot_circuit_runs \
                     SET state = ?2, context_json = COALESCE(?3, context_json), updated_at = datetime('now') \
                     WHERE id = ?1",
                    params![run_id, state, ctx],
                )?
            };
            // Terminal committed AT LEAST ONCE this round — wake so the
            // next pass can re-evaluate pending runs against the freed
            // slot (FIFO promotion). The wake is recorded here; the
            // actual call happens after tx.commit() so a crash mid-tx
            // doesn't wake the worker spuriously.
            terminal_woke = is_terminal_run_state(state) && rows_updated > 0;
        }
        (None, Some(ctx)) => {
            tx.execute(
                "UPDATE autopilot_circuit_runs \
                 SET context_json = ?2, updated_at = datetime('now') \
                 WHERE id = ?1",
                params![run_id, ctx],
            )?;
        }
        (None, None) => {}
    }
    for op in step_ops {
        let outcome_val = op.outcome.clone().flatten();
        let terminal = outcome_val
            .as_deref()
            .map(crate::autopilot::circuit::model::StepOutcome::is_terminal_db_str)
            .unwrap_or(false);
        tx.execute(
            "INSERT INTO autopilot_circuit_run_steps \
                 (run_id, node_id, status, attempt, outcome, error_message, agent_node_id, started_at, completed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'), \
                 CASE WHEN ?10 THEN datetime('now') ELSE NULL END) \
             ON CONFLICT(run_id, node_id) DO UPDATE SET \
                 status = excluded.status, \
                 attempt = excluded.attempt, \
                 outcome = CASE WHEN ?8 THEN NULL \
                     ELSE COALESCE(excluded.outcome, autopilot_circuit_run_steps.outcome) END, \
                 error_message = CASE WHEN ?9 THEN NULL \
                     ELSE COALESCE(excluded.error_message, autopilot_circuit_run_steps.error_message) END, \
                 agent_node_id = COALESCE(excluded.agent_node_id, autopilot_circuit_run_steps.agent_node_id), \
                 started_at = CASE WHEN ?9 THEN datetime('now') \
                     ELSE autopilot_circuit_run_steps.started_at END, \
                 completed_at = CASE WHEN ?10 THEN datetime('now') WHEN ?9 THEN NULL \
                     ELSE autopilot_circuit_run_steps.completed_at END",
            params![
                run_id,
                op.node_id,
                op.status,
                op.attempt,
                outcome_val,
                op.error,
                op.agent_node_id,
                op.fresh_attempt,
                op.fresh_attempt,
                terminal,
            ],
        )?;
    }
    tx.commit()?;
    if terminal_woke {
        crate::services::circuit_worker::wake_circuit_worker();
    }
    Ok(())
}

/// Attach the spawned mesh agent node to its step (called by the seam
/// right after the synchronous stage-1 row creation).
pub fn set_circuit_step_agent_node(
    run_id: i64,
    node_id: &str,
    agent_node_id: i64,
) -> SqlResult<()> {
    let db = super::write_conn();
    let updated = db.execute(
        "UPDATE autopilot_circuit_run_steps SET agent_node_id = ?3 \
         WHERE run_id = ?1 AND node_id = ?2",
        params![run_id, node_id, agent_node_id],
    )?;
    if updated == 0 {
        Err(rusqlite::Error::QueryReturnedNoRows)
    } else {
        Ok(())
    }
}

/// Clear an agent association after a CloseAgentNode effect succeeds. The
/// circuit step remains an audit record, but a retired reviewer must no
/// longer consume mesh/global agent capacity on the run's remaining steps.
pub fn clear_circuit_step_agent_node(run_id: i64, node_id: &str) -> SqlResult<()> {
    let db = super::write_conn();
    db.execute(
        "UPDATE autopilot_circuit_run_steps SET agent_node_id = NULL \
         WHERE run_id = ?1 AND node_id = ?2",
        params![run_id, node_id],
    )?;
    Ok(())
}

/// Circuit ownership metadata for Agent Nodes that still exist. The
/// association lives in the circuit step ledger (not on `agent_nodes`), so
/// the header can identify automated nodes without weakening the satellite-
/// table invariant used by legacy Autopilot.
pub fn list_circuit_agent_ownerships() -> SqlResult<Vec<(i64, i64, i64, String)>> {
    let db = super::read_conn();
    let mut stmt = db.prepare(
        "SELECT DISTINCT s.agent_node_id, r.id, c.id, c.name \
         FROM autopilot_circuit_run_steps s \
         JOIN autopilot_circuit_runs r ON r.id = s.run_id \
         JOIN autopilot_circuits c ON c.id = r.circuit_id \
         JOIN agent_nodes a ON a.id = s.agent_node_id \
         WHERE s.agent_node_id IS NOT NULL AND a.status != 'archived' \
           AND r.id = (SELECT MAX(r2.id) \
                       FROM autopilot_circuit_run_steps s2 \
                       JOIN autopilot_circuit_runs r2 ON r2.id = s2.run_id \
                       WHERE s2.agent_node_id = s.agent_node_id) \
         ORDER BY s.agent_node_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    })?;
    rows.collect()
}

// ---------------------------------------------------------------------------
// Concurrency counters — the inputs to the stepper's capacity snapshot.
// ---------------------------------------------------------------------------

/// Steps currently Running across this circuit's active runs — compared
/// against `autopilot_circuits.concurrency_limit`. Paused runs count:
/// their steps still hold real agents even though the graph is parked.
pub fn count_running_circuit_steps(circuit_id: i64) -> SqlResult<i64> {
    let db = super::read_conn();
    db.query_row(
        "SELECT COUNT(*) FROM autopilot_circuit_run_steps s \
         JOIN autopilot_circuit_runs r ON r.id = s.run_id \
         WHERE r.circuit_id = ?1 AND r.state IN ('running', 'paused') AND s.status = 'running'",
        params![circuit_id],
        |row| row.get(0),
    )
}

/// Distinct piloted agent nodes attached to active runs on this mesh —
/// compared against `meshes.autopilot_concurrency_limit` (the mesh-wide
/// auto-spawned-agent cap). NULL agent ids don't count. Paused runs count
/// (see [`count_running_circuit_steps`]); completed spawn steps remain
/// attached while a later review/feedback step is active so the original
/// implementation agent still consumes capacity.
pub fn count_active_circuit_agent_nodes(mesh_id: i64) -> SqlResult<i64> {
    let db = super::read_conn();
    db.query_row(
        "SELECT COUNT(DISTINCT s.agent_node_id) FROM autopilot_circuit_run_steps s \
         JOIN autopilot_circuit_runs r ON r.id = s.run_id \
         WHERE r.mesh_id = ?1 AND r.state IN ('running', 'paused') \
           AND s.agent_node_id IS NOT NULL",
        params![mesh_id],
        |row| row.get(0),
    )
}

/// Distinct piloted agent nodes across all active circuit runs. The legacy
/// Autopilot pool is app-wide, so the circuit worker combines this count with
/// [`crate::db::count_active_autopilot_nodes_total`] before admitting a new
/// circuit agent.
pub fn count_active_circuit_agent_nodes_total() -> SqlResult<i64> {
    let db = super::read_conn();
    db.query_row(
        "SELECT COUNT(DISTINCT s.agent_node_id) FROM autopilot_circuit_run_steps s \
         JOIN autopilot_circuit_runs r ON r.id = s.run_id \
         WHERE r.state IN ('running', 'paused') \
           AND s.agent_node_id IS NOT NULL",
        [],
        |row| row.get(0),
    )
}

/// **Admitted** circuit runs on this mesh (issue #1467) — the input to
/// the run-admission gate. Counts runs in `running` or `paused` only.
/// Deliberately excludes `pending`: a pending run has NOT yet claimed
/// a circuit-run slot, and the gate exists precisely to decide whether
/// a pending run gets to claim one.
///
/// Why exclude `pending` — counting pending runs toward the cap would
/// self-deadlock: on a mesh with 3 pending runs and cap=2, every
/// pending run's count read sees itself + peers, so 3 < 2 = false and
/// no run ever admits. The fix is the FIFO-faithful shape here: only
/// **admitted** (i.e. `running` or `paused`) runs consume a slot at
/// admission time, and iteration order (`ORDER BY r.id` in the worker's
/// `list_active_circuit_runs`) provides the FIFO promotion — the next
/// un-admitted pending run is admitted the moment the count drops
/// below the cap, with no orphaned admits at the boundary.
///
/// State semantics:
///   * `running` holds capacity (an admitted run's steps may fan out to
///     many agents; the run keeps its one slot regardless of fan-out).
///   * `paused` holds capacity (matches the existing
///     `paused_runs_stay_active_and_counters_count_them` invariant —
///     pause preserves the in-flight agents so resume continues
///     cleanly; the user-chosen semantics in #1467 planning explicitly
///     retain the slot on pause).
///   * `pending` does NOT count (not yet admitted; the gate is the
///     admission decision).
///   * Terminal runs (`completed`/`failed`) do NOT count: a release
///     via [`release_circuit_run`] transitions the row out of this set
///     in a single `UPDATE`, and a second release is a no-op (so we
///     never double-decrement capacity).
///
/// One unit = one admitted run regardless of how many agent nodes the
/// blueprint fans out to. This is the seam that fixes the two-overlap
/// PR-review deadlock (issue #1355 / runs 3+4 of circuit 5) where the
/// agent-node count saturated on the implementation agent and parked
/// the reviewer step in `pending_slot` indefinitely.
pub fn count_active_circuit_runs(mesh_id: i64) -> SqlResult<i64> {
    let db = super::read_conn();
    db.query_row(
        "SELECT COUNT(*) FROM autopilot_circuit_runs \
         WHERE mesh_id = ?1 AND state IN ('running', 'paused')",
        params![mesh_id],
        |row| row.get(0),
    )
}

// ---------------------------------------------------------------------------
// Retention sweep (issue #1236) — bounding an otherwise unbounded ledger.
// ---------------------------------------------------------------------------

/// The set a retention sweep may DELETE outright: terminal runs past the cutoff
/// whose `trigger_identity` is a throwaway timestamp, minus each circuit's
/// newest run.
///
/// Two clauses here are load-bearing and easy to drop by accident:
///
/// * The `LIKE` filter. `interval:<ms>` and `manual:<ms>` embed the fire time,
///   so the identity is never presented twice and the row is pure history once
///   terminal. A GitHub row's `issue:<n>:<label>` identity is STABLE, and the
///   row is the only memory that this circuit already handled that source —
///   `circuit_triggers::mint_unseen_runs` reads it back through
///   [`list_circuit_trigger_identities`] and treats a missing row as "never
///   seen", while `ingest_issues` re-fetches every *open* labelled issue on
///   every poll. Deleting one re-mints a run and re-spawns agents on finished
///   work, so stable identities are compacted instead (see the sweep below).
///   Note the filter is an allow-list: an identity family added later is
///   retained until someone opts it in, which is the safe direction to fail.
/// * The `created_at <` sub-select. The interval cooldown anchors on
///   `MAX(created_at)` over the circuit's runs, and `interval_should_fire`
///   treats `None` as "fire now" — so sweeping a circuit's last surviving row
///   erases its cadence and fires it immediately. Keeping the newest row per
///   circuit preserves the anchor value exactly.
///
/// `?1` is the retention window in days.
const SWEEPABLE_RUNS: &str = "\
    SELECT id FROM autopilot_circuit_runs r \
      WHERE r.state IN ('completed', 'failed') \
        AND r.updated_at < datetime('now', '-' || ?1 || ' days') \
        AND (r.trigger_identity LIKE 'interval:%' OR r.trigger_identity LIKE 'manual:%') \
        AND r.created_at < (SELECT MAX(created_at) FROM autopilot_circuit_runs n \
                             WHERE n.circuit_id = r.circuit_id)";

/// Bound `autopilot_circuit_runs` to a retention window. Returns
/// `(rows_deleted, rows_compacted)`.
///
/// Wrapped in one transaction so a mid-sweep failure can't leave step rows
/// orphaned by a half-applied delete.
pub fn prune_terminal_circuit_runs_older_than(days: i64) -> SqlResult<(usize, usize)> {
    let mut db = super::write_conn();
    let tx = db.transaction()?;
    let counts = prune_terminal_circuit_runs_older_than_inner(&tx, days)?;
    tx.commit()?;
    Ok(counts)
}

/// See [`prune_terminal_circuit_runs_older_than`]. Split out on the
/// `_inner(&Connection)` discipline so callers that already hold the lock (and
/// the tests, against an in-memory DB) reuse one connection.
pub(crate) fn prune_terminal_circuit_runs_older_than_inner(
    conn: &Connection,
    days: i64,
) -> SqlResult<(usize, usize)> {
    // Steps first: the schema declares ON DELETE CASCADE, but enforcement rides
    // on the connection's `foreign_keys` pragma — on for the bundled SQLite,
    // off by default for a system-libsqlite link. The same defensive ordering
    // `delete_autopilot_circuit` documents. Without it the sweep would trade a
    // run leak for a step leak.
    conn.execute(
        &format!("DELETE FROM autopilot_circuit_run_steps WHERE run_id IN ({SWEEPABLE_RUNS})"),
        params![days],
    )?;
    let deleted = conn.execute(
        &format!("DELETE FROM autopilot_circuit_runs WHERE id IN ({SWEEPABLE_RUNS})"),
        params![days],
    )?;

    // Stable-identity rows stay forever as dedupe tombstones, but the issue/PR
    // body they carry is the actual bulk (a row is ~60 bytes empty, and bodies
    // run to kilobytes). Emptying `context_json` keeps the once-only guarantee
    // structural while dropping the weight. Terminal-only: an active run's
    // context still feeds the stepper's template rendering. `updated_at` is
    // deliberately not bumped — compaction is not a state change — and the
    // `<> '{}'` guard keeps a steady-state sweep silent.
    let compacted = conn.execute(
        "UPDATE autopilot_circuit_runs SET context_json = '{}' \
          WHERE state IN ('completed', 'failed') \
            AND updated_at < datetime('now', '-' || ?1 || ' days') \
            AND trigger_identity NOT LIKE 'interval:%' \
            AND trigger_identity NOT LIKE 'manual:%' \
            AND context_json <> '{}'",
        params![days],
    )?;

    Ok((deleted, compacted))
}
