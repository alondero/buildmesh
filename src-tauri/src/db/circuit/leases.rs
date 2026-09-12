//! Circuit run-agent leases, ownership claims, cleanup, and retention.

use rusqlite::{Connection, OptionalExtension, params};

use crate::db::SqlResult;


/// Reserve the number of agent slots a circuit blueprint may need while its
/// run is admitted. The lease is durable and keyed by run, so admission is
/// not inferred from whichever child agent happens to be attached today.
/// Repeated calls are idempotent and may repair a pre-lease active run after
/// an upgrade.
pub fn reserve_circuit_agent_slots(run_id: i64, slots: i64) -> SqlResult<bool> {
    let mut db = crate::db::write_conn();
    reserve_circuit_agent_slots_locked(&mut db, run_id, slots)
}

/// Per-test isolated variant of [`reserve_circuit_agent_slots`] (issue #1691).
/// Opens its own transaction on `conn`; the `_locked` suffix distinguishes
/// this from the `_inner` helpers that operate inside an externally-managed
/// transaction.
pub(crate) fn reserve_circuit_agent_slots_locked(
    conn: &mut Connection,
    run_id: i64,
    slots: i64,
) -> SqlResult<bool> {
    if slots <= 0 {
        return Ok(true);
    }
    let tx = conn.transaction()?;
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
    let db = crate::db::read_conn();
    circuit_agent_slots_reserved_inner(&db, run_id)
}

/// Per-test isolated variant of [`circuit_agent_slots_reserved`] (issue #1691).
pub(crate) fn circuit_agent_slots_reserved_inner(db: &Connection, run_id: i64) -> SqlResult<i64> {
    db.query_row(
        "SELECT slots FROM autopilot_circuit_run_agent_leases WHERE run_id = ?1",
        params![run_id],
        |row| row.get(0),
    )
    .optional()
    .map(|v| v.unwrap_or(0))
}

pub fn count_reserved_circuit_agent_slots_total() -> SqlResult<i64> {
    let db = crate::db::read_conn();
    db.query_row(
        "SELECT COALESCE(SUM(l.slots), 0) \
         FROM autopilot_circuit_run_agent_leases l \
         JOIN autopilot_circuit_runs r ON r.id = l.run_id \
         WHERE r.state IN ('pending', 'running', 'paused')",
        [],
        |row| row.get(0),
    )
}
/// Circuit ownership metadata for Agent Nodes that still exist. The
/// association lives in the circuit step ledger (not on `agent_nodes`), so
/// the header can identify automated nodes without weakening the satellite-
/// table invariant used by legacy Autopilot.
type AgentOwnershipRow = (i64, i64, i64, String, String, Option<i64>);

pub fn list_circuit_agent_ownerships() -> SqlResult<Vec<AgentOwnershipRow>> {
    let db = crate::db::read_conn();
    list_circuit_agent_ownerships_inner(&db)
}

pub(crate) fn list_circuit_agent_ownerships_inner(db: &Connection) -> SqlResult<Vec<AgentOwnershipRow>> {
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

/// Distinct piloted agent nodes across all active circuit runs. The optional
/// Autopilot pool is app-wide, so the circuit worker combines this count with
/// [`count_retained_circuit_agent_nodes_total`] and
/// [`crate::db::count_active_autopilot_nodes_total`] before admitting a new
/// circuit agent. The legacy per-mesh node limit is intentionally not part of
/// this accounting.
pub fn count_active_circuit_agent_nodes_total() -> SqlResult<i64> {
    let db = crate::db::read_conn();
    count_active_circuit_agent_nodes_total_inner(&db)
}

/// Per-test isolated variant of [`count_active_circuit_agent_nodes_total`] (issue #1691).
/// The public function reads the process-global reader pool; this helper
/// takes an explicit `&Connection` so parallel tests can each operate
/// against their own in-memory DB without sharing the global counter.
pub(crate) fn count_active_circuit_agent_nodes_total_inner(db: &Connection) -> SqlResult<i64> {
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
    let db = crate::db::read_conn();
    count_retained_circuit_agent_nodes_total_inner(&db)
}

/// Per-test isolated variant of [`count_retained_circuit_agent_nodes_total`] (issue #1691).
pub(crate) fn count_retained_circuit_agent_nodes_total_inner(db: &Connection) -> SqlResult<i64> {
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
    failed_circuit_agents_for_cleanup_inner(&crate::db::write_conn())
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
    claim_agent_spawn_inner(&crate::db::write_conn(), node_id)
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
    release_agent_spawn_inner(&crate::db::write_conn(), node_id, generation, succeeded)
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
    circuit_agent_cleanup_claim_inner(&crate::db::read_conn(), node_id)
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
    claim_circuit_agent_cleanup_inner(&crate::db::write_conn(), node_id)
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
    release_circuit_agent_cleanup_inner(&crate::db::write_conn(), node_id, generation)
}

/// Atomically verify and renew a cleanup lease immediately before the
/// external process kill.  This closes the gap between selecting a retry
/// candidate and touching the OS process: a resumed/spawned node cannot take
/// over while the cleanup worker still owns the renewed generation.
pub fn renew_circuit_agent_cleanup(node_id: i64, generation: &str) -> SqlResult<bool> {
    renew_circuit_agent_cleanup_inner(&crate::db::write_conn(), node_id, generation)
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
    clear_finished_circuit_cleanup_inner(&crate::db::write_conn())
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
    let db = crate::db::read_conn();
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
    archive_circuit_agent_inner(&crate::db::write_conn(), node_id, claim)
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
    crate::db::update_agent_node_status_inner(&tx, node_id, crate::models::SessionStatus::Archived)?;
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
    let mut db = crate::db::write_conn();
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

// ---------------------------------------------------------------------------
// Tests for reviewer + source ownership semantics (issue #1655: this test
// module was previously embedded mid-file and broke `cargo clippy`'s
// `items_after_test_module` lint; it now lives at the bottom of leases.rs).
// ---------------------------------------------------------------------------

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
