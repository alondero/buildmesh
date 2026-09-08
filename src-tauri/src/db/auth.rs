//! Coordinator and device-session auth persistence (ADR-0008, issue #502).

use rusqlite::{Connection, params};

use crate::models::DeviceSession;

use super::{read_conn, write_conn, SqlResult};

/// Get or create the root remote access token (stored in app_settings).
pub fn get_or_create_root_token() -> SqlResult<String> {
    let db = write_conn();
    get_or_create_root_token_inner(&db)
}

/// Lock-free core, so the HTTP auth layer's tests (`http::auth`, issue #500) can
/// seed a root token on an in-memory connection.
pub fn get_or_create_root_token_inner(conn: &Connection) -> SqlResult<String> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'remote_access_token'",
            [],
            |row| row.get(0),
        )
        .ok();

    if let Some(token) = existing {
        if !token.is_empty() {
            return Ok(token);
        }
    }

    let token = generate_token();
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES ('remote_access_token', ?1)",
        params![&token],
    )?;
    Ok(token)
}

/// Validate the root remote access token (the Admin-role credential, issue #500).
/// Lock-free core so the HTTP auth layer (`http::auth`, issue #500) and the
/// `/api/session` login endpoint can be unit-tested against an in-memory
/// connection — mirroring the coordinator validators' `_inner` pattern. Only the
/// `_inner` form exists: every caller (`resolve_role`, `login_device_session`)
/// already holds the DB lock. The root token is still stored cleartext (hashing
/// deferred to the Keychain slice, #495); an empty presented token never matches
/// an absent stored value.
pub fn validate_root_token_inner(conn: &Connection, token: &str) -> SqlResult<bool> {
    if token.is_empty() {
        return Ok(false);
    }
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'remote_access_token'",
            [],
            |row| row.get(0),
        )
        .ok();
    // Constant-time compare (issue #1240): the root token is the highest-value
    // credential here, and unlike the coordinator tokens it is still stored
    // cleartext (Keychain slice, #495), so a regular `==` would leak matching
    // prefix length via timing. We use `subtle::ConstantTimeEq` rather than a
    // hand-rolled loop: it inserts `core::hint::black_box` / volatile reads so
    // LLVM cannot optimise the comparison into a short-circuit under our
    // `lto = "thin"` release profile. The Choice → bool conversion is
    // `From<Choice> for bool` and is itself constant-time.
    Ok(stored.is_some_and(|s| bool::from(
        subtle::ConstantTimeEq::ct_eq(s.as_bytes(), token.as_bytes()),
    )))
}

// --- Coordinator read API auth (ADR-0008) ---
//
// The coordinator surface is a SEPARATE, capability-scoped credential from the
// mobile root token, gated behind a master enable switch that defaults OFF.
// A read-scoped token can never be used to drive nodes (drive is a future
// slice). All three keys live in app_settings alongside `remote_access_token`.
const COORDINATOR_ENABLED_KEY: &str = "coordinator_api_enabled";
pub(crate) const COORDINATOR_READ_TOKEN_KEY: &str = "coordinator_read_token";
// Drive (write) side (issue #319). The drive scope is a SEPARATE token from the
// read token — a read-scoped credential can never drive a node — behind its own
// enable switch (also defaulting OFF) so drive can be killed independently while
// reads stay up. Both still sit under the coordinator master switch above, so
// disabling the whole surface disables drive too.
const COORDINATOR_DRIVE_ENABLED_KEY: &str = "coordinator_drive_enabled";
pub(crate) const COORDINATOR_DRIVE_TOKEN_KEY: &str = "coordinator_drive_token";

/// Is the coordinator read API enabled? Defaults to `false` (off) for a fresh
/// install, so a naive setup is never an open endpoint.
pub fn coordinator_api_enabled() -> SqlResult<bool> {
    let db = read_conn();
    coordinator_api_enabled_inner(&db)
}

pub fn coordinator_api_enabled_inner(conn: &Connection) -> SqlResult<bool> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            params![COORDINATOR_ENABLED_KEY],
            |row| row.get(0),
        )
        .ok();
    Ok(value.as_deref() == Some("1"))
}

/// Flip the master enable switch for the coordinator read API.
pub fn set_coordinator_api_enabled(enabled: bool) -> SqlResult<()> {
    let db = write_conn();
    set_coordinator_api_enabled_inner(&db, enabled)
}

pub fn set_coordinator_api_enabled_inner(conn: &Connection, enabled: bool) -> SqlResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
        params![COORDINATOR_ENABLED_KEY, if enabled { "1" } else { "0" }],
    )?;
    Ok(())
}

/// Whether the embedded HTTP/WS server may bind beyond loopback (issue #496).
/// Off by default: a fresh install binds only `127.0.0.1`/`::1`, so external
/// devices on the LAN cannot reach the hub without an explicit opt-in. Enabling
/// it is what lets a phone connect over LAN/VPN (TLS for that path is a later
/// slice). The secure default is enforced by `http::start_http_server` reading
/// this before choosing its bind addresses.
const LAN_EXPOSURE_ENABLED_KEY: &str = "lan_exposure_enabled";

/// Is LAN/VPN exposure enabled? Defaults to `false` (loopback-only) so a naive
/// setup is never reachable from another machine.
pub fn lan_exposure_enabled() -> SqlResult<bool> {
    let db = read_conn();
    lan_exposure_enabled_inner(&db)
}

pub fn lan_exposure_enabled_inner(conn: &Connection) -> SqlResult<bool> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            params![LAN_EXPOSURE_ENABLED_KEY],
            |row| row.get(0),
        )
        .ok();
    Ok(value.as_deref() == Some("1"))
}

/// Flip the LAN/VPN exposure switch. Takes effect on the next server start.
pub fn set_lan_exposure_enabled(enabled: bool) -> SqlResult<()> {
    let db = write_conn();
    set_lan_exposure_enabled_inner(&db, enabled)
}

pub fn set_lan_exposure_enabled_inner(conn: &Connection, enabled: bool) -> SqlResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
        params![LAN_EXPOSURE_ENABLED_KEY, if enabled { "1" } else { "0" }],
    )?;
    Ok(())
}

/// Mint (or replace) the read-scoped coordinator token, returning it. Minting a
/// fresh token invalidates any previously issued one.
pub fn generate_coordinator_read_token() -> SqlResult<String> {
    let db = write_conn();
    generate_coordinator_read_token_inner(&db)
}

pub fn generate_coordinator_read_token_inner(conn: &Connection) -> SqlResult<String> {
    // Return the raw token to the caller once; persist only its hash (#495) so a
    // DB dump or rogue agent reading app_settings can't recover the secret.
    let token = generate_token();
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
        params![COORDINATOR_READ_TOKEN_KEY, hash_token(&token)],
    )?;
    Ok(token)
}

/// The stored read token *hash*, if one has been minted (and is non-empty).
/// Used by the status command to report `has_token` — presence only; the value
/// is a SHA-256 hash (#495), never the raw token.
pub fn coordinator_read_token() -> SqlResult<Option<String>> {
    let db = read_conn();
    coordinator_read_token_inner(&db)
}

pub fn coordinator_read_token_inner(conn: &Connection) -> SqlResult<Option<String>> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            params![COORDINATOR_READ_TOKEN_KEY],
            |row| row.get(0),
        )
        .ok();
    Ok(value.filter(|v| !v.is_empty()))
}

/// Validate a presented token for READ access. Rejects unless the API is
/// enabled AND the token matches the minted read token — so disabling the
/// master switch instantly cuts off all read access even with a valid token.
/// Only the `_inner` form exists: the HTTP auth layer (`http::auth::resolve_role`,
/// issue #500) locks the DB once and calls it, so a separate self-locking
/// wrapper would only invite a nested-lock deadlock.
pub fn validate_coordinator_read_token_inner(conn: &Connection, token: &str) -> SqlResult<bool> {
    if token.is_empty() || !coordinator_api_enabled_inner(conn)? {
        return Ok(false);
    }
    // The DB holds only the hash (#495), so hash the presented token and compare
    // hashes. The raw token never has to be reconstructed to authenticate.
    // Constant-time compare via `subtle::ConstantTimeEq` (issue #1240); see
    // `validate_root_token_inner` for the rationale.
    match coordinator_read_token_inner(conn)? {
        Some(stored) => Ok(bool::from(
            subtle::ConstantTimeEq::ct_eq(stored.as_bytes(), hash_token(token).as_bytes()),
        )),
        None => Ok(false),
    }
}

// --- Coordinator drive (write) auth (ADR-0008 §5, issue #319) ---

/// Is the drive side enabled? Defaults to `false` (off), independent of the
/// read side, so a deployment can offer read-only coordination without ever
/// exposing the ability to write to a node's PTY. (No process-global wrapper
/// yet — only `validate_coordinator_drive_token_inner` reads this; a drive
/// Settings slice can add the public getter when it surfaces the state.)
pub fn coordinator_drive_enabled_inner(conn: &Connection) -> SqlResult<bool> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            params![COORDINATOR_DRIVE_ENABLED_KEY],
            |row| row.get(0),
        )
        .ok();
    Ok(value.as_deref() == Some("1"))
}

/// Flip the drive kill-switch. Off by default; killing it stops all driving
/// while leaving the read surface untouched.
pub fn set_coordinator_drive_enabled(enabled: bool) -> SqlResult<()> {
    let db = write_conn();
    set_coordinator_drive_enabled_inner(&db, enabled)
}

pub fn set_coordinator_drive_enabled_inner(conn: &Connection, enabled: bool) -> SqlResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
        params![COORDINATOR_DRIVE_ENABLED_KEY, if enabled { "1" } else { "0" }],
    )?;
    Ok(())
}

/// Mint (or replace) the drive-scoped coordinator token. Distinct from the read
/// token, so granting drive is an explicit, separate act from granting read.
pub fn generate_coordinator_drive_token() -> SqlResult<String> {
    let db = write_conn();
    generate_coordinator_drive_token_inner(&db)
}

pub fn generate_coordinator_drive_token_inner(conn: &Connection) -> SqlResult<String> {
    // Raw token returned once; only its hash is persisted (#495).
    let token = generate_token();
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
        params![COORDINATOR_DRIVE_TOKEN_KEY, hash_token(&token)],
    )?;
    Ok(token)
}

/// The stored drive token *hash*, if one has been minted (and is non-empty).
/// Only the validator reads it today (no process-global getter until a Settings
/// slice reports drive state); kept `_inner` so a future caller can lock once.
/// The value is a SHA-256 hash (#495), never the raw token.
pub fn coordinator_drive_token_inner(conn: &Connection) -> SqlResult<Option<String>> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            params![COORDINATOR_DRIVE_TOKEN_KEY],
            |row| row.get(0),
        )
        .ok();
    Ok(value.filter(|v| !v.is_empty()))
}

/// Validate a presented token for DRIVE access. Rejects unless the coordinator
/// master switch is on AND the drive kill-switch is on AND the token matches the
/// minted drive token. Because the drive token is stored under its own key, a
/// read-scoped token never validates here — drive is its own capability. Only the
/// `_inner` form exists (locked once by `http::auth::resolve_role`, issue #500).
pub fn validate_coordinator_drive_token_inner(conn: &Connection, token: &str) -> SqlResult<bool> {
    if token.is_empty()
        || !coordinator_api_enabled_inner(conn)?
        || !coordinator_drive_enabled_inner(conn)?
    {
        return Ok(false);
    }
    // Stored value is the hash (#495); compare against the hashed presentation.
    // Constant-time compare via `subtle::ConstantTimeEq` (issue #1240); see
    // `validate_root_token_inner` for the rationale.
    match coordinator_drive_token_inner(conn)? {
        Some(stored) => Ok(bool::from(
            subtle::ConstantTimeEq::ct_eq(stored.as_bytes(), hash_token(token).as_bytes()),
        )),
        None => Ok(false),
    }
}

// --- Persistent device sessions (issue #502, PRD #494) ---
//
// A paired phone is identified by its own token, minted at pairing and stored
// here as a SHA-256 hash (never the raw value, mirroring the coordinator
// tokens). Because each device holds a *distinct* token, the IP is no longer an
// auth factor — that's what lets a phone roam across networks — and revoking one
// device (deleting its row) leaves every other device untouched. All functions
// follow the lock-once + `_inner(&Connection)` pattern so the HTTP auth layer
// can validate against an in-memory connection in tests (issue #500).

/// Pair a new device: mint a token, persist only its hash + metadata, and return
/// the row id with the *raw* token (handed to the client exactly once, then only
/// ever re-presented by the client). `label` is a human-friendly name derived
/// from the client's `User-Agent`; `ip` is the peer address at pairing. Only the
/// `_inner` form exists — pairing always happens inside `login_device_session`,
/// which already holds the lock.
pub fn pair_device_session_inner(
    conn: &Connection,
    label: Option<&str>,
    ip: Option<&str>,
) -> SqlResult<(i64, String)> {
    let token = generate_token();
    conn.execute(
        "INSERT INTO device_sessions (token_hash, label, last_ip) VALUES (?1, ?2, ?3)",
        params![hash_token(&token), label, ip],
    )?;
    Ok((conn.last_insert_rowid(), token))
}

/// Resolve a presented token to its device id, or `None` if no live device holds
/// it. A revoked device's row is deleted, so a revoked token resolves to `None`
/// here — which is exactly what makes the next request fail auth. An empty token
/// never matches. Only the `_inner` form exists: the auth layer
/// (`http::auth::resolve_role`) already locks the DB once and passes the
/// connection through, and `login_device_session` calls it under its own lock.
pub fn validate_device_token_inner(conn: &Connection, token: &str) -> SqlResult<Option<i64>> {
    if token.is_empty() {
        return Ok(None);
    }
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM device_sessions WHERE token_hash = ?1",
            params![hash_token(token)],
            |row| row.get(0),
        )
        .ok();
    Ok(id)
}

/// Record activity for a device: bump `last_active_at` to now and refresh the
/// last-seen IP. Called on login refresh and WS-ticket mint, not per request, so
/// a polling client doesn't write the DB on every poll. A no-op for an unknown
/// id (the row may have just been revoked).
pub fn touch_device_session(id: i64, ip: Option<&str>) -> SqlResult<()> {
    let db = write_conn();
    touch_device_session_inner(&db, id, ip)
}

pub fn touch_device_session_inner(conn: &Connection, id: i64, ip: Option<&str>) -> SqlResult<()> {
    conn.execute(
        "UPDATE device_sessions SET last_active_at = datetime('now'), last_ip = ?2 WHERE id = ?1",
        params![id, ip],
    )?;
    Ok(())
}

/// List all paired devices, newest first, for the "Authorized Devices" panel.
/// Returns the wire view (`DeviceSession`) — never the `token_hash`.
pub fn list_device_sessions() -> SqlResult<Vec<DeviceSession>> {
    let db = read_conn();
    list_device_sessions_inner(&db)
}

pub fn list_device_sessions_inner(conn: &Connection) -> SqlResult<Vec<DeviceSession>> {
    let mut stmt = conn.prepare(
        "SELECT id, label, last_ip, created_at, last_active_at \
         FROM device_sessions ORDER BY last_active_at DESC, id DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(DeviceSession {
            id: row.get(0)?,
            label: row.get(1)?,
            last_ip: row.get(2)?,
            created_at: row.get(3)?,
            last_active_at: row.get(4)?,
        })
    })?;
    rows.collect()
}

/// Revoke a device by deleting its row. Returns `true` if a row was removed
/// (`false` if the id was already gone). The caller is responsible for kicking
/// any live WebSocket the device holds (`http::revocation::revoke`); deleting the
/// row alone only blocks the *next* request, not an already-open socket.
pub fn revoke_device_session(id: i64) -> SqlResult<bool> {
    let db = write_conn();
    revoke_device_session_inner(&db, id)
}

pub fn revoke_device_session_inner(conn: &Connection, id: i64) -> SqlResult<bool> {
    let affected = conn.execute("DELETE FROM device_sessions WHERE id = ?1", params![id])?;
    Ok(affected > 0)
}

/// The `POST /api/session` decision (issue #502), resolving what cookie to set:
///
/// - presented token is an **existing device token** → *refresh*: bump the
///   device's activity (new IP for roaming) and hand the same token back, so a
///   re-launching phone keeps its identity instead of accumulating a new device
///   row on every load;
/// - presented token is the **root token** (the pairing secret from the desktop
///   QR) → *pair*: mint a brand-new device session and return its token, which
///   the client then persists in place of the root token;
/// - anything else → `None` (the caller answers 401).
///
/// Returns the effective `(device_id, raw_token)` to set as the `bm_session`
/// cookie. Checking the device token first means a paired client re-presenting
/// its device token never spuriously mints a second device.
pub fn login_device_session(
    presented: &str,
    label: Option<&str>,
    ip: Option<&str>,
) -> SqlResult<Option<(i64, String)>> {
    let db = write_conn();
    login_device_session_inner(&db, presented, label, ip)
}

pub fn login_device_session_inner(
    conn: &Connection,
    presented: &str,
    label: Option<&str>,
    ip: Option<&str>,
) -> SqlResult<Option<(i64, String)>> {
    if let Some(id) = validate_device_token_inner(conn, presented)? {
        touch_device_session_inner(conn, id, ip)?;
        return Ok(Some((id, presented.to_string())));
    }
    if validate_root_token_inner(conn, presented)? {
        return Ok(Some(pair_device_session_inner(conn, label, ip)?));
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Token helpers — moved from `db/mod.rs` (issue #1655). `db/mod.rs` must
// own connection + init + baseline DDL only; these are auth-domain concerns
// even though they're used cross-module by the WS ticket store.
// ---------------------------------------------------------------------------

/// Generate a random 32-character hex token (16 bytes of random data).
/// `pub(crate)` so the WS ticket store (`http::ws_ticket`, issue #500) can
/// reuse the same 128-bit entropy source for its short-lived handshake
/// tickets.
pub(crate) fn generate_token() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let bytes: [u8; 16] = rng.random();
    hex::encode(bytes)
}

/// Hash a token for at-rest storage (issue #495). Returns the lowercase
/// SHA-256 hex (64 chars). Tokens are high-entropy (128-bit random hex from
/// `generate_token`), so a plain SHA-256 is the right primitive here — no
/// salt or slow KDF, which exist to slow brute force on *low-entropy*
/// passwords. Because a raw token is 32 chars and this output is 64, the
/// length alone distinguishes a pre-hashing cleartext value from an
/// already-hashed one (used by `ensure_coordinator_tokens_hashed` to
/// migrate idempotently).
pub(crate) fn hash_token(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(raw.as_bytes()))
}
