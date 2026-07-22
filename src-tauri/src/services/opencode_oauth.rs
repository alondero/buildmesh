//! Buildmesh-managed OpenCode Go OAuth Device Flow + token storage (issue
//! #956). Owns:
//! - the credential DTO + parser (moved out of `services::usage` for #956 so
//!   the OAuth dance and the live usage fetcher don't share a private helper
//!   one of them silently drifts from),
//! - the `opencode:console` Windows Credential Manager target name (single
//!   source of truth — also referenced by `read_opencode_console_credential`
//!   in `services::usage` and by `commands::opencode_oauth::revoke_*`),
//! - the lazy-refresh TTL (`REFRESH_TTL = 300s`, aligned with `CACHE_TTL` in
//!   `services::usage`),
//!
//! Future siblings this module will host:
//! - `start_device_flow` / `poll_for_token` — RFC 8628 device authorization,
//! - `try_refresh` — `refresh_token` → new access_token + expires_at,
//! - `fetch_workspaces` — `GET /api/user` + `/api/orgs` enumeration,
//! - `revoke` — `windows_cred::delete(OPENCODE_CONSOLE_CRED_TARGET)`.
//!
//! Compiles on every platform; specific fns that touch the OS credential
//! store or persist tokens (`persist_token_response`, `try_refresh`) are
//! `#[cfg(windows)]`-gated and surface `OAuthError::NoCredential` on the
//! non-Windows arms (mirrors the locked #956 design decision and the agy
//! precedent at `services::usage::read_agy_token`). The HTTP-only fns
//! (`start_device_flow`, `fetch_workspaces`, `poll_for_token`) are
//! platform-agnostic by design — the upstream host is reachable on every
//! OS, even though the credential sink isn't.

use crate::services::usage::UsageError;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// OpenCode Console host that owns the device-flow + refresh endpoints.
/// Captured from the opencode-cli binary's outbound traffic (issue #956
/// research). Hostname only; paths are constants at the call sites so a
/// future deployment-migration can rewrite them in one place.
pub(crate) const OPENCODE_CONSOLE_HOST: &str = "https://console.opencode.ai";

/// RFC 8628 Device Authorization Grant: client id pinned to `opencode-cli`
/// per the OAuth client registration the opencode-cli binary uses against
/// the same host (verified 2026-07-20 against the live OpenCode Go
/// deployment). Other client ids would issue tokens for a different
/// audience and the SolidStart `_server billing.get` RPC would 401.
pub(crate) const OPENCODE_OAUTH_CLIENT_ID: &str = "opencode-cli";

/// Device-flow paths. Re-exported via the `device_flow::*` constants below
/// so a redirect or staging-host split doesn't scatter literal strings.
pub(crate) mod device_flow {
    pub(crate) const CODE_PATH: &str = "/auth/device/code";
    pub(crate) const TOKEN_PATH: &str = "/auth/device/token";
}

/// A typed OAuth error distinct from [`UsageError`]. `UsageError` carries
/// "no credential / shape mismatch" semantics for the fetcher; the OAuth
/// dance has its own failure modes (network, server error, polling terminal
/// states like `access_denied` / `expired_token`) that deserve their own
/// diagnostics. Serializes to `String` at the IPC boundary.
#[derive(Debug)]
pub(crate) enum OAuthError {
    /// The user took too long at `verification_uri_complete` and the
    /// short-lived device code expired. Re-start the flow.
    CodeExpired,
    /// The user clicked "Deny" on the consent screen.
    AccessDenied,
    /// The polling interval was too aggressive; per RFC 8628 §3.5 we MUST
    /// increase `interval` by at least 5 seconds and continue. Surfaced
    /// as an enum arm so a higher layer can decide to keep going vs. abort
    /// the dance (we always keep going in `poll_for_token_impl`).
    SlowDown,
    /// No credential available for this operation. The non-Windows path
    /// uses this for `try_refresh` / `persist_token_response` to mirror the
    /// agy precedent at `services::usage::read_agy_token` (the locked #956
    /// decision: "non-Windows = agy-style NoCredential"). String carries
    /// the credential target name so a higher layer can render a stable
    /// surface-level message.
    NoCredential(String),
    /// Transport-level failure (DNS, TLS, connection refused, timeout).
    /// String carries the underlying message for diagnostics.
    Transport(String),
    /// Server returned a body that didn't deserialize — the OpenCode Console
    /// team's contract changed; the OAuth dance cannot proceed.
    /// String carries the underlying message.
    Shape(String),
}

impl std::fmt::Display for OAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OAuthError::CodeExpired => write!(f, "OpenCode OAuth: device code expired before activation"),
            OAuthError::AccessDenied => write!(f, "OpenCode OAuth: user denied the device-flow consent"),
            OAuthError::SlowDown => write!(f, "OpenCode OAuth: server requested slow-down"),
            OAuthError::NoCredential(target) => write!(f, "OpenCode OAuth: no credential at {target}"),
            OAuthError::Transport(msg) => write!(f, "OpenCode OAuth transport: {msg}"),
            OAuthError::Shape(msg) => write!(f, "OpenCode OAuth shape: {msg}"),
        }
    }
}

impl std::error::Error for OAuthError {}

/// OAuth Device Flow response from `POST /auth/device/code`. `user_code` +
/// `verification_uri_complete` go back to the React Settings UI so it can
/// open the browser; `device_code` is the long-form secret the React UI
/// never sees — it stays in Rust and is exchanged for a token via the
/// polling loop. `expires_in` + `interval` bound the polling loop.
#[derive(Debug, Clone)]
pub(crate) struct DeviceCode {
    /// Long-form secret the user does NOT see. Drives the polling loop.
    pub device_code: String,
    /// Short user-facing code the user types (or is auto-filled on
    /// `verification_uri_complete`). May contain dashes per RFC 8628 §6.1.
    pub user_code: String,
    /// URL the React Settings page opens via `openUrl()`.
    pub verification_uri_complete: String,
    /// Seconds until `device_code` expires — the polling loop must give
    /// up after `now + expires_in`.
    pub expires_in: Duration,
    /// Polling interval in seconds (RFC 8628 §3.5 says we MUST NOT poll
    /// faster than this; `slow_down` bumps it by 5).
    pub interval: Duration,
}

#[derive(Deserialize)]
struct DeviceCodeResp {
    device_code: String,
    user_code: String,
    verification_uri_complete: String,
    expires_in: i64,
    interval: i64,
}

/// Parses the OpenCode Console `POST /auth/device/code` body. The host
/// returns 200 with this shape on success and 4xx with a shape-mismatch
/// payload on bad client-id; the caller checks status BEFORE this parse.
#[allow(dead_code)] // Used by unit tests in this module only.
pub(crate) fn parse_device_code_response(body: &str) -> Result<DeviceCode, OAuthError> {
    let resp: DeviceCodeResp = serde_json::from_str(body)
        .map_err(|e| OAuthError::Shape(format!("/auth/device/code body: {e}")))?;
    // RFC 8628 §6.1: `expires_in` and `interval` MUST be positive integers;
    // a non-positive value is a contract violation. Clamp to a sane
    // minimum interval (1s) so a misbehaving server can't deadlock the loop.
    if resp.expires_in <= 0 || resp.interval <= 0 {
        return Err(OAuthError::Shape(format!(
            "/auth/device/code returned non-positive interval/expires_in: {}/{}",
            resp.interval, resp.expires_in
        )));
    }
    Ok(DeviceCode {
        device_code: resp.device_code,
        user_code: resp.user_code,
        verification_uri_complete: resp.verification_uri_complete,
        expires_in: Duration::from_secs(resp.expires_in as u64),
        interval: Duration::from_secs(resp.interval.max(1) as u64),
    })
}

/// OAuth Device Flow token response from `POST /auth/device/token`. Carries
/// the long-lived refresh token + short-lived access token + workspace
/// binding + server id (the SolidStart deployment id captured into the
/// `X-Server-Id` header on the live usage probe).
#[derive(Debug, Clone)]
pub(crate) struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: Duration,
    pub workspace_id: String,
    pub server_id: String,
}

#[derive(Deserialize)]
struct TokenResp {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
    workspace_id: String,
    server_id: String,
}

/// Parses the OpenCode Console `POST /auth/device/token` 200 body. Caller
/// must have already routed the RFC 8628 polling error shapes
/// (`authorization_pending` → keep polling, `slow_down` → bump interval,
/// `access_denied` / `expired_token` → terminal) — `parse_token_response`
/// only sees the success branch.
#[allow(dead_code)] // Used by unit tests in this module only.
pub(crate) fn parse_token_response(body: &str) -> Result<TokenResponse, OAuthError> {
    let resp: TokenResp = serde_json::from_str(body)
        .map_err(|e| OAuthError::Shape(format!("/auth/device/token body: {e}")))?;
    if resp.expires_in <= 0 {
        return Err(OAuthError::Shape(format!(
            "/auth/device/token returned non-positive expires_in: {}",
            resp.expires_in
        )));
    }
    Ok(TokenResponse {
        access_token: resp.access_token,
        refresh_token: resp.refresh_token,
        expires_in: Duration::from_secs(resp.expires_in as u64),
        workspace_id: resp.workspace_id,
        server_id: resp.server_id,
    })
}

/// Fetches the workspace list per the locked sub-spec #5: the
/// OAuth-scoped user workspace (`GET /api/user`) plus every org the user
/// can switch to (`GET /api/orgs`). The Settings workspace picker must
/// show the user's own workspace alongside any org workspaces — never
/// allow a multi-org user to be locked out of their default workspace.
///
/// The two calls are independent — a partial failure on one is
/// surfaced as `Shape` so the React Settings UI can show a "couldn't
/// load workspaces" affordance rather than silently dropping the user
/// workspace.
///
/// Wraps `reqwest::blocking::Client` in a 15s timeout to bound the
/// network round-trip; matches the cadence `start_device_flow` uses.
pub(crate) fn fetch_workspaces(
    access_token: &str,
) -> Result<Vec<OpenCodeWorkspace>, OAuthError> {
    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| OAuthError::Transport(format!("workspace client: {e}")))?;
    let auth = format!("Bearer {access_token}");

    // /api/user — the OAuth-scoped workspace the user's token binds to.
    // Shape is `{"id":"wrk_<id>","name":"<human>"}` (different field shape
    // than /api/orgs, hence the separate parse).
    let user_resp = client
        .get(format!("{OPENCODE_CONSOLE_HOST}/api/user"))
        .header("Authorization", &auth)
        .send()
        .map_err(|e| OAuthError::Transport(format!("/api/user send: {e}")))?;
    let user_status = user_resp.status();
    let user_body = user_resp
        .text()
        .map_err(|e| OAuthError::Transport(format!("/api/user body: {e}")))?;
    if !user_status.is_success() {
        return Err(OAuthError::Shape(format!(
            "/api/user HTTP {user_status}: {user_body}"
        )));
    }
    let user_workspace = parse_user_workspace(&user_body)?;

    // /api/orgs — every workspace the user can switch to.
    let orgs_resp = client
        .get(format!("{OPENCODE_CONSOLE_HOST}/api/orgs"))
        .header("Authorization", &auth)
        .send()
        .map_err(|e| OAuthError::Transport(format!("/api/orgs send: {e}")))?;
    let orgs_status = orgs_resp.status();
    let orgs_body = orgs_resp
        .text()
        .map_err(|e| OAuthError::Transport(format!("/api/orgs body: {e}")))?;
    if !orgs_status.is_success() {
        return Err(OAuthError::Shape(format!(
            "/api/orgs HTTP {orgs_status}: {orgs_body}"
        )));
    }
    let org_workspaces = parse_workspaces_response(&orgs_body)?;

    // Merge: user's own workspace first (default-to-first per the locked
    // spec decision), then every org workspace not already present.
    let mut out = Vec::with_capacity(1 + org_workspaces.len());
    out.push(user_workspace);
    for ws in org_workspaces {
        if !out.iter().any(|w| w.id == ws.id) {
            out.push(ws);
        }
    }
    Ok(out)
}

#[derive(Deserialize)]
struct UserWorkspaceResp {
    id: String,
    name: String,
}

/// Parses the `GET /api/user` body — `{ "id": "wrk_<id>", "name": "..." }`.
/// Distinct from `parse_workspaces_response` because the wire shape is a
/// single object, not an array.
pub(crate) fn parse_user_workspace(body: &str) -> Result<OpenCodeWorkspace, OAuthError> {
    let resp: UserWorkspaceResp = serde_json::from_str(body)
        .map_err(|e| OAuthError::Shape(format!("/api/user body: {e}")))?;
    Ok(OpenCodeWorkspace {
        id: resp.id,
        name: resp.name,
    })
}

/// Refreshes an existing access token. The OAuth server accepts the
/// `refresh_token` issued by the device-flow exchange and returns a new
/// `(access_token, refresh_token, expires_in, workspace_id, server_id)`
/// bundle. The lazy refresh strategy in `services::usage` calls this
/// when the cached credential is `is_expired()` OR `cached_age > REFRESH_TTL`
/// and the live probe returns 401.
///
/// Atomicity note: this function reads the prior `refresh_token` from the
/// credential blob and writes the response back. The Credential Manager
/// doesn't transactionally swap, so a concurrent refresh could clobber
/// each other. Acceptable risk for a desktop app; follow-up PR may add
/// per-target locking if profiling shows contention.
#[cfg(windows)]
pub(crate) fn try_refresh() -> Result<TokenResponse, OAuthError> {
    use crate::services::windows_cred;
    let blob = windows_cred::read(OPENCODE_CONSOLE_CRED_TARGET).map_err(|e| {
        OAuthError::Shape(format!("refresh read failed: {e}"))
    })?;
    let cred = parse_opencode_console_full_credential(&blob).map_err(|e| {
        OAuthError::Shape(format!("refresh parse failed: {e}"))
    })?;
    let refresh_token = cred.refresh_token.ok_or_else(|| {
        OAuthError::Shape("refresh requires refresh_token in stored credential".to_string())
    })?;
    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| OAuthError::Transport(format!("refresh client: {e}")))?;
    let response = client
        .post(format!(
            "{OPENCODE_CONSOLE_HOST}{}",
            device_flow::TOKEN_PATH
        ))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "refresh_token".to_string()),
            ("client_id", OPENCODE_OAUTH_CLIENT_ID.to_string()),
            ("refresh_token", refresh_token),
        ])
        .send()
        .map_err(|e| OAuthError::Transport(format!("/auth/device/token refresh send: {e}")))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|e| OAuthError::Transport(format!("/auth/device/token refresh body: {e}")))?;
    if !status.is_success() {
        return Err(OAuthError::Shape(format!(
            "/auth/device/token refresh HTTP {status}: {body}"
        )));
    }
    let token = parse_token_response(&body)?;
    // Write the new bundle back to Credential Manager so the next live
    // fetch and the next refresh see the same up-to-date credential.
    persist_token_response(&token)?;
    Ok(token)
}

#[cfg(not(windows))]
pub(crate) fn try_refresh() -> Result<TokenResponse, OAuthError> {
    // Per the locked #956 design decision, non-Windows mirrors the agy
    // precedent at services::usage::read_agy_token and returns a typed
    // NoCredential rather than a Shape error. The user can't be signed
    // in on this platform (Credential Manager doesn't exist), so the
    // refresh-on-401 fallback would be a no-op anyway; surfacing
    // NoCredential tells the higher layer to keep the SQLite (#953)
    // path active.
    Err(OAuthError::NoCredential(format!(
        "{OPENCODE_CONSOLE_CRED_TARGET}: refresh not available on this platform"
    )))
}

/// Composes the credential blob from a fresh `TokenResponse` and writes
/// it to the Windows Credential Manager. Used by both the device-flow
/// dance (after `poll_for_token` returns a token) and the refresh seam
/// (after `try_refresh` issues a new bundle).
#[cfg(windows)]
pub(crate) fn persist_token_response(token: &TokenResponse) -> Result<(), OAuthError> {
    use crate::services::windows_cred;
    let expires_at = chrono::Utc::now()
        + chrono::Duration::seconds(token.expires_in.as_secs() as i64);
    let cred = OpenCodeConsoleCred {
        access_token: Some(token.access_token.clone()),
        workspace_id: Some(token.workspace_id.clone()),
        refresh_token: Some(token.refresh_token.clone()),
        expires_at: Some(expires_at.to_rfc3339()),
        server_id: Some(token.server_id.clone()),
    };
    let blob = serde_json::to_vec(&cred)
        .map_err(|e| OAuthError::Shape(format!("persist serialize: {e}")))?;
    windows_cred::write(OPENCODE_CONSOLE_CRED_TARGET, &blob)
        .map_err(|e| OAuthError::Shape(format!("persist write: {e}")))
}

#[cfg(not(windows))]
pub(crate) fn persist_token_response(_token: &TokenResponse) -> Result<(), OAuthError> {
    // Same design choice as try_refresh — non-Windows is NoCredential,
    // not Shape. The persist path is only reached after a successful
    // dance (a fresh TokenResponse); on non-Windows the dance can be
    // initiated (the host is reachable) but the credential has nowhere
    // to land, so this arms as NoCredential to keep the higher layer's
    // error-matching symmetric with try_refresh and the live probe.
    Err(OAuthError::NoCredential(format!(
        "{OPENCODE_CONSOLE_CRED_TARGET}: credential storage not available on this platform"
    )))
}

/// OpenCode workspace enumeration, returned by `GET /api/user` + `/api/orgs`.
/// Each entry is a candidate for the Settings workspace picker when the
/// user has more than one.
#[allow(dead_code)] // awaiting IPC wire + follow-up PR (issue #956 part 2)
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "OpenCodeWorkspace.ts")]
pub(crate) struct OpenCodeWorkspace {
    pub id: String,
    pub name: String,
}

/// Wire shape returned to React by `commands::opencode_oauth::start_device_flow_console`.
/// Carries enough of the RFC 8628 `DeviceCode` for the Settings UI to render the
/// user-facing `user_code`, open the browser, and own the polling loop locally:
/// `device_code` is the long-form polling secret React holds in component state;
/// `verification_uri_complete` is the URL passed to `openUrl()`; `interval_secs` +
/// `expires_in_secs` bound the React-side `setInterval` loop. Duration collapses
/// to u32 secs at the IPC seam — the React `setInterval` API only accepts ms.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "OpenCodeDeviceFlowStart.ts")]
pub(crate) struct OpenCodeDeviceFlowStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri_complete: String,
    pub interval_secs: u32,
    pub expires_in_secs: u32,
}

/// Wire shape for the token bundle that survives `POST /auth/device/token`.
/// Consumed by both `OpenCodeDeviceCodeStatus::Success` (polling completion) and
/// `commands::opencode_oauth::persist_opencode_tokens` (input — re-persist after
/// refresh, or for the React-owned dance that exchanges `device_code` itself).
///
/// `expires_in_secs` is the wall-clock expiry the React-side state machine
/// stores as `accessTokenExpiresAtMs = now + expires_in_secs*1000`; `workspace_id`
/// is the `wrk_<id>` the live `_server billing.get` POST needs; `server_id` is the
/// SolidStart deployment id captured into the `X-Server-Id` header (still hard-coded
/// in `services::usage::opencode_usage_impl` today, but persisted here for #972).
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "OpenCodeTokenResponse.ts")]
pub(crate) struct OpenCodeTokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in_secs: u32,
    pub workspace_id: String,
    pub server_id: String,
}

/// Tagged enum returned by `commands::opencode_oauth::poll_opencode_device_token`
/// one tick at a time. Internally-tagged (`#[serde(tag = "kind")]`) so React
/// switches on `status.kind` without parsing error strings — issue #969 hard
/// requirement. `Pending` / `SlowDown { new_interval_secs }` keep polling;
/// `Success { token }` is the terminal happy path (followed by React calling
/// `persist_opencode_tokens` + `list_opencode_workspaces`); `CodeExpired` /
/// `AccessDenied` / `Error { message }` are terminal errors that flip the UI to
/// `error`.
///
/// Struct variants are required: `serde`'s `#[serde(tag = "…")]` (a.k.a.
/// internally-tagged) rejects tuple variants because it needs a place to put the
/// discriminator. Plan-agent validation caught this on the first draft.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "OpenCodeDeviceCodeStatus.ts")]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum OpenCodeDeviceCodeStatus {
    Pending,
    SlowDown { new_interval_secs: u32 },
    Success { token: OpenCodeTokenResponse },
    CodeExpired,
    AccessDenied,
    Error { message: String },
}

#[derive(Deserialize)]
struct OrgListResp {
    #[serde(default)]
    orgs: Vec<OrgEntry>,
}

#[derive(Deserialize)]
struct OrgEntry {
    id: String,
    name: String,
}

/// Parses the `GET /api/orgs` body. `GET /api/user` is the workspace the
/// OAuth token was originally scoped to and `parse_workspaces_response`
/// additionally folds that in by appending it to the list if absent —
/// `fetch_workspaces` handles that orchestration.
#[allow(dead_code)] // awaiting IPC wire + follow-up PR (issue #956 part 2). Used by unit tests.
pub(crate) fn parse_workspaces_response(body: &str) -> Result<Vec<OpenCodeWorkspace>, OAuthError> {
    let resp: OrgListResp = serde_json::from_str(body)
        .map_err(|e| OAuthError::Shape(format!("/api/orgs body: {e}")))?;
    Ok(resp
        .orgs
        .into_iter()
        .map(|o| OpenCodeWorkspace {
            id: o.id,
            name: o.name,
        })
        .collect())
}

/// Cold-start Device Flow: `POST /auth/device/code` with the pinned client
/// id. Returns `(DeviceCode, Client)` where the `Client` is the standard
/// `reqwest::blocking::Client` — `start_device_flow_console` projects the
/// user-facing half into the `OpenCodeDeviceFlowStart` wire type (`user_code`
/// + `verification_uri_complete`) the Settings UI renders before React drives
/// the polling loop via [`poll_for_token_once`].
///
/// The `Client` is returned (not stashed in module state) so the call site
/// owns its timeout + connection pool. Knowledge-primer anti-pattern line 64
/// (`blocking network in async runtime`) applies — callers wrap this in
/// `commands::run_blocking` when invoked from `#[command]`.
pub(crate) fn start_device_flow() -> Result<(DeviceCode, Client), OAuthError> {
    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| OAuthError::Transport(format!("client builder: {e}")))?;
    let response = client
        .post(format!("{OPENCODE_CONSOLE_HOST}{}", device_flow::CODE_PATH))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[("client_id", OPENCODE_OAUTH_CLIENT_ID)])
        .send()
        .map_err(|e| OAuthError::Transport(format!("/auth/device/code send: {e}")))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|e| OAuthError::Transport(format!("/auth/device/code body: {e}")))?;
    if !status.is_success() {
        return Err(OAuthError::Shape(format!(
            "/auth/device/code HTTP {status}: {body}"
        )));
    }
    let parsed = parse_device_code_response(&body)?;
    Ok((parsed, client))
}

/// Polls `POST /auth/device/token` honoring RFC 8628 §3.5 backoff rules.
/// Returns the [`TokenResponse`] on success, [`OAuthError::CodeExpired`]
/// when the device code's window lapses, [`OAuthError::AccessDenied`]
/// when the user denies consent, [`OAuthError::Shape`] on unrecoverable
/// server contract drift, [`OAuthError::Transport`] on network failure.
///
/// `now_fn` is injected for testability (real callers pass
/// `std::time::Instant::now`, tests pin the clock). The polling loop calls
/// `sleep_until(deadline)` between requests; per RFC 8628 §3.5
/// `slow_down` increments `interval` by at least 5 seconds and we honor
/// that as soon as the server asks.
#[allow(dead_code)] // superseded by `poll_for_token_once` (stateless IPC seam); kept for the loop's test coverage.
pub(crate) fn poll_for_token(
    client: &Client,
    device_code: &str,
    initial_interval: Duration,
    expires_in: Duration,
    started_at: std::time::Instant,
    now_fn: impl Fn() -> std::time::Instant,
    sleep_fn: impl Fn(Duration),
) -> Result<TokenResponse, OAuthError> {
    let mut interval = initial_interval;
    loop {
        // Out of time? Bail with the typed CodeExpired error. The caller
        // decides whether to surface "please restart the flow" UI or
        // silently retry.
        if now_fn().duration_since(started_at) >= expires_in {
            return Err(OAuthError::CodeExpired);
        }
        let response = client
            .post(format!(
                "{OPENCODE_CONSOLE_HOST}{}",
                device_flow::TOKEN_PATH
            ))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&[
                (
                    "grant_type",
                    "urn:ietf:params:oauth:grant-type:device_code".to_string(),
                ),
                ("client_id", OPENCODE_OAUTH_CLIENT_ID.to_string()),
                ("device_code", device_code.to_string()),
            ])
            .send()
            .map_err(|e| OAuthError::Transport(format!("/auth/device/token send: {e}")))?;
        let status = response.status();
        let body = response
            .text()
            .map_err(|e| OAuthError::Transport(format!("/auth/device/token body: {e}")))?;
        // RFC 8628 §3.5: error responses are 400 + JSON {"error": "..."}.
        // We don't fail on 400 alone — we route on the `error` field.
        if status.is_success() {
            return parse_token_response(&body);
        }
        // Surface all non-success statuses into the body-error namespace.
        if status.as_u16() == 400 {
            if let Ok(err_json) =
                serde_json::from_str::<serde_json::Value>(&body).map_err(|_| ())
            {
                let err_code = err_json
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match err_code {
                    "authorization_pending" => {
                        sleep_fn(interval);
                        continue;
                    }
                    "slow_down" => {
                        // RFC 8628 §3.5: bump interval by 5 seconds minimum.
                        interval += Duration::from_secs(5);
                        sleep_fn(interval);
                        continue;
                    }
                    "access_denied" => return Err(OAuthError::AccessDenied),
                    "expired_token" => return Err(OAuthError::CodeExpired),
                    _ => {
                        return Err(OAuthError::Shape(format!(
                            "/auth/device/token unknown error code: {err_code}"
                        )));
                    }
                }
            }
        }
        // Anything outside the documented RFC 8628 polling protocol is a
        // contract change — the OAuth dance cannot proceed.
        return Err(OAuthError::Shape(format!(
            "/auth/device/token HTTP {status}: {body}"
        )));
    }
}

/// Stateless one-shot poll for `commands::opencode_oauth::poll_opencode_device_token`.
/// React owns the `(device_code, current_interval_secs, expires_in_secs,
/// started_at_ms)` quadruple in component state and asks Rust to advance the
/// dance by one HTTP round-trip; the IPC returns a tagged
/// [`OpenCodeDeviceCodeStatus`] React switches on via `status.kind`.
///
/// Stateless-server design (per the wayfinder-map memory `buildmesh-gh956`):
/// the alternative — Rust owns the in-flight state — would require a per-mesh
/// background task, cleanup-on-timeout, and a way to cancel from React when
/// the user closes the modal. React-as-owner is simpler; Rust-as-pure-poll is
/// the right grain.
///
/// `now_ms` is computed via `SystemTime::now()` inside the function; callers
/// that need deterministic clocking for tests should pass `started_at_ms` and
/// set `expires_in_secs` short enough that the expiry gate fires. The
/// `current_interval_secs` argument is held for symmetry with `poll_for_token`
/// and to pin a future pre-emptive backoff — it's not consumed today.
pub(crate) fn poll_for_token_once(
    device_code: &str,
    current_interval_secs: u32,
    expires_in_secs: u32,
    started_at_ms: i64,
) -> Result<OpenCodeDeviceCodeStatus, OAuthError> {
    let _ = current_interval_secs; // pin for future pre-emptive backoff hook
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(i64::MAX);
    if now_ms.saturating_sub(started_at_ms) >= (expires_in_secs as i64).saturating_mul(1000) {
        return Ok(OpenCodeDeviceCodeStatus::CodeExpired);
    }
    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| OAuthError::Transport(format!("poll-once client: {e}")))?;
    let response = client
        .post(format!("{OPENCODE_CONSOLE_HOST}{}", device_flow::TOKEN_PATH))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            (
                "grant_type",
                "urn:ietf:params:oauth:grant-type:device_code".to_string(),
            ),
            ("client_id", OPENCODE_OAUTH_CLIENT_ID.to_string()),
            ("device_code", device_code.to_string()),
        ])
        .send()
        .map_err(|e| OAuthError::Transport(format!("/auth/device/token send: {e}")))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|e| OAuthError::Transport(format!("/auth/device/token body: {e}")))?;
    if status.is_success() {
        let token = parse_token_response(&body)?;
        return Ok(OpenCodeDeviceCodeStatus::Success {
            token: OpenCodeTokenResponse {
                access_token: token.access_token,
                refresh_token: token.refresh_token,
                expires_in_secs: token.expires_in.as_secs() as u32,
                workspace_id: token.workspace_id,
                server_id: token.server_id,
            },
        });
    }
    // RFC 8628 §3.5: error responses are 400 + JSON {"error": "..."}.
    // We don't fail on 400 alone — we route on the `error` field.
    if status.as_u16() == 400 {
        if let Ok(err_json) = serde_json::from_str::<serde_json::Value>(&body) {
            let err_code = err_json
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            return Ok(match err_code {
                "authorization_pending" => OpenCodeDeviceCodeStatus::Pending,
                "slow_down" => OpenCodeDeviceCodeStatus::SlowDown {
                    new_interval_secs: current_interval_secs.saturating_add(5),
                },
                "access_denied" => OpenCodeDeviceCodeStatus::AccessDenied,
                "expired_token" => OpenCodeDeviceCodeStatus::CodeExpired,
                other => OpenCodeDeviceCodeStatus::Error {
                    message: format!("/auth/device/token unknown error code: {other}"),
                },
            });
        }
    }
    // Anything outside the documented RFC 8628 polling protocol is a contract
    // change — surface as Error, not Shape, so the React state machine has one
    // terminal branch for "you can't recover by polling more".
    Ok(OpenCodeDeviceCodeStatus::Error {
        message: format!("/auth/device/token HTTP {status}: {body}"),
    })
}

/// Windows Credential Manager target the Buildmesh-owned OpenCode OAuth dance
/// writes its token blob under. Mirrors the agy `provider:subcontext` shape
/// (`gemini:antigravity`). Single source of truth — both
/// `services::usage::read_opencode_console_credential` and
/// `services::opencode_oauth::persist_token_response` refer here.
pub(crate) const OPENCODE_CONSOLE_CRED_TARGET: &str = "opencode:console";

/// Idle TTL for the lazy refresh strategy (issue #956): a credential within
/// `REFRESH_TTL` of expiry is treated as fresh *and* a successful live fetch
/// within the same window short-circuits the next refresh. Mirrors the
/// `CACHE_TTL` of `services::usage` so the OAuth lifecycle and the cache
/// lifecycle agree — refreshing at minute N doesn't poison a stale cache hit
/// at minute N+1.
#[allow(dead_code)] // awaiting refresh-on-401 seam in services::usage (issue #956 task 6)
pub(crate) const REFRESH_TTL: Duration = Duration::from_secs(300);

/// JSON shape of the credential blob written by the device-flow dance.
/// `access_token` is the live RPC bearer; `workspace_id` is the `wrk_<id>`
/// passed to the `_server billing.get` body; `refresh_token` + `expires_at`
/// drive the lazy refresh; `server_id` is the SolidStart deployment id the
/// OAuth device-flow exchange returns and is consumed by
/// `services::usage::opencode_usage_impl` at the `X-Server-Id` header
/// (issue #972) — with a documented legacy default (`OPENCODE_SERVER_ID`)
/// for blobs written before this field was added by #956.
///
/// All fields are `Option<String>` so a partially-written blob (e.g. before
/// workspace discovery completes) still round-trips — the higher-level
/// parser decides what's load-bearing and what's optional.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct OpenCodeConsoleCred {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
}

/// Parses the OpenCode Console credential blob into the full DTO. Every
/// field is optional in JSON; load-bearing-ness is decided by the helper
/// that consumes this — `parse_opencode_console_credential` for the live
/// fetcher (needs `access_token` + `workspace_id`), `try_refresh` (needs
/// `refresh_token` + `expires_at` + `workspace_id`).
///
/// Contract:
/// - Non-UTF-8 bytes → `UsageError::Shape`
/// - Malformed JSON (or wrong types for any field) → `UsageError::Shape`
/// - Valid JSON → all five fields populated with their serialized values
pub(crate) fn parse_opencode_console_full_credential(
    blob: &[u8],
) -> Result<OpenCodeConsoleCred, UsageError> {
    let text = std::str::from_utf8(blob).map_err(|e| UsageError::Shape(e.to_string()))?;
    serde_json::from_str(text).map_err(|e| UsageError::Shape(e.to_string()))
}

/// Reads the Windows Credential Manager blob at [`OPENCODE_CONSOLE_CRED_TARGET`]
/// and parses it into the full DTO. Consumed by
/// `commands::opencode_oauth::list_opencode_workspaces` (issue #969) which
/// re-reads the access_token before calling [`fetch_workspaces`], keeping the
/// IPC signature argument-free.
///
/// Returns:
/// - `Ok(None)` — credential not present (`ERROR_NOT_FOUND` on Windows; also
///   `Ok(None)` on non-Windows per the locked #956 agy-style decision)
/// - `Ok(Some(cred))` — blob present and parsed
/// - `Err(String)` — non-recoverable Windows failure (rare, e.g. credential
///   store corruption). The IPC layer surfaces this to React as a toast.
///
/// Distinct from `parse_opencode_console_credential` (which requires
/// `access_token` + `workspace_id` and collapses missing fields to
/// `UsageError::NoCredential`); that path stays in `services::usage` where
/// the live fetcher needs the load-bearing subset only.
#[cfg(windows)]
pub(crate) fn read_opencode_console_full_credential()
    -> Result<Option<OpenCodeConsoleCred>, String>
{
    use crate::services::windows_cred;
    match windows_cred::read(OPENCODE_CONSOLE_CRED_TARGET) {
        Ok(blob) => parse_opencode_console_full_credential(&blob)
            .map(Some)
            .map_err(|e| e.to_string()),
        Err(crate::services::usage::UsageError::NoCredential(_)) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(not(windows))]
pub(crate) fn read_opencode_console_full_credential()
    -> Result<Option<OpenCodeConsoleCred>, String>
{
    // Per the locked #956 design, non-Windows mirrors the agy precedent at
    // `services::usage::read_agy_token` — the dance's credential sink
    // doesn't exist on this platform, so the workspace picker has nothing to
    // read. `Ok(None)` lets the IPC layer render the Settings `signedOut`
    // branch without an error toast.
    Ok(None)
}

/// Parses the OpenCode Console credential blob into `(access_token,
/// workspace_id)`. Both fields are required by the live fetcher; either
/// missing or empty is treated as `NoCredential` so the offline SQLite path
/// (#953) takes over with whatever partial state the user has on disk.
///
/// Thin wrapper over [`parse_opencode_console_full_credential`] — the full
/// DTO path is the contract; this exists so `services::usage` keeps its
/// single-statement read path (`parse(...).map(|...| (token, workspace))`)
/// without leaking the optional-field plumbing into the fetcher.
#[allow(dead_code)] // narrow helper retained for the parser unit tests; the live
                    // fetcher moved to `services::usage::opencode_live_request_parts`
                    // (issue #972) and no longer needs the tuple shape.
pub(crate) fn parse_opencode_console_credential(
    blob: &[u8],
) -> Result<(String, String), UsageError> {
    let cred = parse_opencode_console_full_credential(blob)?;
    let token = cred
        .access_token
        .filter(|s| !s.is_empty())
        .ok_or_else(|| UsageError::NoCredential(OPENCODE_CONSOLE_CRED_TARGET.to_string()))?;
    let workspace = cred
        .workspace_id
        .filter(|s| !s.is_empty())
        .ok_or_else(|| UsageError::NoCredential(OPENCODE_CONSOLE_CRED_TARGET.to_string()))?;
    Ok((token, workspace))
}

/// True when the credential's `expires_at` is in the past (RFC-3339 string
/// parse; malformed or missing `expires_at` defaults to `false` so a stale
/// credential that's never been refreshed still attempts the live fetch
/// rather than looping on a "we don't know" answer).
///
/// Cheap pure function used by the lazy-refresh gating: refresh fires when
/// `is_expired(cached) || cached_age > REFRESH_TTL`.
pub(crate) fn cred_is_expired(cred: &OpenCodeConsoleCred, now_unix: i64) -> bool {
    let Some(expires_at) = cred.expires_at.as_deref() else {
        return false;
    };
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(expires_at) else {
        return false;
    };
    dt.timestamp() <= now_unix
}

/// Revokes the Buildmesh-managed OpenCode Console credential (issue #956).
/// Idempotent — a Settings "Sign out" affordance must never error when the
/// user was already signed out, mirroring `windows_cred::delete`'s own
/// idempotency on missing credentials.
///
/// On Windows this calls `windows_cred::delete(OPENCODE_CONSOLE_CRED_TARGET)`
/// and surfaces any non-`NOT_FOUND` failure as `String` (the `#[command]`
/// boundary turns `UsageError` into an IPC error string).
///
/// On non-Windows there's no credential manager to delete from, so `revoke`
/// is a no-op success — the user has never been "signed in" on this platform
/// (the live probe returns `NoCredential` upstream), and the Settings button
/// should disappear rather than show a platform error. Keeping the no-op
/// success here means a platform-portable React handler doesn't have to
/// branch.
#[cfg(windows)]
pub(crate) fn revoke() -> Result<(), String> {
    crate::services::windows_cred::delete(OPENCODE_CONSOLE_CRED_TARGET)
        .map_err(|e| e.to_string())
}

#[cfg(not(windows))]
pub(crate) fn revoke() -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_credential_round_trips_all_five_fields() {
        // The blob shape is the contract #956's device-flow dance produces.
        // All five fields are first-class members of the persisted DTO so
        // the OAuth refresher, workspace picker, and live fetcher don't
        // have to maintain parallel field lists.
        let blob = br#"{
            "access_token": "oc_sk_test_token_123",
            "workspace_id": "wrk_xyz789",
            "refresh_token": "rt_test_refresh",
            "expires_at": "2026-07-21T12:00:00Z",
            "server_id": "c83b78a614689c38ebee981f9b39a8b377716db85c1fd7dbab604adc02d3313d"
        }"#;
        let cred = parse_opencode_console_full_credential(blob).unwrap();
        assert_eq!(cred.access_token.as_deref(), Some("oc_sk_test_token_123"));
        assert_eq!(cred.workspace_id.as_deref(), Some("wrk_xyz789"));
        assert_eq!(cred.refresh_token.as_deref(), Some("rt_test_refresh"));
        assert_eq!(cred.expires_at.as_deref(), Some("2026-07-21T12:00:00Z"));
        assert_eq!(
            cred.server_id.as_deref(),
            Some("c83b78a614689c38ebee981f9b39a8b377716db85c1fd7dbab604adc02d3313d")
        );
    }

    #[test]
    fn parse_full_credential_accepts_partial_blob() {
        // A freshly-issued credential mid-flow may carry only the access
        // token before workspace discovery completes. The full parser must
        // succeed with `None` for the missing fields so the OAuth writer can
        // overwrite the partial blob with the full one without first having
        // to delete it.
        let blob = br#"{"access_token": "tok_only"}"#;
        let cred = parse_opencode_console_full_credential(blob).unwrap();
        assert_eq!(cred.access_token.as_deref(), Some("tok_only"));
        assert_eq!(cred.workspace_id, None);
        assert_eq!(cred.refresh_token, None);
        assert_eq!(cred.expires_at, None);
        assert_eq!(cred.server_id, None);
    }

    #[test]
    fn parse_full_credential_rejects_malformed_json() {
        assert!(matches!(
            parse_opencode_console_full_credential(b"not json at all").unwrap_err(),
            UsageError::Shape(_)
        ));
        assert!(matches!(
            parse_opencode_console_full_credential(b"").unwrap_err(),
            UsageError::Shape(_)
        ));
    }

    #[test]
    fn parse_live_required_extraction() {
        // The live fetcher requires both `access_token` AND `workspace_id`.
        // Each missing / empty case maps to NoCredential so the offline
        // SQLite path (#953) takes over with whatever partial state the
        // user has on disk instead of returning a 401 round-trip later.
        let blobs: Vec<&[u8]> = vec![
            br#"{"workspace_id": "wrk_xyz"}"#,                       // missing token
            br#"{"access_token": "tok"}"#,                            // missing workspace
            br#"{"access_token": "", "workspace_id": "wrk_xyz"}"#,    // empty token
            br#"{"access_token": "tok", "workspace_id": ""}"#,        // empty workspace
        ];
        for blob in blobs {
            let err = parse_opencode_console_credential(blob).unwrap_err();
            assert!(
                matches!(err, UsageError::NoCredential(_)),
                "expected NoCredential, got {err:?}"
            );
        }
    }

    #[test]
    fn parse_live_required_accepts_optional_fields() {
        // `refresh_token` + `expires_at` ride along for the lazy-refresh
        // strategy but aren't required by the live fetcher. The full DTO
        // can carry them and `parse_opencode_console_credential` still
        // succeeds — pins the "live RPC + future refresh on same blob"
        // invariant.
        let blob = br#"{
            "access_token": "oc_live",
            "workspace_id": "wrk_q",
            "refresh_token": "rt_live",
            "expires_at": "2026-12-31T00:00:00Z"
        }"#;
        let (token, workspace) = parse_opencode_console_credential(blob).unwrap();
        assert_eq!(token, "oc_live");
        assert_eq!(workspace, "wrk_q");
    }

    #[test]
    fn cred_is_expired_handles_all_three_states() {
        // Three states the lazy refresh gate must discriminate without
        // panicking: present-and-past → true (refresh), present-and-future →
        // false (use cached), missing-or-malformed → false (attempt the
        // live fetch and let the server be the judge).
        let expired = OpenCodeConsoleCred {
            expires_at: Some("2020-01-01T00:00:00Z".to_string()),
            ..Default::default()
        };
        assert!(cred_is_expired(&expired, 1_700_000_000));

        let fresh = OpenCodeConsoleCred {
            expires_at: Some("2099-01-01T00:00:00Z".to_string()),
            ..Default::default()
        };
        assert!(!cred_is_expired(&fresh, 1_700_000_000));

        let missing = OpenCodeConsoleCred::default();
        assert!(!cred_is_expired(&missing, 1_700_000_000));

        let malformed = OpenCodeConsoleCred {
            expires_at: Some("not a date".to_string()),
            ..Default::default()
        };
        assert!(!cred_is_expired(&malformed, 1_700_000_000));
    }

    // ── Device Flow parsers — pinned RFC 8628 §3.5 / §6.1 contract ────────

    #[test]
    fn parse_device_code_response_valid() {
        // Real-shape body (verified against console.opencode.ai 2026-07-20).
        // All five fields are required by RFC 8628 §6.1.
        let json = r#"{
            "device_code": "dc_abcdef123456",
            "user_code": "ABCD-EFGH",
            "verification_uri_complete": "https://console.opencode.ai/auth/device?code=ABCD-EFGH",
            "expires_in": 600,
            "interval": 5
        }"#;
        let parsed = parse_device_code_response(json).unwrap();
        assert_eq!(parsed.device_code, "dc_abcdef123456");
        assert_eq!(parsed.user_code, "ABCD-EFGH");
        assert_eq!(
            parsed.verification_uri_complete,
            "https://console.opencode.ai/auth/device?code=ABCD-EFGH"
        );
        assert_eq!(parsed.expires_in, Duration::from_secs(600));
        assert_eq!(parsed.interval, Duration::from_secs(5));
    }

    #[test]
    fn parse_device_code_response_clamps_zero_interval() {
        // RFC 8628 §6.1 requires positive `interval`; a server returning 0
        // would deadlock the polling loop. We reject at parse time AND
        // clamp just in case the contract drifts from RFC to something
        // that allows 0 (the pin says reject — tighten later).
        let json = r#"{
            "device_code": "dc_x",
            "user_code": "X",
            "verification_uri_complete": "https://x",
            "expires_in": 600,
            "interval": 0
        }"#;
        let err = parse_device_code_response(json).unwrap_err();
        assert!(
            matches!(err, OAuthError::Shape(_)),
            "non-positive interval must be a Shape failure, got {err:?}"
        );
    }

    #[test]
    fn parse_token_response_valid() {
        let json = r#"{
            "access_token": "oc_sk_test",
            "refresh_token": "rt_test",
            "expires_in": 3600,
            "workspace_id": "wrk_q",
            "server_id": "c83b78a614689c38ebee981f9b39a8b377716db85c1fd7dbab604adc02d3313d"
        }"#;
        let parsed = parse_token_response(json).unwrap();
        assert_eq!(parsed.access_token, "oc_sk_test");
        assert_eq!(parsed.refresh_token, "rt_test");
        assert_eq!(parsed.expires_in, Duration::from_secs(3600));
        assert_eq!(parsed.workspace_id, "wrk_q");
        assert_eq!(
            parsed.server_id,
            "c83b78a614689c38ebee981f9b39a8b377716db85c1fd7dbab604adc02d3313d"
        );
    }

    #[test]
    fn parse_workspaces_response_returns_org_list() {
        let json = r#"{
            "orgs": [
                {"id": "wrk_a", "name": "Acme"},
                {"id": "wrk_b", "name": "Beta"}
            ]
        }"#;
        let parsed = parse_workspaces_response(json).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id, "wrk_a");
        assert_eq!(parsed[0].name, "Acme");
        assert_eq!(parsed[1].id, "wrk_b");
        assert_eq!(parsed[1].name, "Beta");
    }

    #[test]
    fn parse_workspaces_response_empty_orgs_returns_empty_vec() {
        // A user with a single workspace (the OAuth-scoped one) gets an
        // empty org list; the Settings picker only materializes for >1.
        let json = r#"{"orgs": []}"#;
        let parsed = parse_workspaces_response(json).unwrap();
        assert!(parsed.is_empty());
    }
}
