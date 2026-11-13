//! Unit tests for `device_session_expiry`.
//!
//! The suite lives in this sibling file (not inline in the owner) so the
//! owner carries no test bodies — see `docs/development/rust-conventions.md`.

use super::*;
use rusqlite::Connection;

/// In-memory connection with just the `device_sessions` table the prune
/// helper needs. Kept in sync with the `CREATE TABLE` in `db::init`.
fn db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE device_sessions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            token_hash TEXT NOT NULL UNIQUE,
            label TEXT,
            last_ip TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            last_active_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )
    .unwrap();
    conn
}

/// Insert a device session whose activity is `modifier` away from now.
/// `None` means "now" (a device that just authenticated).
fn insert(conn: &Connection, hash: &str, modifier: Option<&str>) {
    let modifier_sql = match modifier {
        Some(m) => format!(", {m}"),
        None => String::new(),
    };
    conn.execute(
        &format!(
            "INSERT INTO device_sessions (token_hash, last_active_at) \
             VALUES (?1, datetime('now'{modifier_sql}))"
        ),
        [hash],
    )
    .unwrap();
}

/// The headline AC: an orphan is deleted, a device that is still in use
/// survives.
#[test]
fn the_sweep_deletes_idle_rows_and_keeps_active_ones() {
    let conn = db();
    insert(&conn, "fresh", None);
    insert(
        &conn,
        "idle",
        Some(&format!("'-{} days'", DEVICE_SESSION_IDLE_DAYS + 1)),
    );

    let expired =
        crate::db::prune_inactive_device_sessions_inner(&conn, DEVICE_SESSION_IDLE_DAYS).unwrap();
    assert_eq!(expired.len(), 1);

    let remaining: Vec<String> = conn
        .prepare("SELECT token_hash FROM device_sessions")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<String>, _>>()
        .unwrap();
    assert_eq!(remaining, vec!["fresh".to_string()]);
}

/// A row exactly on the boundary survives — the comparison is strict `<`,
/// so a device that just turned the idle age is not deleted on the same
/// tick.
#[test]
fn the_boundary_row_is_kept() {
    let conn = db();
    insert(
        &conn,
        "boundary",
        Some(&format!("'-{} days'", DEVICE_SESSION_IDLE_DAYS)),
    );
    let expired =
        crate::db::prune_inactive_device_sessions_inner(&conn, DEVICE_SESSION_IDLE_DAYS).unwrap();
    assert!(expired.is_empty());
}

/// A device that keeps authenticating never ages into the sweep's window:
/// the refresh in the auth layer keeps its `last_active_at` warm.
#[test]
fn a_refreshed_device_never_becomes_idle() {
    let conn = db();
    insert(&conn, "busy", Some("'-100 days'"));
    // The row is a hundred days idle — but the device authenticated a
    // moment ago, so the refresh moved its activity forward.
    crate::db::refresh_device_session_activity_inner(&conn, 1, Some("10.0.0.5")).unwrap();

    let expired =
        crate::db::prune_inactive_device_sessions_inner(&conn, DEVICE_SESSION_IDLE_DAYS).unwrap();
    assert!(expired.is_empty());
}

/// An empty table is a no-op: the sweep logs only on a non-zero prune, so
/// it must return `Ok(0)` cleanly rather than erroring.
#[test]
fn an_empty_table_is_a_no_op() {
    let conn = db();
    let expired =
        crate::db::prune_inactive_device_sessions_inner(&conn, DEVICE_SESSION_IDLE_DAYS).unwrap();
    assert!(expired.is_empty());
}
