//! Autopilot Circuits ledger: blueprint/run/step CRUD and the engine's
//! atomic [`commit_circuit_advance`] seam (spec #1205).

use rusqlite::{Connection, OptionalExtension, params};

use crate::agent::provider::SpawnOptionId;
use crate::autopilot::circuit::vocabulary::{RunState, StepStatus};
use crate::db::SqlResult;
use crate::models::{AutopilotCircuit, AutopilotCircuitRun, AutopilotCircuitRunStep};

// Circuits — CRUD for the blueprint rows.
// ---------------------------------------------------------------------------

/// Normalise the title-bar reviewer override into the stored run-context value.
///
/// The id is a Spawn Option id — `<harness>` or the composite
/// `harness:provider_id` — so the segment that decides whether this is a real
/// agent is the harness. A blank value collapses to `None` (inherit the
/// app-wide Reviewer provider, then the source agent); the Terminal harness is
/// rejected outright.
///
/// The Start Review picker already omits Terminal, but the backend must hold
/// its own invariant: the reviewer-provider cascade treats any non-empty string
/// as the winner, so an unchecked `invoke` would mint a review run whose
/// "reviewer" is a plain shell. Validation is input-only, so it applies to
/// authored Circuits too, even though they ignore the value.
fn normalize_reviewer_provider(value: Option<String>) -> Result<Option<String>, String> {
    let Some(value) = value else { return Ok(None) };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    // Issue #1659 circuit-trigger entry seam: route the harness-half
    // extraction through the typed `SpawnOptionId` so the same first-`:`-
    // split rule every other entry seam uses applies here. A bare
    // `"terminal"` parses as a native row; a Proxied form
    // (`"terminal:<provider>"`) — rejected today by
    // `BUILTIN_HARNESS_IDS` but the contract holds regardless —
    // would also match.
    if SpawnOptionId::from(trimmed).harness_id() == "terminal" {
        return Err("Terminal cannot be used as the reviewer provider.".into());
    }
    Ok(Some(trimmed.to_string()))
}

/// Atomically claim a source agent and create its review run. The source id is
/// stored relationally on the run; the context copy remains for graph
/// template expansion and backwards-compatible diagnostics.
///
/// `reviewer_provider` is the per-run reviewer provider (a Spawn Option id,
/// possibly a composite `harness:provider`) chosen at the title-bar Start
/// Review control. When set it overrides the app-wide Reviewer provider
/// snapshot in this run's context, so the reviewer agent spawns on the
/// provider the user picked. It applies to the built-in review preset only:
/// an authored Circuit carries its own reviewer provider in its graph.
/// Validated by [`normalize_reviewer_provider`] regardless of Circuit kind.
///
/// **First writer wins.** If the source agent already owns a live run, the
/// early-return below hands back that run's id and `max_rounds` /
/// `reviewer_provider` are not applied — the dialog hides the form in this
/// state, so the only way here is a retry or IPC race.
pub fn create_node_circuit_run(
    node_id: i64,
    selected_circuit_id: Option<i64>,
    max_rounds: i32,
    reviewer_provider: Option<String>,
) -> Result<i64, String> {
    let reviewer_override = normalize_reviewer_provider(reviewer_provider)?;
    let mut db = crate::db::write_conn();
    let tx = db.transaction().map_err(|e| e.to_string())?;
    let node = crate::db::agent_node::get_agent_node_by_id_inner(&tx, node_id).map_err(|e| e.to_string())?;
    let existing: Option<i64> = tx.query_row(
        &format!(
            "SELECT id FROM autopilot_circuit_runs
             WHERE source_agent_node_id = ?1 AND state IN ({})
             LIMIT 1",
            RunState::SQL_IN_LIVE
        ),
        params![node_id], |row| row.get(0),
    ).optional().map_err(|e| e.to_string())?;
    if let Some(id) = existing { return Ok(id); }
    let owned: bool = tx.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM autopilot_circuit_run_steps s \
             JOIN autopilot_circuit_runs r ON r.id = s.run_id \
             WHERE s.agent_node_id = ?1 AND r.state IN ({})) \
             OR EXISTS(SELECT 1 FROM autopilot_runs WHERE node_id = ?1 \
             AND state IN ('implementing','finishing','suffix_pending'))",
            RunState::SQL_IN_LIVE
        ),
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
    context.with_app_reviewer_provider();
    context.set("source.agent_id", node_id.to_string());
    context.set("source.name", &node.name);
    context.set("source.path", crate::env::node_working_path(&node).spawn_path);
    let base_ref: String = tx.query_row("SELECT base_ref FROM meshes WHERE id = ?1", params![node.mesh_id], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    context.set("source.base_ref", base_ref);
    if selected_circuit_id.is_none() {
        if let Some(provider) = reviewer_override.as_deref() {
            context.set("review.provider", provider);
        }
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
    let db = crate::db::write_conn();
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

pub(crate) fn get_autopilot_circuit_inner(
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
    let db = crate::db::read_conn();
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
    let db = crate::db::read_conn();
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
    let db = crate::db::read_conn();
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
    let db = crate::db::read_conn();
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
    let db = crate::db::read_conn();
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
    let db = crate::db::read_conn();
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
               AND r.state IN ({}) \
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
             WHERE circuit_id IN ({}) AND state IN ({}) \
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
        RunState::SQL_IN_TERMINAL,
        ids.join(","),
        RunState::SQL_IN_ADMITTED
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
        let placeholders = std::iter::repeat_n("?", run_ids.len()).collect::<Vec<_>>().join(",");
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
    let db = crate::db::write_conn();
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
    let db = crate::db::write_conn();
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
    let mut db = crate::db::write_conn();
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
    let mut db = crate::db::write_conn();
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
/// Atomically terminalise one active run and return its attached Agent Nodes
/// so the command layer can retire their processes/worktrees after the DB
/// stops the worker from driving the run. A missing row is already gone
/// (deleted between render and click) and returns an empty agent list —
/// callers must not string-match the driver error for this case.
pub fn cancel_circuit_run(run_id: i64) -> SqlResult<Vec<i64>> {
    let mut db = crate::db::write_conn();
    let tx = db.transaction()?;
    let result = cancel_circuit_run_inner(&tx, run_id)?;
    tx.commit()?;
    Ok(result.agents)
}

/// One run's cancel writes against an already-open transaction. Shared by
/// the single and batch paths so both observe identical state transitions.
struct CancelRunWrite {
    agents: Vec<i64>,
    source: Option<i64>,
    cancelled: bool,
}

fn cancel_circuit_run_inner(tx: &Connection, run_id: i64) -> SqlResult<CancelRunWrite> {
    let row: Option<(String, Option<i64>)> = tx.query_row(
        "SELECT state, source_agent_node_id FROM autopilot_circuit_runs WHERE id = ?1",
        params![run_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    let Some((state, source)) = row else {
        return Ok(CancelRunWrite { agents: vec![], source: None, cancelled: false });
    };
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
    if RunState::is_live_db_str(&state) {
        tx.execute(
            &format!(
                "UPDATE autopilot_circuit_runs SET state = ?2, context_json = json_remove(context_json, '$.\"cleanup.pending\"'), updated_at = datetime('now') \
                 WHERE id = ?1 AND state IN ({})",
                RunState::SQL_IN_LIVE
            ),
            params![run_id, RunState::Cancelled.as_db_str()],
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
    if !RunState::is_terminal_db_str(&state) {
        tx.execute(
            &format!(
                "UPDATE autopilot_circuit_run_steps \
                 SET status = ?2, outcome = ?2, completed_at = datetime('now') \
                 WHERE run_id = ?1 AND status IN ({})",
                StepStatus::SQL_IN_IN_FLIGHT
            ),
            params![run_id, StepStatus::Cancelled.as_db_str()],
        )?;
    }
    tx.execute(
        "DELETE FROM autopilot_circuit_run_agent_leases WHERE run_id = ?1",
        params![run_id],
    )?;
    let cancelled = RunState::is_live_db_str(&state);
    Ok(CancelRunWrite { agents, source, cancelled })
}

/// Batch cancel for queue/activity hygiene: every listed run is
/// terminalised in ONE transaction, so a 50-run "Cancel all" costs one
/// write txn, one worker wake, and one UI event — not an N+1 storm.
/// Missing rows are skipped (already gone between render and click).
/// Returns attached agent ids, source node ids to unregister, and the ids
/// actually transitioned to cancelled.
pub struct BatchCancelResult {
    pub agents: Vec<i64>,
    pub sources: Vec<i64>,
    pub cancelled: Vec<i64>,
}

pub fn cancel_circuit_runs(run_ids: &[i64]) -> SqlResult<BatchCancelResult> {
    let mut db = crate::db::write_conn();
    let tx = db.transaction()?;
    let mut agents: Vec<i64> = Vec::new();
    let mut sources: Vec<i64> = Vec::new();
    let mut cancelled: Vec<i64> = Vec::new();
    // De-duplicate the payload so one id cannot double-count agents.
    let mut seen = std::collections::HashSet::with_capacity(run_ids.len());
    for run_id in run_ids {
        if !seen.insert(run_id) {
            continue;
        }
        let write = cancel_circuit_run_inner(&tx, *run_id)?;
        agents.extend(write.agents);
        if let Some(source) = write.source {
            if !sources.contains(&source) {
                sources.push(source);
            }
        }
        if write.cancelled {
            cancelled.push(*run_id);
        }
    }
    agents.sort_unstable();
    agents.dedup();
    sources.sort_unstable();
    sources.dedup();
    cancelled.sort_unstable();
    tx.commit()?;
    Ok(BatchCancelResult { agents, sources, cancelled })
}

/// Runs whose attached agents may still need retiring while a circuit is
/// deleted. Terminal rows are included so a deletion can be retried after a
/// transient process/worktree cleanup failure without orphaning retained
/// agents from a completed or failed run.
pub fn list_circuit_run_ids_for_cleanup(circuit_id: i64) -> SqlResult<Vec<i64>> {
    let db = crate::db::read_conn();
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
    let db = crate::db::read_conn();
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
    let db = crate::db::read_conn();
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
    let db = crate::db::write_conn();
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
    let db = crate::db::write_conn();
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
    crate::autopilot::circuit::stepper::RunState::from_db_str(state).is_terminal()
}

/// One run row by id, or `None` when the id is unknown.
pub fn get_circuit_run(run_id: i64) -> SqlResult<Option<AutopilotCircuitRun>> {
    let db = crate::db::read_conn();
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
    let db = crate::db::read_conn();
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
    let mut db = crate::db::write_conn();
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
    let db = crate::db::write_conn();
    let updated = db.execute(
        "UPDATE autopilot_circuit_run_steps SET agent_node_id = ?3, parent_agent_node_id = ?4 \
         WHERE run_id = ?1 AND node_id = ?2",
        params![run_id, node_id, agent_node_id, parent_agent_node_id],
    )?;
    Ok(updated > 0)
}
/// Clear an agent association after a CloseAgentNode effect succeeds. The
/// circuit step remains an audit record, but a retired reviewer must no
/// longer consume mesh/global agent capacity on the run's remaining steps.
pub fn clear_circuit_step_agent_node(run_id: i64, node_id: &str) -> SqlResult<()> {
    let db = crate::db::write_conn();
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
    let db = crate::db::write_conn();
    db.execute(
        "UPDATE autopilot_circuit_run_steps SET agent_node_id = NULL \
         WHERE run_id = ?1 AND agent_node_id = ?2",
        params![run_id, agent_node_id],
    )?;
    Ok(())
}
// Concurrency counters — the inputs to the stepper's capacity snapshot.
// ---------------------------------------------------------------------------

/// Steps currently Running across this circuit's active runs — compared
/// against `autopilot_circuits.concurrency_limit`. Paused runs count:
/// their steps still hold real agents even though the graph is parked.
pub fn count_running_circuit_steps(circuit_id: i64) -> SqlResult<i64> {
    let db = crate::db::read_conn();
    db.query_row(
        "SELECT COUNT(*) FROM autopilot_circuit_run_steps s \
         JOIN autopilot_circuit_runs r ON r.id = s.run_id \
         WHERE r.circuit_id = ?1 AND r.state IN ('running', 'paused') AND s.status = 'running'",
        params![circuit_id],
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
    let db = crate::db::read_conn();
    db.query_row(
        "SELECT COUNT(*) FROM autopilot_circuit_runs \
         WHERE mesh_id = ?1 AND state IN ('running', 'paused')",
        params![mesh_id],
        |row| row.get(0),
    )
}
