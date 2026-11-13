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
//!
//! **One walk answers both questions** (issue #589). A request's credentials
//! are resolved once, and the resolution reports the [`Role`] *and* the paired
//! device the credential belongs to. The WebSocket-ticket mint needs both —
//! it has to authorize the request and bind the ticket to a device — and
//! resolving them separately meant two DB checkouts per mint and a window in
//! which the two passes could disagree. A walk also refreshes a device
//! session's recorded activity when it has gone stale, which is what keeps a
//! device that is still in use out of the expiry sweep's window.

use rusqlite::Connection;
use std::net::IpAddr;

use crate::db;
use crate::http::request;
use crate::http::response::Response;

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
    /// A credential was presented, but the database reader pool was
    /// temporarily exhausted. This is a server availability failure, not a
    /// revoked credential, so callers must return 503 rather than 401.
    Unavailable,
}

/// The result of resolving a request's credentials once (issue #589).
///
/// A request's headers are walked **once**: the role the credential proves and,
/// when the credential belongs to a paired device, which device row it is.
/// The paths that need both — the WebSocket-ticket mint — used to resolve the
/// role and then walk the credentials again for the device id, which meant two
/// DB checkouts per mint and a window in which the two passes could disagree
/// (a revoke landing between them). One walk removes both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResolvedAuth {
    /// The role the credential proves. `None` when no valid credential was
    /// presented.
    pub role: Option<Role>,
    /// The paired device the credential belongs to, when it is a device
    /// session token. `None` for the root token (which owns no device row and
    /// is unrevocable) and for a coordinator credential.
    pub device_id: Option<i64>,
}

/// What one credential walk found, before any side effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct CredentialWalk {
    role: Option<Role>,
    device: Option<db::DeviceSessionRef>,
}

/// Resolve a request's credentials in ONE walk.
///
/// A request carrying no cookie and no bearer token resolves to *no role*
/// *without* touching the DB — so an unauthenticated probe does not reach a
/// lookup (the dispatcher tests below run without an initialized global DB, so
/// the early return is also what keeps them DB-free). Otherwise it checks the
/// DB out once and delegates to [`resolve_auth_inner`], the single resolution
/// implementation the unit tests also drive against a seeded connection — so
/// there is no production/test logic to keep in lockstep.
///
/// `peer_ip` is the address the request arrived from. It is recorded on the
/// device row when the row's activity is stale (see below) and is otherwise
/// unused.
pub fn resolve_auth(headers: &str, peer_ip: Option<IpAddr>) -> Result<ResolvedAuth, ()> {
    if request::extract_token_from_cookies(headers).is_none()
        && request::bearer_token(headers).is_none()
    {
        return Ok(ResolvedAuth::default());
    }
    let walk = {
        let conn = db::read_conn().map_err(|_| ())?;
        resolve_auth_inner(&conn, headers)?
    };
    let resolved = ResolvedAuth {
        role: walk.role,
        device_id: walk.device.map(|device| device.id),
    };
    // A device that is still in use must not age into the expiry sweep's
    // window (issue #589). The row's recorded activity is refreshed when it
    // is stale — the window is short enough that a busy client is not writing
    // on every poll — so a device that keeps authenticating keeps its row.
    if let Some(device) = walk.device {
        if device.activity_stale {
            // A failed refresh is not fatal: the request itself still
            // authenticates, and the next one retries the stamp.
            let peer_ip = peer_ip.map(|ip| ip.to_string());
            let _ = db::refresh_device_session_activity(device.id, peer_ip.as_deref());
        }
    }
    Ok(resolved)
}

/// The credential → [`Role`] resolution, checked in priority order against a
/// single connection: root first (cookie or bearer → Admin), then the bearer
/// token against the drive- then read-scoped coordinator tokens. Lock-free so
/// the unit tests can drive it against a seeded in-memory connection (the test
/// binary has no initialized global DB).
///
/// A SQLite failure is an error, not a "no valid credential": the caller turns
/// it into `503 Service Unavailable`, because a storage fault reported as a
/// revoked credential signs the phone out for no reason (the mobile client
/// treats 401 as "this device was revoked" and clears its credential).
fn resolve_auth_inner(conn: &Connection, headers: &str) -> Result<CredentialWalk, ()> {
    let cookie = request::extract_token_from_cookies(headers);
    let bearer = request::bearer_token(headers);

    // Admin: the root token, presented as either the bm_session cookie or a
    // bearer header (the latter is how POST /api/session mints the cookie); OR a
    // per-device session token (issue #502), which is also an Admin-surface
    // credential. Device tokens are what a paired phone holds after pairing —
    // distinct per device, so revoking one (deleting its row) drops *its* Admin
    // access without touching the root token or other devices.
    if let Some(t) = cookie.as_deref() {
        if db::validate_root_token_inner(conn, t).map_err(|_| ())? {
            return Ok(CredentialWalk {
                role: Some(Role::Admin),
                device: None,
            });
        }
        if let Some(device) = db::resolve_device_session_inner(conn, t).map_err(|_| ())? {
            return Ok(CredentialWalk {
                role: Some(Role::Admin),
                device: Some(device),
            });
        }
    }
    if let Some(t) = bearer.as_deref() {
        if db::validate_root_token_inner(conn, t).map_err(|_| ())? {
            return Ok(CredentialWalk {
                role: Some(Role::Admin),
                device: None,
            });
        }
        if let Some(device) = db::resolve_device_session_inner(conn, t).map_err(|_| ())? {
            return Ok(CredentialWalk {
                role: Some(Role::Admin),
                device: Some(device),
            });
        }
        // Coordinator surface: bearer only. Check drive before read so the
        // more-capable scope wins when both happen to match (they don't in
        // practice — distinct tokens — but the order makes the intent clear).
        if db::validate_coordinator_drive_token_inner(conn, t).map_err(|_| ())? {
            return Ok(CredentialWalk {
                role: Some(Role::CoordinatorWrite),
                device: None,
            });
        }
        if db::validate_coordinator_read_token_inner(conn, t).map_err(|_| ())? {
            return Ok(CredentialWalk {
                role: Some(Role::CoordinatorRead),
                device: None,
            });
        }
    }
    Ok(CredentialWalk::default())
}

/// A WebSocket invitation cannot outlive revocation of its issuing device.
pub(crate) fn device_session_is_active(id: i64) -> Result<bool, ()> {
    let conn = db::read_conn().map_err(|_| ())?;
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM device_sessions WHERE id = ?1)",
        [id],
        |row| row.get(0),
    )
    .map_err(|_| ())
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

/// Authorize a request for a required scope. Missing/invalid credentials map to
/// `Unauthorized` (401), a wrong surface to `Forbidden` (403), and a temporary
/// reader-pool failure to `Unavailable` (503).
///
/// `peer_ip` is the address the request arrived from; it is recorded on a
/// paired device's row when that row's activity is stale (issue #589).
pub fn authorize(headers: &str, required: RequiredScope, peer_ip: Option<IpAddr>) -> AuthOutcome {
    match resolve_auth(headers, peer_ip) {
        Ok(resolved) => outcome(resolved.role, required),
        Err(()) => AuthOutcome::Unavailable,
    }
}

/// The status the dispatcher must return for a role that was already resolved
/// once — `None` when the request may proceed.
///
/// The WebSocket-ticket mint resolves the role *and* the device id in one walk
/// (issue #589) and then asks this whether the role satisfies the route's
/// scope, so it never needs a second credential walk.
pub fn deny_for_role(role: Option<Role>, required: RequiredScope) -> Option<Response> {
    deny_response(outcome(role, required))
}

fn outcome(role: Option<Role>, required: RequiredScope) -> AuthOutcome {
    match role {
        None => AuthOutcome::Unauthorized,
        Some(role) if satisfies(role, required) => AuthOutcome::Ok(role),
        Some(_) => AuthOutcome::Forbidden,
    }
}

/// Map a failed [`AuthOutcome`] to the matching empty-body status response.
/// `Ok` returns `None` so the dispatcher can proceed.
pub fn deny_response(outcome: AuthOutcome) -> Option<Response> {
    match outcome {
        AuthOutcome::Ok(_) => None,
        AuthOutcome::Unauthorized => Some(Response::empty("401 Unauthorized")),
        AuthOutcome::Forbidden => Some(Response::empty("403 Forbidden")),
        AuthOutcome::Unavailable => Some(Response::empty("503 Service Unavailable")),
    }
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
