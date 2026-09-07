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

/// Atomically claim a source agent and create its review run. The source id is
/// stored relationally on the run; the context copy remains for graph
/// template expansion and backwards-compatible diagnostics.
pub fn create_node_circuit_run(node_id: i64, selected_circuit_id: Option<i64>, max_rounds: i32) -> Result<i64, String> {
    let mut db = super::write_conn();
    let tx = db.transaction().map_err(|e| e.to_string())?;
    let node = super::get_agent_node_by_id_inner(&tx, node_id).map_err(|e| e.to_string())?;
    let existing: Option<i64> = tx.query_row(
        "SELECT id FROM autopilot_circuit_runs
         WHERE source_agent_node_id = ?1 AND state IN ('pending','running','paused')
         LIMIT 1",
        params![node_id], |row| row.get(0),
    ).optional().map_err(|e| e.to_string())?;
    if let Some(id) = existing { return Ok(id); }
    let owned: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM autopilot_circuit_run_steps s \
         JOIN autopilot_circuit_runs r ON r.id = s.run_id \
         WHERE s.agent_node_id = ?1 AND r.state IN ('pending','running','paused')) \
         OR EXISTS(SELECT 1 FROM autopilot_runs WHERE node_id = ?1 \
         AND state IN ('implementing','finishing','suffix_pending'))",
        params![node_id], |row| row.get(0),
    ).map_err(|e| e.to_string())?;
    if owned { return Err("This agent is already controlled by an active Autopilot run.".into()); }
    if !matches!(node.status, crate::models::SessionStatus::Running | crate::models::SessionStatus::AwaitingInput | crate::models::SessionStatus::Completed | crate::models::SessionStatus::Ready) {
        return Err("Resume the agent before starting a review.".into());
    }
    let review_config: Option<(Option<String>, Option<String>)> = if selected_circuit_id.is_none() {
        Some(tx.query_row(
            "SELECT NULLIF(TRIM(model), ''), NULLIF(TRIM(effort), '') FROM meshes WHERE id = ?1",
            params![node.mesh_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let (circuit_id, name) = if let Some(id) = selected_circuit_id {
        let circuit = get_autopilot_circuit_inner(&tx, id).map_err(|e| e.to_string())?
            .ok_or("Circuit no longer exists")?;
        let graph = crate::autopilot::circuit::model::CircuitGraph::from_json(&circuit.graph_json)?;
        graph.validate()?;
        if circuit.mesh_id != node.mesh_id || graph.roots().is_empty()
            || graph.roots().iter().any(|n| !matches!(n.kind, crate::autopilot::circuit::model::CircuitNodeKind::Manual)) {
            return Err("Select a manual Circuit from this agent's Mesh.".into());
        }
        (id, circuit.name)
    } else {
        let (review_model, review_effort) = review_config.clone().unwrap_or_default();
        let graph = crate::autopilot::circuit::model::CircuitGraph::agent_review(
            &node.provider,
            review_model.clone(),
            review_effort.clone(),
            max_rounds,
        );
        graph.validate()?;
        let name = format!("Review agent {}", node_id);
        let description = "Review an existing agent and return findings until approved";
        let existing: Option<(i64, String)> = tx.query_row(
            "SELECT id, name FROM autopilot_circuits
             WHERE mesh_id = ?1 AND is_preset = 1
             ORDER BY id LIMIT 1",
            params![node.mesh_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional().map_err(|e| e.to_string())?;
        if let Some((id, existing_name)) = existing {
            (id, existing_name)
        } else {
            tx.execute(
                "INSERT INTO autopilot_circuits
                 (mesh_id, name, description, enabled, concurrency_limit, graph_json, is_preset)
                 VALUES (?1, ?2, ?3, 0, 2, ?4, 1)",
                params![node.mesh_id, name, description, graph.to_json()?],
            ).map_err(|e| e.to_string())?;
            (tx.last_insert_rowid(), name)
        }
    };
    let mut context = crate::autopilot::circuit::context::CircuitContext::new();
    context.with_circuit(circuit_id, &name, node.mesh_id);
    context.set("source.agent_id", node_id.to_string());
    context.set("source.name", &node.name);
    context.set("source.path", crate::env::node_working_path(&node).spawn_path);
    let base_ref: String = tx.query_row("SELECT base_ref FROM meshes WHERE id = ?1", params![node.mesh_id], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    context.set("source.base_ref", base_ref);
    if selected_circuit_id.is_none() {
        context.set("source.review_preset", "1");
        context.set("source.provider", &node.provider);
        context.set(
            "source.model",
            review_config
                .as_ref()
                .and_then(|(model, _)| model.as_deref())
                .unwrap_or(""),
        );
        context.set(
            "source.effort",
            review_config
                .as_ref()
                .and_then(|(_, effort)| effort.as_deref())
                .unwrap_or(""),
        );
    }
    context.set("retry.attempt", "1");
    context.set("retry.max_retries", max_rounds.to_string());
    tx.execute(
        "INSERT INTO autopilot_circuit_runs
         (circuit_id, mesh_id, trigger_identity, context_json, queue_position, source_agent_node_id)
         VALUES (?1, ?2, ?3, ?4,
                 (SELECT COALESCE(MAX(queue_position),0)+1 FROM autopilot_circuit_runs WHERE mesh_id=?2),
                 ?5)",
        params![circuit_id, node.mesh_id, format!("manual:agent:{node_id}:{}", uuid::Uuid::new_v4()), context.to_json()?, node_id],
    ).map_err(|e| e.to_string())?;
    let run_id = tx.last_insert_rowid();
    tx.commit().map_err(|e| e.to_string())?;
    Ok(run_id)
}

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
                graph_json, created_at, updated_at, is_preset \
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
        is_preset: row.get::<_, i64>(9)? != 0,
    })
}

pub fn list_autopilot_circuits(mesh_id: i64) -> SqlResult<Vec<AutopilotCircuit>> {
    let db = super::read_conn();
    let mut stmt = db.prepare(
        "SELECT id, mesh_id, name, description, enabled, concurrency_limit, \
                graph_json, created_at, updated_at, is_preset \
         FROM autopilot_circuits WHERE mesh_id = ?1 AND is_preset = 0 ORDER BY id",
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
                graph_json, created_at, updated_at, is_preset \
         FROM autopilot_circuits WHERE enabled = 1 AND is_preset = 0 ORDER BY id",
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

/// A mesh's user-authored circuits plus any built-in preset with run history
/// WITH every running/paused ledger plus bounded terminal history, in ONE
/// mutex acquisition. Presets are execution-only rows (the UI hides their
/// blueprint controls), but their terminal ledgers remain visible so a user
/// can inspect blocked or exhausted review results. Pending runs have their
/// own complete mesh queue; excluding them here prevents the queue and ledger
/// from presenting the same run twice.
pub fn list_circuits_with_recent_runs(
    mesh_id: i64,
    runs_per_circuit: i64,
) -> SqlResult<Vec<(AutopilotCircuit, Vec<CircuitRunLedger>)>> {
    let db = super::read_conn();
    list_circuits_with_recent_runs_inner(&db, mesh_id, runs_per_circuit)
}

pub(crate) fn list_circuits_with_recent_runs_inner(
    db: &Connection,
    mesh_id: i64,
    runs_per_circuit: i64,
) -> SqlResult<Vec<(AutopilotCircuit, Vec<CircuitRunLedger>)>> {
    let mut stmt = db.prepare(
        "SELECT id, mesh_id, name, description, enabled, concurrency_limit, \
                graph_json, created_at, updated_at, is_preset \
         FROM autopilot_circuits
         WHERE mesh_id = ?1
           AND (is_preset = 0 OR EXISTS (
             SELECT 1 FROM autopilot_circuit_runs r
             WHERE r.circuit_id = autopilot_circuits.id
           ))
         ORDER BY id",
    )?;
    let circuits: Vec<AutopilotCircuit> =
        stmt.query_map(params![mesh_id], map_circuit_row)?.collect::<SqlResult<_>>()?;
    if circuits.is_empty() {
        return Ok(vec![]);
    }
    let ids: Vec<String> = circuits.iter().map(|c| c.id.to_string()).collect();
    // Keep both ordinary history and recovery history bounded in SQLite. A
    // failed run or a node with an outstanding cleanup request gets a small
    // recovery window even when it is older than the ordinary history cap;
    // presets are not an excuse to stream their entire lifetime ledger.
    const RECOVERY_RUNS_PER_CIRCUIT: i64 = 50;
    let mut stmt = db.prepare(&format!(
        "WITH terminal AS ( \
             SELECT r.id, r.circuit_id, r.mesh_id, r.trigger_identity, r.state, \
                    r.context_json, r.source_agent_node_id, r.created_at, r.updated_at, \
                    c.is_preset, c.graph_json, ROW_NUMBER() OVER (PARTITION BY r.circuit_id ORDER BY r.id DESC) AS history_rank \
             FROM autopilot_circuit_runs r JOIN autopilot_circuits c ON c.id=r.circuit_id \
             WHERE r.circuit_id IN ({}) \
               AND r.state NOT IN ('pending', 'running', 'paused') \
         ), attention_candidates AS ( \
             SELECT DISTINCT terminal.id, terminal.circuit_id, terminal.mesh_id, terminal.trigger_identity, terminal.state, \
                    terminal.context_json, terminal.source_agent_node_id, terminal.created_at, terminal.updated_at \
             FROM terminal \
             LEFT JOIN autopilot_circuit_run_steps s ON s.run_id = terminal.id \
             LEFT JOIN agent_node_lifecycle_leases l ON l.node_id = s.agent_node_id \
             LEFT JOIN json_each(CASE WHEN json_valid(terminal.graph_json) THEN terminal.graph_json ELSE '{{}}' END, '$.nodes') review_node ON json_extract(review_node.value, '$.type.type') = 'review_verdict' \
             LEFT JOIN autopilot_circuit_run_steps review_step ON review_step.run_id = terminal.id \
                 AND review_step.node_id = json_extract(review_node.value, '$.id') \
             WHERE terminal.state = 'failed' OR l.cleanup_requested = 1 \
                OR (review_step.id IS NOT NULL AND COALESCE(review_step.outcome, '') <> 'completed') \
         ), attention AS ( \
             SELECT attention_candidates.*, ROW_NUMBER() OVER (PARTITION BY circuit_id ORDER BY id DESC) AS attention_rank \
             FROM attention_candidates \
         ), visible AS ( \
             SELECT id, circuit_id, mesh_id, trigger_identity, state, \
                    context_json, source_agent_node_id, created_at, updated_at \
             FROM autopilot_circuit_runs \
             WHERE circuit_id IN ({}) AND state IN ('running', 'paused') \
             UNION ALL \
             SELECT id, circuit_id, mesh_id, trigger_identity, state, \
                    context_json, source_agent_node_id, created_at, updated_at \
             FROM terminal WHERE history_rank <= ?1 \
             UNION \
             SELECT id, circuit_id, mesh_id, trigger_identity, state, \
                    context_json, source_agent_node_id, created_at, updated_at \
             FROM attention WHERE attention_rank <= ?2 \
         ) \
         SELECT id, circuit_id, mesh_id, trigger_identity, state, \
                context_json, source_agent_node_id, created_at, updated_at \
         FROM visible ORDER BY circuit_id, id DESC",
        ids.join(","),
        ids.join(",")
    ))?;
    let visible_runs: Vec<AutopilotCircuitRun> = stmt
        .query_map(params![runs_per_circuit.max(0), RECOVERY_RUNS_PER_CIRCUIT], |row| {
            Ok(AutopilotCircuitRun {
                id: row.get(0)?,
                circuit_id: row.get(1)?,
                mesh_id: row.get(2)?,
                trigger_identity: row.get(3)?,
                state: row.get(4)?,
                context_json: row.get(5)?,
                source_agent_node_id: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })?
        .collect::<SqlResult<_>>()?;

    let mut runs_by_circuit: std::collections::HashMap<i64, Vec<AutopilotCircuitRun>> =
        std::collections::HashMap::new();
    for run in visible_runs {
        runs_by_circuit.entry(run.circuit_id).or_default().push(run);
    }

    let run_ids: Vec<i64> = runs_by_circuit.values().flatten().map(|run| run.id).collect();
    let mut steps_by_run: std::collections::HashMap<i64, Vec<AutopilotCircuitRunStep>> = std::collections::HashMap::new();
    if !run_ids.is_empty() {
        let placeholders = std::iter::repeat("?").take(run_ids.len()).collect::<Vec<_>>().join(",");
        let mut step_stmt = db.prepare(&format!(
            "SELECT id, run_id, node_id, agent_node_id, status, attempt, \
                    outcome, error_message, started_at, completed_at \
             FROM autopilot_circuit_run_steps WHERE run_id IN ({placeholders}) ORDER BY run_id, id"
        ))?;
        let step_rows = step_stmt.query_map(rusqlite::params_from_iter(run_ids.iter()), map_step_row)?;
        for step in step_rows {
            let step = step?;
            steps_by_run.entry(step.run_id).or_default().push(step);
        }
    }

    let mut out = Vec::with_capacity(circuits.len());
    for circuit in circuits {
        let runs = runs_by_circuit.remove(&circuit.id).unwrap_or_default();
        let ledgers = runs.into_iter().map(|run| CircuitRunLedger {
            steps: steps_by_run.remove(&run.id).unwrap_or_default(),
            run,
        }).collect();
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
    tx.execute(
        "DELETE FROM autopilot_circuit_run_agent_leases WHERE run_id IN \
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
    conn.execute(
        "DELETE FROM autopilot_circuit_run_agent_leases WHERE run_id IN \
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

/// Hydrate the Circuits Probe's ledger and queue from one read connection so
/// both views observe the same database snapshot.
pub fn list_circuit_probe(
    mesh_id: i64,
    runs_per_circuit: i64,
) -> SqlResult<(
    Vec<(AutopilotCircuit, Vec<CircuitRunLedger>)>,
    Vec<(AutopilotCircuitRun, String)>,
)> {
    let db = super::read_conn();
    let circuits = list_circuits_with_recent_runs_inner(&db, mesh_id, runs_per_circuit)?;
    let queue = list_queued_circuit_runs_inner(&db, mesh_id)?;
    Ok((circuits, queue))
}

/// Swap one pending run with its adjacent queue neighbour. Returns false at
/// the front/back boundary. Running and terminal rows cannot be reordered.
pub fn move_queued_circuit_run(run_id: i64, toward_front: bool) -> SqlResult<bool> {
    let mut db = super::write_conn();
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
    let mut db = super::write_conn();
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
/// / keyboard reorder seam). Only pending rows on the same mesh are touched;
/// running/terminal ids in the payload are ignored, unknown ids abort the
/// write. Returns the number of rows repositioned.
pub fn reorder_queued_circuit_runs(mesh_id: i64, ordered_run_ids: &[i64]) -> SqlResult<usize> {
    if ordered_run_ids.is_empty() {
        return Ok(0);
    }
    let mut db = super::write_conn();
    let tx = db.transaction()?;
    // Validate all ids belong to this mesh queue before writing, so a stale
    // drag payload never half-applies.
    let placeholders = std::iter::repeat("?").take(ordered_run_ids.len()).collect::<Vec<_>>().join(",");
    let valid: Vec<i64> = {
        let mut stmt = tx.prepare(&format!(
            "SELECT id FROM autopilot_circuit_runs \
             WHERE mesh_id = ? AND state = 'pending' AND id IN ({placeholders})"
        ))?;
        let mut params: Vec<&dyn rusqlite::ToSql> = vec![&mesh_id];
        for id in ordered_run_ids {
            params.push(id);
        }
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| row.get(0))?;
        let collected = rows.collect::<SqlResult<Vec<i64>>>()?;
        collected
    };
    if valid.len() != ordered_run_ids.len() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    // Anchor on the current minimum so the rewrite never collides with
    // concurrent MAX+1 mints racing this transaction.
    let base: i64 = tx.query_row(
        "SELECT COALESCE(MIN(queue_position), 0) FROM autopilot_circuit_runs \
         WHERE mesh_id = ?1 AND state = 'pending'",
        params![mesh_id],
        |row| row.get(0),
    )?;
    for (index, run_id) in ordered_run_ids.iter().enumerate() {
        tx.execute(
            "UPDATE autopilot_circuit_runs SET queue_position = ?2 \
             WHERE id = ?1 AND state = 'pending'",
            params![run_id, base + index as i64],
        )?;
    }
    tx.commit()?;
    Ok(ordered_run_ids.len())
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
    if matches!(state.as_str(), "pending" | "running" | "paused") {
        tx.execute(
            "UPDATE autopilot_circuit_runs SET state = 'cancelled', context_json = json_remove(context_json, '$.\"cleanup.pending\"'), updated_at = datetime('now') \
             WHERE id = ?1 AND state IN ('pending', 'running', 'paused')",
            params![run_id],
        )?;
        tx.execute(
            "INSERT INTO agent_node_lifecycle_leases (node_id, cleanup_requested)
             SELECT DISTINCT s.agent_node_id, 1
             FROM autopilot_circuit_run_steps s
             JOIN agent_nodes a ON a.id = s.agent_node_id
             WHERE s.run_id = ?1 AND s.agent_node_id IS NOT NULL
               AND s.agent_node_id IS NOT (SELECT source_agent_node_id FROM autopilot_circuit_runs WHERE id = ?1)
             ON CONFLICT(node_id) DO UPDATE SET
               cleanup_requested = 1, retired = 0, updated_at = unixepoch()",
            params![run_id],
        )?;
    }
    // Terminalise the ledger in the same transaction as the run state. A
    // stale worker commit is rejected after this point, so incomplete steps
    // must not remain frozen as `running`/`queued` in the audit UI.
    if !matches!(state.as_str(), "completed" | "failed") {
        tx.execute(
            "UPDATE autopilot_circuit_run_steps \
             SET status = 'cancelled', outcome = 'cancelled', completed_at = datetime('now') \
             WHERE run_id = ?1 AND status IN ('pending_slot', 'queued', 'running', 'blocked')",
            params![run_id],
        )?;
    }
    tx.execute(
        "DELETE FROM autopilot_circuit_run_agent_leases WHERE run_id = ?1",
        params![run_id],
    )?;
    tx.commit()?;
    Ok(agents)
}

/// Runs whose attached agents may still need retiring while a circuit is
/// deleted. Terminal rows are included so a deletion can be retried after a
/// transient process/worktree cleanup failure without orphaning retained
/// agents from a completed or failed run.
pub fn list_circuit_run_ids_for_cleanup(circuit_id: i64) -> SqlResult<Vec<i64>> {
    let db = super::read_conn();
    let mut stmt = db.prepare(
        "SELECT id FROM autopilot_circuit_runs \
         WHERE circuit_id = ?1 AND state IN ('pending', 'running', 'paused', 'completed', 'failed', 'cancelled') \
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
                r.context_json, r.source_agent_node_id, r.created_at, r.updated_at, \
                c.enabled, c.concurrency_limit, c.graph_json, c.name \
         FROM autopilot_circuit_runs r \
         JOIN autopilot_circuits c ON c.id = r.circuit_id \
         WHERE r.state IN ('pending', 'running', 'paused') \
         ORDER BY r.mesh_id, CASE WHEN r.state = 'pending' THEN 1 ELSE 0 END, r.queue_position, r.id",
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
                source_agent_node_id: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            },
            circuit_enabled: row.get::<_, i64>(9)? != 0,
            circuit_concurrency_limit: row.get(10)?,
            circuit_graph_json: row.get(11)?,
            circuit_name: row.get(12)?,
        })
    })?;
    rows.collect()
}

pub fn list_circuit_runs(circuit_id: i64, limit: i64) -> SqlResult<Vec<AutopilotCircuitRun>> {
    let db = super::read_conn();
    let mut stmt = db.prepare(
        "SELECT id, circuit_id, mesh_id, trigger_identity, state, \
                context_json, source_agent_node_id, created_at, updated_at \
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
            source_agent_node_id: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
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
                context_json, source_agent_node_id, created_at, updated_at \
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
            source_agent_node_id: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
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
/// Mirrors the stepper's `StepWrite`; for both `outcome` and `error`, outer
/// `None` means "leave as-is" while `Some(None)` explicitly clears the stored
/// value.
#[derive(Debug, Clone, PartialEq)]
pub struct CircuitStepOp {
    pub node_id: String,
    pub status: String,
    pub outcome: Option<Option<String>>,
    pub error: Option<Option<String>>,
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
        // A crash or an older worker may have left a lease row behind after
        // the run became terminal. It is no longer counted for admission, but
        // remove the durable record while this writer transaction is already
        // holding the run lock.
        tx.execute(
            "DELETE FROM autopilot_circuit_run_agent_leases WHERE run_id = ?1",
            params![run_id],
        )?;
        tx.commit()?;
        return Ok(());
    }
    let prior_context_json = if run_state.map(is_terminal_run_state).unwrap_or(false) {
        tx.query_row(
            "SELECT context_json FROM autopilot_circuit_runs WHERE id = ?1",
            params![run_id],
            |row| row.get::<_, String>(0),
        ).optional()?
    } else {
        None
    };
    let mut terminal_woke = false;
    match (run_state, context_json) {
        (Some(state), ctx) => {
            let rows_updated = if is_terminal_run_state(state) {
                tx.execute(
                    "UPDATE autopilot_circuit_runs \
                     SET state = ?2, context_json = json_remove(COALESCE(?3, context_json), '$.\"cleanup.pending\"'), updated_at = datetime('now') \
                     WHERE id = ?1 AND state IN ('pending', 'running', 'paused')",
                    params![run_id, state, ctx],
                )?
            } else {
                tx.execute(
                    "UPDATE autopilot_circuit_runs \
                     SET state = ?2, context_json = json_remove(COALESCE(?3, context_json), '$.\"cleanup.pending\"'), updated_at = datetime('now') \
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
                 SET context_json = json_remove(?2, '$.\"cleanup.pending\"'), updated_at = datetime('now') \
                 WHERE id = ?1",
                params![run_id, ctx],
            )?;
        }
        (None, None) => {}
    }
    for op in step_ops {
        let outcome_val = op.outcome.clone().flatten();
        let outcome_changed = op.outcome.is_some();
        let error_val = op.error.clone().flatten();
        let error_changed = op.error.is_some();
        let terminal = outcome_val
            .as_deref()
            .map(crate::autopilot::circuit::model::StepOutcome::is_terminal_db_str)
            .unwrap_or(false);
        tx.execute(
            "INSERT INTO autopilot_circuit_run_steps \
                 (run_id, node_id, status, attempt, outcome, error_message, agent_node_id, started_at, completed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'), \
                  CASE WHEN ?12 THEN datetime('now') ELSE NULL END) \
             ON CONFLICT(run_id, node_id) DO UPDATE SET \
                 status = excluded.status, \
                 attempt = excluded.attempt, \
                  outcome = CASE WHEN ?8 THEN NULL \
                      WHEN ?9 THEN excluded.outcome \
                      ELSE autopilot_circuit_run_steps.outcome END, \
                   error_message = CASE WHEN ?8 THEN NULL \
                      WHEN ?10 THEN excluded.error_message \
                      ELSE autopilot_circuit_run_steps.error_message END, \
                  agent_node_id = COALESCE(excluded.agent_node_id, autopilot_circuit_run_steps.agent_node_id), \
                  started_at = CASE WHEN ?11 THEN datetime('now') \
                      ELSE autopilot_circuit_run_steps.started_at END, \
                   completed_at = CASE WHEN ?12 THEN datetime('now') WHEN ?11 THEN NULL \
                      ELSE autopilot_circuit_run_steps.completed_at END",
            params![
                run_id,
                op.node_id,
                op.status,
                op.attempt,
                outcome_val,
                error_val,
                op.agent_node_id,
                op.fresh_attempt,
                outcome_changed,
                error_changed,
                op.fresh_attempt,
                terminal,
            ],
        )?;
    }
    if terminal_woke {
        // Cleanup ownership is node-scoped and survives run retention. The
        // legacy context marker is consumed as an input only; it is never
        // persisted back into the historical ledger.
        let state = run_state.unwrap_or_default();
        let cleanup_requested = state == "failed" || state == "cancelled" || context_json.or(prior_context_json.as_deref())
            .and_then(|ctx| serde_json::from_str::<serde_json::Value>(ctx).ok())
            .and_then(|ctx| ctx.get("cleanup.pending").and_then(|v| v.as_str()).map(|v| v == "1"))
            .unwrap_or(false);
        if cleanup_requested {
            tx.execute(
                "INSERT INTO agent_node_lifecycle_leases (node_id, cleanup_requested)
                 SELECT DISTINCT s.agent_node_id, 1
                 FROM autopilot_circuit_run_steps s
                 JOIN autopilot_circuit_runs r ON r.id = s.run_id
                 JOIN agent_nodes a ON a.id = s.agent_node_id
                 WHERE s.run_id = ?1 AND s.agent_node_id IS NOT NULL
                   AND s.agent_node_id IS NOT r.source_agent_node_id
                 ON CONFLICT(node_id) DO UPDATE SET
                   cleanup_requested = 1, retired = 0, updated_at = unixepoch()",
                params![run_id],
            )?;
        }
        tx.execute(
            "DELETE FROM autopilot_circuit_run_agent_leases WHERE run_id = ?1",
            params![run_id],
        )?;
    }
    tx.commit()?;
    if terminal_woke {
        crate::services::circuit_worker::wake_circuit_worker();
    }
    Ok(())
}

/// Attach a spawned agent and its optional presentation parent to a step.
/// Parentage is supplied by the circuit domain/worker at the point the
/// relationship is known; the persistence layer stores it without knowing
/// anything about blueprint names or step roles.
pub fn set_circuit_step_agent_node_with_parent(
    run_id: i64,
    node_id: &str,
    agent_node_id: i64,
    parent_agent_node_id: Option<i64>,
) -> SqlResult<bool> {
    let db = super::write_conn();
    let updated = db.execute(
        "UPDATE autopilot_circuit_run_steps SET agent_node_id = ?3, parent_agent_node_id = ?4 \
         WHERE run_id = ?1 AND node_id = ?2",
        params![run_id, node_id, agent_node_id, parent_agent_node_id],
    )?;
    Ok(updated > 0)
}

/// Reserve the number of agent slots a circuit blueprint may need while its
/// run is admitted. The lease is durable and keyed by run, so admission is
/// not inferred from whichever child agent happens to be attached today.
/// Repeated calls are idempotent and may repair a pre-lease active run after
/// an upgrade.
pub fn reserve_circuit_agent_slots(run_id: i64, slots: i64) -> SqlResult<bool> {
    if slots <= 0 {
        return Ok(true);
    }
    let mut db = super::write_conn();
    let tx = db.transaction()?;
    let live: Option<String> = tx
        .query_row(
            "SELECT state FROM autopilot_circuit_runs WHERE id = ?1",
            params![run_id],
            |row| row.get(0),
        )
        .optional()?;
    if !matches!(live.as_deref(), Some("pending" | "running" | "paused")) {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO autopilot_circuit_run_agent_leases (run_id, slots) VALUES (?1, ?2) \
         ON CONFLICT(run_id) DO UPDATE SET slots = MAX(slots, excluded.slots)",
        params![run_id, slots],
    )?;
    tx.commit()?;
    Ok(true)
}

pub fn circuit_agent_slots_reserved(run_id: i64) -> SqlResult<i64> {
    let db = super::read_conn();
    db.query_row(
        "SELECT slots FROM autopilot_circuit_run_agent_leases WHERE run_id = ?1",
        params![run_id],
        |row| row.get(0),
    )
    .optional()
    .map(|v| v.unwrap_or(0))
}

pub fn count_reserved_circuit_agent_slots_total() -> SqlResult<i64> {
    let db = super::read_conn();
    db.query_row(
        "SELECT COALESCE(SUM(l.slots), 0) \
         FROM autopilot_circuit_run_agent_leases l \
         JOIN autopilot_circuit_runs r ON r.id = l.run_id \
         WHERE r.state IN ('pending', 'running', 'paused')",
        [],
        |row| row.get(0),
    )
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

/// Clear an association by the newly-created Agent Node id. This is the
/// abort seam for an async spawn that loses a cancellation/delete race after
/// the worker has attached the node but before the task has launched it.
pub fn clear_circuit_step_agent_node_by_agent_id(run_id: i64, agent_node_id: i64) -> SqlResult<()> {
    let db = super::write_conn();
    db.execute(
        "UPDATE autopilot_circuit_run_steps SET agent_node_id = NULL \
         WHERE run_id = ?1 AND agent_node_id = ?2",
        params![run_id, agent_node_id],
    )?;
    Ok(())
}

/// Circuit ownership metadata for Agent Nodes that still exist. The
/// association lives in the circuit step ledger (not on `agent_nodes`), so
/// the header can identify automated nodes without weakening the satellite-
/// table invariant used by legacy Autopilot.
type AgentOwnershipRow = (i64, i64, i64, String, String, Option<i64>);

pub fn list_circuit_agent_ownerships() -> SqlResult<Vec<AgentOwnershipRow>> {
    let db = super::read_conn();
    list_circuit_agent_ownerships_inner(&db)
}

fn list_circuit_agent_ownerships_inner(db: &Connection) -> SqlResult<Vec<AgentOwnershipRow>> {
    let mut stmt = db.prepare(
        "SELECT DISTINCT s.agent_node_id, r.id, c.id, c.name, r.state, s.parent_agent_node_id \
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
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
    })?;
    let mut ownerships: Vec<AgentOwnershipRow> = rows.collect::<SqlResult<_>>()?;
    let mut sources = db.prepare(
        // Keep source overrides deterministic when an agent has more than one
        // circuit run. The latest run remains visible after it completes so
        // the source node can show the compact Done state until a newer run
        // takes ownership.
        "SELECT a.id, r.id, c.id, c.name, r.state FROM autopilot_circuit_runs r \
         JOIN autopilot_circuits c ON c.id = r.circuit_id \
         JOIN agent_nodes a ON a.id = r.source_agent_node_id \
         WHERE a.status != 'archived' \
           AND r.id = (SELECT MAX(r2.id) FROM autopilot_circuit_runs r2 \
                       WHERE r2.source_agent_node_id = a.id) \
         ORDER BY a.id, r.id",
    )?;
    for row in sources.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)))? {
        let (node, run, circuit, name, state) = row?;
        if ownerships.iter().any(|owned| owned.0 == node && owned.1 > run) {
            // A source run is only an override when it is at least as new as
            // the node's latest step ownership. Historical source runs must
            // never hide a newer active step run.
            continue;
        }
        let source = (node, run, circuit, name, state, None);
        ownerships.retain(|owned| owned.0 != source.0);
        ownerships.push(source);
    }
    ownerships.sort_by_key(|row| row.0);
    Ok(ownerships)
}

#[cfg(test)]
mod activity_ownership_tests {
    use super::*;

    #[test]
    fn reviewer_activity_uses_run_relationships_and_survives_restart() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes (id, name, path) VALUES (1, 'activities', '/repo');
          INSERT INTO agent_nodes (id, mesh_id, name, path, branch, env, provider, status)
            VALUES (1, 1, 'implementation', '/repo', 'main', 'windows', 'terminal', 'ready'),
                   (2, 1, 'review', '/review', 'main', 'windows', 'terminal', 'running'),
                   (3, 1, 'unrelated', '/other', 'main', 'windows', 'terminal', 'running');
          INSERT INTO autopilot_circuits (id, mesh_id, name, graph_json)
            VALUES (1, 1, 'review', '{\"blueprint\":\"issue_driven_autopilot_review\"}');
          INSERT INTO autopilot_circuit_runs (id, circuit_id, mesh_id, trigger_identity, state)
            VALUES (1, 1, 1, 'issue:1', 'running');
          INSERT INTO autopilot_circuit_run_steps (run_id, node_id, status, agent_node_id, parent_agent_node_id)
            VALUES (1, 'implementer', 'completed', 1, NULL), (1, 'reviewer', 'running', 2, 1),
                   (1, 'verdict', 'running', 2, 1), (1, 'other', 'running', 3, NULL);").unwrap();
        let read_parent = || {
            let rows = list_circuit_agent_ownerships_inner(&db).unwrap();
            assert_eq!(rows.len(), 3, "multiple steps referring to a reviewer must not duplicate it");
            assert_eq!(rows.iter().find(|r| r.0 == 3).unwrap().5, None);
            rows.iter().find(|r| r.0 == 2).unwrap().5
        };
        assert_eq!(read_parent(), Some(1));
        assert_eq!(read_parent(), Some(1), "all grouping information is in the ledger");
        db.execute("UPDATE autopilot_circuits SET graph_json = '{}'", []).unwrap();
        assert_eq!(read_parent(), Some(1), "presentation parentage survives without blueprint parsing");
        db.execute("UPDATE autopilot_circuit_runs SET source_agent_node_id = 1", []).unwrap();
        assert_eq!(read_parent(), Some(1), "run source does not overwrite explicit step parentage");
        db.execute("UPDATE autopilot_circuit_runs SET state = 'paused'", []).unwrap();
        assert_eq!(read_parent(), Some(1));
        db.execute("UPDATE autopilot_circuit_runs SET state = 'completed'", []).unwrap();
        assert_eq!(read_parent(), Some(1), "a retained reviewer remains inspectable");
        db.execute("UPDATE agent_nodes SET status = 'archived' WHERE id = 2", []).unwrap();
        assert!(list_circuit_agent_ownerships_inner(&db).unwrap().iter().all(|r| r.0 != 2));
    }

    #[test]
    fn source_ownership_retains_latest_terminal_run_for_done_indicator() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes (id, name, path) VALUES (1, 'sources', '/repo');
          INSERT INTO agent_nodes (id, mesh_id, name, path, branch, env, provider, status)
            VALUES (4, 1, 'source', '/repo', 'main', 'windows', 'terminal', 'completed');
          INSERT INTO autopilot_circuits (id, mesh_id, name, graph_json)
            VALUES (1, 1, 'review', '{}');
          INSERT INTO autopilot_circuit_runs (id, circuit_id, mesh_id, trigger_identity, state, source_agent_node_id)
            VALUES (1, 1, 1, 'issue:1', 'running', 4);").unwrap();

        let rows = list_circuit_agent_ownerships_inner(&db).unwrap();
        assert_eq!(rows, vec![(4, 1, 1, "review".to_owned(), "running".to_owned(), None)]);

        for state in ["completed", "failed", "cancelled"] {
            db.execute("UPDATE autopilot_circuit_runs SET state = ?1 WHERE id = 1", [state]).unwrap();
            let rows = list_circuit_agent_ownerships_inner(&db).unwrap();
            assert_eq!(rows[0].4, state);
        }

        db.execute("INSERT INTO autopilot_circuit_runs (id, circuit_id, mesh_id, trigger_identity, state, source_agent_node_id) VALUES (2, 1, 1, 'issue:2', 'running', 4)", []).unwrap();
        let rows = list_circuit_agent_ownerships_inner(&db).unwrap();
        assert_eq!(rows[0].1, 2, "a newer source run supersedes older terminal history");
        assert_eq!(rows[0].4, "running");
    }

    #[test]
    fn older_source_history_does_not_replace_newer_step_ownership() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&db).unwrap();
        db.execute_batch("INSERT INTO meshes (id, name, path) VALUES (1, 'mixed', '/repo');
          INSERT INTO agent_nodes (id, mesh_id, name, path, branch, env, provider, status)
            VALUES (5, 1, 'mixed-node', '/repo', 'main', 'windows', 'terminal', 'running'),
                   (8, 1, 'parent-node', '/repo', 'main', 'windows', 'terminal', 'completed');
          INSERT INTO autopilot_circuits (id, mesh_id, name, graph_json)
            VALUES (1, 1, 'review', '{}');
          INSERT INTO autopilot_circuit_runs (id, circuit_id, mesh_id, trigger_identity, state, source_agent_node_id)
            VALUES (1, 1, 1, 'issue:1', 'completed', 5),
                   (2, 1, 1, 'issue:2', 'running', NULL);
          INSERT INTO autopilot_circuit_run_steps (run_id, node_id, status, agent_node_id, parent_agent_node_id)
            VALUES (2, 'worker', 'running', 5, 8);").unwrap();

        let rows = list_circuit_agent_ownerships_inner(&db).unwrap();
        assert_eq!(rows, vec![(5, 2, 1, "review".to_owned(), "running".to_owned(), Some(8))]);
    }
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

/// Distinct piloted agent nodes across all active circuit runs. The optional
/// Autopilot pool is app-wide, so the circuit worker combines this count with
/// [`count_retained_circuit_agent_nodes_total`] and
/// [`crate::db::count_active_autopilot_nodes_total`] before admitting a new
/// circuit agent. The legacy per-mesh node limit is intentionally not part of
/// this accounting.
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

pub fn count_retained_circuit_agent_nodes_total() -> SqlResult<i64> {
    let db = super::read_conn();
    db.query_row(
        "SELECT COUNT(DISTINCT s.agent_node_id) FROM autopilot_circuit_run_steps s \
         JOIN autopilot_circuit_runs r ON r.id = s.run_id \
         JOIN agent_nodes a ON a.id = s.agent_node_id \
         WHERE r.state IN ('completed', 'failed', 'cancelled') \
           AND s.agent_node_id IS NOT NULL AND a.status != 'archived'",
        [],
        |row| row.get(0),
    )
}

const NODE_LEASE_TTL_SECS: i64 = 300;

/// Failed associations remain the durable cleanup retry ledger. Historic
/// terminal runs are not opted in unless their node-level cleanup request was
/// recorded by the circuit terminal transition.
pub fn list_failed_circuit_agents_for_cleanup() -> SqlResult<Vec<i64>> {
    // The legacy-import bridge is intentionally write-on-read for databases
    // upgraded from the pre-v43 JSON marker, so use the write connection for
    // this one recovery query.
    failed_circuit_agents_for_cleanup_inner(&super::write_conn())
}

fn lifecycle_generation(node_id: i64, prefix: &str) -> String {
    format!("{prefix}:{}:{node_id}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default())
}

fn import_legacy_cleanup_requests(conn: &Connection) -> SqlResult<()> {
    conn.execute(
        "INSERT OR IGNORE INTO agent_node_lifecycle_leases (node_id, cleanup_requested)
         SELECT DISTINCT s.agent_node_id, 1
         FROM autopilot_circuit_runs r
         JOIN autopilot_circuit_run_steps s ON s.run_id = r.id
         JOIN agent_nodes a ON a.id = s.agent_node_id
         WHERE s.agent_node_id IS NOT NULL AND a.status != 'archived'
           AND s.agent_node_id IS NOT r.source_agent_node_id
           AND json_extract(r.context_json, '$.\"cleanup.pending\"') = '1'",
        [],
    )?;
    Ok(())
}

/// Expire only the lease that timed out.  The cleanup request itself is a
/// durable recovery fact and must survive an abandoned worker or spawn lease;
/// deleting the whole row would silently drop that fact.
fn expire_lifecycle_leases(conn: &Connection) -> SqlResult<()> {
    conn.execute(
        "UPDATE agent_node_lifecycle_leases
         SET cleanup_generation = NULL, cleanup_expires_at = NULL,
             updated_at = unixepoch()
         WHERE cleanup_expires_at IS NOT NULL AND cleanup_expires_at <= unixepoch()",
        [],
    )?;
    conn.execute(
        "UPDATE agent_node_lifecycle_leases
         SET spawn_generation = NULL, spawn_expires_at = NULL,
             updated_at = unixepoch()
         WHERE spawn_expires_at IS NOT NULL AND spawn_expires_at <= unixepoch()",
        [],
    )?;
    Ok(())
}

/// Generic node spawn lease. The low-level spawn pipeline depends only on
/// this node lifecycle interface; circuit cleanup is one caller, not a
/// special case in the orchestrator.
pub fn claim_agent_spawn(node_id: i64) -> SqlResult<Option<String>> {
    claim_agent_spawn_inner(&super::write_conn(), node_id)
}

pub(crate) fn claim_agent_spawn_inner(conn: &Connection, node_id: i64) -> SqlResult<Option<String>> {
    let tx = conn.unchecked_transaction()?;
    import_legacy_cleanup_requests(&tx)?;
    expire_lifecycle_leases(&tx)?;
    tx.execute(
        "INSERT OR IGNORE INTO agent_node_lifecycle_leases (node_id)
         SELECT id FROM agent_nodes WHERE id = ?1 AND status != 'archived'",
        params![node_id],
    )?;
    let generation = lifecycle_generation(node_id, "spawn");
    let updated = tx.execute(
        "UPDATE agent_node_lifecycle_leases
         SET spawn_generation = ?2, spawn_expires_at = unixepoch() + ?3,
             updated_at = unixepoch()
         WHERE node_id = ?1 AND retired = 0
           AND spawn_generation IS NULL AND cleanup_generation IS NULL",
        params![node_id, generation, NODE_LEASE_TTL_SECS],
    )?;
    tx.commit()?;
    Ok((updated > 0).then_some(generation))
}

pub fn release_agent_spawn(node_id: i64, generation: &str, succeeded: bool) -> SqlResult<()> {
    release_agent_spawn_inner(&super::write_conn(), node_id, generation, succeeded)
}

pub(crate) fn release_agent_spawn_inner(
    conn: &Connection,
    node_id: i64,
    generation: &str,
    succeeded: bool,
) -> SqlResult<()> {
    conn.execute(
        "UPDATE agent_node_lifecycle_leases
         SET spawn_generation = NULL, spawn_expires_at = NULL,
             cleanup_requested = CASE WHEN ?3 THEN 0 ELSE cleanup_requested END,
             updated_at = unixepoch()
         WHERE node_id = ?1 AND spawn_generation = ?2",
        params![node_id, generation, succeeded],
    )?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn agent_spawn_claim_inner(conn: &Connection, node_id: i64) -> SqlResult<Option<String>> {
    Ok(conn.query_row(
        "SELECT spawn_generation FROM agent_node_lifecycle_leases WHERE node_id = ?1",
        params![node_id],
        |row| row.get::<_, Option<String>>(0),
    ).optional()?.flatten())
}

#[cfg(test)]
pub(crate) fn claim_circuit_agent_spawn_inner(conn: &Connection, node_id: i64) -> SqlResult<Option<String>> {
    claim_agent_spawn_inner(conn, node_id)
}

#[cfg(test)]
pub(crate) fn circuit_agent_spawn_claim_inner(conn: &Connection, node_id: i64) -> SqlResult<Option<String>> {
    agent_spawn_claim_inner(conn, node_id)
}

#[cfg(test)]
pub(crate) fn release_circuit_agent_spawn_inner(conn: &Connection, node_id: i64, generation: &str) -> SqlResult<()> {
    release_agent_spawn_inner(conn, node_id, generation, true)
}

pub fn circuit_agent_cleanup_claim(node_id: i64) -> SqlResult<Option<String>> {
    circuit_agent_cleanup_claim_inner(&super::read_conn(), node_id)
}

/// Generic read seam for spawn callers. The implementation is shared with
/// circuit cleanup, but the spawn module does not depend on circuit semantics.
pub fn agent_cleanup_claim(node_id: i64) -> SqlResult<Option<String>> {
    circuit_agent_cleanup_claim(node_id)
}

pub(crate) fn circuit_agent_cleanup_claim_inner(conn: &Connection, node_id: i64) -> SqlResult<Option<String>> {
    conn.query_row(
        "SELECT cleanup_generation FROM agent_node_lifecycle_leases
         WHERE node_id = ?1 AND cleanup_generation IS NOT NULL
           AND cleanup_expires_at > unixepoch()",
        params![node_id],
        |row| row.get(0),
    ).optional()
}

pub fn claim_circuit_agent_cleanup(node_id: i64) -> SqlResult<Option<String>> {
    claim_circuit_agent_cleanup_inner(&super::write_conn(), node_id)
}

pub(crate) fn claim_circuit_agent_cleanup_inner(conn: &Connection, node_id: i64) -> SqlResult<Option<String>> {
    let tx = conn.unchecked_transaction()?;
    import_legacy_cleanup_requests(&tx)?;
    expire_lifecycle_leases(&tx)?;
    tx.execute(
        "INSERT OR IGNORE INTO agent_node_lifecycle_leases (node_id, cleanup_requested)
         SELECT id, 1 FROM agent_nodes WHERE id = ?1 AND status != 'archived'",
        params![node_id],
    )?;
    let generation = lifecycle_generation(node_id, "cleanup");
    let updated = tx.execute(
        "UPDATE agent_node_lifecycle_leases
         SET cleanup_generation = ?2, cleanup_expires_at = unixepoch() + ?3,
             updated_at = unixepoch()
         WHERE node_id = ?1 AND cleanup_requested = 1 AND retired = 0
           AND cleanup_generation IS NULL AND spawn_generation IS NULL
           AND EXISTS (
             SELECT 1 FROM autopilot_circuit_run_steps s
             JOIN autopilot_circuit_runs r ON r.id = s.run_id
             JOIN agent_nodes a ON a.id = s.agent_node_id
             WHERE s.agent_node_id = agent_node_lifecycle_leases.node_id
               AND r.state IN ('completed', 'failed', 'cancelled')
               AND a.status != 'archived'
               AND a.id IS NOT r.source_agent_node_id
               AND NOT EXISTS (SELECT 1 FROM autopilot_circuit_runs borrowed
                   WHERE borrowed.source_agent_node_id = a.id
                     AND borrowed.state IN ('pending', 'running', 'paused'))
               AND NOT EXISTS (SELECT 1 FROM autopilot_circuit_run_steps other
                   JOIN autopilot_circuit_runs active ON active.id = other.run_id
                   WHERE other.agent_node_id = a.id
                     AND active.state IN ('running', 'paused'))
           )",
        params![node_id, generation, NODE_LEASE_TTL_SECS],
    )?;
    let result = if updated > 0 {
        Some(generation)
    } else {
        tx.query_row(
            "SELECT cleanup_generation FROM agent_node_lifecycle_leases
             WHERE node_id = ?1 AND cleanup_generation IS NOT NULL",
            params![node_id],
            |row| row.get(0),
        ).optional()?
    };
    tx.commit()?;
    Ok(result)
}

pub fn release_circuit_agent_cleanup(node_id: i64, generation: &str) -> SqlResult<()> {
    release_circuit_agent_cleanup_inner(&super::write_conn(), node_id, generation)
}

/// Atomically verify and renew a cleanup lease immediately before the
/// external process kill.  This closes the gap between selecting a retry
/// candidate and touching the OS process: a resumed/spawned node cannot take
/// over while the cleanup worker still owns the renewed generation.
pub fn renew_circuit_agent_cleanup(node_id: i64, generation: &str) -> SqlResult<bool> {
    renew_circuit_agent_cleanup_inner(&super::write_conn(), node_id, generation)
}

pub(crate) fn renew_circuit_agent_cleanup_inner(
    conn: &Connection,
    node_id: i64,
    generation: &str,
) -> SqlResult<bool> {
    let changed = conn.execute(
        "UPDATE agent_node_lifecycle_leases
         SET cleanup_expires_at = unixepoch() + ?3, updated_at = unixepoch()
         WHERE node_id = ?1 AND cleanup_generation = ?2
           AND cleanup_requested = 1 AND retired = 0
           AND cleanup_expires_at > unixepoch() AND spawn_generation IS NULL",
        params![node_id, generation, NODE_LEASE_TTL_SECS],
    )?;
    Ok(changed > 0)
}

pub(crate) fn release_circuit_agent_cleanup_inner(conn: &Connection, node_id: i64, generation: &str) -> SqlResult<()> {
    conn.execute(
        "UPDATE agent_node_lifecycle_leases
         SET cleanup_generation = NULL, cleanup_expires_at = NULL,
             updated_at = unixepoch()
         WHERE node_id = ?1 AND cleanup_generation = ?2",
        params![node_id, generation],
    )?;
    Ok(())
}

pub fn clear_finished_circuit_cleanup() -> SqlResult<()> {
    clear_finished_circuit_cleanup_inner(&super::write_conn())
}

pub(crate) fn clear_finished_circuit_cleanup_inner(conn: &Connection) -> SqlResult<()> {
    conn.execute(
        "UPDATE agent_node_lifecycle_leases
         SET cleanup_requested = 0, updated_at = unixepoch()
         WHERE cleanup_requested = 1 AND retired = 0
           AND EXISTS (SELECT 1 FROM agent_nodes a WHERE a.id = node_id AND a.status = 'archived')
           AND cleanup_generation IS NULL AND spawn_generation IS NULL",
        [],
    )?;
    Ok(())
}

pub(crate) fn failed_circuit_agents_for_cleanup_inner(conn: &rusqlite::Connection) -> SqlResult<Vec<i64>> {
    import_legacy_cleanup_requests(conn)?;
    let mut stmt = conn.prepare(
        "SELECT l.node_id FROM agent_node_lifecycle_leases l
         JOIN agent_nodes a ON a.id = l.node_id
         WHERE l.cleanup_requested = 1 AND l.retired = 0
           AND a.status != 'archived'
           AND l.cleanup_generation IS NULL AND l.spawn_generation IS NULL
           AND NOT EXISTS (SELECT 1 FROM autopilot_circuit_runs borrowed
               WHERE borrowed.source_agent_node_id = l.node_id
                 AND borrowed.state IN ('pending', 'running', 'paused'))
           AND NOT EXISTS (SELECT 1 FROM autopilot_circuit_run_steps other
               JOIN autopilot_circuit_runs active ON active.id = other.run_id
               WHERE other.agent_node_id = l.node_id
                 AND active.state IN ('running', 'paused'))
         ORDER BY l.node_id")?;
    let ids = stmt.query_map([], |row| row.get(0))?.collect();
    ids
}

pub fn count_active_circuit_agent_nodes_for_run(run_id: i64) -> SqlResult<i64> {
    let db = super::read_conn();
    db.query_row(
        "SELECT COUNT(DISTINCT s.agent_node_id) \
         FROM autopilot_circuit_run_steps s \
         JOIN autopilot_circuit_runs r ON r.id = s.run_id \
         WHERE r.id = ?1 AND r.state IN ('running', 'paused') \
           AND s.agent_node_id IS NOT NULL",
        params![run_id],
        |row| row.get(0),
    )
}

/// Archive and acknowledge cleanup together so a later manual resume cannot
/// be mistaken for unfinished cleanup of the old circuit attempt.
pub fn archive_circuit_agent(node_id: i64, claim: &str) -> SqlResult<Vec<(i64, String)>> {
    archive_circuit_agent_inner(&super::write_conn(), node_id, claim)
}

pub(crate) fn archive_circuit_agent_inner(conn: &Connection, node_id: i64, claim: &str) -> SqlResult<Vec<(i64, String)>> {
    let tx = conn.unchecked_transaction()?;
    let claim_matches: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_node_lifecycle_leases l
            JOIN agent_nodes a ON a.id = l.node_id
            WHERE l.node_id = ?1 AND a.status != 'archived'
              AND l.cleanup_generation = ?2
              AND l.cleanup_expires_at > unixepoch()
              AND l.spawn_generation IS NULL
              AND l.cleanup_requested = 1)",
        params![node_id, claim],
        |row| row.get(0),
    )?;
    if !claim_matches {
        tx.commit()?;
        return Ok(Vec::new());
    }
    super::update_agent_node_status_inner(&tx, node_id, crate::models::SessionStatus::Archived)?;
    let runs = {
        let mut statement = tx.prepare("SELECT DISTINCT r.id, r.state FROM autopilot_circuit_runs r
            JOIN autopilot_circuit_run_steps s ON s.run_id=r.id
            WHERE s.agent_node_id=?1 AND r.state IN ('completed','failed','cancelled')")?;
        let rows = statement.query_map(params![node_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect::<SqlResult<Vec<_>>>()?
    };
    tx.execute(
        "UPDATE agent_node_lifecycle_leases
         SET cleanup_generation = NULL, cleanup_expires_at = NULL,
             cleanup_requested = 0, retired = 1, updated_at = unixepoch()
         WHERE node_id = ?1 AND cleanup_generation = ?2",
        params![node_id, claim],
    )?;
    tx.commit()?;
    Ok(runs)
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
///   * Terminal runs (`completed`/`failed`) do NOT count: a terminal
///     `commit_circuit_advance` transitions the row out of this set in a
///     single `UPDATE`, and a repeated terminal signal is a no-op (so we
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
        AND NOT EXISTS (SELECT 1 FROM autopilot_circuit_run_steps s \
                        JOIN agent_node_lifecycle_leases l ON l.node_id = s.agent_node_id \
                        WHERE s.run_id = r.id AND l.cleanup_requested = 1) \
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
            AND NOT EXISTS (SELECT 1 FROM autopilot_circuit_run_steps s \
                            JOIN agent_node_lifecycle_leases l ON l.node_id = s.agent_node_id \
                            WHERE s.run_id = autopilot_circuit_runs.id AND l.cleanup_requested = 1) \
            AND updated_at < datetime('now', '-' || ?1 || ' days') \
            AND trigger_identity NOT LIKE 'interval:%' \
            AND trigger_identity NOT LIKE 'manual:%' \
            AND context_json <> '{}'",
        params![days],
    )?;

    Ok((deleted, compacted))
}
