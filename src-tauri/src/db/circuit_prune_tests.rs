//! Circuit-run retention sweep tests (issue #1236).
//!
//! `autopilot_circuit_runs` was unbounded. Interval and manual triggers mint a
//! fresh row per fire — their `trigger_identity` embeds a millisecond timestamp,
//! so the schema's `UNIQUE (circuit_id, trigger_identity)` never collapses them —
//! and GitHub-triggered rows persist whole issue/PR bodies in `context_json`.
//! Nothing deleted terminal rows outside the circuit/mesh delete cascade.
//!
//! The sweep is deliberately two-tier, because the two identity families carry
//! different meaning:
//!
//! * `interval:<ms>` / `manual:<ms>` — the identity is a timestamp and is never
//!   re-presented, so a terminal row carries no dedupe value. Deleted outright.
//! * `issue:<n>:<label>` / `pr:<n>:<label>` — the identity is STABLE, and the row
//!   is the only record that this circuit already handled that source.
//!   `services::circuit_triggers::mint_unseen_runs` reads it back through
//!   `list_circuit_trigger_identities` and treats a missing row as "never seen",
//!   while `ingest_issues` re-fetches every *open* labelled issue on every poll.
//!   Deleting one would re-mint a run and re-spawn agents on finished work, so
//!   the row is kept forever and only its body is dropped.

use super::circuit::prune_terminal_circuit_runs_older_than_inner;
use rusqlite::Connection;

/// In-memory DB carrying the real evolved schema (the `evolve_to` precedent in
/// `circuit_tests`), plus one mesh and one circuit for runs to hang off. Kept
/// off the process-global DB so these tests stay parallel-safe.
fn prune_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "
        CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE meshes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            layout TEXT NOT NULL DEFAULT 'grid',
            position INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        INSERT OR REPLACE INTO app_settings (key, value) VALUES ('schema_version', '33');
        INSERT INTO meshes (name, path) VALUES ('m', '/tmp/m');
        ",
    )
    .unwrap();
    crate::db::migrations::evolve_to(crate::db::migrations::SCHEMA_VERSION, &conn).unwrap();
    conn.execute(
        "INSERT INTO autopilot_circuits (id, mesh_id, name) VALUES (1, 1, 'c1')",
        [],
    )
    .unwrap();
    conn
}

/// Insert one run on circuit 1, aged `days_ago` on BOTH timestamps: the sweep
/// filters on `updated_at`, while `created_at` is the interval cadence anchor.
///
/// `days_ago` is interpolated rather than bound because SQLite has no
/// bound-parameter form for a `datetime()` modifier — the same constraint
/// `coordinator_ledger_maintenance` documents in its own prune tests. Every
/// caller is a literal in this module, so the injection surface is nil.
fn insert_run(conn: &Connection, identity: &str, state: &str, days_ago: i64, body: &str) -> i64 {
    insert_run_for(conn, 1, identity, state, days_ago, body)
}

fn insert_run_for(
    conn: &Connection,
    circuit_id: i64,
    identity: &str,
    state: &str,
    days_ago: i64,
    body: &str,
) -> i64 {
    conn.execute(
        &format!(
            "INSERT INTO autopilot_circuit_runs
                 (circuit_id, mesh_id, trigger_identity, state, context_json,
                  created_at, updated_at)
             VALUES (?1, 1, ?2, ?3, ?4,
                     datetime('now', '-{days_ago} days'),
                     datetime('now', '-{days_ago} days'))"
        ),
        rusqlite::params![circuit_id, identity, state, body],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn insert_step(conn: &Connection, run_id: i64, node_id: &str) {
    conn.execute(
        "INSERT INTO autopilot_circuit_run_steps (run_id, node_id) VALUES (?1, ?2)",
        rusqlite::params![run_id, node_id],
    )
    .unwrap();
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn identities(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT trigger_identity FROM autopilot_circuit_runs ORDER BY id")
        .unwrap();
    let rows = stmt.query_map([], |r| r.get(0)).unwrap();
    rows.collect::<Result<Vec<String>, _>>().unwrap()
}

fn anchor(conn: &Connection, circuit_id: i64) -> String {
    conn.query_row(
        "SELECT MAX(created_at) FROM autopilot_circuit_runs WHERE circuit_id = ?1",
        rusqlite::params![circuit_id],
        |r| r.get(0),
    )
    .unwrap()
}

/// The headline leak: an interval circuit mints a row per fire forever. Old
/// terminal interval rows go, and their step rows go with them — the schema
/// declares `ON DELETE CASCADE`, but enforcement depends on the connection's
/// `foreign_keys` pragma, so the sweep deletes steps explicitly (the
/// `delete_autopilot_circuit` precedent).
#[test]
fn prune_drops_old_terminal_interval_runs_and_their_steps() {
    let conn = prune_db();
    let old = insert_run(&conn, "interval:1000", "completed", 60, "{}");
    insert_step(&conn, old, "build");
    insert_step(&conn, old, "test");
    let failed = insert_run(&conn, "interval:2000", "failed", 50, "{}");
    insert_step(&conn, failed, "build");
    // Newest row — kept as the cadence anchor; see the dedicated test below.
    insert_run(&conn, "interval:3000", "completed", 40, "{}");

    let (deleted, _) = prune_terminal_circuit_runs_older_than_inner(&conn, 30).unwrap();

    assert_eq!(deleted, 2, "both non-newest terminal interval rows are swept");
    assert_eq!(count(&conn, "autopilot_circuit_runs"), 1);
    assert_eq!(
        count(&conn, "autopilot_circuit_run_steps"),
        0,
        "no orphan step rows survive the sweep"
    );
}

/// `manual:<ms>` (Trigger Now) shares the timestamp-identity shape, so it is
/// swept on the same terms as `interval:`.
#[test]
fn prune_drops_old_terminal_manual_runs() {
    let conn = prune_db();
    insert_run(&conn, "manual:1000", "completed", 60, "{}");
    insert_run(&conn, "interval:9000", "completed", 1, "{}"); // newest, fresh

    let (deleted, _) = prune_terminal_circuit_runs_older_than_inner(&conn, 30).unwrap();

    assert_eq!(deleted, 1);
    assert_eq!(identities(&conn), vec!["interval:9000"]);
}

/// In-flight work is never swept, however old. A run parked on an agent slot
/// for weeks is live state, not garbage.
#[test]
fn prune_keeps_active_runs_regardless_of_age() {
    let conn = prune_db();
    insert_run(&conn, "interval:1", "pending", 90, "{}");
    insert_run(&conn, "interval:2", "running", 90, "{}");
    insert_run(&conn, "interval:3", "paused", 90, "{}");

    let (deleted, _) = prune_terminal_circuit_runs_older_than_inner(&conn, 30).unwrap();

    assert_eq!(deleted, 0);
    assert_eq!(count(&conn, "autopilot_circuit_runs"), 3);
}

/// The regression this sweep must not cause. The interval cooldown anchors on
/// `MAX(created_at)` across the circuit's runs, and `interval_should_fire(None, ..)`
/// returns `true` — so sweeping a circuit's LAST row erases its cadence and fires
/// it immediately. Every run here is old and terminal; the newest must survive
/// and hold the anchor unchanged.
#[test]
fn prune_preserves_the_newest_run_so_the_interval_anchor_survives() {
    let conn = prune_db();
    insert_run(&conn, "interval:1000", "completed", 90, "{}");
    insert_run(&conn, "interval:2000", "completed", 75, "{}");
    insert_run(&conn, "interval:3000", "completed", 60, "{}");
    let before = anchor(&conn, 1);

    let (deleted, _) = prune_terminal_circuit_runs_older_than_inner(&conn, 30).unwrap();

    assert_eq!(deleted, 2);
    assert_eq!(identities(&conn), vec!["interval:3000"]);
    assert_eq!(
        before,
        anchor(&conn, 1),
        "the cadence anchor must be byte-identical after a sweep"
    );
}

/// The anchor is per-circuit, not global: sweeping circuit 1 must not strand
/// circuit 2 without its own newest row.
#[test]
fn prune_preserves_the_newest_run_of_every_circuit_independently() {
    let conn = prune_db();
    conn.execute(
        "INSERT INTO autopilot_circuits (id, mesh_id, name) VALUES (2, 1, 'c2')",
        [],
    )
    .unwrap();
    insert_run_for(&conn, 1, "interval:1000", "completed", 90, "{}");
    insert_run_for(&conn, 1, "interval:2000", "completed", 80, "{}");
    insert_run_for(&conn, 2, "interval:3000", "completed", 70, "{}");

    let (deleted, _) = prune_terminal_circuit_runs_older_than_inner(&conn, 30).unwrap();

    assert_eq!(deleted, 1, "only circuit 1's older row goes");
    assert_eq!(identities(&conn), vec!["interval:2000", "interval:3000"]);
}

/// GitHub identities are STABLE, so the row is the dedupe memory that stops a
/// circuit reprocessing the same issue. It survives forever; only the body it
/// carries — the actual bulk — is dropped.
#[test]
fn prune_keeps_github_rows_as_dedupe_tombstones_but_drops_their_bodies() {
    let conn = prune_db();
    let body = r#"{"issue":{"body":"a very long issue body ..."}}"#;
    insert_run(&conn, "issue:42:ready-for-agent", "completed", 60, body);
    insert_run(&conn, "pr:7:needs-review", "failed", 60, body);
    insert_run(&conn, "interval:9000", "completed", 1, "{}"); // newest, fresh

    let (deleted, compacted) = prune_terminal_circuit_runs_older_than_inner(&conn, 30).unwrap();

    assert_eq!(deleted, 0, "a stable identity is never deleted");
    assert_eq!(compacted, 2);
    assert_eq!(
        identities(&conn),
        vec![
            "issue:42:ready-for-agent",
            "pr:7:needs-review",
            "interval:9000"
        ],
        "every dedupe tombstone is still on file"
    );
    let mut stmt = conn
        .prepare(
            "SELECT context_json FROM autopilot_circuit_runs \
             WHERE trigger_identity NOT LIKE 'interval:%' ORDER BY id",
        )
        .unwrap();
    let bodies: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(bodies, vec!["{}", "{}"], "bodies are compacted away");
}

/// A GitHub run still inside the retention window keeps its body, so the Probe
/// tab's recent-run view still shows what it fired on.
#[test]
fn prune_leaves_recent_github_bodies_intact() {
    let conn = prune_db();
    let body = r#"{"issue":{"body":"recent"}}"#;
    insert_run(&conn, "issue:42:ready-for-agent", "completed", 5, body);

    let (deleted, compacted) = prune_terminal_circuit_runs_older_than_inner(&conn, 30).unwrap();

    assert_eq!((deleted, compacted), (0, 0));
    let stored: String = conn
        .query_row("SELECT context_json FROM autopilot_circuit_runs", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(stored, body);
}

/// An active GitHub run keeps its body however old — the stepper still reads
/// `context_json` to render that run's templates.
#[test]
fn prune_keeps_the_body_of_an_active_github_run() {
    let conn = prune_db();
    let body = r#"{"issue":{"body":"still running"}}"#;
    insert_run(&conn, "issue:42:ready-for-agent", "running", 90, body);

    let (deleted, compacted) = prune_terminal_circuit_runs_older_than_inner(&conn, 30).unwrap();

    assert_eq!((deleted, compacted), (0, 0));
    let stored: String = conn
        .query_row("SELECT context_json FROM autopilot_circuit_runs", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(stored, body);
}

/// A second sweep with nothing new to do reports zero work. The maintenance
/// worker logs only on a non-zero result, so a steady-state tick must stay
/// silent rather than re-compacting rows it already emptied.
#[test]
fn prune_is_idempotent_and_quiet_in_steady_state() {
    let conn = prune_db();
    insert_run(
        &conn,
        "issue:42:ready-for-agent",
        "completed",
        60,
        r#"{"a":1}"#,
    );
    insert_run(&conn, "interval:1000", "completed", 60, "{}");
    insert_run(&conn, "interval:2000", "completed", 40, "{}");

    assert_eq!(
        prune_terminal_circuit_runs_older_than_inner(&conn, 30).unwrap(),
        (1, 1)
    );
    assert_eq!(
        prune_terminal_circuit_runs_older_than_inner(&conn, 30).unwrap(),
        (0, 0),
        "steady state does no work and logs nothing"
    );
}

/// An empty table is a clean no-op, matching the drive-ledger sweep's contract.
#[test]
fn prune_on_an_empty_table_is_a_no_op() {
    let conn = prune_db();
    assert_eq!(
        prune_terminal_circuit_runs_older_than_inner(&conn, 30).unwrap(),
        (0, 0)
    );
}

/// A row exactly at the retention boundary is kept — the sweep uses
/// `updated_at <`, mirroring the drive-ledger sweep, so a row that just turned
/// 30 days old is not swept on the same tick.
#[test]
fn prune_keeps_a_row_at_the_retention_boundary() {
    let conn = prune_db();
    insert_run(&conn, "interval:1000", "completed", 30, "{}");
    insert_run(&conn, "interval:2000", "completed", 1, "{}"); // newest, fresh

    let (deleted, _) = prune_terminal_circuit_runs_older_than_inner(&conn, 30).unwrap();

    assert_eq!(deleted, 0);
    assert_eq!(count(&conn, "autopilot_circuit_runs"), 2);
}

/// Pins the shipped constant to the sweep's behaviour. Every test above passes
/// a literal window, which would stay green even if the worker were wired to a
/// nonsense retention value; this one straddles
/// [`CIRCUIT_RUN_RETENTION_DAYS`] itself.
#[test]
fn the_shipped_retention_constant_straddles_the_cutoff() {
    use crate::services::coordinator_ledger_maintenance::CIRCUIT_RUN_RETENTION_DAYS as DAYS;

    let conn = prune_db();
    insert_run(&conn, "interval:1000", "completed", DAYS + 1, "{}");
    insert_run(&conn, "interval:2000", "completed", DAYS - 1, "{}");
    insert_run(&conn, "interval:3000", "completed", 0, "{}"); // newest, holds the anchor

    let (deleted, _) = prune_terminal_circuit_runs_older_than_inner(&conn, DAYS).unwrap();

    assert_eq!(deleted, 1, "only the row past the window goes");
    assert_eq!(identities(&conn), vec!["interval:2000", "interval:3000"]);
}
