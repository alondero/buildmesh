//! Coordinator drive idempotency ledger (issue #320 / #750, ADR-0008 §6).

use rusqlite::{Connection, params};

use super::{write_conn, SqlResult};

// --- Coordinator drive idempotency ledger (issue #320, ADR-0008 §6) ---
//
// A Coordinator on a flaky network retries a timed-out drive; the caller-supplied
// idempotency key lets Buildmesh recognise the retry and replay the original
// verdict instead of sending the prompt twice. The store deals in the verdict's
// wire string (`"delivered"`/`"unverified"`) so this layer never depends on the
// `coordinator::drive` module — the drive side owns the string↔enum mapping.
// Lock-once + `_inner(&Connection)` so the logic is unit-testable in memory.

/// The outcome of an atomic claim attempt (issue #750, item 1). Drives the
/// orchestrator: `Claimed` means the caller owns the row and must drive +
/// finalize; `Replay` means a finalized peer row exists with the same prompt
/// payload, return its verdict; `Mismatch` means a finalized peer row exists
/// but the *prompt* differs — Stripe-style reject (#750 item 2); `InProgress`
/// means a peer holds the row in `pending` (the caller's brief wait inside
/// `drive_node_idempotent` polls for finalize before surfacing this).
///
/// A genuine read error is propagated as `Err` (not collapsed into any
/// variant) so the drive path can fail safe — same fail-safe contract the
/// pre-#750 lookup established (issue #320 review).
#[derive(Debug, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// This caller inserted the pending row; proceed to drive.
    Claimed,
    /// The row was already finalized with the same prompt; replay its verdict.
    Replay { verdict: String },
    /// The row was already finalized but with a different prompt; reject.
    Mismatch,
    /// The row is `pending` — another caller is currently driving this key.
    InProgress,
}

/// Maximum age (in seconds) a `pending` row may sit in the ledger before the
/// next claim attempt treats it as orphaned (crash-mid-send) and reclaims it.
/// A successful drive is sub-millisecond; 30s is generous enough that a
/// live, currently-driving peer is never reclaimed out from under itself, and
/// short enough that a crashed Buildmesh doesn't lock out a key for long.
pub const PENDING_CLAIM_TIMEOUT_SECS: i64 = 30;

/// Atomic claim-before-send (issue #750, item 1). In a single transaction:
///   1. Reclaim any `pending` row older than `PENDING_CLAIM_TIMEOUT_SECS`
///      for this `(node_id, key)` (a crash-mid-send row must not block the
///      key forever — a retry can re-send the prompt).
///   2. Try to `INSERT OR IGNORE` a fresh `pending` row with the prompt
///      hash. If this caller wins the race (no prior row exists), the
///      orchestrator proceeds to drive; if a prior row exists, read its
///      state and map to `Replay`/`Mismatch`/`InProgress`.
///
/// Lock-once + `_inner(&Connection)` so the drive logic is unit-testable
/// in-memory (`db::drive_idempotency_tests`). Returns `Err(_)` only on a
/// real DB failure (lock, IO, corruption) — the same fail-safe contract
/// the pre-#750 `lookup` established (issue #320 review): the orchestrator
/// must never mistake "couldn't read" for "key never seen".
pub fn claim_drive_prompt(
    node_id: i64,
    key: &str,
    prompt_hash: &str,
) -> SqlResult<ClaimOutcome> {
    let db = write_conn();
    claim_drive_prompt_inner(&db, node_id, key, prompt_hash)
}

pub fn claim_drive_prompt_inner(
    conn: &Connection,
    node_id: i64,
    key: &str,
    prompt_hash: &str,
) -> SqlResult<ClaimOutcome> {
    let tx = conn.unchecked_transaction()?;

    // Step 1: orphan recovery. Any `pending` row older than the timeout is
    // from a crashed prior attempt — the prompt never landed, so a retry
    // is safe to re-send. DELETE (not UPDATE-to-expired) so the slot is
    // open for the new INSERT OR IGNORE below.
    tx.execute(
        "DELETE FROM coordinator_drive_prompts
            WHERE node_id = ?1 AND idempotency_key = ?2
              AND status = 'pending'
              AND claimed_at < datetime('now', '-' || ?3 || ' seconds')",
        params![node_id, key, PENDING_CLAIM_TIMEOUT_SECS],
    )?;

    // Step 2: try to insert a fresh pending row. `INSERT OR IGNORE` is the
    // race resolver: only one concurrent claim survives, the rest fall into
    // the SELECT below.
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO coordinator_drive_prompts
             (node_id, idempotency_key, status, claimed_at, prompt_hash, verdict)
             VALUES (?1, ?2, 'pending', datetime('now'), ?3, '')",
        params![node_id, key, prompt_hash],
    )?;

    if inserted == 1 {
        // We won the race — return early without the SELECT (no peer row to
        // read). The finalize step will UPDATE this row after the send.
        tx.commit()?;
        return Ok(ClaimOutcome::Claimed);
    }

    // Step 3: we lost (or hit a finalized row). Read its current state to
    // decide which `ClaimOutcome` variant to surface. The SELECT can't
    // realistically miss — we just inserted or already had a row — but if
    // it does, propagate the `Err` rather than collapse it to a default
    // outcome (fail-safe contract from issue #320 review).
    let (status, verdict, stored_hash) = tx.query_row(
        "SELECT status, verdict, prompt_hash FROM coordinator_drive_prompts
             WHERE node_id = ?1 AND idempotency_key = ?2",
        params![node_id, key],
        |row| {
            let status: String = row.get(0)?;
            let verdict: String = row.get(1)?;
            let stored_hash: String = row.get(2)?;
            Ok((status, verdict, stored_hash))
        },
    )?;

    tx.commit()?;

    Ok(match (status.as_str(), verdict.is_empty()) {
        // Pre-v32 rows have a non-empty `verdict` (v23 required it NOT NULL)
        // but `status='pending'` from the v32 column DEFAULT — treat them as
        // finalized, not in-progress, so a Coordinator hitting a pre-v32
        // ledger row replays the verdict rather than getting a phantom
        // `InProgress` and stalling for `PENDING_CLAIM_TIMEOUT_SECS`.
        // The `prompt_hash == ''` default still makes a key-reuse-with-
        // different-payload surface as Mismatch (same-key-same-verdict but
        // empty stored hash) — acceptable because drive is off-by-default
        // and unreleased (#313), so there are no real pre-v32 callers.
        (_, false) if stored_hash == prompt_hash => ClaimOutcome::Replay { verdict },
        (_, false) => ClaimOutcome::Mismatch,
        // Live `pending` row with an empty verdict — a real peer's
        // in-flight drive. The orchestrator briefly polls for finalize.
        ("pending", true) => ClaimOutcome::InProgress,
        // A finalized row with no recorded verdict (shouldn't happen on
        // v32+, where finalize always writes `verdict` together with
        // `status`). Treated as Mismatch rather than a silent InProgress
        // so the caller can mint a fresh key rather than wait for a row
        // that will never finalize.
        _ => ClaimOutcome::Mismatch,
    })
}

/// Finalize a claim: UPDATE the `pending` row to its terminal status +
/// verdict. Idempotent (UPDATE is naturally so) and does not insert if no
/// `pending` row exists — a finalized row is left alone (the first verdict
/// wins, mirroring the pre-#750 `INSERT OR IGNORE` rule). Returns the number
/// of rows actually changed so the caller can log a warning when a drive
/// completed but the finalize found no row to update.
pub fn finalize_drive_prompt(
    node_id: i64,
    key: &str,
    verdict: VerdictStr<'_>,
) -> SqlResult<usize> {
    let db = write_conn();
    finalize_drive_prompt_inner(&db, node_id, key, verdict)
}

pub fn finalize_drive_prompt_inner(
    conn: &Connection,
    node_id: i64,
    key: &str,
    verdict: VerdictStr<'_>,
) -> SqlResult<usize> {
    // Only update `status` + `verdict`; the `prompt_hash` column stays as it
    // was set during the claim — a future claim must be able to read it back
    // and Replay (status == terminal AND stored_hash == incoming_hash) rather
    // than Mismatch because we cleared the hash on finalize.
    conn.execute(
        "UPDATE coordinator_drive_prompts
             SET status = ?3, verdict = ?4
             WHERE node_id = ?1 AND idempotency_key = ?2 AND status = 'pending'",
        params![node_id, key, verdict.as_status_str(), verdict.as_str()],
    )
}

/// Verdict wrapper so callers don't have to thread both the DB status string
/// ("delivered" / "unverified") and the wire verdict string. They're the same
/// word today; this keeps the call site clean if they ever diverge.
pub enum VerdictStr<'a> {
    Delivered,
    Unverified,
    // PhantomData-friendly lifetime so the enum stays `'a`-parameterised
    // (matches the wire form the tests use); the `PhantomData` is invisible
    // to consumers and disappears at compile time.
    #[doc(hidden)]
    _Phantom(std::marker::PhantomData<&'a ()>),
}

impl<'a> VerdictStr<'a> {
    /// The DB status string (`'delivered'` / `'unverified'`).
    pub fn as_status_str(&self) -> &'static str {
        match self {
            VerdictStr::Delivered => "delivered",
            VerdictStr::Unverified => "unverified",
            VerdictStr::_Phantom(_) => unreachable!("PhantomData is uninhabited"),
        }
    }

    /// The verdict wire string (same value as `as_status_str` today, but kept
    /// distinct so a future schema split can move the two columns apart).
    pub fn as_str(&self) -> &'static str {
        self.as_status_str()
    }
}

/// Release a claim when the drive itself failed (so a retry can re-attempt
/// rather than wait on a `pending` row the orchestrator never finalized).
/// Only deletes `pending` rows — a finalized row stays put, and a second
/// `NotLive` retry still sees the verdict if the row happens to already be
/// terminal from a peer.
pub fn release_drive_prompt_claim(node_id: i64, key: &str) -> SqlResult<usize> {
    let db = write_conn();
    release_drive_prompt_claim_inner(&db, node_id, key)
}

pub fn release_drive_prompt_claim_inner(
    conn: &Connection,
    node_id: i64,
    key: &str,
) -> SqlResult<usize> {
    conn.execute(
        "DELETE FROM coordinator_drive_prompts
             WHERE node_id = ?1 AND idempotency_key = ?2 AND status = 'pending'",
        params![node_id, key],
    )
}

/// Bounded-age prune (issue #750, item 3). Deletes every row whose
/// `created_at` is older than `days` days — same shape as the
/// `pending_worktree_removals` drain prior art. The background worker
/// (`services::coordinator_ledger_maintenance`) calls this on a 30-minute
/// cadence; a startup sweep runs from the same module. Returns the number of
/// rows deleted (informational; the worker logs non-zero results).
pub fn prune_drive_prompts_older_than(days: i64) -> SqlResult<usize> {
    let db = write_conn();
    prune_drive_prompts_older_than_inner(&db, days)
}

pub fn prune_drive_prompts_older_than_inner(
    conn: &Connection,
    days: i64,
) -> SqlResult<usize> {
    conn.execute(
        "DELETE FROM coordinator_drive_prompts
             WHERE created_at < datetime('now', '-' || ?1 || ' days')",
        params![days],
    )
}
