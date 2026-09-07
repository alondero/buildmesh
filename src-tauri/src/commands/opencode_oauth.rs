//! Tauri commands for the Buildmesh-managed OpenCode Go OAuth dance (issue
//! #956 — wire landed in #969). Thin IPC adapters over
//! [`crate::services::opencode_oauth`] — the heavy lifting (Device Flow
//! polling, refresh-on-401, workspace enumeration) lives in the service
//! module; each `#[command]` here is a `run_blocking` one-liner that forwards
//! to a single service method. Mirrors the `commands::usage::get_provider_meters`
//! layout where the IPC surface is the public boundary and `services::usage`
//! owns the logic.
//!
//! Stateless-server design: React owns the in-flight dance state (the
//! `device_code`, the polling interval, the started-at timestamp). Each
//! `poll_opencode_device_token` call advances the dance by one HTTP round-trip
//! and returns a tagged [`crate::services::opencode_oauth::OpenCodeDeviceCodeStatus`]
//! React switches on. No Rust-side per-mesh task, no cleanup-on-timeout — the
//! dance is cancelled by React unmount (modal close).

use std::time::Duration;

use tauri::{command, AppHandle, Emitter};

use crate::services::opencode_oauth::{
    OpenCodeConsoleStatus, OpenCodeDeviceCodeStatus, OpenCodeDeviceFlowStart,
    OpenCodeTokenResponse, OPENCODE_CONSOLE_CHANGED_EVENT,
};

/// Emits `opencode-console-changed` on the desktop bus and busts the
/// Rust-side per-provider usage cache. The emitted event is the
/// trigger `useOpencodeAccountInvalidation` listens for so the React
/// side's Usage tab re-fetches with `force=true`. The cache bust
/// handles the rare case where another Rust caller (not React) is
/// the source of the change. Both are no-ops when nothing changed;
/// `set_cached_usage` is keyed by provider name so the cache entry
/// only exists if a previous `opencode_usage()` call populated it.
fn emit_opencode_console_changed(app: &AppHandle) {
    // `invalidate_provider_cache` is the Rust-side mirror of the React
    // re-fetch. Even if no listeners exist, the next
    // `getProviderMeters(false)` call will hit the live probe rather
    // than serving a stale envelope matched against the wrong
    // workspace_id.
    crate::services::usage::invalidate_provider_cache("opencode");
    let _ = app.emit(OPENCODE_CONSOLE_CHANGED_EVENT, ());
}

/// Kicks off the RFC 8628 Device Flow: `POST /auth/device/code` with the pinned
/// client id. Returns the `user_code` + `verification_uri_complete` React uses
/// to render the "go activate this code" prompt and the browser-open; the
/// `device_code`, `interval_secs`, and `expires_in_secs` bound the React-side
/// polling loop (`commands::opencode_oauth::poll_opencode_device_token`).
///
/// The blocking `reqwest::Client` is dropped at the seam — it has no cross-IPC
/// representation. `run_blocking` parks the work on the blocking thread pool
/// so an a 15s `reqwest::Client::builder().timeout` can't stall a Tauri worker.
/// Mirrors `commands::agent_node_discovery::discover_agent_nodes` (inline
/// closure form).
#[command]
pub async fn start_device_flow_console() -> Result<OpenCodeDeviceFlowStart, String> {
    crate::commands::run_blocking("start_device_flow_console", move || {
        crate::services::opencode_oauth::start_device_flow()
            .map(|(device_code, _client)| OpenCodeDeviceFlowStart {
                device_code: device_code.device_code,
                user_code: device_code.user_code,
                verification_uri_complete: device_code.verification_uri_complete,
                interval_secs: device_code.interval.as_secs() as u32,
                expires_in_secs: device_code.expires_in.as_secs() as u32,
            })
            .map_err(|e| e.to_string())
    })
    .await
}

/// One polling round-trip against `POST /auth/device/token`. React owns the
/// `(device_code, current_interval_secs, original_expires_in_secs, started_at_ms)`
/// quadruple in component state; the IPC just advances the dance one HTTP
/// call and returns a tagged enum React switches on via `status.kind`.
///
/// `original_expires_in_secs` is the immutable window length captured at
/// `start_device_flow_console` time, NOT a per-tick countdown — see
/// `services::opencode_oauth::device_code_is_expired` for the gate and
/// issue #1010 for the rationale.
///
/// Stateless: no Rust-side task, no per-mesh cleanup. React cancels by
/// clearing its `setInterval` on unmount. The CLI-friendly
/// `current_interval_secs` arg is held for symmetry with the server-owned
/// poll loop and a future pre-emptive backoff hook; it's not consumed today.
///
/// Errors collapse from `OAuthError::Transport` (network) to `Error { message }`
/// at the seam — the React side has one terminal branch for "you can't recover
/// by polling more", regardless of whether the underlying cause was DNS,
/// timeout, or a contract drift.
#[command]
pub async fn poll_opencode_device_token(
    device_code: String,
    current_interval_secs: u32,
    original_expires_in_secs: u32,
    started_at_ms: i64,
) -> Result<OpenCodeDeviceCodeStatus, String> {
    crate::commands::run_blocking("poll_opencode_device_token", move || {
        crate::services::opencode_oauth::poll_for_token_once(
            &device_code,
            current_interval_secs,
            original_expires_in_secs,
            started_at_ms,
        )
        .map_err(|e| e.to_string())
    })
    .await
}

/// Returns the workspace list `GET /api/user` + `/api/orgs`.
///
/// `access_token` is optional: when present (the first-time sign-in case,
/// where the fresh token is still in the React reducer and the credential
/// blob hasn't been persisted yet), the IPC uses it directly. When
/// absent (the signedIn view's auto-refresh on workspace switch —
/// workspace binding is read-only after sign-in — and any future
/// "refresh signed-in state" probe), the IPC falls back to the persisted
/// credential. Without this fallback, the first-time sign-in flow would
/// read an empty/missing credential, return `[]`, and persist the token
/// with `workspace_id: None` — which the live probe (services::usage::
/// opencode_live_request_parts) refuses to dispatch.
///
/// Returns `Err(String)` only on a non-recoverable Windows failure (rare);
/// a missing credential (`Ok(None)` from the service layer) collapses here
/// to an empty vec, which the React state machine handles as "the dance
/// has started but no workspaces yet" rather than a toast.
#[command]
pub async fn list_opencode_workspaces(
    access_token: Option<String>,
) -> Result<Vec<crate::services::opencode_oauth::OpenCodeWorkspace>, String> {
    crate::commands::run_blocking("list_opencode_workspaces", move || {
        let token = if let Some(t) = access_token.filter(|s| !s.is_empty()) {
            t
        } else {
            // Fall back to the persisted credential. A missing credential
            // collapses to an empty vec — the React state machine renders
            // "no workspaces" rather than erroring out.
            let cred = crate::services::opencode_oauth::read_opencode_console_full_credential()?
                .ok_or_else(|| "no opencode credential".to_string())?;
            cred.access_token
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "opencode credential missing access_token".to_string())?
        };
        crate::services::opencode_oauth::fetch_workspaces(&token).map_err(|e| e.to_string())
    })
    .await
}

/// Persists a fresh token bundle (issue #969: the React side calls this right
/// after `list_opencode_workspaces` resolves, with the first workspace's id
/// threaded in as `workspace_id`). On Windows this is a Credential Manager
/// write; on non-Windows the service returns `OAuthError::NoCredential` per
/// the locked #956 decision, surfaced here as `Err(String)` matching every
/// other IPC boundary contract.
///
/// `workspace_id` and `server_id` are sourced separately from the token
/// response because the live server's token body (verified 2026-07-23) does
/// not carry them: `workspace_id` comes from `GET /api/user` (the React flow
/// calls `list_opencode_workspaces` first and passes the first workspace's
/// id here), and `server_id` defaults to the legacy `OPENCODE_SERVER_ID`
/// constant in `services::usage` when the caller passes `None`.
#[command]
pub async fn persist_opencode_tokens(
    app: AppHandle,
    token: OpenCodeTokenResponse,
    workspace_id: Option<String>,
    server_id: Option<String>,
) -> Result<(), String> {
    let result = crate::commands::run_blocking("persist_opencode_tokens", move || {
        let inner = crate::services::opencode_oauth::TokenResponse {
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            expires_in: Duration::from_secs(token.expires_in_secs as u64),
        };
        crate::services::opencode_oauth::persist_token_response(
            &inner,
            workspace_id.as_deref(),
            server_id.as_deref(),
        )
        .map_err(|e| e.to_string())
    })
    .await;
    if result.is_ok() {
        emit_opencode_console_changed(&app);
    }
    result
}

/// Revokes the Buildmesh-managed OpenCode Console credential by deleting
/// the Windows Credential Manager entry under `opencode:console`. Idempotent
/// — the Settings "Sign out" affordance never errors on a no-op (mirrors
/// `windows_cred::delete`'s own idempotency on missing credentials and the
/// non-Windows no-op successor in `crate::services::opencode_oauth::revoke`).
///
/// Returns `Ok(())` on success or a `String` describing a non-recoverable
/// Windows failure (rare — e.g. credential-store corruption). Network/HTTP
/// errors are NOT possible; this command does no I/O beyond the local
/// credential manager call.
#[command]
pub async fn revoke_opencode_console(app: AppHandle) -> Result<(), String> {
    let result = crate::commands::run_blocking("revoke_opencode_console", || {
        crate::services::opencode_oauth::revoke()
    })
    .await;
    if result.is_ok() {
        emit_opencode_console_changed(&app);
    }
    result
}

/// Returns the read-only session state for the OpenCode Console OAuth
/// card. Read on mount by `OpenCodeAccountCard` to render `signedIn`
/// without re-running the dance. Bundles the persisted credential
/// (when present) with the workspace picker list (queried from the
/// live Console host with the bearer), the active workspace id, and
/// the access-token expiry epoch.
///
/// Errors collapse to `Err(String)` only on a non-recoverable Windows
/// failure (rare); a missing credential collapses to
/// `signed_in: false` so the React side stays in `signedOut` rather
/// than erroring out.
#[command]
pub async fn get_opencode_console_status() -> Result<OpenCodeConsoleStatus, String> {
    crate::commands::run_blocking("get_opencode_console_status", move || {
        crate::services::opencode_oauth::read_opencode_console_status().map_err(|e| e.to_string())
    })
    .await
}

/// Switches the persisted credential's `workspace_id` to a new value
/// without touching the bearer or refresh_token. The live probe at
/// `services::usage::opencode_usage_impl_with_hosts` reads
/// `workspace_id` from the freshly-persisted blob, so the next
/// `getProviderMeters(force=true)` fetch returns usage for the
/// newly-bound account.
///
/// Errors:
/// - `Err("no opencode credential".to_string())` — no credential is
///   persisted (the user never signed in or already signed out).
///   Returned as a String rather than a typed error so the React
///   side's catch block treats it identically to a transport failure.
/// - `Err(String)` — non-recoverable Windows failure (rare).
///
/// Emits `opencode-console-changed` on success so the Usage tab
/// re-fetches with `force=true` without waiting for the 5-minute
/// cache TTL to lapse.
#[command]
pub async fn set_opencode_console_workspace(
    app: AppHandle,
    workspace_id: String,
) -> Result<(), String> {
    let result = crate::commands::run_blocking("set_opencode_console_workspace", move || {
        if workspace_id.is_empty() {
            return Err("workspace_id must be non-empty".to_string());
        }
        crate::services::opencode_oauth::set_active_workspace(&workspace_id)
            .map_err(|e| e.to_string())
    })
    .await;
    if result.is_ok() {
        emit_opencode_console_changed(&app);
    }
    result
}
