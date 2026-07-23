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

use tauri::command;

use crate::services::opencode_oauth::{
    OpenCodeDeviceFlowStart, OpenCodeDeviceCodeStatus, OpenCodeTokenResponse,
};

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
    token: OpenCodeTokenResponse,
    workspace_id: Option<String>,
    server_id: Option<String>,
) -> Result<(), String> {
    crate::commands::run_blocking("persist_opencode_tokens", move || {
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
    .await
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
pub async fn revoke_opencode_console() -> Result<(), String> {
    crate::services::opencode_oauth::revoke()
}
