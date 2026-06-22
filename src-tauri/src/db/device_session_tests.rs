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
    assert_eq!(db::validate_device_token_inner(&conn, &token).unwrap(), Some(id));
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
    assert_eq!(db::validate_device_token_inner(&conn, &token).unwrap(), None);
    // Revoking an already-gone id reports nothing was removed.
    assert!(!db::revoke_device_session_inner(&conn, id).unwrap());
}

#[test]
fn list_returns_metadata_without_the_hash() {
    let conn = db();
    let (id, _) =
        db::pair_device_session_inner(&conn, Some("Pixel"), Some("192.168.1.9")).unwrap();
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
fn touch_refreshes_the_last_seen_ip() {
    // Roaming: a device's IP changes but its token (and id) stay put.
    let conn = db();
    let (id, _) = db::pair_device_session_inner(&conn, None, Some("10.0.0.1")).unwrap();
    db::touch_device_session_inner(&conn, id, Some("172.16.0.4")).unwrap();
    let devices = db::list_device_sessions_inner(&conn).unwrap();
    assert_eq!(devices[0].last_ip.as_deref(), Some("172.16.0.4"));
}

#[test]
fn touch_is_a_noop_for_an_unknown_id() {
    let conn = db();
    // No row with id 999 — must not error (the row may have just been revoked).
    db::touch_device_session_inner(&conn, 999, Some("1.2.3.4")).unwrap();
    assert!(db::list_device_sessions_inner(&conn).unwrap().is_empty());
}

#[test]
fn login_with_root_token_pairs_a_new_device() {
    let conn = db();
    let root = db::get_or_create_root_token_inner(&conn).unwrap();
    let (id, token) =
        db::login_device_session_inner(&conn, &root, Some("iPhone"), Some("10.0.0.2"))
            .unwrap()
            .expect("root token must pair a device");
    // The returned token is a fresh device token, not the root token.
    assert_ne!(token, root);
    assert_eq!(db::validate_device_token_inner(&conn, &token).unwrap(), Some(id));
    assert_eq!(db::list_device_sessions_inner(&conn).unwrap().len(), 1);
}

#[test]
fn login_with_existing_device_token_refreshes_without_minting_a_second_device() {
    // A re-launching phone re-presents its device token; it must keep its
    // identity, not accumulate a new row each time.
    let conn = db();
    let root = db::get_or_create_root_token_inner(&conn).unwrap();
    let (id, device_token) =
        db::login_device_session_inner(&conn, &root, None, Some("10.0.0.2"))
            .unwrap()
            .unwrap();

    let (again_id, again_token) =
        db::login_device_session_inner(&conn, &device_token, None, Some("172.16.9.9"))
            .unwrap()
            .expect("a known device token must refresh");
    assert_eq!(again_id, id, "same device id on refresh");
    assert_eq!(again_token, device_token, "same token handed back");
    assert_eq!(
        db::list_device_sessions_inner(&conn).unwrap().len(),
        1,
        "refresh must not create a second device"
    );
    // Roaming IP was recorded.
    assert_eq!(
        db::list_device_sessions_inner(&conn).unwrap()[0].last_ip.as_deref(),
        Some("172.16.9.9")
    );
}

#[test]
fn login_with_an_unknown_token_is_rejected() {
    let conn = db();
    db::get_or_create_root_token_inner(&conn).unwrap();
    assert_eq!(
        db::login_device_session_inner(&conn, "bogus", None, None).unwrap(),
        None
    );
}
