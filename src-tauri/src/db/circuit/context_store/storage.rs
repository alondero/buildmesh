//! The physical storage of a Circuit Run's context (issue #2672).
//!
//! This module is the storage detail behind the logical context seam in
//! [`super`]. It owns the size threshold, the side table and the storage
//! fingerprint. Nothing outside the seam uses it: a caller asks for a
//! logical read or a logical change, and never learns which half of the
//! context a value happens to live in.
//!
//! The rule it encodes is one line: a value longer than
//! [`BULKY_VALUE_BYTES`] lives in `circuit_run_artifacts`, everything else
//! lives in `autopilot_circuit_runs.context_json`.

use std::collections::BTreeMap;

use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::db::SqlResult;

/// Context values longer than this move out of `context_json`.
///
/// The threshold is a storage decision, not a semantic one: a value above it
/// is an agent output, an evidence blob or a trigger body — text a run reads
/// on demand rather than a flag the stepper decides on. Everything at or
/// below it stays in the column, so the decision-relevant half of a context
/// is still one small row.
pub(super) const BULKY_VALUE_BYTES: usize = 1024;

/// The reserved context key that fingerprints the stored bulky set.
pub(super) const DIGEST_KEY: &str = "context.artifact_digest";

/// Create the side table. Idempotent: the migration runner calls it on every
/// launch so a database that predates the table heals itself.
///
/// The table carries no index beyond its primary key. Every read filters by
/// `run_id`, which the primary key's leftmost column already covers, and the
/// node-scoped delete uses a key-prefix range instead of the `node_id` column.
/// An index nothing reads is a page written on every first insert — the
/// opposite of what this table is for.
pub(super) fn ensure_table(conn: &Connection) -> SqlResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS circuit_run_artifacts (
            run_id INTEGER NOT NULL REFERENCES autopilot_circuit_runs(id) ON DELETE CASCADE,
            key TEXT NOT NULL,
            node_id TEXT NOT NULL DEFAULT '',
            attempt INTEGER NOT NULL DEFAULT 0,
            value TEXT NOT NULL,
            bytes INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (run_id, key)
        );
        DROP INDEX IF EXISTS idx_circuit_run_artifacts_run_node;",
    )
}

/// The stored column of one run, exactly as it is on disk.
///
/// The payload is returned unchanged: the split is a storage optimisation,
/// not a validation step, so a historical payload is neither normalised nor
/// erased by a read.
pub(super) fn read_column(conn: &Connection, run_id: i64) -> SqlResult<String> {
    conn.query_row(
        "SELECT context_json FROM autopilot_circuit_runs WHERE id = ?1",
        [run_id],
        |row| row.get(0),
    )
}

/// The stored column of one run and its history revision, read together.
///
/// One statement, so both come from the same snapshot. A run that has moved
/// to another state has no stored context to fence against, so this reports
/// no column rather than a stale one.
pub(super) fn read_column_and_history(
    conn: &Connection,
    run_id: i64,
    state: &str,
) -> SqlResult<Option<(String, i64)>> {
    conn.query_row(
        "SELECT r.context_json,
                (SELECT COALESCE(MAX(id),0) FROM circuit_run_history WHERE run_id=r.id)
         FROM autopilot_circuit_runs r
         WHERE r.id=?1 AND r.state=?2",
        params![run_id, state],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
}

/// The decision-relevant values of a stored column.
///
/// The fingerprint key is not a value of the run's context, so it is not
/// returned. A payload that is not a flat string map has no values to
/// return: it has no map to read.
pub(super) fn parse_small(column_json: &str) -> BTreeMap<String, String> {
    let Ok(map) = serde_json::from_str::<BTreeMap<String, String>>(column_json) else {
        return BTreeMap::new();
    };
    map.into_iter()
        .filter(|(key, _)| key != DIGEST_KEY)
        .collect()
}

/// Whether a value belongs in the side table rather than the column.
pub(super) fn is_bulky(key: &str, value: &str) -> bool {
    value.len() > BULKY_VALUE_BYTES && !always_small(key)
}

/// The stored columns of several runs, keyed by run id.
///
/// One query per chunk of runs, so a list projection stays bounded whatever
/// the number of runs.
pub(super) fn read_columns_for_runs(
    conn: &Connection,
    run_ids: &[i64],
) -> SqlResult<BTreeMap<i64, String>> {
    let mut columns = BTreeMap::new();
    const CHUNK_SIZE: usize = 500;
    for chunk in run_ids.chunks(CHUNK_SIZE) {
        let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, context_json FROM autopilot_circuit_runs WHERE id IN ({placeholders})"
        );
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (run_id, column_json) = row?;
            columns.insert(run_id, column_json);
        }
    }
    Ok(columns)
}

/// The stored bulky values of one run.
pub(super) fn load_bulky(conn: &Connection, run_id: i64) -> SqlResult<BTreeMap<String, String>> {
    let mut statement = conn
        .prepare("SELECT key, value FROM circuit_run_artifacts WHERE run_id = ?1 ORDER BY key")?;
    let rows = statement.query_map([run_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let mut values = BTreeMap::new();
    for row in rows {
        let (key, value) = row?;
        values.insert(key, value);
    }
    Ok(values)
}

/// The stored bulky values of several runs, keyed by run id.
pub(super) fn load_bulky_for_runs(
    conn: &Connection,
    run_ids: &[i64],
) -> SqlResult<BTreeMap<i64, BTreeMap<String, String>>> {
    let mut values: BTreeMap<i64, BTreeMap<String, String>> = BTreeMap::new();
    // Ids per `IN (...)` query, keeping each statement well under SQLite's
    // bound-variable limit.
    const CHUNK_SIZE: usize = 500;
    for chunk in run_ids.chunks(CHUNK_SIZE) {
        let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT run_id, key, value FROM circuit_run_artifacts
             WHERE run_id IN ({placeholders}) ORDER BY run_id, key"
        );
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (run_id, key, value) = row?;
            values.entry(run_id).or_default().insert(key, value);
        }
    }
    Ok(values)
}

/// Store one bulky value.
///
/// This always writes. The caller holds the values the run already stores,
/// so it skips the call when the value did not change: a repeated advance
/// leaves an unchanged value alone entirely — not even touched — and does
/// not dirty the side table's pages.
pub(super) fn upsert_bulky(
    conn: &Connection,
    run_id: i64,
    key: &str,
    value: &str,
) -> SqlResult<()> {
    conn.execute(
        "INSERT INTO circuit_run_artifacts (run_id, key, node_id, attempt, value, bytes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(run_id, key) DO UPDATE SET
             value = excluded.value,
             bytes = excluded.bytes,
             updated_at = datetime('now')",
        params![
            run_id,
            key,
            node_id_of(key),
            attempt_of(key),
            value,
            value.len() as i64
        ],
    )?;
    Ok(())
}

/// Drop the stored bulky value of one key.
pub(super) fn delete_bulky(conn: &Connection, run_id: i64, key: &str) -> SqlResult<usize> {
    conn.execute(
        "DELETE FROM circuit_run_artifacts WHERE run_id = ?1 AND key = ?2",
        params![run_id, key],
    )
}

/// Delete the stored half of the runs a query names.
///
/// A run's stored half rides with the run: a sweep that removes runs removes
/// their stored values with them. The caller owns the query that names the
/// runs; the seam owns the table they live in.
pub(super) fn delete_stored_values_for_runs(
    conn: &Connection,
    run_ids_query: &str,
    params: impl rusqlite::Params,
) -> SqlResult<usize> {
    conn.execute(
        &format!("DELETE FROM circuit_run_artifacts WHERE run_id IN ({run_ids_query})"),
        params,
    )
}

/// Drop the stored bulky values whose key starts with `segment.`.
///
/// The range is `key >= segment. AND key < segment/`, which is exact: a dot
/// and a slash are neighbours in the byte order, so every key that starts with
/// the segment's dotted prefix sorts below the bound, and no key that does not
/// start with it can. The bound stays inside ASCII whatever the segment
/// holds, so it is always a valid UTF-8 string — and the comparison is
/// byte-wise, like SQLite's `BINARY` collation, so a node id holding `_`
/// (SQLite's single character wildcard) cannot match a sibling node's values.
pub(super) fn delete_bulky_with_prefix(
    conn: &Connection,
    run_id: i64,
    segment: &str,
) -> SqlResult<usize> {
    conn.execute(
        "DELETE FROM circuit_run_artifacts
         WHERE run_id = ?1 AND key >= ?2 AND key < ?3",
        params![run_id, format!("{segment}."), format!("{segment}/")],
    )
}

/// Fingerprint a set of bulky values.
///
/// The fingerprint covers the values, not just their keys, so a rewritten
/// output changes it.
pub(super) fn digest_of(values: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    for (key, value) in values {
        hasher.update(key.as_bytes());
        hasher.update([0u8]);
        hasher.update(value.as_bytes());
        hasher.update([0u8]);
    }
    hex::encode(hasher.finalize())
}

/// Context paths the run's wait tracking reads.
///
/// A blocker message, a capacity window and a continuation's delivery state
/// are what the stepper's waits and the operator's blocked-step surfaces
/// decide on. They stay in the column whatever their size, so a run's
/// projections read them without joining the side table.
///
/// The wait comparison itself reads the logical context, so this is where
/// those paths live rather than a rule the comparison depends on.
fn always_small(key: &str) -> bool {
    key.ends_with(".continuation.delivery")
        || key.ends_with(".observation_blocker")
        || key.ends_with(".capacity_wait")
        || key.contains(".wait.")
}

/// The circuit node a context path belongs to, or an empty string for a
/// run-level path such as `source.output`.
///
/// The node id is the segment between `node.` and the next `.`. Circuit node
/// ids are identifiers (`await_source`, `reviewer`, `verdict`), so the first
/// dot ends them. A node id that itself contains a dot is recorded up to its
/// first dot: the column describes the value, and the key is what addresses
/// it.
fn node_id_of(key: &str) -> &str {
    key.strip_prefix("node.")
        .and_then(|rest| rest.split_once('.'))
        .map(|(node, _)| node)
        .unwrap_or("")
}

/// The attempt a context path belongs to, or `0` when it is not
/// attempt-scoped. `node.reviewer.evidence.2` is attempt 2.
///
/// The attempt is read off the `evidence.` segment, which is where the ledger
/// writes it.
fn attempt_of(key: &str) -> i64 {
    key.strip_prefix("node.")
        .and_then(|rest| rest.split_once('.'))
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.strip_prefix("evidence."))
        .and_then(|attempt| attempt.parse::<i64>().ok())
        .unwrap_or(0)
}
