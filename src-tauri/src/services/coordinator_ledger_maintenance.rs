//! Bounded-age ledger GC (issue #750, item 3; issue #1236).
//!
//! The `coordinator_drive_prompts` table records one row per
//! `(node_id, idempotency_key)` a Coordinator has ever driven — by design
//! unbounded in cardinality across a long app lifetime (keys are caller-supplied
//! UUIDs). The bounded-age prune here deletes rows older than
//! [`LEDGER_RETENTION_DAYS`] so the table's size stays proportional to
//! "unique drives per retention window" rather than "unique drives ever".
//!
//! ## Second tenant: circuit runs (#1236)
//!
//! `autopilot_circuit_runs` had the same unbounded shape for the same reason —
//! an interval circuit mints a row per fire because its `trigger_identity`
//! embeds a timestamp, so the UNIQUE constraint never collapses them. The
//! worker therefore runs a second sweep, [`prune_circuit_runs_once`], on the
//! same tick. It shares this module rather than spawning a thread of its own:
//! identical cadence, identical failure handling, and one bounded DELETE each
//! does not justify a second sleeping thread. The two sweeps are called
//! independently so one erroring does not skip the other.
//!
//! That sweep is not a plain age DELETE — a circuit run's row doubles as the
//! memory that stops a circuit reprocessing a GitHub issue, so stable-identity
//! rows are compacted rather than deleted. `db::circuit::SWEEPABLE_RUNS` and
//! [`CIRCUIT_RUN_RETENTION_DAYS`] carry the reasoning.
//!
//! ## Why a 7-day window
//!
//! A Coordinator on a flaky network retries a timed-out drive in seconds;
//! the legitimate replay window is sub-second. 7 days is generous enough to
//! cover every realistic retry (including a Coordinator working through a
//! long-lived todo list) while bounding the table at roughly
//! "unique-drives-per-week" rows. The same shape as Stripe's idempotency
//! retention. Bump-able later without a schema change — `LEDGER_RETENTION_DAYS`
//! is the single source of truth.
//!
//! ## Why not piggyback on the pool worker
//!
//! The pool worker (`services::pool_worker`) is mesh-pool specific: every
//! tick is gated on per-mesh idle, every loop iterates worktree-enabled
//! meshes. Ledger GC is unrelated — it doesn't care about mesh activity,
//! doesn't iterate meshes, and each prune is a bounded SQL statement rather
//! than a per-mesh walk. A dedicated worker keeps the failure isolation
//! obvious; what belongs here is bounded-age GC, and nothing else.
//!
//! ## Worker shape
//!
//! Mirrors `services::autopilot::start_autopilot_worker` (issue #482, PRD
//! #480): a `std::thread::spawn` whose body is
//! `loop { sweeps...; sleep(PRUNE_INTERVAL); }`. The very first iteration
//! runs immediately (no startup delay) — the sweeps are fast and bounded, and
//! running once on every app launch is the point. Failures are logged and
//! swallowed per sweep: a transient SQLite error must not kill the worker
//! thread, nor stop a sibling sweep (the next tick re-tries).

use std::time::Duration;

/// Maximum age (in days) a `coordinator_drive_prompts` row may sit in the
/// ledger before the next prune sweep deletes it. Tuned in the module
/// docstring — 7 days covers every realistic retry window while bounding
/// the table.
pub const LEDGER_RETENTION_DAYS: i64 = 7;

/// Cadence the worker prunes the ledger. 30 minutes is a long interval —
/// pruning is not hot path, and a `prune_older_than` DELETE on a
/// single-digit-K-row table is sub-millisecond. The same long-interval
/// shape as `warm_pool::TICK_SLOW` (30 s scaled up — pruning is rarer
/// because a row only counts toward the bound at end-of-retention).
pub const PRUNE_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// Maximum age (in days) a *terminal* `autopilot_circuit_runs` row may sit
/// before the next sweep collects it (issue #1236).
///
/// Longer than [`LEDGER_RETENTION_DAYS`] because these rows are user-facing
/// history, not idempotency scratch: the Probe tab's run ledger is how someone
/// works out why a circuit did what it did last month. 30 days keeps a useful
/// audit trail while still bounding an interval circuit — at a 60 s cadence
/// that is ~43k rows steady-state instead of unbounded growth.
///
/// Retention is NOT the dedupe horizon, which is the subtlety worth keeping in
/// view here. `UNIQUE (circuit_id, trigger_identity)` is what stops a circuit
/// reprocessing a source, and for GitHub triggers that identity is a stable
/// issue number — so its row must outlive any window. The sweep resolves this
/// by never deleting a stable-identity row at all (it empties the row's stored
/// body instead); this constant only bounds throwaway timestamp identities.
/// See `db::circuit::SWEEPABLE_RUNS` for the full reasoning.
pub const CIRCUIT_RUN_RETENTION_DAYS: i64 = 30;

/// Start the GC worker. Called once from `lib.rs::setup`, alongside the
/// other background workers (`pool_worker`, `autopilot_worker`). The worker
/// runs forever in its own thread; failures inside either sweep are logged
/// and swallowed so a transient error can't kill the loop.
///
/// No `AppHandle` parameter is taken — the GC doesn't emit events today. If
/// a future enhancement (e.g. diagnostics emit `ledger-pruned`) needs to
/// reach the frontend, the signature can grow the handle without breaking
/// the call site (one extra arg).
pub fn start_worker() {
    std::thread::Builder::new()
        .name("coordinator-ledger-gc".to_string())
        .spawn(|| {
            // The very first iteration runs immediately — the startup sweep
            // is the whole point of having a worker at all. No `STARTUP_DELAY`
            // (unlike `autopilot_worker`) because a single bounded DELETE is
            // safe to run on every launch, including one whose prior session
            // crashed before the periodic sweep could fire.
            loop {
                if let Err(e) = prune_once() {
                    tracing::warn!(
                        "coordinator_ledger_gc: prune sweep failed (next tick will retry): {e}"
                    );
                }
                // Independent of the drive-ledger sweep on purpose: one table
                // erroring must not skip the other's sweep for a full tick.
                if let Err(e) = prune_circuit_runs_once() {
                    tracing::warn!(
                        "circuit_run_gc: prune sweep failed (next tick will retry): {e}"
                    );
                }
                std::thread::sleep(PRUNE_INTERVAL);
            }
        })
        .expect("failed to spawn coordinator-ledger-gc thread");
}

/// One prune pass: delete every row whose `created_at` is older than
/// [`LEDGER_RETENTION_DAYS`]. Returns the number of rows deleted
/// (informational — `0` is the steady-state answer after the first sweep
/// on a healthy app). Errors are propagated so the worker can log + retry
/// on the next tick.
pub fn prune_once() -> Result<usize, String> {
    let deleted = crate::db::prune_drive_prompts_older_than(LEDGER_RETENTION_DAYS)
        .map_err(|e| e.to_string())?;
    if deleted > 0 {
        tracing::info!(
            "coordinator_ledger_gc: pruned {deleted} drive-ledger row(s) older than {LEDGER_RETENTION_DAYS} days"
        );
    }
    Ok(deleted)
}

/// One circuit-run sweep: bound `autopilot_circuit_runs` to
/// [`CIRCUIT_RUN_RETENTION_DAYS`] (issue #1236). Returns
/// `(rows_deleted, rows_compacted)` — deleted rows are spent interval/manual
/// fires, compacted rows are GitHub tombstones that keep their identity but
/// give up the issue/PR body they were storing.
///
/// Errors propagate so the worker can log and retry on the next tick.
pub fn prune_circuit_runs_once() -> Result<(usize, usize), String> {
    let (deleted, compacted) =
        crate::db::circuit::prune_terminal_circuit_runs_older_than(CIRCUIT_RUN_RETENTION_DAYS)
            .map_err(|e| e.to_string())?;
    if deleted > 0 || compacted > 0 {
        tracing::info!(
            "circuit_run_gc: deleted {deleted} spent circuit-run row(s) and compacted \
             {compacted} trigger tombstone(s) older than {CIRCUIT_RUN_RETENTION_DAYS} days"
        );
    }
    Ok((deleted, compacted))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// In-memory connection with just the `coordinator_drive_prompts` table
    /// the prune helper needs.
    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE coordinator_drive_prompts (
                node_id INTEGER NOT NULL,
                idempotency_key TEXT NOT NULL,
                verdict TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'pending',
                claimed_at TEXT NOT NULL DEFAULT (datetime('now')),
                prompt_hash TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (node_id, idempotency_key)
            );
            CREATE INDEX idx_coordinator_drive_prompts_created_at
                ON coordinator_drive_prompts(created_at);",
        )
        .unwrap();
        conn
    }

    fn insert(conn: &Connection, node: i64, key: &str, age_modifier: Option<&str>) {
        // `age_modifier` is the optional modifier list passed as the
        // second-and-later arguments to `datetime()` — e.g. `None` for
        // "now" (`datetime('now')`), `Some("'-10 days'")` (becomes
        // `datetime('now', '-10 days')`). SQLite's `datetime()` accepts
        // modifiers only as additional arguments after an anchor timestamp;
        // `datetime('-10 days')` alone returns NULL. We interpolate the
        // modifier list because there's no bound-parameter form for the
        // modifier slot — the test author is in full control of the input
        // (only ever literal strings from this helper), so the injection
        // risk is bounded to test code.
        let modifier_sql = match age_modifier {
            Some(m) => format!(", {m}"),
            None => String::new(),
        };
        conn.execute(
            &format!(
                "INSERT INTO coordinator_drive_prompts
                     (node_id, idempotency_key, status, claimed_at, prompt_hash, verdict, created_at)
                 VALUES (?1, ?2, 'delivered', datetime('now'), '', 'delivered', datetime('now'{modifier_sql}))"
            ),
            rusqlite::params![node, key],
        )
        .unwrap();
    }

    /// The headline AC for item 3: rows older than the retention window are
    /// deleted, newer rows survive. `prune_once` uses the DB helper, which
    /// honours the `LEDGER_RETENTION_DAYS` constant — we hand-call the DB
    /// helper here so the test doesn't need to set up the global DB mutex.
    #[test]
    fn prune_keeps_newer_and_drops_older_rows() {
        let conn = db();
        insert(&conn, 1, "fresh", None);
        insert(&conn, 2, "old1", Some("'-10 days'"));
        insert(&conn, 3, "old2", Some("'-30 days'"));

        let pruned = crate::db::prune_drive_prompts_older_than_inner(&conn, LEDGER_RETENTION_DAYS)
            .unwrap();
        assert_eq!(pruned, 2);

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM coordinator_drive_prompts",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        let key: String = conn
            .query_row(
                "SELECT idempotency_key FROM coordinator_drive_prompts",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(key, "fresh");
    }

    /// A retention-window sweep on an empty table is a no-op (no panic, 0
    /// deletions). Important: the worker logs `info!` only on a non-zero
    /// prune, so an empty sweep must return `Ok(0)` cleanly rather than
    /// erroring.
    #[test]
    fn empty_table_is_a_no_op() {
        let conn = db();
        let pruned = crate::db::prune_drive_prompts_older_than_inner(&conn, LEDGER_RETENTION_DAYS)
            .unwrap();
        assert_eq!(pruned, 0);
    }

    /// A row exactly at the retention boundary is kept (the SQL uses
    /// `created_at <` rather than `created_at <=`, so the boundary row
    /// survives the sweep). Pins the contract: a row that just turned 7
    /// days old should not be deleted on the same tick.
    #[test]
    fn boundary_row_is_kept() {
        let conn = db();
        insert(
            &conn,
            1,
            "boundary",
            Some(&format!("'-{} days'", LEDGER_RETENTION_DAYS)),
        );
        let pruned = crate::db::prune_drive_prompts_older_than_inner(&conn, LEDGER_RETENTION_DAYS)
            .unwrap();
        assert_eq!(pruned, 0);
    }
}