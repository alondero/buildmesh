//! Unit tests for the persistent device-session store (issue #502).
//!
//! Exercises the lock-free `_inner` helpers against an in-memory connection,
//! mirroring `http::auth::tests` — role resolution and these CRUD helpers share
//! the same `device_sessions` table shape.

use crate::db;
use rusqlite::Connection;

/// In-memory connection with just the `device_sessions` table the helpers need.
/// Kept in sync with the `CREATE TABLE` in `db::init`.
fn db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE device_sessions (
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

#[test]
fn pair_then_validate_roundtrips_to_the_same_id() {
    let conn = db();
    let (id, token) =
        db::pair_device_session_inner(&conn, Some("Safari on iPhone"), Some("10.0.0.5")).unwrap();
    assert_eq!(
        db::validate_device_token_inner(&conn, &token).unwrap(),
        Some(id)
    );
}

#[test]
fn raw_token_is_never_stored() {
    // Only the SHA-256 hash lands in the table — a DB dump can't reveal the token.
    let conn = db();
    let (_, token) = db::pair_device_session_inner(&conn, None, None).unwrap();
    let stored: String = conn
        .query_row("SELECT token_hash FROM device_sessions", [], |r| r.get(0))
        .unwrap();
    assert_ne!(stored, token, "token_hash must not equal the raw token");
    assert_eq!(stored.len(), 64, "SHA-256 hex is 64 chars");
}

#[test]
fn unknown_and_empty_tokens_do_not_validate() {
    let conn = db();
    db::pair_device_session_inner(&conn, None, None).unwrap();
    assert_eq!(
        db::validate_device_token_inner(&conn, "not-a-real-token").unwrap(),
        None
    );
    assert_eq!(db::validate_device_token_inner(&conn, "").unwrap(), None);
}

#[test]
fn revoke_deletes_the_row_so_the_token_stops_validating() {
    let conn = db();
    let (id, token) = db::pair_device_session_inner(&conn, None, None).unwrap();
    assert!(db::revoke_device_session_inner(&conn, id).unwrap());
    // The whole point of #502: a revoked token must immediately fail auth.
    assert_eq!(
        db::validate_device_token_inner(&conn, &token).unwrap(),
        None
    );
    // Revoking an already-gone id reports nothing was removed.
    assert!(!db::revoke_device_session_inner(&conn, id).unwrap());
}

#[test]
fn list_returns_metadata_without_the_hash() {
    let conn = db();
    let (id, _) = db::pair_device_session_inner(&conn, Some("Pixel"), Some("192.168.1.9")).unwrap();
    let devices = db::list_device_sessions_inner(&conn).unwrap();
    assert_eq!(devices.len(), 1);
    let d = &devices[0];
    assert_eq!(d.id, id);
    assert_eq!(d.label.as_deref(), Some("Pixel"));
    assert_eq!(d.last_ip.as_deref(), Some("192.168.1.9"));
    assert!(!d.created_at.is_empty());
    assert!(!d.last_active_at.is_empty());
}

#[test]
fn refreshing_the_activity_refreshes_the_last_seen_ip() {
    // Roaming: a device's IP changes but its token (and id) stay put.
    let conn = db();
    let (id, _) = db::pair_device_session_inner(&conn, None, Some("10.0.0.1")).unwrap();
    db::refresh_device_session_activity_inner(&conn, id, Some("172.16.0.4")).unwrap();
    let devices = db::list_device_sessions_inner(&conn).unwrap();
    assert_eq!(devices[0].last_ip.as_deref(), Some("172.16.0.4"));
}

#[test]
fn refreshing_the_activity_without_an_address_keeps_the_last_seen_ip() {
    // A refresh that has no peer address to record must not erase the
    // address the device was last seen from.
    let conn = db();
    let (id, _) = db::pair_device_session_inner(&conn, None, Some("10.0.0.1")).unwrap();
    db::refresh_device_session_activity_inner(&conn, id, None).unwrap();
    let devices = db::list_device_sessions_inner(&conn).unwrap();
    assert_eq!(devices[0].last_ip.as_deref(), Some("10.0.0.1"));
}

#[test]
fn refreshing_the_activity_is_a_noop_for_an_unknown_id() {
    let conn = db();
    // No row with id 999 — must not error (the row may have just been revoked).
    db::refresh_device_session_activity_inner(&conn, 999, Some("1.2.3.4")).unwrap();
    assert!(db::list_device_sessions_inner(&conn).unwrap().is_empty());
}

/// Insert a device session whose activity is `modifier` away from now.
/// `None` means "now" (a device that just authenticated). `modifier` is a
/// SQLite datetime modifier such as `"-91 days"`; it is quoted here because
/// SQLite only accepts the modifier slot as a string literal.
fn insert_idle(conn: &Connection, hash: &str, modifier: Option<&str>) {
    let modifier_sql = match modifier {
        Some(m) => format!(", '{m}'"),
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

#[test]
fn an_expired_session_stops_authenticating() {
    // The headline AC for issue #589: the sweep deletes the orphan's row, and
    // the token that row held stops resolving — so it can no longer
    // authenticate as Admin.
    let conn = db();
    let (id, token) = db::pair_device_session_inner(&conn, Some("Pixel"), None).unwrap();
    conn.execute(
        "UPDATE device_sessions SET last_active_at = datetime('now', '-200 days') WHERE id = ?1",
        [id],
    )
    .unwrap();
    assert_eq!(
        db::validate_device_token_inner(&conn, &token).unwrap(),
        Some(id),
        "the row is still there before the sweep"
    );

    let expired = db::prune_inactive_device_sessions_inner(&conn, 90).unwrap();
    assert_eq!(expired, vec![id]);
    assert_eq!(
        db::validate_device_token_inner(&conn, &token).unwrap(),
        None,
        "a swept device session must not authenticate any more"
    );
    assert!(db::list_device_sessions_inner(&conn).unwrap().is_empty());
}

#[test]
fn the_sweep_keeps_a_device_that_is_still_authenticating() {
    let conn = db();
    let (id, _) = db::pair_device_session_inner(&conn, None, None).unwrap();
    conn.execute(
        "UPDATE device_sessions SET last_active_at = datetime('now', '-200 days') WHERE id = ?1",
        [id],
    )
    .unwrap();
    // The device authenticates again — the auth layer refreshes its activity.
    db::refresh_device_session_activity_inner(&conn, id, None).unwrap();

    let expired = db::prune_inactive_device_sessions_inner(&conn, 90).unwrap();
    assert!(expired.is_empty());
    assert_eq!(db::list_device_sessions_inner(&conn).unwrap().len(), 1);
}

#[test]
fn the_sweep_only_touches_rows_that_are_idle() {
    let conn = db();
    insert_idle(&conn, "fresh", None);
    insert_idle(&conn, "idle", Some("-91 days"));
    insert_idle(&conn, "older", Some("-400 days"));

    let expired = db::prune_inactive_device_sessions_inner(&conn, 90).unwrap();
    assert_eq!(expired.len(), 2);
    let remaining: Vec<String> = conn
        .prepare("SELECT token_hash FROM device_sessions")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<String>, _>>()
        .unwrap();
    assert_eq!(remaining, vec!["fresh".to_string()]);
}

#[test]
fn resolving_a_device_session_reports_whether_its_activity_is_stale() {
    // The auth layer refreshes the activity only when it is stale, so the
    // lookup has to answer both halves in one query.
    let conn = db();
    let (id, token) = db::pair_device_session_inner(&conn, None, None).unwrap();
    let resolved = db::resolve_device_session_inner(&conn, &token)
        .unwrap()
        .unwrap();
    assert_eq!(resolved.id, id);
    assert!(!resolved.activity_stale);

    conn.execute(
        "UPDATE device_sessions SET last_active_at = datetime('now', '-2 hours') WHERE id = ?1",
        [id],
    )
    .unwrap();
    let idle = db::resolve_device_session_inner(&conn, &token)
        .unwrap()
        .unwrap();
    assert!(idle.activity_stale);
}

#[test]
fn resolving_an_unknown_or_empty_device_token_reports_no_session() {
    let conn = db();
    db::pair_device_session_inner(&conn, None, None).unwrap();
    assert_eq!(
        db::resolve_device_session_inner(&conn, "not-a-real-token").unwrap(),
        None
    );
    assert_eq!(db::resolve_device_session_inner(&conn, "").unwrap(), None);
}
