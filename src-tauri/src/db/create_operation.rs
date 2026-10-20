//! Remote create-and-start operation ledger (issue #2331).
//!
//! `POST /nodes/create` used to be a plain "insert, then spawn" pair: a phone
//! that lost the response after the node row committed had no way to find out
//! whether the work happened, so it either duplicated the node (second row,
//! second worktree, second process) or showed the user nothing at all.
//!
//! This module is the durable half of the fix. A client generates an
//! `operation_key` once per submitted idea and reuses it for every retry of
//! that submission. The key is bound to the node id **in the same transaction
//! as the node INSERT** ([`bind_create_operation_tx`]) and carries:
//!
//! * `request_fingerprint` — SHA-256 of the normalized request, so reusing a
//!   key with *different* content is a conflict rather than a silent replay of
//!   someone else's idea.
//! * `start_state` — the durable start outcome, so a restart between insertion
//!   and startup still exposes a recoverable "committed but not started" state
//!   instead of a phantom success.
//!
//! Every entry point follows the repo's lock-once + `_inner(&Connection)` rule
//! so the logic is unit-testable against an isolated connection instead of the
//! process-global writer.

use rusqlite::{params, Connection};

use super::{read_conn, write_conn, SqlResult};

/// How far a create-and-start operation got after its node row committed.
///
/// `Created` is the honest "row exists, nothing has been attempted yet" state —
/// it is what a restart between insertion and startup leaves behind, and what
/// an App-unavailable response must report rather than a fake success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateStartState {
    /// Node row committed; the start was never attempted (or was skipped).
    Created,
    /// The process started.
    Started,
    /// The start was attempted and failed. `start_error` carries why.
    StartFailed,
}

impl CreateStartState {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Started => "started",
            Self::StartFailed => "start_failed",
        }
    }

    pub fn from_db_str(raw: &str) -> SqlResult<Self> {
        match raw {
            "created" => Ok(Self::Created),
            "started" => Ok(Self::Started),
            "start_failed" => Ok(Self::StartFailed),
            // Fail loud: an unknown value means the string enum and the DB
            // column drifted. Collapsing it to `Created` would report a
            // started node as not-started.
            other => Err(rusqlite::Error::InvalidParameterName(format!(
                "unknown create start_state: {other}"
            ))),
        }
    }
}

/// A ledger row: the durable identity of one submitted idea.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateOperation {
    pub operation_key: String,
    pub node_id: i64,
    pub request_fingerprint: String,
    pub start_state: CreateStartState,
    pub start_error: Option<String>,
}

/// The result of trying to bind an operation key to a node row.
///
/// `Bound` means this caller created the node and owns the start attempt;
/// `Replay` means the key was already bound (same content) so the caller must
/// return the recorded node instead of creating again; `Conflict` means the
/// key exists under *different* normalized content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateBindOutcome {
    Bound { node_id: i64 },
    Replay { node_id: i64 },
    Conflict,
}

/// Claim outcome for the key, resolved before any expensive work happens.
///
/// `None` from [`claim_create_operation_inner`] means the key was never seen;
/// that case is deliberately not a variant, because "go ahead and create" and
/// "I own this key" are different statements — the authoritative race is
/// resolved inside the node INSERT's transaction by
/// [`bind_create_operation_tx`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateClaim {
    /// The key already exists with the *same* normalized request. The caller
    /// replays the recorded node — no second row, worktree, or process.
    Replay {
        node_id: i64,
        start_state: CreateStartState,
        start_error: Option<String>,
    },
    /// The key exists with *different* normalized content. The caller must
    /// reject with 409 rather than binding a second idea to the first key.
    Conflict,
}

/// Bind the operation key inside the caller's open transaction.
///
/// Deliberately takes `&Connection` (a `Transaction` derefs to one) rather
/// than opening its own: the whole point is that the key binding and the node
/// INSERT commit or roll back together. Returns [`CreateBindOutcome::Replay`]
/// when the key is already bound, so a concurrent duplicate request loses the
/// race *inside the same transaction* and rolls back its own node row instead
/// of leaving an orphan.
pub fn bind_create_operation_tx(
    conn: &Connection,
    operation_key: &str,
    request_fingerprint: &str,
    node_id: i64,
) -> SqlResult<CreateBindOutcome> {
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO agent_node_create_operations
             (operation_key, node_id, request_fingerprint, start_state)
         VALUES (?1, ?2, ?3, 'created')",
        params![operation_key, node_id, request_fingerprint],
    )?;

    if inserted == 1 {
        return Ok(CreateBindOutcome::Bound { node_id });
    }

    // The key is taken. Whether that is a replay or a conflict is the
    // caller's call (it owns the fingerprint semantics), so read the binding
    // back rather than deciding here.
    let existing: (i64, String) = conn.query_row(
        "SELECT node_id, request_fingerprint FROM agent_node_create_operations
             WHERE operation_key = ?1",
        params![operation_key],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    if existing.1 == request_fingerprint {
        Ok(CreateBindOutcome::Replay {
            node_id: existing.0,
        })
    } else {
        // Same key, different idea. The caller rolls back its node row and
        // answers 409 — never silently attaching a second idea to the first.
        Ok(CreateBindOutcome::Conflict)
    }
}

/// Look up an existing operation by key. `None` means the key was never seen.
///
/// Test-only seam: production reads the ledger through
/// [`claim_create_operation`], which resolves replay-vs-conflict against the
/// incoming fingerprint. A bare read has no production caller — the retry
/// contract is the fingerprint comparison, not a "does this key exist" probe —
/// so it is gated rather than left as dead code behind `#[allow(dead_code)]`.
#[cfg(test)]
pub fn find_create_operation(operation_key: &str) -> SqlResult<Option<CreateOperation>> {
    let db = read_conn()?;
    find_create_operation_inner(&db, operation_key)
}

pub fn find_create_operation_inner(
    conn: &Connection,
    operation_key: &str,
) -> SqlResult<Option<CreateOperation>> {
    // Only "no such row" means the key was never seen. Collapsing a real read
    // error into `None` would report a bound key as absent and make the caller
    // create a *second* node — precisely the duplicate this ledger exists to
    // prevent. The drive ledger's fail-safe contract (#320 review) applies here
    // for the same reason.
    let row = match conn.query_row(
        "SELECT operation_key, node_id, request_fingerprint, start_state, start_error
             FROM agent_node_create_operations WHERE operation_key = ?1",
        params![operation_key],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        },
    ) {
        Ok(row) => Some(row),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(error) => return Err(error),
    };

    let Some((key, node_id, fingerprint, state, start_error)) = row else {
        return Ok(None);
    };

    Ok(Some(CreateOperation {
        operation_key: key,
        node_id,
        request_fingerprint: fingerprint,
        start_state: CreateStartState::from_db_str(&state)?,
        start_error,
    }))
}

/// Claim a key before doing any expensive work (the phone's "Create" tap).
///
/// Same contract as [`bind_create_operation_tx`] but usable on its own, so a
/// retry that finds an already-committed operation can short-circuit before
/// it pays for configuration resolution. A conflict is decided here (where the
/// incoming fingerprint is known) and returned as [`CreateClaim::Conflict`].
pub fn claim_create_operation(
    operation_key: &str,
    request_fingerprint: &str,
) -> SqlResult<Option<CreateClaim>> {
    let db = read_conn()?;
    claim_create_operation_inner(&db, operation_key, request_fingerprint)
}

pub fn claim_create_operation_inner(
    conn: &Connection,
    operation_key: &str,
    request_fingerprint: &str,
) -> SqlResult<Option<CreateClaim>> {
    let Some(existing) = find_create_operation_inner(conn, operation_key)? else {
        // Key never seen — the caller proceeds to create. The authoritative
        // race resolution still happens in `bind_create_operation_tx`, which
        // runs inside the node INSERT's transaction.
        return Ok(None);
    };

    if existing.request_fingerprint != request_fingerprint {
        return Ok(Some(CreateClaim::Conflict));
    }

    Ok(Some(CreateClaim::Replay {
        node_id: existing.node_id,
        start_state: existing.start_state,
        start_error: existing.start_error,
    }))
}

/// Record the durable outcome of the start attempt.
///
/// Runs *after* the spawn call and deliberately does not hold any lock across
/// it: the process work must stay outside the writer mutex (issue #1228).
pub fn record_create_start_result(
    node_id: i64,
    state: CreateStartState,
    start_error: Option<&str>,
) -> SqlResult<usize> {
    let db = write_conn();
    record_create_start_result_inner(&db, node_id, state, start_error)
}

pub fn record_create_start_result_inner(
    conn: &Connection,
    node_id: i64,
    state: CreateStartState,
    start_error: Option<&str>,
) -> SqlResult<usize> {
    conn.execute(
        "UPDATE agent_node_create_operations
             SET start_state = ?2, start_error = ?3, updated_at = datetime('now')
           WHERE node_id = ?1",
        params![node_id, state.as_db_str(), start_error],
    )
}
