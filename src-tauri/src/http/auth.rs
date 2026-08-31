//! Role resolution and scope authorization for the embedded HTTP server
//! (issue #500, two-tier RBAC).
//!
//! This is the *policy* layer that sits above `request` (the IO/parse layer):
//! it maps a request's credentials — read from HEADERS ONLY (the `bm_session`
//! cookie or an `Authorization: Bearer` token; never the URL, #500 AC3) — onto a
//! [`Role`], and decides whether that role satisfies a route's [`RequiredScope`].
//!
//! The two roles are **disjoint surfaces**, not a privilege hierarchy:
//! - **Admin** (the root token) owns the mobile `/api/*` surface and the
//!   WebSockets. It is *not* accepted on the coordinator routes.
//! - **Coordinator** (read- or drive-scoped tokens) owns `/nodes*`. A
//!   coordinator token is *not* accepted on Admin routes — hitting one yields a
//!   `403 Forbidden` (authenticated but wrong surface), distinct from the
//!   `401 Unauthorized` returned when no valid credential is presented at all.

use rusqlite::Connection;
use crate::http::MaybeTls;

use crate::db;
use crate::http::request;

/// Which surface a presented credential proves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The root remote-access token — the mobile/admin surface.
    Admin,
    /// A coordinator read-scoped token.
    CoordinatorRead,
    /// A coordinator drive-scoped token (implies read on the coordinator surface).
    CoordinatorWrite,
}

/// The minimum capability a route demands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredScope {
    /// Mobile/admin routes — only the root token satisfies.
    Admin,
    /// Coordinator read routes — a read OR drive token satisfies.
    CoordinatorRead,
    /// Coordinator drive routes — only a drive token satisfies.
    CoordinatorWrite,
}

/// The result of an authorization check, carrying the HTTP status the dispatcher
/// must return on the two failure paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthOutcome {
    /// Authorized — proceed, carrying the resolved role.
    Ok(Role),
    /// No valid credential presented → `401 Unauthorized`.
    Unauthorized,
    /// A valid credential of the wrong role → `403 Forbidden`.
    Forbidden,
}

/// Resolve the role proven by a request's headers. A request carrying no cookie
/// and no bearer token returns `None` *without* touching the DB — so an
/// unauthenticated probe never reaches a lookup (and the inline dispatcher
/// tests, which run without an initialized global DB, stay DB-free). Otherwise it
/// locks the DB once and delegates to [`resolve_role_inner`], the single
/// resolution implementation the unit tests also drive against a seeded
/// connection — so there is no production/test logic to keep in lockstep.
pub fn resolve_role(headers: &str) -> Option<Role> {
    if request::extract_token_from_cookies(headers).is_none()
        && request::bearer_token(headers).is_none()
    {
        return None;
    }
    let conn = db::try_read_conn().ok()?;
    resolve_role_inner(&conn, headers)
}

/// The credential → [`Role`] resolution, checked in priority order against a
/// single connection: root first (cookie or bearer → Admin), then the bearer
/// token against the drive- then read-scoped coordinator tokens. Lock-free so
/// the unit tests can drive it against a seeded in-memory connection (the test
/// binary has no initialized global DB).
fn resolve_role_inner(conn: &Connection, headers: &str) -> Option<Role> {
    let cookie = request::extract_token_from_cookies(headers);
    let bearer = request::bearer_token(headers);

    // Admin: the root token, presented as either the bm_session cookie or a
    // bearer header (the latter is how POST /api/session mints the cookie); OR a
    // per-device session token (issue #502), which is also an Admin-surface
    // credential. Device tokens are what a paired phone holds after pairing —
    // distinct per device, so revoking one (deleting its row) drops *its* Admin
    // access without touching the root token or other devices.
    if let Some(t) = cookie.as_deref() {
        if db::validate_root_token_inner(conn, t).unwrap_or(false)
            || db::validate_device_token_inner(conn, t)
                .unwrap_or(None)
                .is_some()
        {
            return Some(Role::Admin);
        }
    }
    if let Some(t) = bearer.as_deref() {
        if db::validate_root_token_inner(conn, t).unwrap_or(false)
            || db::validate_device_token_inner(conn, t)
                .unwrap_or(None)
                .is_some()
        {
            return Some(Role::Admin);
        }
        // Coordinator surface: bearer only. Check drive before read so the
        // more-capable scope wins when both happen to match (they don't in
        // practice — distinct tokens — but the order makes the intent clear).
        if db::validate_coordinator_drive_token_inner(conn, t).unwrap_or(false) {
            return Some(Role::CoordinatorWrite);
        }
        if db::validate_coordinator_read_token_inner(conn, t).unwrap_or(false) {
            return Some(Role::CoordinatorRead);
        }
    }
    None
}

/// Recover the device-session id a request authenticates as, if any (issue
/// #502). Returns `None` for the root token (which has no device row and is
/// unrevocable) and for every coordinator credential. The dispatcher uses this
/// to stamp `last_active` and to bind a minted WS ticket to the device, so a
/// later revocation can find and kick that device's live socket. Mirrors
/// [`resolve_role`]'s DB-free fast path for unauthenticated probes.
pub fn resolve_device_session(headers: &str) -> Option<i64> {
    if request::extract_token_from_cookies(headers).is_none()
        && request::bearer_token(headers).is_none()
    {
        return None;
    }
    let conn = db::try_read_conn().ok()?;
    resolve_device_session_inner(&conn, headers)
}

fn resolve_device_session_inner(conn: &Connection, headers: &str) -> Option<i64> {
    if let Some(t) = request::extract_token_from_cookies(headers) {
        if let Some(id) = db::validate_device_token_inner(conn, &t).unwrap_or(None) {
            return Some(id);
        }
    }
    if let Some(t) = request::bearer_token(headers) {
        if let Some(id) = db::validate_device_token_inner(conn, &t).unwrap_or(None) {
            return Some(id);
        }
    }
    None
}

/// Does `role` satisfy `required`? Disjoint surfaces: Admin never satisfies a
/// coordinator scope and vice-versa; drive implies read.
fn satisfies(role: Role, required: RequiredScope) -> bool {
    matches!(
        (required, role),
        (RequiredScope::Admin, Role::Admin)
            | (
                RequiredScope::CoordinatorRead,
                Role::CoordinatorRead | Role::CoordinatorWrite
            )
            | (RequiredScope::CoordinatorWrite, Role::CoordinatorWrite)
    )
}

/// Authorize a request for a required scope. `None` resolved → `Unauthorized`
/// (401); a role that doesn't satisfy the scope → `Forbidden` (403).
pub fn authorize(headers: &str, required: RequiredScope) -> AuthOutcome {
    outcome(resolve_role(headers), required)
}

fn outcome(role: Option<Role>, required: RequiredScope) -> AuthOutcome {
    match role {
        None => AuthOutcome::Unauthorized,
        Some(role) if satisfies(role, required) => AuthOutcome::Ok(role),
        Some(_) => AuthOutcome::Forbidden,
    }
}

/// Dispatcher convenience: authorize and, on failure, write the matching status
/// line and return `None` so the caller can `return` immediately. On success
/// returns `Some(role)` and writes nothing.
pub async fn guard(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    headers: &str,
    required: RequiredScope,
) -> Option<Role> {
    match authorize(headers, required) {
        AuthOutcome::Ok(role) => Some(role),
        AuthOutcome::Unauthorized => {
            let _ = request::write_status_only(lines, "401 Unauthorized").await;
            None
        }
        AuthOutcome::Forbidden => {
            let _ = request::write_status_only(lines, "403 Forbidden").await;
            None
        }
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn root_token_bearer_resolves_admin() {
        let conn = seeded_db();
        let root = db::get_or_create_root_token_inner(&conn).unwrap();
        assert_eq!(
            resolve_role_inner(&conn, &bearer(&root)),
            Some(Role::Admin)
        );
    }

    #[test]
    fn root_token_cookie_resolves_admin() {
        let conn = seeded_db();
        let root = db::get_or_create_root_token_inner(&conn).unwrap();
        assert_eq!(
            resolve_role_inner(&conn, &cookie(&root)),
            Some(Role::Admin)
        );
    }

    #[test]
    fn device_token_bearer_resolves_admin() {
        // A paired device's token is an Admin-surface credential (issue #502).
        let conn = seeded_db();
        let (_, token) = db::pair_device_session_inner(&conn, Some("iPhone"), None).unwrap();
        assert_eq!(resolve_role_inner(&conn, &bearer(&token)), Some(Role::Admin));
    }

    #[test]
    fn device_token_cookie_resolves_admin() {
        let conn = seeded_db();
        let (_, token) = db::pair_device_session_inner(&conn, None, None).unwrap();
        assert_eq!(resolve_role_inner(&conn, &cookie(&token)), Some(Role::Admin));
    }

    #[test]
    fn revoked_device_token_is_unauthorized() {
        // The hard AC: once revoked, the token must stop authenticating — a
        // later HTTP request resolves to no role at all (→ 401).
        let conn = seeded_db();
        let (id, token) = db::pair_device_session_inner(&conn, None, None).unwrap();
        db::revoke_device_session_inner(&conn, id).unwrap();
        assert_eq!(resolve_role_inner(&conn, &bearer(&token)), None);
        assert_eq!(
            outcome(resolve_role_inner(&conn, &bearer(&token)), RequiredScope::Admin),
            AuthOutcome::Unauthorized
        );
    }

    #[test]
    fn resolve_device_session_recovers_the_id_for_a_device_but_not_the_root_token() {
        let conn = seeded_db();
        let (id, token) = db::pair_device_session_inner(&conn, None, None).unwrap();
        assert_eq!(resolve_device_session_inner(&conn, &bearer(&token)), Some(id));
        assert_eq!(resolve_device_session_inner(&conn, &cookie(&token)), Some(id));
        // The root token authenticates as Admin but owns no device row.
        let root = db::get_or_create_root_token_inner(&conn).unwrap();
        assert_eq!(resolve_device_session_inner(&conn, &bearer(&root)), None);
    }

    #[test]
    fn coordinator_read_token_resolves_read() {
        let conn = seeded_db();
        let read = db::generate_coordinator_read_token_inner(&conn).unwrap();
        db::set_coordinator_api_enabled_inner(&conn, true).unwrap();
        assert_eq!(
            resolve_role_inner(&conn, &bearer(&read)),
            Some(Role::CoordinatorRead)
        );
    }

    #[test]
    fn coordinator_drive_token_resolves_write_and_satisfies_read() {
        let conn = seeded_db();
        let drive = db::generate_coordinator_drive_token_inner(&conn).unwrap();
        db::set_coordinator_api_enabled_inner(&conn, true).unwrap();
        db::set_coordinator_drive_enabled_inner(&conn, true).unwrap();
        let role = resolve_role_inner(&conn, &bearer(&drive));
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
        let role = resolve_role_inner(&conn, &bearer(&read));
        assert_eq!(outcome(role, RequiredScope::Admin), AuthOutcome::Forbidden);
    }

    #[test]
    fn root_token_on_coordinator_route_is_forbidden() {
        // Disjoint surfaces: the root token does NOT reach the coordinator API.
        let conn = seeded_db();
        let root = db::get_or_create_root_token_inner(&conn).unwrap();
        let role = resolve_role_inner(&conn, &bearer(&root));
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
        let role = resolve_role_inner(&conn, &bearer(&read));
        assert_eq!(
            outcome(role, RequiredScope::CoordinatorWrite),
            AuthOutcome::Forbidden
        );
    }

    #[test]
    fn no_credential_is_unauthorized() {
        let conn = seeded_db();
        let headers = "Host: localhost\r\n";
        assert_eq!(resolve_role_inner(&conn, headers), None);
        assert_eq!(
            outcome(None, RequiredScope::Admin),
            AuthOutcome::Unauthorized
        );
    }

    #[test]
    fn invalid_bearer_is_unauthorized() {
        let conn = seeded_db();
        let role = resolve_role_inner(&conn, &bearer("not-a-real-token"));
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
        assert_eq!(resolve_role_inner(&conn, headers), None);
    }
}
