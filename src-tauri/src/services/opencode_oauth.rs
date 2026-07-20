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
//! `#![cfg(windows)]` for the I/O surface; non-Windows callers in this module
//! and `commands::opencode_oauth` return `UsageError::NoCredential("OpenCode
//! Console … is only available on Windows")` to mirror the agy precedent
//! at `services::usage::read_agy_token`.

use crate::services::usage::UsageError;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Windows Credential Manager target the Buildmesh-owned OpenCode OAuth dance
/// writes its token blob under. Mirrors the agy `provider:subcontext` shape
/// (`gemini:antigravity`). Single source of truth — both
/// `services::usage::read_opencode_console_credential` and
/// `services::opencode_oauth::write_credential_blob` refer here.
pub(crate) const OPENCODE_CONSOLE_CRED_TARGET: &str = "opencode:console";

/// Idle TTL for the lazy refresh strategy (issue #956): a credential within
/// `REFRESH_TTL` of expiry is treated as fresh *and* a successful live fetch
/// within the same window short-circuits the next refresh. Mirrors the
/// `CACHE_TTL` of `services::usage` so the OAuth lifecycle and the cache
/// lifecycle agree — refreshing at minute N doesn't poison a stale cache hit
/// at minute N+1.
pub(crate) const REFRESH_TTL: Duration = Duration::from_secs(300);

/// JSON shape of the credential blob written by the device-flow dance.
/// `access_token` is the live RPC bearer; `workspace_id` is the `wrk_<id>`
/// passed to the `_server billing.get` body; `refresh_token` + `expires_at`
/// drive the lazy refresh; `server_id` is the SolidStart deployment id
/// (`c83b78a6…`) — currently hard-coded in
/// `services::usage::opencode_usage_impl` at the `X-Server-Id` header, but
/// persisted here so a future multi-deployment scenario can read it from
/// the blob rather than re-emitting a binary string.
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

/// Parses the OpenCode Console credential blob into `(access_token,
/// workspace_id)`. Both fields are required by the live fetcher; either
/// missing or empty is treated as `NoCredential` so the offline SQLite path
/// (#953) takes over with whatever partial state the user has on disk.
///
/// Thin wrapper over [`parse_opencode_console_full_credential`] — the full
/// DTO path is the contract; this exists so `services::usage` keeps its
/// single-statement read path (`parse(...).map(|...| (token, workspace))`)
/// without leaking the optional-field plumbing into the fetcher.
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
}
