//! One-shot compaction of duplicate Circuit Run History observations (issue #2600).
//!
//! The append path now refuses to write an observation that is already on
//! record ([`super::evidence::append_history`]), so a duplicate can only be
//! *historical*: a hub that ran before that guard holds the same observation
//! thousands of times. The reported hub held 1.95M history rows for 115k
//! distinct observations — one run alone carried 132k copies of a single
//! rollout completion, and another 30k copies of one status projection.
//!
//! This sweep removes those copies. Each group of copies keeps its **first**
//! row (when Buildmesh first saw the fact) and its **last** row (when it last
//! saw it); the copies in between are deleted. No distinct fact is lost: the
//! deleted rows are byte-for-byte the two that stay, and the run's revision
//! still points at a row that exists, because the newest row of the run is
//! the last of its own group and is kept.
//!
//! ## Bounded work
//!
//! The sweep runs from the ledger GC worker, so it has to behave on a
//! database that holds a gigabyte of history, and it must not freeze the
//! application while it does:
//!
//! - the read runs in batches on a **reader** connection, released between
//!   batches, so the writer lock is not held for more than one batch;
//! - the duplicates are found in memory — a group key and a payload hash,
//!   not the payloads — so the pass reads the rows in id order;
//! - the deletes and the hash writes go out in small batches, each its own
//!   transaction on the **writer**, released between batches. A delete that
//!   frees a gigabyte of pages would otherwise write that gigabyte into the
//!   write-ahead log; the automatic checkpoint keeps the log bounded between
//!   batches (see [`crate::db::WAL_SIZE_LIMIT_BYTES`]);
//! - the sweep records that it ran, so it runs once rather than on each
//!   tick.

use std::collections::HashMap;

use rusqlite::{params, Connection};

use crate::db::SqlResult;

/// How many rows one read batch pulls.
///
/// The read runs on a reader connection that is released between batches, so
/// one slow batch does not hold a reader — and the writer — for minutes.
const READ_BATCH: usize = 20_000;

/// How many rows one delete batch removes.
///
/// The batch is one transaction, so its size bounds how much the
/// write-ahead log can grow before the automatic checkpoint truncates it.
const DELETE_BATCH: usize = 5_000;

/// How many survivor rows one hash batch updates.
const HASH_BATCH: usize = 5_000;

/// The `app_settings` key that records the sweep has run.
const COMPACTED_FLAG: &str = "circuit_history_observations_compacted_v1";

/// The identity of one observation: the run, node, attempt and source it
/// belongs to, plus the hash of its payload.
///
/// Two observations with the same key are the same fact recorded twice. The
/// payload itself is not part of the key — only its hash — so the map this
/// builds holds one small entry per group rather than a copy of each
/// payload.
#[derive(PartialEq, Eq, Hash)]
struct GroupKey {
    run_id: i64,
    node_id: String,
    attempt: i64,
    source: String,
    hash: String,
}

/// What the sweep found: the rows to delete, and the rows that stay.
struct CompactionPlan {
    /// Rows that are a copy of another row in their group.
    duplicates: Vec<i64>,
    /// The rows that stay, with the hash the dedupe index needs.
    survivors: Vec<(i64, String)>,
}

/// The sweep: collapse the duplicate observation rows.
///
/// Returns the number of duplicate rows deleted. Callers that want the sweep
/// to run at most once per database use
/// [`compact_duplicate_observations_once`], which records that it ran.
pub(crate) fn compact_duplicate_observations() -> SqlResult<usize> {
    // The read runs on a reader connection, released between batches, so the
    // writer lock is free for the rest of the application while the history
    // is walked.
    let plan = plan_duplicates(|cursor| {
        let db = crate::db::read_conn()?;
        read_batch(&db, cursor)
    })?;
    apply_plan(&plan)
}

/// The sweep, once per database.
///
/// Returns the number of duplicate rows deleted. A second call on a compacted
/// database is a no-op: the sweep records that it ran, so a database that has
/// already been compacted is not walked again.
pub fn compact_duplicate_observations_once() -> SqlResult<usize> {
    if sweep_already_ran()? {
        return Ok(0);
    }
    let deleted = compact_duplicate_observations()?;
    let db = crate::db::write_conn();
    db.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, '1')",
        params![COMPACTED_FLAG],
    )?;
    Ok(deleted)
}

/// Whether the sweep has already run on this database.
fn sweep_already_ran() -> SqlResult<bool> {
    let db = crate::db::read_conn()?;
    let ran: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM app_settings WHERE key = ?1 AND value = '1')",
            params![COMPACTED_FLAG],
            |row| row.get(0),
        )
        .unwrap_or(false);
    Ok(ran)
}

/// Find the duplicate observation rows and the survivors.
///
/// The rows are read in id order, so the first row seen for a group is the
/// group's first and the latest is its last. When a group repeats, the
/// *previous* copy already has a successor, so it is a middle — unless it is
/// the group's first, which stays. The last copy of a group is not deleted,
/// because no row follows it.
fn plan_duplicates<F>(mut read: F) -> SqlResult<CompactionPlan>
where
    F: FnMut(i64) -> SqlResult<Vec<(i64, GroupKey)>>,
{
    // group key -> (the group's first row, the group's latest row so far)
    let mut groups: HashMap<GroupKey, (i64, i64)> = HashMap::new();
    let mut duplicates: Vec<i64> = Vec::new();
    let mut cursor = 0i64;
    loop {
        let batch = read(cursor)?;
        let Some(last) = batch.last().map(|(id, _)| *id) else {
            break;
        };
        for (id, key) in batch {
            let existing = groups.get(&key).copied();
            match existing {
                None => {
                    groups.insert(key, (id, id));
                }
                Some((first, previous)) => {
                    if previous != first {
                        duplicates.push(previous);
                    }
                    groups.insert(key, (first, id));
                }
            }
        }
        cursor = last;
    }
    let survivors = groups
        .into_iter()
        .flat_map(|(key, (first, last))| {
            let hash = key.hash;
            if first == last {
                vec![(first, hash)]
            } else {
                vec![(first, hash.clone()), (last, hash)]
            }
        })
        .collect();
    Ok(CompactionPlan {
        duplicates,
        survivors,
    })
}

/// Read one batch of observation rows, starting after `cursor`.
fn read_batch(conn: &Connection, cursor: i64) -> SqlResult<Vec<(i64, GroupKey)>> {
    let mut stmt = conn.prepare(
        "SELECT id, run_id, COALESCE(node_id, ''), COALESCE(attempt, -1), COALESCE(source, ''), detail
         FROM circuit_run_history
         WHERE kind = 'observation' AND id > ?1
         ORDER BY id LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![cursor, READ_BATCH as i64], |row| {
        let id: i64 = row.get(0)?;
        let detail: String = row.get(5)?;
        Ok((
            id,
            GroupKey {
                run_id: row.get(1)?,
                node_id: row.get(2)?,
                attempt: row.get(3)?,
                source: row.get(4)?,
                hash: super::evidence::detail_hash(&detail),
            },
        ))
    })?;
    rows.collect::<Result<Vec<_>, _>>()
}

/// Apply the plan: delete the duplicates, then hash the survivors.
fn apply_plan(plan: &CompactionPlan) -> SqlResult<usize> {
    let mut deleted = 0usize;
    for chunk in plan.duplicates.chunks(DELETE_BATCH) {
        // The writer is taken and released per batch: the lock is not held
        // for more than one batch's worth of deletes.
        let mut db = crate::db::write_conn();
        delete_rows(&mut db, chunk)?;
        deleted += chunk.len();
    }
    for chunk in plan.survivors.chunks(HASH_BATCH) {
        let mut db = crate::db::write_conn();
        hash_rows(&mut db, chunk)?;
    }
    Ok(deleted)
}

/// Delete one batch of duplicate rows in its own transaction.
fn delete_rows(db: &mut Connection, chunk: &[i64]) -> SqlResult<()> {
    let tx = db.transaction()?;
    {
        let mut stmt = tx.prepare("DELETE FROM circuit_run_history WHERE id = ?1")?;
        for id in chunk {
            stmt.execute(params![id])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Give one batch of surviving rows the hash the dedupe index is built on.
///
/// Without it a surviving row is invisible to the append-path dedupe, so the
/// same observation could still be written one more time after the sweep.
fn hash_rows(db: &mut Connection, chunk: &[(i64, String)]) -> SqlResult<()> {
    let tx = db.transaction()?;
    {
        let mut stmt =
            tx.prepare("UPDATE circuit_run_history SET detail_sha256 = ?2 WHERE id = ?1")?;
        for (id, hash) in chunk {
            stmt.execute(params![id, hash])?;
        }
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
#[path = "history_compaction_tests.rs"]
mod tests;
