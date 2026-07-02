//! Unit tests for the coordinator drive idempotency ledger (issue #320).
//!
//! Exercises the lock-free `_inner` helpers against an in-memory connection,
//! following the `device_session_tests` pattern. The ledger is what makes a
//! Coordinator's retry a no-op: a `(node_id, key)` records the honest verdict
//! once and replays it thereafter (ADR-0008 §6).

use crate::db;
use rusqlite::Connection;

/// In-memory connection with just the `coordinator_drive_prompts` table the
/// helpers need. Kept in sync with the `CREATE TABLE` in `db::init`.
fn db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE coordinator_drive_prompts (
            node_id INTEGER NOT NULL,
            idempotency_key TEXT NOT NULL,
            verdict TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (node_id, idempotency_key)
        );",
    )
    .unwrap();
    conn
}

#[test]
fn unknown_key_looks_up_to_none() {
    let conn = db();
    assert_eq!(
        db::lookup_drive_prompt_verdict_inner(&conn, 7, "never-seen").unwrap(),
        None
    );
}

#[test]
fn record_then_lookup_round_trips() {
    let conn = db();
    db::record_drive_prompt_verdict_inner(&conn, 7, "k", "delivered").unwrap();
    assert_eq!(
        db::lookup_drive_prompt_verdict_inner(&conn, 7, "k").unwrap(),
        Some("delivered".to_string())
    );
}

#[test]
fn lookup_is_scoped_by_node() {
    // The same key on a different node is a distinct row — an accidentally
    // reused key must not leak one node's verdict to another.
    let conn = db();
    db::record_drive_prompt_verdict_inner(&conn, 7, "shared", "delivered").unwrap();
    assert_eq!(
        db::lookup_drive_prompt_verdict_inner(&conn, 9, "shared").unwrap(),
        None,
        "node 9 has never seen this key"
    );
}

#[test]
fn a_read_error_propagates_rather_than_looking_like_none() {
    // Fail-safe (issue #320 review): a genuine read failure must be `Err`, NOT
    // `Ok(None)` — otherwise the drive path mistakes "couldn't check" for "key
    // never seen" and re-delivers. Here the ledger table is absent, so the SELECT
    // errors; the helper must surface that error, not swallow it to None.
    let conn = Connection::open_in_memory().unwrap();
    assert!(
        db::lookup_drive_prompt_verdict_inner(&conn, 7, "k").is_err(),
        "an unreadable ledger must propagate an error, not report the key as new"
    );
}

#[test]
fn first_write_wins_on_a_duplicate_key() {
    // `INSERT OR IGNORE` keeps the original verdict authoritative even if a racing
    // second write slipped through with a different verdict — the retry replays
    // exactly what the first send produced.
    let conn = db();
    db::record_drive_prompt_verdict_inner(&conn, 7, "k", "delivered").unwrap();
    db::record_drive_prompt_verdict_inner(&conn, 7, "k", "unverified").unwrap();
    assert_eq!(
        db::lookup_drive_prompt_verdict_inner(&conn, 7, "k").unwrap(),
        Some("delivered".to_string()),
        "the first recorded verdict must not be overwritten"
    );
}
