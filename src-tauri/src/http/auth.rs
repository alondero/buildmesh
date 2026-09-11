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

/// Everything a request's credentials prove, resolved in a **single**
/// reader-pool lease.
///
/// Before issue #1533's review, the ws-ticket mint path resolved the role
/// (one `try_read_conn()`), dropped that lease, then resolved the device
/// session (a second `try_read_conn()`, re-running the same
/// `validate_device_token_inner` query), and only then locked the writer —
/// three separate acquisitions for one endpoint, in a fix whose whole point
/// was to stop starving the reader pool. Bundling the two answers means one
/// lease and one device-token lookup per request (review item 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    /// The surface this credential proves.
    pub role: Role,
    /// The `device_sessions` row id this request authenticates as, if any
    /// (issue #502). `None` for the root token (no device row, unrevocable)
    /// and for every coordinator credential.
    pub device_session_id: Option<i64>,
}

/// The result of an authorization check, carrying the HTTP status the dispatcher
/// must return on the failure paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthOutcome {
    /// Authorized — proceed, carrying the resolved [`Identity`].
    Ok(Identity),
    /// No valid credential presented → `401 Unauthorized`.
    Unauthorized,
    /// A valid credential of the wrong role → `403 Forbidden`.
    Forbidden,
    /// The reader pool is saturated — the request would otherwise be
    /// authenticated, but we can't read the credential row to confirm.
    /// `503 Service Unavailable` (issue #1533 review: distinguishing
    /// "no credentials" from "DB busy" prevents the pre-#1533 regression
    /// where valid clients under load got logged out and had their
    /// sessions cleared by a misleading 401).
    ServiceUnavailable,
}

/// Resolve everything a request's headers prove, in one reader-pool lease.
/// A request carrying no cookie and no bearer token returns `Ok(None)`
/// *without* touching the DB — so an unauthenticated probe never reaches a
/// lookup (and the inline dispatcher tests, which run without an initialized
/// global DB, stay DB-free). Otherwise it checks out **one** connection and
/// delegates to [`resolve_identity_inner`], the single resolution
/// implementation the unit tests also drive against a seeded connection — so
/// there is no production/test logic to keep in lockstep.
///
/// Returns [`db::DbResult<Option<Identity>>`] (issue #1533 review) so the
/// caller can distinguish "no credentials" (`Ok(None)` → `401`) from "DB
/// busy" (`Err(DbError::ReaderPoolExhausted { .. })` → `503`). The pre-#1533
/// `Option<Role>` collapsed these two into `401`, logging valid clients out
/// under load.
pub fn resolve_identity(headers: &str) -> db::DbResult<Option<Identity>> {
    if request::extract_token_from_cookies(headers).is_none()
        && request::bearer_token(headers).is_none()
    {
        return Ok(None);
    }
    let conn = db::try_read_conn()?;
    Ok(resolve_identity_inner(&conn, headers))
}

/// The credential → [`Identity`] resolution against a single connection.
/// Lock-free so the unit tests can drive it against a seeded in-memory
/// connection (the test binary has no initialized global DB).
///
/// Each credential's device-session lookup runs **at most once** and its
/// result feeds both the role decision and the device-session id. The
/// pre-review shape ran that query twice — once in the role resolver, once
/// in a separate device resolver on a second lease (review item 3).
///
/// Precedence is unchanged from that shape, deliberately:
/// - Role: cookie (root, then device) → bearer (root, then device, then
///   coordinator drive, then coordinator read). Drive is checked before read
///   so the more-capable scope wins when both match.
/// - Device id: cookie's device row first, then the bearer's. This is
///   independent of *which* credential proved the role, so a request
///   presenting a root cookie alongside a device bearer still resolves the
///   device — the behaviour the separate resolver had.
fn resolve_identity_inner(conn: &Connection, headers: &str) -> Option<Identity> {
    let cookie = request::extract_token_from_cookies(headers);
    let bearer = request::bearer_token(headers);

    let cookie_device = cookie
        .as_deref()
        .and_then(|t| db::validate_device_token_inner(conn, t).unwrap_or(None));
    let bearer_device = bearer
        .as_deref()
        .and_then(|t| db::validate_device_token_inner(conn, t).unwrap_or(None));
    let device_session_id = cookie_device.or(bearer_device);

    // Admin: the root token, presented as either the bm_session cookie or a
    // bearer header (the latter is how POST /api/session mints the cookie); OR a
    // per-device session token (issue #502), which is also an Admin-surface
    // credential. Device tokens are what a paired phone holds after pairing —
    // distinct per device, so revoking one (deleting its row) drops *its* Admin
    // access without touching the root token or other devices.
    let role = if cookie
        .as_deref()
        .is_some_and(|t| db::validate_root_token_inner(conn, t).unwrap_or(false))
        || cookie_device.is_some()
    {
        Some(Role::Admin)
    } else if let Some(t) = bearer.as_deref() {
        if db::validate_root_token_inner(conn, t).unwrap_or(false) || bearer_device.is_some() {
            Some(Role::Admin)
        } else if db::validate_coordinator_drive_token_inner(conn, t).unwrap_or(false) {
            Some(Role::CoordinatorWrite)
        } else if db::validate_coordinator_read_token_inner(conn, t).unwrap_or(false) {
            Some(Role::CoordinatorRead)
        } else {
            None
        }
    } else {
        None
    };

    role.map(|role| Identity {
        role,
        device_session_id,
    })
}

/// Test-only projection of [`resolve_identity_inner`] down to the role. Keeps
/// the role-resolution tests reading as role assertions without giving
/// production a second entry point that could drift from the real resolver.
#[cfg(test)]
fn resolve_role_inner(conn: &Connection, headers: &str) -> Option<Role> {
    resolve_identity_inner(conn, headers).map(|i| i.role)
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
/// (401); a role that doesn't satisfy the scope → `Forbidden` (403);
/// pool exhaustion → `ServiceUnavailable` (503).
///
/// On success the outcome carries the whole [`Identity`], so a caller that
/// also needs the device-session id (the ws-ticket mint) gets it from the
/// lease this call already took rather than checking out a second one
/// (issue #1533 review item 3).
pub fn authorize(headers: &str, required: RequiredScope) -> AuthOutcome {
    match resolve_identity(headers) {
        Ok(None) => AuthOutcome::Unauthorized,
        Ok(Some(identity)) if satisfies(identity.role, required) => AuthOutcome::Ok(identity),
        Ok(Some(_)) => AuthOutcome::Forbidden,
        Err(db::DbError::ReaderPoolExhausted { .. }) => AuthOutcome::ServiceUnavailable,
        // `NotInitialized` is a transient startup condition, not a
        // credential failure. Returning 503 lets the client retry once
        // `init()` has completed; returning 401 would log them out for
        // what's actually a server-side boot state.
        Err(db::DbError::NotInitialized) => AuthOutcome::ServiceUnavailable,
        // Other DB errors (e.g. SQLITE_BUSY from the writer) propagate as
        // 503 — the dispatcher treats them as retryable too. A real
        // `Sqlite(...)` failure here would be a deeper bug (the resolver
        // only does single-row reads), but matching on it explicitly keeps
        // a future failure from being silently classified as `Unauthorized`.
        Err(db::DbError::Sqlite(_)) => AuthOutcome::ServiceUnavailable,
    }
}

#[doc(hidden)]
#[cfg(test)]
fn outcome(role: Option<Role>, required: RequiredScope) -> AuthOutcome {
    match role {
        None => AuthOutcome::Unauthorized,
        Some(role) if satisfies(role, required) => AuthOutcome::Ok(Identity {
            role,
            device_session_id: None,
        }),
        Some(_) => AuthOutcome::Forbidden,
    }
}

/// Dispatcher convenience: authorize and, on failure, write the matching status
/// line and return `None` so the caller can `return` immediately. On success
/// returns `Some(identity)` and writes nothing. Callers that only care about
/// pass/fail use `.is_none()`; the ws-ticket mint reads
/// [`Identity::device_session_id`] off the returned value instead of taking a
/// second reader lease.
pub async fn guard(
    lines: &mut tokio::io::BufStream<MaybeTls>,
    headers: &str,
    required: RequiredScope,
) -> Option<Identity> {
    match authorize(headers, required) {
        AuthOutcome::Ok(identity) => Some(identity),
        AuthOutcome::Unauthorized => {
            let _ = request::write_status_only(lines, "401 Unauthorized").await;
            None
        }
        AuthOutcome::Forbidden => {
            let _ = request::write_status_only(lines, "403 Forbidden").await;
            None
        }
        AuthOutcome::ServiceUnavailable => {
            // 503 + `Retry-After: 1` (issue #1533 review: a bare 503 invites
            // a client stampede on transient pool exhaustion).
            let _ = request::write_service_unavailable_with_retry(lines).await;
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
    fn resolve_identity_recovers_the_device_id_for_a_device_but_not_the_root_token() {
        let conn = seeded_db();
        let (id, token) = db::pair_device_session_inner(&conn, None, None).unwrap();
        let device_id = |headers: &str| {
            resolve_identity_inner(&conn, headers).and_then(|i| i.device_session_id)
        };
        assert_eq!(device_id(&bearer(&token)), Some(id));
        assert_eq!(device_id(&cookie(&token)), Some(id));
        // The root token authenticates as Admin but owns no device row.
        let root = db::get_or_create_root_token_inner(&conn).unwrap();
        assert_eq!(device_id(&bearer(&root)), None);
    }

    /// A root cookie presented alongside a device bearer must still resolve
    /// the device id. The pre-review code got this from a *separate*
    /// device-session resolver that ignored which credential proved the role;
    /// folding both answers into one lease must not quietly drop the device
    /// (issue #1533 review item 3).
    #[test]
    fn root_cookie_with_device_bearer_still_resolves_the_device_id() {
        let conn = seeded_db();
        let root = db::get_or_create_root_token_inner(&conn).unwrap();
        let (id, device) = db::pair_device_session_inner(&conn, None, None).unwrap();
        let headers = format!(
            "Host: localhost\r\nCookie: bm_session={}\r\nAuthorization: Bearer {}\r\n",
            root, device
        );
        assert_eq!(
            resolve_identity_inner(&conn, &headers),
            Some(Identity {
                role: Role::Admin,
                device_session_id: Some(id),
            })
        );
    }

    /// A device credential resolves its own id in the same pass that proves
    /// the role — the ws-ticket mint reads it straight off this value instead
    /// of taking a second reader lease.
    #[test]
    fn device_credential_resolves_role_and_id_together() {
        let conn = seeded_db();
        let (id, token) = db::pair_device_session_inner(&conn, None, None).unwrap();
        assert_eq!(
            resolve_identity_inner(&conn, &bearer(&token)),
            Some(Identity {
                role: Role::Admin,
                device_session_id: Some(id),
            })
        );
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
            AuthOutcome::Ok(Identity {
                role: Role::CoordinatorWrite,
                device_session_id: None,
            })
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
