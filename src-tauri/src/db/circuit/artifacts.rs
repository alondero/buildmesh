//! Bulky circuit-run context values live in their own table (issue #2601).
//!
//! `autopilot_circuit_runs.context_json` is rewritten as one blob on every
//! [`crate::db::circuit::ledger::commit_circuit_advance`]. When it carried
//! full agent outputs and evidence blobs, each advance carried tens of
//! kilobytes of context through the commit, and every query that inspected
//! the context had to page and parse all of it — the retention sweep's
//! `recovery.from_run_id` probe parsed every candidate run's context once per
//! candidate.
//!
//! This module owns the split in one place:
//!
//! - [`SplitContext::parse`] separates the decision-relevant values (which stay
//!   in `context_json`) from the bulky ones (which move to
//!   `circuit_run_artifacts`).
//! - [`write_context`] writes the bulky values — once, when they change — and
//!   returns the JSON for the `context_json` column.
//! - [`merge_into`] / [`load_context`] read the bulky values back on demand.
//!
//! The split happens inside the engine's single atomic commit point
//! (`ledger::commit_circuit_advance_inner`), so a crash can never leave a
//! context half-split.
//!
//! ## What a reader has to do
//!
//! A run's context is two halves. A reader that wants the whole context reads
//! the column *and* the side table: [`load_context`], [`load_context_map`],
//! [`joined_context`] and [`merge_values`] join them, and the operator-facing
//! projections join them too ([`join_context_json`]) because the Probe reads a
//! review report and the issue body it searches by out of the run's
//! `context_json`.
//!
//! A reader that wants only the decision-relevant half reads the column alone.
//! The worker's freshness fence is one of them: it compares the stored column
//! with the one it loaded, so it must never see a joined context.
//!
//! ## The digest
//!
//! `context_json` carries one reserved key, [`DIGEST_KEY`], that fingerprints
//! the stored bulky set. The worker's freshness fence compares the stored
//! `context_json` with the one it loaded; without the fingerprint a concurrent
//! writer that changed only a bulky value would leave the column byte
//! identical, the fence would not fire, and the worker would write back a
//! context that no longer held the newer value. The digest makes a bulky
//! change visible in the column the fences already read. A run with nothing
//! bulky in it carries no digest, so its context stays exactly what it was.
//!
//! The digest is a storage fingerprint, not part of the run's context: the
//! joined views drop it, so the context the worker and the Probe read is the
//! context the run holds.
//!
//! ## A stored value the column does not mention
//!
//! A write replaces the run's bulky set: the values it carries are stored,
//! the values that became small are deleted, and a stored value that the new
//! context does not mention at all *stays*. The context a writer submits is
//! the whole context, so an omission means "this value is unchanged", not
//! "this value is gone". The two paths that really clear a node's working
//! state — the operator's step recovery and a review extension — delete the
//! stored values explicitly before they write, so nothing is resurrected.
//!
//! The rule has a consequence worth knowing before a stepper change leans on
//! it: [`CircuitContext::remove`] does not clear a bulky value. A removed key
//! is absent from the context the writer submits, and an absent key means
//! "unchanged", so the stored copy stays and comes back on the next read.
//! Removing a key therefore works for the small values in the column — a
//! counter, a flag, a stamp — and a bulky value has to be cleared with
//! [`delete_node_values`] or [`delete_values_with_prefix`]. As the stepper
//! evolves, a removal of a bulky key needs one of those calls beside it.

use std::collections::BTreeMap;

use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

use crate::circuit::context::CircuitContext;
use crate::db::SqlResult;

/// Context values longer than this move out of `context_json`.
///
/// The threshold is a storage decision, not a semantic one: a value above it
/// is an agent output, an evidence blob or a trigger body — text a run reads
/// on demand rather than a flag the stepper decides on. Everything at or below
/// it stays in the column, so the decision-relevant half of a context is
/// still one small row.
pub const BULKY_VALUE_BYTES: usize = 1024;

/// The reserved context key that fingerprints the stored bulky set.
pub const DIGEST_KEY: &str = "context.artifact_digest";

/// One bulky context value on its way to the side table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkyValue {
    /// The dotted context path, e.g. `node.reviewer.output`.
    pub key: String,
    /// The circuit node the value belongs to; empty for run-level values.
    pub node_id: String,
    /// The attempt the value belongs to; `0` when it is not attempt-scoped.
    pub attempt: i64,
    /// The value itself.
    pub value: String,
}

/// The two halves of a split context.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SplitContext {
    /// The decision-relevant values, keyed by their dotted context path.
    pub small: BTreeMap<String, String>,
    /// The bulky values, in the order they were read.
    pub bulky: Vec<BulkyValue>,
    /// The original payload when it was not a flat string map. The split is a
    /// storage optimisation, not a validation step, so such a payload is
    /// stored verbatim rather than rejected.
    pub verbatim: Option<String>,
}

impl SplitContext {
    /// Split a serialised context into the part that stays in `context_json`
    /// and the bulky values that move to the side table.
    pub fn parse(context_json: &str) -> Self {
        let Ok(map) = serde_json::from_str::<BTreeMap<String, String>>(context_json) else {
            return Self {
                small: BTreeMap::new(),
                bulky: Vec::new(),
                verbatim: Some(context_json.to_string()),
            };
        };
        let mut small = BTreeMap::new();
        let mut bulky = Vec::new();
        for (key, value) in map {
            if key == DIGEST_KEY {
                // Recomputed from the stored set on every write.
                continue;
            }
            if value.len() > BULKY_VALUE_BYTES && !always_small(&key) {
                bulky.push(BulkyValue {
                    node_id: node_id_of(&key).to_string(),
                    attempt: attempt_of(&key),
                    key,
                    value,
                });
            } else {
                small.insert(key, value);
            }
        }
        Self {
            small,
            bulky,
            verbatim: None,
        }
    }

    /// The JSON for the `context_json` column, without the digest.
    pub fn small_json(&self) -> String {
        match &self.verbatim {
            Some(raw) => raw.clone(),
            None => encode(&self.small),
        }
    }
}

/// Create the side table. Idempotent: the migration runner calls it on every
/// launch so a database that predates the table heals itself.
///
/// The table carries no index beyond its primary key. Every read filters by
/// `run_id`, which the primary key's leftmost column already covers, and the
/// node-scoped delete uses a key-prefix range instead of the `node_id` column.
/// An index nothing reads is a page written on every first insert — the
/// opposite of what this table is for.
pub fn ensure_table(conn: &Connection) -> SqlResult<()> {
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

/// Write a run's context: the bulky values to the side table, the rest to
/// `context_json`. Returns the JSON for the `context_json` column.
///
/// The caller owns the transaction: the split must land in the same
/// transaction as the run-state and step writes it belongs to.
pub fn write_context(tx: &Connection, run_id: i64, context_json: &str) -> SqlResult<String> {
    persist_split(tx, run_id, &SplitContext::parse(context_json))
}

/// The part of [`write_context`] that assumes the run row already exists, so
/// the stored bulky set can be read back to fingerprint it.
///
/// A stored value the new context does not mention stays stored: the context
/// a writer submits is the whole context, so an omission means "unchanged".
/// A value that is small again supersedes the stored copy and is deleted. See
/// the module's "A stored value the column does not mention" for what that
/// means for a caller that removes a key.
pub fn persist_split(tx: &Connection, run_id: i64, split: &SplitContext) -> SqlResult<String> {
    if split.verbatim.is_some() {
        return Ok(split.small_json());
    }
    // Read the stored set once. A value that did not change is left alone
    // entirely — not even touched — so a repeated advance does not dirty the
    // side table's pages.
    let stored: BTreeMap<String, String> = load_values(tx, run_id)?.into_iter().collect();
    let mut settled = stored.clone();
    for artifact in &split.bulky {
        if stored.get(&artifact.key) == Some(&artifact.value) {
            continue;
        }
        tx.execute(
            "INSERT INTO circuit_run_artifacts (run_id, key, node_id, attempt, value, bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(run_id, key) DO UPDATE SET
                 value = excluded.value,
                 bytes = excluded.bytes,
                 updated_at = datetime('now')",
            params![
                run_id,
                artifact.key,
                artifact.node_id,
                artifact.attempt,
                artifact.value,
                artifact.value.len() as i64
            ],
        )?;
        settled.insert(artifact.key.clone(), artifact.value.clone());
    }
    // A value that is small again supersedes the stored copy of it.
    for key in stored.keys() {
        if split.small.contains_key(key) {
            tx.execute(
                "DELETE FROM circuit_run_artifacts WHERE run_id = ?1 AND key = ?2",
                params![run_id, key],
            )?;
            settled.remove(key);
        }
    }
    let digest = digest_of(&settled);
    let mut small = split.small.clone();
    // The fingerprint only earns its place once there is something to
    // fingerprint: a run with no bulky values carries no digest, so its
    // context stays exactly what it was.
    if !settled.is_empty() {
        small.insert(DIGEST_KEY.to_string(), digest);
    }
    Ok(encode(&small))
}

/// The bulky values stored for a run.
pub fn load_values(conn: &Connection, run_id: i64) -> SqlResult<Vec<(String, String)>> {
    let mut statement = conn
        .prepare("SELECT key, value FROM circuit_run_artifacts WHERE run_id = ?1 ORDER BY key")?;
    let rows = statement.query_map([run_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

/// Merge the stored bulky values into a context.
///
/// A value already present in the context wins: the column is the newer
/// write. The two halves never overlap in practice — a key is either small in
/// `context_json` or bulky in the side table — but the rule keeps a stale
/// side-table copy from resurrecting a value the column has moved past.
pub fn merge_into(conn: &Connection, run_id: i64, context: &mut CircuitContext) -> SqlResult<()> {
    for (key, value) in load_values(conn, run_id)? {
        // A lifecycle blocker is runtime state, not context: the context
        // filter drops it on the way in, so joining it back would resurrect a
        // veto the filter removed.
        if crate::circuit::context::is_lifecycle_blocker_key(&key) {
            continue;
        }
        if context.get(&key).is_none() {
            context.set(&key, value);
        }
    }
    // The digest fingerprints the stored set. It is a storage detail, not part
    // of the run's context, so the joined view drops it.
    context.remove(DIGEST_KEY);
    Ok(())
}

/// Join a column's context with the stored bulky values, as a flat map.
///
/// A context that is not a flat map is kept verbatim in the column (see
/// [`SplitContext::parse`]), so it has no map to join into and the map comes
/// back empty — in that case the column already is the whole context.
///
/// The map keeps a lifecycle blocker, because it is a faithful view of what
/// the run holds. [`joined_context`] is the view the worker reads, and that
/// one drops the blocker, the way the context filter always did.
pub fn merge_values(column_json: &str, stored: &[(String, String)]) -> BTreeMap<String, String> {
    let mut map = serde_json::from_str::<BTreeMap<String, String>>(column_json).unwrap_or_default();
    for (key, value) in stored {
        map.entry(key.clone()).or_insert_with(|| value.clone());
    }
    map.remove(DIGEST_KEY);
    map
}

/// One run's full context, from a column's JSON and preloaded bulky values.
///
/// The worker-facing read. A context that is not a flat map is an error, the
/// same way [`load_context`] reports it.
pub fn joined_context(
    column_json: &str,
    stored: &[(String, String)],
) -> Result<CircuitContext, crate::db::DbError> {
    let mut context =
        CircuitContext::from_json(column_json).map_err(crate::db::DbError::Rendered)?;
    for (key, value) in stored {
        // A lifecycle blocker is runtime state, not context: the context
        // filter drops it on the way in, so joining it back would resurrect a
        // veto the filter removed.
        if crate::circuit::context::is_lifecycle_blocker_key(key) {
            continue;
        }
        if context.get(key).is_none() {
            context.set(key, value);
        }
    }
    context.remove(DIGEST_KEY);
    Ok(context)
}

/// Rejoin the stored bulky values into a run's `context_json`.
///
/// The operator-facing projections hand the frontend one JSON string per run,
/// and the Probe reads the bulky half out of it: a review report, the issue
/// body it searches by. The column holds only the small half, so the
/// projection rejoins the two before the run leaves the database layer.
pub fn join_context_json(column_json: &str, stored: &[(String, String)]) -> String {
    let Ok(mut map) = serde_json::from_str::<BTreeMap<String, String>>(column_json) else {
        // A context that is not a flat map is kept verbatim in the column, so
        // the column already is the whole context.
        return column_json.to_string();
    };
    for (key, value) in stored {
        map.entry(key.clone()).or_insert_with(|| value.clone());
    }
    map.remove(DIGEST_KEY);
    serde_json::to_string(&map).unwrap_or_else(|_| column_json.to_string())
}

/// The stored bulky values of several runs, keyed by run id.
pub fn load_values_for_runs(
    conn: &Connection,
    run_ids: &[i64],
) -> SqlResult<BTreeMap<i64, Vec<(String, String)>>> {
    let mut values: BTreeMap<i64, Vec<(String, String)>> = BTreeMap::new();
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
            values.entry(run_id).or_default().push((key, value));
        }
    }
    Ok(values)
}

/// Load one run's full context: the column plus the stored bulky values.
pub fn load_context(conn: &Connection, run_id: i64) -> Result<CircuitContext, crate::db::DbError> {
    let raw: String = conn.query_row(
        "SELECT context_json FROM autopilot_circuit_runs WHERE id = ?1",
        [run_id],
        |row| row.get(0),
    )?;
    let mut context = CircuitContext::from_json(&raw).map_err(crate::db::DbError::Rendered)?;
    merge_into(conn, run_id, &mut context)?;
    Ok(context)
}

/// Load one run's full context through the read pool.
///
/// The worker calls this instead of reading `context_json` directly: the
/// bulky half of the context lives in the side table, and the stepper needs
/// it to resolve `{{node.reviewer.output}}` and to read a step's evidence.
pub fn load_run_context(run_id: i64) -> Result<CircuitContext, crate::db::DbError> {
    let db = crate::db::read_conn()?;
    load_context(&db, run_id)
}

/// Load one run's full context as a flat map, for the callers that work on
/// the map rather than on a [`CircuitContext`].
pub fn load_context_map(
    conn: &Connection,
    run_id: i64,
) -> Result<BTreeMap<String, String>, crate::db::DbError> {
    let raw: String = conn.query_row(
        "SELECT context_json FROM autopilot_circuit_runs WHERE id = ?1",
        [run_id],
        |row| row.get(0),
    )?;
    let mut map = serde_json::from_str::<BTreeMap<String, String>>(&raw)?;
    for (key, value) in load_values(conn, run_id)? {
        map.entry(key).or_insert(value);
    }
    map.remove(DIGEST_KEY);
    Ok(map)
}

/// Drop the stored bulky values of one circuit node.
///
/// The operator's step recovery clears a node's whole working state, so the
/// side table has to follow: a leftover copy would come back on the next read
/// and the recovered step would judge stale evidence.
pub fn delete_node_values(conn: &Connection, run_id: i64, node_id: &str) -> SqlResult<usize> {
    delete_values_with_prefix(conn, run_id, &format!("node.{node_id}"))
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
pub fn delete_values_with_prefix(
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
fn digest_of(values: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    for (key, value) in values {
        hasher.update(key.as_bytes());
        hasher.update([0u8]);
        hasher.update(value.as_bytes());
        hasher.update([0u8]);
    }
    hex::encode(hasher.finalize())
}

/// Context paths the ledger's wait tracking reads.
///
/// The wait history is written by comparing the previous and the next
/// context, so these paths have to stay in `context_json` even when they are
/// long: a blocker message or a capacity window that moved to the side table
/// would be invisible to the comparison and the wait would never be recorded.
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

fn encode(map: &BTreeMap<String, String>) -> String {
    serde_json::to_string(map).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
#[path = "artifacts_tests.rs"]
mod tests;
