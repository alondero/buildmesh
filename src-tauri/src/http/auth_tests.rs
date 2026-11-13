//! Unit tests for `auth`.
//!
//! The suite lives in this sibling file (not inline in the owner) so the
//! owner carries no test bodies — see `docs/development/rust-conventions.md`.

use super::*;

/// A minimal current-schema in-memory DB with the auth keys the resolver
/// reads. Mirrors the seed in `coordinator::tests` but only needs
/// `app_settings` since role resolution never touches meshes/nodes.
fn seeded_db() -> Connection {
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

fn cookie(token: &str) -> String {
    format!("Host: localhost\r\nCookie: bm_session={}\r\n", token)
}

fn bearer(token: &str) -> String {
    format!("Host: localhost\r\nAuthorization: Bearer {}\r\n", token)
}

/// The role one credential walk proves. The walk also reports the device
/// the credential belongs to; these tests are about the role half.
fn role_of(conn: &Connection, headers: &str) -> Option<Role> {
    resolve_auth_inner(conn, headers).unwrap().role
}

/// The device id one credential walk proves.
fn device_of(conn: &Connection, headers: &str) -> Option<i64> {
    resolve_auth_inner(conn, headers)
        .unwrap()
        .device
        .map(|device| device.id)
}

#[test]
fn root_token_bearer_resolves_admin() {
    let conn = seeded_db();
    let root = db::get_or_create_root_token_inner(&conn).unwrap();
    assert_eq!(role_of(&conn, &bearer(&root)), Some(Role::Admin));
}

#[test]
fn root_token_cookie_resolves_admin() {
    let conn = seeded_db();
    let root = db::get_or_create_root_token_inner(&conn).unwrap();
    assert_eq!(role_of(&conn, &cookie(&root)), Some(Role::Admin));
}

#[test]
fn device_token_bearer_resolves_admin() {
    // A paired device's token is an Admin-surface credential (issue #502).
    let conn = seeded_db();
    let (_, token) = db::pair_device_session_inner(&conn, Some("iPhone"), None).unwrap();
    assert_eq!(role_of(&conn, &bearer(&token)), Some(Role::Admin));
}

#[test]
fn device_token_cookie_resolves_admin() {
    let conn = seeded_db();
    let (_, token) = db::pair_device_session_inner(&conn, None, None).unwrap();
    assert_eq!(role_of(&conn, &cookie(&token)), Some(Role::Admin));
}

#[test]
fn revoked_device_token_is_unauthorized() {
    // The hard AC: once revoked, the token must stop authenticating — a
    // later HTTP request resolves to no role at all (→ 401).
    let conn = seeded_db();
    let (id, token) = db::pair_device_session_inner(&conn, None, None).unwrap();
    db::revoke_device_session_inner(&conn, id).unwrap();
    assert_eq!(role_of(&conn, &bearer(&token)), None);
    assert_eq!(
        outcome(role_of(&conn, &bearer(&token)), RequiredScope::Admin),
        AuthOutcome::Unauthorized
    );
}

#[test]
fn unavailable_database_is_not_reported_as_revoked_credentials() {
    let response = deny_response(AuthOutcome::Unavailable).unwrap();
    assert_eq!(response.status_code(), 503);
}

#[test]
fn one_walk_reports_both_the_role_and_the_device() {
    // Issue #589: the WS-ticket mint needs the role *and* the device id.
    // One walk now answers both, so the two can never disagree.
    let conn = seeded_db();
    let (id, token) = db::pair_device_session_inner(&conn, None, None).unwrap();
    let walk = resolve_auth_inner(&conn, &bearer(&token)).unwrap();
    assert_eq!(walk.role, Some(Role::Admin));
    assert_eq!(walk.device.map(|device| device.id), Some(id));
    // The root token authenticates as Admin but owns no device row.
    let root = db::get_or_create_root_token_inner(&conn).unwrap();
    let root_walk = resolve_auth_inner(&conn, &bearer(&root)).unwrap();
    assert_eq!(root_walk.role, Some(Role::Admin));
    assert_eq!(root_walk.device, None);
}

#[test]
fn a_freshly_paired_device_is_not_stale_but_an_idle_one_is() {
    // The activity stamp the expiry sweep measures against is refreshed
    // when — and only when — the row's recorded activity has gone stale.
    let conn = seeded_db();
    let (id, token) = db::pair_device_session_inner(&conn, None, None).unwrap();
    let fresh = resolve_auth_inner(&conn, &bearer(&token)).unwrap();
    assert!(!fresh.device.expect("device").activity_stale);

    conn.execute(
        "UPDATE device_sessions SET last_active_at = datetime('now', '-2 hours') WHERE id = ?1",
        [id],
    )
    .unwrap();
    let idle = resolve_auth_inner(&conn, &bearer(&token)).unwrap();
    assert!(idle.device.expect("device").activity_stale);
}

#[test]
fn a_storage_fault_is_not_reported_as_a_revoked_credential() {
    // The walk reads the device row through the same connection the role
    // checks use, so a failure there is a failure of the whole walk — it must
    // surface as "unavailable" (503), not as "no valid credential" (401). A
    // 401 signs the phone out and clears its credential; a storage fault must
    // never do that.
    //
    // The device_sessions table is dropped so the lookup fails the way a
    // broken schema or a locked file would.
    let conn = seeded_db();
    conn.execute_batch("DROP TABLE device_sessions;").unwrap();
    assert!(
        resolve_auth_inner(&conn, &bearer("some-token")).is_err(),
        "a failing lookup must not read as an unauthenticated request"
    );
}

#[test]
fn coordinator_read_token_resolves_read() {
    let conn = seeded_db();
    let read = db::generate_coordinator_read_token_inner(&conn).unwrap();
    db::set_coordinator_api_enabled_inner(&conn, true).unwrap();
    assert_eq!(role_of(&conn, &bearer(&read)), Some(Role::CoordinatorRead));
}

#[test]
fn coordinator_drive_token_resolves_write_and_satisfies_read() {
    let conn = seeded_db();
    let drive = db::generate_coordinator_drive_token_inner(&conn).unwrap();
    db::set_coordinator_api_enabled_inner(&conn, true).unwrap();
    db::set_coordinator_drive_enabled_inner(&conn, true).unwrap();
    let role = role_of(&conn, &bearer(&drive));
    assert_eq!(role, Some(Role::CoordinatorWrite));
    // Drive satisfies a read-scoped route.
    assert_eq!(
        outcome(role, RequiredScope::CoordinatorRead),
        AuthOutcome::Ok(Role::CoordinatorWrite)
    );
}

#[test]
fn coordinator_token_on_admin_route_is_forbidden() {
    // AC2: a valid coordinator token must be 403 (not 401) on an Admin route.
    let conn = seeded_db();
    let read = db::generate_coordinator_read_token_inner(&conn).unwrap();
    db::set_coordinator_api_enabled_inner(&conn, true).unwrap();
    let role = role_of(&conn, &bearer(&read));
    assert_eq!(outcome(role, RequiredScope::Admin), AuthOutcome::Forbidden);
}

#[test]
fn root_token_on_coordinator_route_is_forbidden() {
    // Disjoint surfaces: the root token does NOT reach the coordinator API.
    let conn = seeded_db();
    let root = db::get_or_create_root_token_inner(&conn).unwrap();
    let role = role_of(&conn, &bearer(&root));
    assert_eq!(
        outcome(role, RequiredScope::CoordinatorRead),
        AuthOutcome::Forbidden
    );
}

#[test]
fn read_token_cannot_drive() {
    let conn = seeded_db();
    let read = db::generate_coordinator_read_token_inner(&conn).unwrap();
    db::set_coordinator_api_enabled_inner(&conn, true).unwrap();
    let role = role_of(&conn, &bearer(&read));
    assert_eq!(
        outcome(role, RequiredScope::CoordinatorWrite),
        AuthOutcome::Forbidden
    );
}

#[test]
fn no_credential_is_unauthorized() {
    let conn = seeded_db();
    let headers = "Host: localhost\r\n";
    assert_eq!(role_of(&conn, headers), None);
    assert_eq!(
        outcome(None, RequiredScope::Admin),
        AuthOutcome::Unauthorized
    );
}

#[test]
fn invalid_bearer_is_unauthorized() {
    let conn = seeded_db();
    let role = role_of(&conn, &bearer("not-a-real-token"));
    assert_eq!(role, None);
    assert_eq!(
        outcome(role, RequiredScope::Admin),
        AuthOutcome::Unauthorized
    );
}

#[test]
fn url_token_is_never_a_credential() {
    // A `?token=` in some URL is irrelevant: resolution reads headers only.
    // Headers carrying no Authorization/Cookie resolve to None even though a
    // real root token exists in the DB.
    let conn = seeded_db();
    let _root = db::get_or_create_root_token_inner(&conn).unwrap();
    let headers = "Host: localhost\r\n";
    assert_eq!(role_of(&conn, headers), None);
}

#[test]
fn a_revoked_device_is_reported_as_no_device_in_the_same_walk() {
    // The single walk must not report a device the caller cannot act on:
    // a revoked row is gone, so the walk reports neither role nor device.
    let conn = seeded_db();
    let (id, token) = db::pair_device_session_inner(&conn, None, None).unwrap();
    db::revoke_device_session_inner(&conn, id).unwrap();
    assert_eq!(device_of(&conn, &bearer(&token)), None);
    assert_eq!(role_of(&conn, &bearer(&token)), None);
}
