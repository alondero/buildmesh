//! The logical context seam for a Circuit Run (issue #2672).
//!
//! A run's context is stored in two halves: the decision-relevant values in
//! `autopilot_circuit_runs.context_json` and the bulky ones in
//! `circuit_run_artifacts` (issue #2601). That split is a storage decision,
//! and before this module it leaked into every caller. A reader had to know
//! which half a value lived in and join the two by hand; a writer had to know
//! that a key it did not mention meant "unchanged" rather than "gone"; and a
//! caller that wanted a value removed had to delete the physical row beside
//! the logical removal, or the value came back on the next read.
//!
//! This module is the seam between the callers and that storage. Callers ask
//! for logical reads and logical changes; the adapter owns the size
//! threshold, the split, the join, the physical deletes and the storage
//! fingerprints.
//!
//! ## The three contracts
//!
//! - [`StoredContext`] — the stored context and its [`ContextRevision`]. A
//!   reader that needs only the decision-relevant half — a freshness check,
//!   an admission probe, a run's review budget — reads this and never
//!   touches the side table.
//! - [`LogicalContext`] — the materialized logical context: every value the
//!   run holds, both halves joined.
//! - [`ContextChange`] — a change to a run's context, in one of two shapes:
//!   a [`ContextMutation`], which is partial (a key it does not mention is
//!   unchanged, and it can remove keys and reset scopes explicitly), or a
//!   [`ContextReplacement`], which is complete (a key it does not mention is
//!   deleted).
//!
//! ## Why the two change shapes are separate types
//!
//! They answer different questions. "Here is the whole context, take it or
//! leave it" and "here is a change to part of the context" cannot share a
//! type without one of them lying: a partial change that dropped its
//! omitted keys would delete values it never loaded, and a complete
//! replacement that kept them would never delete anything. Naming the
//! difference makes the caller say which one it means.
//!
//! ## The fingerprint
//!
//! The column carries one reserved key that fingerprints the stored bulky
//! set. A change to the bulky half alone would otherwise leave the column
//! byte-identical, and the freshness fence — which reads the column — would
//! not fire, so a worker could write back a context that no longer held the
//! newer value. The fingerprint makes a bulky change visible in the column
//! the fence already reads. It is a storage detail: it never appears in a
//! rendered logical context, and it is not a value a caller reads.
//!
//! ## Runtime blockers
//!
//! A lifecycle blocker is runtime state, not context: it is a live veto
//! projected onto a run while an agent is blocked. It never travels in a
//! persisted context, and a historical one is cleaned up when the context is
//! written. A logical read never resurrects one.
//!
//! ## The seam's own rules
//!
//! - A value longer than the storage threshold lives in the side table, and
//!   everything else lives in the column. The threshold is a storage detail:
//!   a caller's set, get and remove mean the same thing on either side of
//!   it.
//! - A value that is unchanged is not rewritten, so a repeated change does
//!   not dirty the side table's pages.
//! - The caller owns the transaction: a change lands in the same
//!   transaction as the run-state and step writes it belongs to, so a crash
//!   can never leave a context half-written.
//!
//! ## What a change hands back
//!
//! [`apply`] returns an [`AppliedContext`]: the column JSON to write, and the
//! run's logical context on both sides of the change. A caller that needs to
//! know what moved — the wait history, for one — reads those two views
//! instead of reading either half of the storage again.

mod storage;

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::circuit::context::CircuitContext;
use crate::db::{DbError, SqlResult};

use storage::DIGEST_KEY;

/// An opaque freshness token for a run's stored context.
///
/// The token fingerprints the whole stored context — the decision-relevant
/// half in the column and the fingerprint of the bulky half — so a change to
/// either half moves it. It is a storage detail: it never appears in a
/// rendered logical context, and a caller cannot read meaning into it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContextRevision(String);

impl Default for ContextRevision {
    /// The revision of an empty stored context.
    fn default() -> Self {
        Self::of_column("")
    }
}

impl ContextRevision {
    /// The revision of a stored context column.
    ///
    /// A caller mints the expected revision from the column it loaded, and
    /// compares it with the revision of the column the database holds now.
    /// Two columns with the same revision are the same stored context.
    pub fn of_column(column_json: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(column_json.as_bytes());
        Self(hex::encode(hasher.finalize()))
    }
}

/// A run's stored context: the decision-relevant half, plus the revision
/// that fingerprints the whole stored context.
///
/// Reading this never touches the side table, so the freshness, admission
/// and retention paths stay free of bulky rehydration.
#[derive(Debug, Clone)]
pub struct StoredContext {
    /// The decision-relevant values.
    small: BTreeMap<String, String>,
    /// The freshness token of the whole stored context.
    revision: ContextRevision,
}

impl StoredContext {
    /// The stored view of a column a caller already holds.
    ///
    /// A caller that has the run's column in hand — because it loaded the run
    /// row — reads the decision-relevant half through this, without asking
    /// the database for it again.
    pub fn of_column(column_json: &str) -> Self {
        Self {
            small: stored_small(column_json),
            revision: ContextRevision::of_column(column_json),
        }
    }

    /// The freshness token of the stored context.
    pub fn revision(&self) -> &ContextRevision {
        &self.revision
    }

    /// One decision-relevant value.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.small.get(key).map(String::as_str)
    }
}

/// A run's materialized logical context: every value the run holds, the
/// decision-relevant half and the bulky half joined.
///
/// The freshness token belongs to the storage, not to this view. A caller
/// that fences a later write mints the expected revision from the column it
/// holds ([`ContextRevision::of_column`]) or reads it through
/// [`StoredContext`]. A logical context that carried its own revision would
/// invite a caller to fence a write against the context it had just read —
/// comparing the stored context with itself, which never fires.
#[derive(Debug, Clone, Default)]
pub struct LogicalContext {
    values: BTreeMap<String, String>,
}

/// Two logical contexts are equal when they hold the same values.
impl PartialEq for LogicalContext {
    fn eq(&self, other: &Self) -> bool {
        self.values == other.values
    }
}

impl Eq for LogicalContext {}

impl LogicalContext {
    /// Join a stored column and the stored bulky values into the run's
    /// logical context.
    ///
    /// A stored payload that is not a flat map has no logical context: this
    /// reports that as an error rather than inventing one. A value already
    /// in the column wins — the column is the newer write.
    pub fn join(column_json: &str, stored: &BTreeMap<String, String>) -> Result<Self, DbError> {
        let values: BTreeMap<String, String> = serde_json::from_str(column_json)
            .map_err(|error| DbError::Rendered(format!("invalid context_json: {error}")))?;
        Ok(Self::from_parts(values, stored))
    }

    /// Join a stored column and the stored bulky values, reading as far as
    /// the payload allows.
    ///
    /// A stored payload that is not a flat map has no logical context of its
    /// own, but the bulky half is still part of the run's context. This
    /// returns what it can read, for the callers that look for one value
    /// rather than for the whole context.
    pub fn join_best_effort(column_json: &str, stored: &BTreeMap<String, String>) -> Self {
        let values: BTreeMap<String, String> =
            serde_json::from_str(column_json).unwrap_or_default();
        Self::from_parts(values, stored)
    }

    fn from_parts(mut values: BTreeMap<String, String>, stored: &BTreeMap<String, String>) -> Self {
        // The fingerprint is a storage detail, not a value of the run's
        // context.
        values.remove(DIGEST_KEY);
        for (key, value) in stored {
            values.entry(key.clone()).or_insert_with(|| value.clone());
        }
        // A lifecycle blocker is runtime state, not context: a logical read
        // never resurrects one.
        values.retain(|key, _| !crate::circuit::context::is_lifecycle_blocker_key(key));
        Self { values }
    }

    /// One value of the run's context.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// Every value the run holds.
    pub fn values(&self) -> &BTreeMap<String, String> {
        &self.values
    }

    /// The runtime view: the context a run may act on.
    ///
    /// This is the pure [`CircuitContext`] the stepper reads and writes. It
    /// is the logical context, so it carries no lifecycle blocker: a blocker
    /// is a live veto, not a value of the run.
    pub fn runtime(&self) -> CircuitContext {
        CircuitContext::from_values(self.values.clone())
    }
}
impl From<LogicalContext> for ContextReplacement {
    fn from(logical: LogicalContext) -> Self {
        Self {
            values: logical.values,
            verbatim: None,
        }
    }
}

/// A dotted context prefix a reset clears.
///
/// The scope covers every key that starts with the prefix and a dot:
/// `node.reviewer` covers `node.reviewer.output` and
/// `node.reviewer.evidence.1`, and not `node.reviewer` itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextScope {
    segment: String,
}

impl ContextScope {
    /// A scope covering every key under a dotted segment.
    pub fn segment(segment: impl Into<String>) -> Self {
        Self {
            segment: segment.into(),
        }
    }

    /// A scope covering one circuit node's whole working state.
    pub fn node(node_id: &str) -> Self {
        Self::segment(format!("node.{node_id}"))
    }

    /// The dotted prefix the scope covers.
    pub fn segment_str(&self) -> &str {
        &self.segment
    }
}

/// A partial change to a run's logical context.
///
/// A key the mutation does not mention is unchanged — in both halves of the
/// storage. A key it removes is removed from both halves. A scope it resets
/// loses every key under that prefix, in both halves.
///
/// The removals and the resets are applied before the puts, so a put is the
/// last word on the key it names.
#[derive(Debug, Clone, Default)]
pub struct ContextMutation {
    puts: BTreeMap<String, String>,
    removals: BTreeSet<String>,
    resets: Vec<ContextScope>,
}

impl ContextMutation {
    /// A mutation that changes nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set one value.
    ///
    /// The value lands in whichever half of the storage it belongs to; the
    /// caller does not choose, and does not have to know.
    pub fn put(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.puts.insert(key.into(), value.into());
        self
    }

    /// Remove one value, from whichever half holds it.
    pub fn remove(&mut self, key: impl Into<String>) -> &mut Self {
        self.removals.insert(key.into());
        self
    }

    /// Clear every value under a scope.
    pub fn reset(&mut self, scope: ContextScope) -> &mut Self {
        self.resets.push(scope);
        self
    }

    /// A mutation that carries a context as it stands.
    ///
    /// Every value the context holds is set, and every key it removed is
    /// removed. A key the context does not mention is unchanged, so a value
    /// another writer stored in the meantime survives.
    pub fn from_context(context: &CircuitContext) -> Self {
        let mut mutation = Self::new();
        for (key, value) in context.values() {
            mutation.put(key.clone(), value.clone());
        }
        for key in context.removed() {
            mutation.remove(key.to_string());
        }
        mutation
    }
}

/// A complete replacement of a run's logical context.
///
/// Every key the replacement omits is deleted, in both halves of the
/// storage. A replacement is the only change that can clear a value its
/// author never loaded, so it is named and typed separately from a
/// [`ContextMutation`]: a caller that means "this is everything" says so.
///
/// A replacement cannot be built from a partial view. [`StoredContext`] is
/// the partial view — the decision-relevant half alone — and it offers no
/// way to become a replacement.
#[derive(Debug, Clone, Default)]
pub struct ContextReplacement {
    values: BTreeMap<String, String>,
    /// The payload as it stands when it is not a flat string map.
    ///
    /// The split is a storage optimisation, not a validation step, so such a
    /// payload is stored as it stands rather than rejected or normalised.
    verbatim: Option<String>,
}

impl ContextReplacement {
    /// A replacement that holds these values.
    ///
    /// The reserved fingerprint key is not a value of the run's context: the
    /// seam recomputes it from the stored set on every write, so a value
    /// that claims to be one is dropped rather than stored.
    pub fn with_values(values: BTreeMap<String, String>) -> Self {
        Self {
            values: values
                .into_iter()
                .filter(|(key, _)| key != DIGEST_KEY)
                .collect(),
            verbatim: None,
        }
    }

    /// A replacement that holds the values of a whole serialized context.
    ///
    /// A payload that is not a flat string map has no values to take: it is
    /// kept as it stands, so a historical payload is neither normalised nor
    /// erased.
    pub fn whole(context_json: impl AsRef<str>) -> Self {
        let context_json = context_json.as_ref();
        match serde_json::from_str::<BTreeMap<String, String>>(context_json) {
            Ok(values) => Self::with_values(values),
            Err(_) => Self {
                values: BTreeMap::new(),
                verbatim: Some(context_json.to_string()),
            },
        }
    }

    /// The payload as it stands, when it is not a flat string map.
    pub fn verbatim(&self) -> Option<&str> {
        self.verbatim.as_deref()
    }
}

/// A change to a run's logical context.
#[derive(Debug, Clone)]
pub enum ContextChange {
    /// A partial change. A key it does not mention is unchanged.
    Mutation(ContextMutation),
    /// A complete replacement. A key it does not mention is deleted.
    Replacement(ContextReplacement),
}

impl From<ContextMutation> for ContextChange {
    fn from(mutation: ContextMutation) -> Self {
        Self::Mutation(mutation)
    }
}

impl From<ContextReplacement> for ContextChange {
    fn from(replacement: ContextReplacement) -> Self {
        Self::Replacement(replacement)
    }
}

impl ContextChange {
    /// The change that carries a context as it stands.
    ///
    /// Every value the context holds is set, and every key it removed is
    /// removed. A key the context does not mention is unchanged, so a value
    /// another writer stored in the meantime survives.
    pub fn from_context(context: &CircuitContext) -> Self {
        Self::Mutation(ContextMutation::from_context(context))
    }
}

/// Create the side table. Idempotent: the migration runner calls it on every
/// launch so a database that predates the table heals itself.
pub fn ensure_table(conn: &Connection) -> SqlResult<()> {
    storage::ensure_table(conn)
}

/// Delete the stored half of the runs a caller's query names.
///
/// A run's stored half rides with the run: a sweep that removes runs removes
/// their stored values with them. The caller owns the query that names the
/// runs; the seam owns the table they live in.
pub fn delete_stored_values_for_runs(
    conn: &Connection,
    run_ids_query: &str,
    params: impl rusqlite::Params,
) -> SqlResult<usize> {
    storage::delete_stored_values_for_runs(conn, run_ids_query, params)
}

/// The run's history revision, when the run's stored context is still the
/// one the caller loaded.
///
/// A caller that loaded a context and later writes it compares the two: a
/// context that moved on means the caller's view is stale. The run's state is
/// part of that view, so a run that has moved to another state is stale in
/// the same way.
///
/// The check reads the stored column alone: the bulky half is never
/// rehydrated to answer it. The column and the history revision come from
/// one statement, so they come from the same snapshot: a write that lands
/// between two reads would report a revision the caller never saw, and the
/// fence would accept what it should refuse.
pub fn revision_if_unchanged(
    conn: &Connection,
    run_id: i64,
    state: &str,
    expected: &ContextRevision,
) -> SqlResult<Option<i64>> {
    let Some((column_json, revision)) = storage::read_column_and_history(conn, run_id, state)?
    else {
        return Ok(None);
    };
    if StoredContext::of_column(&column_json).revision() != expected {
        return Ok(None);
    }
    Ok(Some(revision))
}

/// Whether a stored context carries values the side table holds.
///
/// The one-time backfill that moves a run's context into the side table
/// asks this before it writes: a run with nothing bulky has nothing to
/// move, and its column stays exactly as it is.
fn has_bulky_values(column_json: &str) -> bool {
    let values = match small_values(column_json) {
        Ok(values) => values,
        Err(_) => return false,
    };
    values
        .iter()
        .any(|(key, value)| key != DIGEST_KEY && storage::is_bulky(key, value))
}

/// Move a run's stored context into the split storage.
///
/// The one-time backfill that adopts the split for the contexts written
/// before it asks the seam to do the work: which values move, and where they
/// go, is the seam's business, not the migration's.
///
/// A run whose context has nothing to move keeps the column it has, and
/// this reports that by returning nothing for the caller to write.
pub fn adopt_split_storage(
    conn: &Connection,
    run_id: i64,
    column_json: &str,
) -> SqlResult<Option<String>> {
    if !has_bulky_values(column_json) {
        return Ok(None);
    }
    let replacement = ContextReplacement::whole(column_json);
    let applied = apply_replacement(conn, run_id, &replacement)?;
    Ok(Some(applied.column_json().to_string()))
}

/// The decision-relevant values of a stored context column.
///
/// A stored payload that is not a flat map has no values: this reports that
/// as an error, the way a logical read does.
pub fn small_values(column_json: &str) -> Result<BTreeMap<String, String>, DbError> {
    let values: BTreeMap<String, String> = serde_json::from_str(column_json)
        .map_err(|error| DbError::Rendered(format!("invalid context_json: {error}")))?;
    Ok(values
        .into_iter()
        .filter(|(key, _)| key != DIGEST_KEY)
        .collect())
}

/// The materialized logical context of one run.
pub fn load_logical(conn: &Connection, run_id: i64) -> Result<LogicalContext, DbError> {
    let column_json = storage::read_column(conn, run_id)?;
    let stored = storage::load_bulky(conn, run_id)?;
    LogicalContext::join(&column_json, &stored)
}

/// The materialized logical context of one run, through the read pool.
///
/// The worker calls this instead of reading `context_json` directly: the
/// bulky half of the context lives in the side table, and the stepper needs
/// it to resolve `{{node.reviewer.output}}` and to read a step's evidence.
pub fn load_logical_run(run_id: i64) -> Result<LogicalContext, DbError> {
    let db = crate::db::read_conn()?;
    load_logical(&db, run_id)
}

/// The logical context of one run, read as far as it can be read.
///
/// A stored payload that is not a flat map has no logical context of its
/// own, but the bulky half is still part of the run's context. This returns
/// what it can read, for the callers that look for one value rather than
/// for the whole context.
pub fn load_logical_best_effort(conn: &Connection, run_id: i64) -> SqlResult<LogicalContext> {
    let column_json = storage::read_column(conn, run_id)?;
    let stored = storage::load_bulky(conn, run_id)?;
    Ok(LogicalContext::join_best_effort(&column_json, &stored))
}

/// The materialized logical contexts of several runs.
///
/// The bulky half is read in one query per chunk of runs, so a list
/// projection stays bounded whatever the number of runs.
pub fn load_logical_for_runs(
    conn: &Connection,
    run_ids: &[i64],
) -> Result<BTreeMap<i64, LogicalContext>, DbError> {
    let columns = storage::read_columns_for_runs(conn, run_ids)?;
    let stored = storage::load_bulky_for_runs(conn, run_ids)?;
    // A run with no stored bulky values reads as an empty half.
    let empty = BTreeMap::new();
    let mut contexts = BTreeMap::new();
    for (run_id, column_json) in columns {
        let values = stored.get(&run_id).unwrap_or(&empty);
        let context = LogicalContext::join(&column_json, values)?;
        contexts.insert(run_id, context);
    }
    Ok(contexts)
}

/// The context of one run as an operator-facing projection serves it.
///
/// The two halves are joined: the Probe reads a review report and the issue
/// body it searches by out of the run's context. A stored payload that is
/// not a flat map is served as it stands — the projection never invents a
/// logical context for a payload it cannot read.
pub fn load_served_json(conn: &Connection, run_id: i64) -> SqlResult<String> {
    let column_json = storage::read_column(conn, run_id)?;
    let stored = storage::load_bulky(conn, run_id)?;
    Ok(served_json(&column_json, &stored))
}

/// The contexts of several runs as an operator-facing projection serves
/// them.
///
/// The bulky half is read in one query per chunk of runs, so a list
/// projection stays bounded whatever the number of runs.
pub fn load_served_json_for_runs(
    conn: &Connection,
    run_ids: &[i64],
) -> SqlResult<BTreeMap<i64, String>> {
    let columns = storage::read_columns_for_runs(conn, run_ids)?;
    let stored = storage::load_bulky_for_runs(conn, run_ids)?;
    // A run with no stored bulky values reads as an empty half.
    let empty = BTreeMap::new();
    Ok(columns
        .into_iter()
        .map(|(run_id, column_json)| {
            let values = stored.get(&run_id).unwrap_or(&empty);
            let served = served_json(&column_json, values);
            (run_id, served)
        })
        .collect())
}

/// The outcome of applying a change to a run's context.
///
/// The caller gets the column JSON to write, and the logical context on both
/// sides of the change, so it can ask what moved without reading either half
/// of the storage again.
#[derive(Debug, Clone)]
pub struct AppliedContext {
    /// The JSON for the `context_json` column.
    column_json: String,
    /// The run's logical context before the change.
    before: LogicalContext,
    /// The run's logical context after the change.
    after: LogicalContext,
}

impl AppliedContext {
    /// The JSON for the `context_json` column.
    pub fn column_json(&self) -> &str {
        &self.column_json
    }

    /// The run's logical context before the change.
    pub fn before(&self) -> &LogicalContext {
        &self.before
    }

    /// The run's logical context after the change.
    pub fn after(&self) -> &LogicalContext {
        &self.after
    }
}

/// Apply a change to a run's logical context and return the new column JSON.
///
/// The caller owns the transaction: the change must land in the same
/// transaction as the run-state and step writes it belongs to, so a crash
/// can never leave the context half-written.
pub fn apply(conn: &Connection, run_id: i64, change: &ContextChange) -> SqlResult<AppliedContext> {
    let column_json = storage::read_column(conn, run_id)?;
    let mut bulky = storage::load_bulky(conn, run_id)?;
    let before = LogicalContext::join_best_effort(&column_json, &bulky);
    let mut small;

    match change {
        ContextChange::Mutation(mutation) => {
            // A mutation keeps the keys it does not mention, so it has to
            // read the payload it is keeping. A payload it cannot read is a
            // payload it cannot keep: the write fails closed, the way it
            // did before the seam, rather than replacing a historical
            // payload with the mutation's keys.
            small = stored_small_strict(&column_json)?;
            // A value that is small again supersedes the stored copy of it,
            // so a key that lives in both halves keeps the column's copy
            // alone and the stored row goes.
            for key in small.keys() {
                if bulky.contains_key(key) {
                    storage::delete_bulky(conn, run_id, key)?;
                    bulky.remove(key);
                }
            }
            // Removals and resets first, so a put is the last word on the
            // key it names.
            for key in &mutation.removals {
                small.remove(key);
                if bulky.remove(key).is_some() {
                    storage::delete_bulky(conn, run_id, key)?;
                }
            }
            for scope in &mutation.resets {
                let prefix = format!("{}.", scope.segment_str());
                small.retain(|key, _| !key.starts_with(&prefix));
                bulky.retain(|key, _| !key.starts_with(&prefix));
                // The stored half goes too: a leftover row would come back
                // on the next read and the cleared working state would
                // judge stale evidence.
                storage::delete_bulky_with_prefix(conn, run_id, scope.segment_str())?;
            }
            for (key, value) in &mutation.puts {
                apply_put(conn, run_id, &mut small, &mut bulky, key, value)?;
            }
        }
        ContextChange::Replacement(replacement) => {
            if let Some(raw) = replacement.verbatim() {
                // A payload that is not a flat map is stored as it stands:
                // the split is a storage optimisation, not a validation step,
                // so such a payload is neither normalised nor erased. The
                // stored half is left alone for the same reason.
                let after = LogicalContext::join_best_effort(raw, &bulky);
                return Ok(AppliedContext {
                    column_json: raw.to_string(),
                    before,
                    after,
                });
            }
            // A replacement is the whole context: every key it does not
            // mention is gone.
            let mut new_small = BTreeMap::new();
            let mut new_bulky = BTreeMap::new();
            for (key, value) in &replacement.values {
                if storage::is_bulky(key, value) {
                    new_bulky.insert(key.clone(), value.clone());
                } else {
                    new_small.insert(key.clone(), value.clone());
                }
            }
            for key in bulky.keys() {
                if !new_bulky.contains_key(key) {
                    storage::delete_bulky(conn, run_id, key)?;
                }
            }
            for (key, value) in &new_bulky {
                if bulky.get(key) != Some(value) {
                    storage::upsert_bulky(conn, run_id, key, value)?;
                }
            }
            small = new_small;
            bulky = new_bulky;
        }
    }

    // A lifecycle blocker is runtime state, not context: it never travels
    // in a persisted context, and a historical one is cleaned up here.
    drop_runtime_blockers(conn, run_id, &mut small, &mut bulky)?;

    let column_json = encode_column(&small, &bulky);
    let after = LogicalContext::from_parts(small, &bulky);
    Ok(AppliedContext {
        column_json,
        before,
        after,
    })
}

/// Apply a partial change to a run's logical context.
///
/// A key the mutation does not mention is unchanged.
pub fn apply_mutation(
    conn: &Connection,
    run_id: i64,
    mutation: &ContextMutation,
) -> SqlResult<AppliedContext> {
    apply(conn, run_id, &ContextChange::Mutation(mutation.clone()))
}

/// Apply a complete replacement of a run's logical context.
///
/// A key the replacement does not mention is deleted.
pub fn apply_replacement(
    conn: &Connection,
    run_id: i64,
    replacement: &ContextReplacement,
) -> SqlResult<AppliedContext> {
    apply(
        conn,
        run_id,
        &ContextChange::Replacement(replacement.clone()),
    )
}

/// Store one value, in whichever half of the storage it belongs to.
///
/// A value that changed half supersedes the copy the other half holds, so
/// the two halves never disagree about one key.
fn apply_put(
    conn: &Connection,
    run_id: i64,
    small: &mut BTreeMap<String, String>,
    bulky: &mut BTreeMap<String, String>,
    key: &str,
    value: &str,
) -> SqlResult<()> {
    if storage::is_bulky(key, value) {
        // A bulky value supersedes the column's copy of it.
        small.remove(key);
        if bulky.get(key).map(String::as_str) != Some(value) {
            storage::upsert_bulky(conn, run_id, key, value)?;
            bulky.insert(key.to_string(), value.to_string());
        }
    } else {
        // A small value supersedes the stored copy of it.
        if bulky.remove(key).is_some() {
            storage::delete_bulky(conn, run_id, key)?;
        }
        small.insert(key.to_string(), value.to_string());
    }
    Ok(())
}

/// Drop the lifecycle blockers from a stored context, in both halves.
///
/// A blocker is runtime state. A historical one is cleaned up rather than
/// carried forward: a persisted veto would outlive the report that raised
/// it.
fn drop_runtime_blockers(
    conn: &Connection,
    run_id: i64,
    small: &mut BTreeMap<String, String>,
    bulky: &mut BTreeMap<String, String>,
) -> SqlResult<()> {
    let blocked: Vec<String> = small
        .keys()
        .chain(bulky.keys())
        .filter(|key| crate::circuit::context::is_lifecycle_blocker_key(key))
        .cloned()
        .collect();
    for key in blocked {
        small.remove(&key);
        if bulky.remove(&key).is_some() {
            storage::delete_bulky(conn, run_id, &key)?;
        }
    }
    Ok(())
}

/// The decision-relevant values of a stored column.
fn stored_small(column_json: &str) -> BTreeMap<String, String> {
    storage::parse_small(column_json)
        .into_iter()
        .filter(|(key, _)| !crate::circuit::context::is_lifecycle_blocker_key(key))
        .collect()
}

/// The decision-relevant values of a stored column, read strictly.
///
/// A payload that is not a flat string map has no values to read. A write
/// that keeps the keys it does not mention has to read them, so it reports
/// the error a logical read reports instead of writing over a payload it
/// cannot read.
fn stored_small_strict(column_json: &str) -> Result<BTreeMap<String, String>, rusqlite::Error> {
    let values = small_values(column_json)
        .map_err(|error| rusqlite::Error::InvalidParameterName(error.to_string()))?;
    Ok(values
        .into_iter()
        .filter(|(key, _)| !crate::circuit::context::is_lifecycle_blocker_key(key))
        .collect())
}

/// The JSON for the `context_json` column.
fn encode_column(small: &BTreeMap<String, String>, bulky: &BTreeMap<String, String>) -> String {
    let mut column = small.clone();
    // The fingerprint only earns its place once there is something to
    // fingerprint: a run with no bulky values carries none, so its context
    // stays exactly what it was.
    if !bulky.is_empty() {
        column.insert(DIGEST_KEY.to_string(), storage::digest_of(bulky));
    }
    serde_json::to_string(&column).unwrap_or_else(|_| "{}".to_string())
}

/// The context of a run as an operator-facing projection serves it.
///
/// A stored payload that is not a flat map is served as it stands: the
/// projection never invents a logical context for a payload it cannot read.
fn served_json(column_json: &str, stored: &BTreeMap<String, String>) -> String {
    let Ok(mut map) = serde_json::from_str::<BTreeMap<String, String>>(column_json) else {
        return column_json.to_string();
    };
    for (key, value) in stored {
        map.entry(key.clone()).or_insert_with(|| value.clone());
    }
    map.remove(DIGEST_KEY);
    map.retain(|key, _| !crate::circuit::context::is_lifecycle_blocker_key(key));
    serde_json::to_string(&map).unwrap_or_else(|_| column_json.to_string())
}

#[cfg(test)]
#[path = "context_store_tests.rs"]
mod tests;
