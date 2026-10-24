// Sibling test suite for the owning module. The suite lives in its own
// file so the production module keeps only the declaration below — the
// repo's rust-test-layout gate requires it.

use super::*;
use crate::services::usage::adapter::spawn_loopback;
use crate::services::usage::cache::invalidate_cache;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
// ─── OpenCode ─────────────────────────────────────────────────────────

#[test]
fn parse_opencode_auth_json_valid() {
    let temp_dir = std::env::temp_dir();
    let path = temp_dir.join("opencode_auth.json");
    let json = r#"{
        "google": { "type": "api", "key": "AIzaSy..." },
        "opencode-go": { "type": "api", "key": "sk-D54t4e3..." }
    }"#;
    fs::write(&path, json).unwrap();
    let key = read_opencode_token(path.clone()).unwrap();
    assert_eq!(key, "sk-D54t4e3...");
    let _ = fs::remove_file(path);
}

#[test]
fn parse_opencode_auth_json_missing_or_empty_key() {
    let temp_dir = std::env::temp_dir();
    let path = temp_dir.join("opencode_auth_empty.json");
    let json = r#"{
        "opencode-go": { "type": "api", "key": "" }
    }"#;
    fs::write(&path, json).unwrap();
    let err = read_opencode_token(path.clone()).unwrap_err();
    assert!(matches!(err, UsageError::NoCredential(_)));
    let _ = fs::remove_file(path);
}

#[test]
fn test_calculate_opencode_windows_impl() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE TABLE session (
            id TEXT PRIMARY KEY,
            time_created INTEGER NOT NULL,
            cost REAL DEFAULT 0 NOT NULL
        )",
        [],
    )
    .unwrap();

    let now_ms = chrono::Utc::now().timestamp_millis();

    // 5 hours limit = $12.00, we put $3.00 -> 25.0%
    // Weekly limit = $30.00, we put $3.00 + $6.00 = $9.00 -> 30.0%
    // Monthly limit = $60.00, we put $9.00 + $15.00 = $24.00 -> 40.0%
    conn.execute(
        "INSERT INTO session (id, time_created, cost) VALUES (?, ?, ?)",
        rusqlite::params!["ses1", now_ms - 2 * 60 * 60 * 1000, 3.0],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO session (id, time_created, cost) VALUES (?, ?, ?)",
        rusqlite::params!["ses2", now_ms - 2 * 24 * 60 * 60 * 1000, 6.0],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO session (id, time_created, cost) VALUES (?, ?, ?)",
        rusqlite::params!["ses3", now_ms - 15 * 24 * 60 * 60 * 1000, 15.0],
    )
    .unwrap();
    // Outside 30 days, should not be included
    conn.execute(
        "INSERT INTO session (id, time_created, cost) VALUES (?, ?, ?)",
        rusqlite::params!["ses4", now_ms - 40 * 24 * 60 * 60 * 1000, 100.0],
    )
    .unwrap();

    let windows = calculate_opencode_windows_impl(&conn).unwrap();
    assert_eq!(windows.len(), 3);

    assert_eq!(windows[0].label, "5-hour");
    assert_eq!(windows[0].used_percent, Some(25.0));

    assert_eq!(windows[1].label, "Weekly");
    assert_eq!(windows[1].used_percent, Some(30.0));

    assert_eq!(windows[2].label, "Monthly");
    assert_eq!(windows[2].used_percent, Some(40.0));
}

// ── OpenCode Go live `_server billing.get` probe (issue #957) ────────

#[test]
fn parse_opencode_billing_response_full() {
    // Pinned fixture: the documented `billing.get` reply shape (issue #957
    // sub-spec point 5). All three windows + their reset countdowns are
    // present and must round-trip through `UsageWindow` byte-for-byte.
    let json = r#"{
        "windows": [
            {"label": "5-hour",  "usedPercent": 25.0, "resetsAt": "2026-07-20T22:00:00Z"},
            {"label": "Weekly",  "usedPercent": 12.0, "resetsAt": "2026-07-22T00:00:00Z"},
            {"label": "Monthly", "usedPercent":  4.5, "resetsAt": "2026-08-01T00:00:00Z"}
        ]
    }"#;
    let (windows, detail) = parse_opencode_billing_response(json).unwrap();
    assert_eq!(windows.len(), 3);
    assert_eq!(windows[0].label, "5-hour");
    assert_eq!(windows[0].used_percent, Some(25.0));
    assert_eq!(
        windows[0].resets_at.as_deref(),
        Some("2026-07-20T22:00:00Z")
    );
    assert_eq!(windows[1].label, "Weekly");
    assert_eq!(windows[1].used_percent, Some(12.0));
    assert_eq!(windows[2].label, "Monthly");
    assert_eq!(windows[2].used_percent, Some(4.5));
    assert_eq!(detail, None);
}

#[test]
fn parse_opencode_billing_response_partial_5hour_only() {
    // Sub-spec point 5: a partial reply that carries only the 5-hour
    // window must parse cleanly (one row out, no error) rather than
    // failing closed. This matches how SolidStart server functions can
    // early-return the most-pressed window before the others.
    let json = r#"{
        "windows": [
            {"label": "5-hour", "usedPercent": 80.0, "resetsAt": "2026-07-20T22:00:00Z"}
        ]
    }"#;
    let (windows, detail) = parse_opencode_billing_response(json).unwrap();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].label, "5-hour");
    assert_eq!(windows[0].used_percent, Some(80.0));
    assert_eq!(
        windows[0].resets_at.as_deref(),
        Some("2026-07-20T22:00:00Z")
    );
    assert_eq!(detail, None);
}

#[test]
fn parse_opencode_billing_response_missing_windows_array_is_shape_error() {
    // Required field — a body without `windows` is malformed, not "all
    // quotas are 0". This is the silent-zero-windowing trap the parser
    // MUST fail loudly against so the live fetch returns unavailable
    // and the degradation chain falls through to SQLite.
    let json = r#"{"foo": "bar"}"#;
    let err = parse_opencode_billing_response(json).unwrap_err();
    assert!(
        matches!(err, UsageError::Shape(_)),
        "expected Shape error, got {err:?}"
    );
}

#[test]
fn parse_opencode_billing_response_empty_windows_reports_detail() {
    // A well-formed reply with an empty `windows` array surfaces the
    // user-facing "no active quotas" detail so the UI doesn't render a
    // mysteriously empty meter. Mirrors the `parse_agy_models_empty_reports_detail`
    // contract so the Usage tab copy stays consistent across providers.
    let json = r#"{"windows": []}"#;
    let (windows, detail) = parse_opencode_billing_response(json).unwrap();
    assert!(windows.is_empty());
    assert_eq!(
        detail.as_deref(),
        Some("No active OpenCode Go quotas found")
    );
}

#[test]
fn parse_opencode_billing_response_filters_malformed_windows() {
    // Per the stricter shape contract (issue #957 sub-spec point 4), a
    // window missing `usedPercent` is a shape failure — it MUST NOT
    // render as a "5-hour: (no data)" row, which is the silent-blank
    // gauge the spec sought to prevent. Such windows are filtered out
    // and any surviving valid windows still parse.
    let json = r#"{
        "windows": [
            {"label": "5-hour", "usedPercent": 25.0, "resetsAt": "2026-07-20T22:00:00Z"},
            {"label": "no-data-window"},
            {"usedPercent": 50.0, "resetsAt": "2026-07-22T00:00:00Z"},
            {}
        ]
    }"#;
    let (windows, detail) = parse_opencode_billing_response(json).unwrap();
    assert_eq!(windows.len(), 1, "only the fully-formed window survives");
    assert_eq!(windows[0].label, "5-hour");
    assert_eq!(windows[0].used_percent, Some(25.0));
    assert_eq!(detail, None);
}

#[test]
fn parse_opencode_billing_response_all_windows_malformed_reports_detail() {
    // Edge case: every window is malformed. The parser succeeds (the
    // shape is well-formed), the empty-filtered result surfaces the
    // "no active quotas" detail. The SQLite fallback is NOT triggered
    // here because the shape contract is satisfied — empty + detail is
    // a valid "no quotas configured" reply, distinct from "shape
    // failure" (which would be `{"foo":"bar"}`).
    let json = r#"{"windows": [{"label": "junk"}, {"usedPercent": null}]}"#;
    let (windows, detail) = parse_opencode_billing_response(json).unwrap();
    assert!(windows.is_empty());
    assert_eq!(
        detail.as_deref(),
        Some("No active OpenCode Go quotas found")
    );
}

// The OpenCode Console credential parser tests (formerly
// `parse_opencode_console_credential_*`) were relocated to
// `services::opencode_oauth` alongside the DTO + parser itself for #956.

// ── choose_opencode_usage — the heart of the degradation chain ────────

fn fake_reading(used_percent: f64) -> UsageOutcome {
    UsageOutcome::Reading {
        windows: vec![UsageWindow {
            label: "5-hour".to_string(),
            used_percent: Some(used_percent),
            resets_at: None,
        }],
        balance: None,
        meters: vec![],
        detail: None,
    }
}

fn fake_unavailable(msg: &str) -> UsageOutcome {
    UsageOutcome::Unavailable {
        reason: msg.to_string(),
    }
}

#[test]
fn choose_opencode_usage_live_success_wins() {
    // Live returned real numbers → SQLite is ignored. The 75% figure is
    // the live value; the 50% figure is the SQLite value — neither
    // matches the other's source, so the assertion is unambiguous.
    let live = fake_reading(75.0);
    let sqlite = fake_reading(50.0);
    match choose_opencode_usage(Some(live), sqlite) {
        UsageOutcome::Reading { windows, .. } => {
            assert_eq!(windows[0].used_percent, Some(75.0));
        }
        other => panic!("expected live Reading outcome, got: {other:?}"),
    }
}

#[test]
fn choose_opencode_usage_live_error_falls_back_to_sqlite() {
    // THE pin: live attempted AND failed (HTTP 401 / 5xx / shape) →
    // SQLite is returned. A future refactor that drops the
    // `Reading`-only guard would surface the failure in the Probe UI
    // instead of the SQLite windows; this test catches it.
    let live = fake_unavailable("API error 401: Unauthorized");
    let sqlite = fake_reading(50.0);
    match choose_opencode_usage(Some(live), sqlite) {
        UsageOutcome::Reading { windows, .. } => {
            assert_eq!(windows[0].used_percent, Some(50.0));
        }
        other => panic!("expected sqlite Reading outcome, got: {other:?}"),
    }
}

#[test]
fn choose_opencode_usage_no_credential_falls_back_to_sqlite() {
    // No `opencode:console` credential at all (read returned
    // NoCredential, collapsed to None) → SQLite is returned. The user
    // who has run `opencode auth login` but not yet finished #956's
    // device flow lands here.
    let sqlite = fake_reading(50.0);
    match choose_opencode_usage(None, sqlite) {
        UsageOutcome::Reading { windows, .. } => {
            assert_eq!(windows[0].used_percent, Some(50.0));
        }
        other => panic!("expected sqlite Reading outcome, got: {other:?}"),
    }
}

#[test]
fn needs_retry_on_401_matches_rejected_outcome_not_error_text() {
    // Issue #1758: the reactive retry gate matches on the `Rejected`
    // outcome. A `Rejected` 403 (revocation by another name) retries;
    // an `Unavailable` whose body merely mentions "401" does not —
    // the old substring match fired a spurious refresh there.
    assert!(needs_retry_on_401(Some(&UsageOutcome::Rejected {
        hint: "API error 401: Unauthorized".to_string(),
    })));
    assert!(needs_retry_on_401(Some(&UsageOutcome::Rejected {
        hint: "API error 403: Forbidden".to_string(),
    })));
    assert!(!needs_retry_on_401(Some(&fake_unavailable(
        "API error 500: error 401 in body"
    ))));
    assert!(!needs_retry_on_401(Some(&fake_reading(25.0))));
    assert!(!needs_retry_on_401(None));
}

// ── opencode_usage_impl — end-to-end fallback integration ──────────────

// ── resolve_opencode_server_id — issue #972 ─────────────────────────────
//
// The live `_server billing.get` probe must read its `X-Server-Id`
// header from the persisted `OpenCodeConsoleCred.server_id`, falling
// back to the legacy `OPENCODE_SERVER_ID` constant for credentials
// written before #956 added the field. These pure tests pin the
// resolver contract; the empty-string case is hand-edit safety.

fn cred_with_server_id(server_id: Option<&str>) -> OpenCodeConsoleCred {
    OpenCodeConsoleCred {
        access_token: Some("tok".to_string()),
        workspace_id: Some("wrk".to_string()),
        refresh_token: None,
        expires_at: None,
        server_id: server_id.map(str::to_owned),
    }
}

#[test]
fn resolve_opencode_server_id_prefers_persisted_value() {
    // Issue #972 acceptance #1: when `server_id` is present and
    // non-empty, the resolver returns it verbatim — the live probe
    // sends that value in the `X-Server-Id` header.
    let cred = cred_with_server_id(Some("custom-deployment-id-abc123"));
    assert_eq!(
        resolve_opencode_server_id(&cred),
        "custom-deployment-id-abc123"
    );
}

#[test]
fn resolve_opencode_server_id_falls_back_to_constant_when_missing() {
    // Issue #972 acceptance #2: a blob without `server_id` (the
    // pre-#956 shape) keeps emitting the legacy constant so existing
    // users don't lose their live probe.
    let cred = cred_with_server_id(None);
    assert_eq!(resolve_opencode_server_id(&cred), OPENCODE_SERVER_ID);
}

#[test]
fn resolve_opencode_server_id_falls_back_when_empty_string() {
    // A hand-edited blob with `"server_id": ""` is treated as missing
    // so the resolver never returns a useless empty `X-Server-Id`
    // header. The fallback branch's warn-once still fires.
    let cred = cred_with_server_id(Some(""));
    assert_eq!(resolve_opencode_server_id(&cred), OPENCODE_SERVER_ID);
}

#[test]
fn opencode_live_request_parts_returns_persisted_server_id_for_header() {
    // Issue #972 acceptance #5: a credential with a non-default
    // `server_id` causes the live probe to send THAT value (not the
    // legacy constant) in the `X-Server-Id` header. The token /
    // workspace round-trip is pinned in the same assertion so a
    // future refactor that drops the header binding still fails.
    let cred = cred_with_server_id(Some("custom-deployment-id-xyz"));
    let (token, workspace_id, server_id) =
        opencode_live_request_parts(&cred).expect("credential is complete");
    assert_eq!(token, "tok");
    assert_eq!(workspace_id, "wrk");
    assert_eq!(
        server_id, "custom-deployment-id-xyz",
        "header must read from cred.server_id, not OPENCODE_SERVER_ID"
    );
    assert_ne!(
        server_id, OPENCODE_SERVER_ID,
        "must not silently fall back to the constant"
    );
}

#[test]
fn opencode_live_request_parts_uses_legacy_constant_when_persisted_missing() {
    // The matching legacy-default branch: when the credential has no
    // `server_id`, the header value IS the constant — that's how
    // pre-#956 blobs continue to probe SolidStart without an
    // immediate re-auth.
    let cred = cred_with_server_id(None);
    let (_token, _workspace_id, server_id) =
        opencode_live_request_parts(&cred).expect("credential is complete");
    assert_eq!(server_id, OPENCODE_SERVER_ID);
}

#[test]
fn opencode_live_request_parts_returns_none_when_token_missing() {
    // A blob missing `access_token` (e.g. mid-flow) must collapse to
    // `None` so the SQLite fallback runs. Mirrors how the live path
    // originally responded to a `NoCredential` read.
    let mut cred = cred_with_server_id(Some("custom"));
    cred.access_token = None;
    assert!(opencode_live_request_parts(&cred).is_none());
}

#[test]
fn opencode_live_request_parts_returns_none_when_workspace_missing() {
    // Same invariant for `workspace_id` — the body needs a value
    // even when the token is present.
    let mut cred = cred_with_server_id(Some("custom"));
    cred.workspace_id = None;
    assert!(opencode_live_request_parts(&cred).is_none());
}

#[test]
fn opencode_usage_impl_with_sqlite_db_only_returns_sqlite_windows() {
    // Pin the degradation chain (issue #957 sub-spec point 4): with a
    // fake home containing a valid auth.json + opencode.db but NO
    // `opencode:console` credential in the OS store, the fetcher MUST
    // run the SQLite path and return its windows. We exercise this via
    // the testable `opencode_usage_impl(home)` seam (the public
    // `opencode_usage()` reads `home_dir()` — we can't override that
    // without mutating USERPROFILE globally across the test suite).
    let unique = format!(
        "opencode_test_fallback_{}_{:?}",
        std::process::id(),
        std::thread::current().id()
    );
    let temp = std::env::temp_dir().join(unique);
    let _ = fs::remove_dir_all(&temp);
    let opencode_dir = temp.join(".local").join("share").join("opencode");
    fs::create_dir_all(&opencode_dir).unwrap();

    // Auth.json present (SQLite path's logged-in gate) with a non-empty
    // `opencode-go` key. This is the same shape `opencode auth login`
    // produces on a real workstation.
    fs::write(
        opencode_dir.join("auth.json"),
        r#"{"opencode-go": {"type": "api", "key": "sk-test-abc"}}"#,
    )
    .unwrap();

    // Seed an opencode.db with one recent session whose cost puts us at
    // 50% of the 5-hour $12 limit. The weekly/monthly rows are computed
    // from the same session so they'll report small percentages too.
    let db_path = opencode_dir.join("opencode.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "CREATE TABLE session (
            id TEXT PRIMARY KEY,
            time_created INTEGER NOT NULL,
            cost REAL DEFAULT 0 NOT NULL
        )",
        [],
    )
    .unwrap();
    let now_ms = chrono::Utc::now().timestamp_millis();
    conn.execute(
        "INSERT INTO session (id, time_created, cost) VALUES (?, ?, ?)",
        rusqlite::params!["ses_fallback", now_ms - 30 * 60 * 1000, 6.0],
    )
    .unwrap();
    drop(conn);

    // Live path will fail with NoCredential (no Windows Credential
    // Manager entry exists in this test) — but the SQLite fallback
    // MUST still produce real windows. Pin that.
    match opencode_usage_impl(&temp) {
        UsageOutcome::Reading { windows, .. } => {
            assert_eq!(windows.len(), 3);
            assert_eq!(windows[0].label, "5-hour");
            assert_eq!(
                windows[0].used_percent,
                Some(50.0),
                "$6 of $12 5-hour limit should yield 50%"
            );
        }
        other => panic!("expected sqlite Reading outcome, got: {other:?}"),
    }

    let _ = fs::remove_dir_all(&temp);
}

// ── Refresh seam gate (issue #970) ──────────────────────────────────
//
// The seam in `opencode_usage_impl` refreshes the bearer before the live
// `_server billing.get` HTTP call when EITHER:
//   - the cached credential's `expires_at` is in the past, OR
//   - the cached live-fetch result is older than REFRESH_TTL (the credential
//     was fresh at fetch time but is plausibly near expiry by now).
//
// These tests pin both halves of the gate without needing to mock
// Windows Credential Manager — `opencode_needs_refresh` is a pure
// function over the inputs we already have.

#[test]
fn opencode_needs_refresh_when_credential_is_expired() {
    // The primary trigger: `expires_at` is in the past → MUST refresh
    // regardless of cache age. A credential that's already past expiry
    // is going to 401 the next live fetch, so we mint a new bearer
    // proactively.
    let cred = OpenCodeConsoleCred {
        expires_at: Some("2020-01-01T00:00:00Z".to_string()),
        ..Default::default()
    };
    assert!(opencode_needs_refresh(&cred, None, 1_700_000_000));
    assert!(opencode_needs_refresh(
        &cred,
        Some(Duration::from_secs(0)),
        1_700_000_000
    ));
}

#[test]
fn opencode_needs_refresh_when_cache_is_stale_but_credential_claims_fresh() {
    // The belt-and-braces trigger: `expires_at` claims the token is
    // still valid, but the cached live fetch is older than REFRESH_TL.
    // Token might have been near expiry at fetch time and now IS
    // expired; refreshing proactively avoids the 401 round-trip.
    let cred = OpenCodeConsoleCred {
        expires_at: Some("2099-01-01T00:00:00Z".to_string()),
        ..Default::default()
    };
    let now = 1_700_000_000;
    assert!(
        opencode_needs_refresh(&cred, Some(Duration::from_secs(301)), now),
        "cache older than REFRESH_TL must trigger refresh"
    );
    assert!(
        !opencode_needs_refresh(&cred, Some(Duration::from_secs(299)), now),
        "cache within REFRESH_TL must NOT trigger refresh"
    );
}

#[test]
fn opencode_needs_refresh_no_op_when_fresh_and_no_cache() {
    // Two ways to skip the refresh: a fresh credential with no cached
    // fetch yet (first call to opencode_usage_impl this process), AND
    // a fresh credential with a recent cached fetch. Both must NOT
    // trigger refresh so we don't burn a /auth/device/token round-trip
    // on every usage panel poll.
    let cred = OpenCodeConsoleCred {
        expires_at: Some("2099-01-01T00:00:00Z".to_string()),
        ..Default::default()
    };
    assert!(!opencode_needs_refresh(&cred, None, 1_700_000_000));
    assert!(!opencode_needs_refresh(
        &cred,
        Some(Duration::from_secs(0)),
        1_700_000_000
    ));
    assert!(!opencode_needs_refresh(
        &cred,
        Some(Duration::from_secs(300)),
        1_700_000_000
    ));
}

#[test]
fn opencode_needs_refresh_handles_missing_or_malformed_expires_at() {
    // A credential without `expires_at` (legacy or pre-#956 blob) is
    // treated as "unknown" by `cred_is_expired` (returns false) so the
    // cache-age half of the gate is the only refresh trigger. Same for
    // a malformed timestamp — we don't want a parsing error to fire a
    // refresh that we then can't use.
    let missing = OpenCodeConsoleCred::default();
    let malformed = OpenCodeConsoleCred {
        expires_at: Some("not a date".to_string()),
        ..Default::default()
    };
    let now = 1_700_000_000;
    // Missing expires_at + no cache → no refresh (let the live fetch try).
    assert!(!opencode_needs_refresh(&missing, None, now));
    // Missing expires_at + stale cache → refresh (cache age wins).
    assert!(opencode_needs_refresh(
        &missing,
        Some(Duration::from_secs(600)),
        now
    ));
    // Malformed expires_at + no cache → no refresh.
    assert!(!opencode_needs_refresh(&malformed, None, now));
}

// ── Refresh-on-401 — mocked HTTP integration (issue #971) ─────────
//
// The four headline scenarios from #971's Verification section:
//   1. Credential expired → refresh succeeds → live probe succeeds.
//   2. Credential expired → refresh fails → SQLite fallback.
//   3. Credential fresh → no refresh → live probe succeeds.
//   4. Credential fresh → no refresh → live 401 → refresh-on-the-spot
//      → live probe succeeds.
//
// `spawn_loopback` stands up a `tiny_http` server on `127.0.0.1:0`
// that dispatches on `req.url()`. The two URLs mirror the production
// paths: `/_server` for the live probe, `/auth/device/token` for the
// refresh. Each test seeds a credential with the shape produced by
// `persist_token_response` (issue #956) and counts calls per path so
// we can assert the orchestration, not just the final envelope.
//
// Pattern lifted from `services::opencode_oauth::tests::spawn_loopback`
// (issue #967) — the `tiny_http` crate is already a regular
// `[dependencies]` entry, so this adds no new crate.

/// Builds a temp home with a `auth.json` + `opencode.db` so the
/// SQLite fallback has something to render. The session row seeds
/// the 5-hour window at 50% — the SQLite fallback tests assert this
/// number verbatim so a regression in the roll-up math surfaces
/// here rather than in the live probe path.
fn make_opencode_home(label: &str) -> std::path::PathBuf {
    let unique = format!(
        "opencode_test_refresh_{}_{}_{:?}",
        label,
        std::process::id(),
        std::thread::current().id()
    );
    let temp = std::env::temp_dir().join(unique);
    let _ = fs::remove_dir_all(&temp);
    let opencode_dir = temp.join(".local").join("share").join("opencode");
    fs::create_dir_all(&opencode_dir).unwrap();
    // auth.json gates the SQLite fallback's `logged_in` branch.
    fs::write(
        opencode_dir.join("auth.json"),
        r#"{"opencode-go": {"type": "api", "key": "sk-test-abc"}}"#,
    )
    .unwrap();
    // One session row, $6 spent → 50% of the $12 5-hour limit.
    let db_path = opencode_dir.join("opencode.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "CREATE TABLE session (
            id TEXT PRIMARY KEY,
            time_created INTEGER NOT NULL,
            cost REAL DEFAULT 0 NOT NULL
        )",
        [],
    )
    .unwrap();
    let now_ms = chrono::Utc::now().timestamp_millis();
    conn.execute(
        "INSERT INTO session (id, time_created, cost) VALUES (?, ?, ?)",
        rusqlite::params!["ses_refresh", now_ms - 30 * 60 * 1000, 6.0],
    )
    .unwrap();
    drop(conn);
    temp
}

/// Wipes the process-wide `USAGE_CACHE` so a previous test's cached
/// envelope can't short-circuit the live path. Issue #970's cache-age
/// gate (cached_age > REFRESH_TTL) would otherwise leak state across
/// tests in the same process.
fn clear_usage_cache() {
    invalidate_cache();
}

/// Constructs a credential with the shape produced by
/// `services::opencode_oauth::persist_token_response`.
fn build_cred(
    access_token: &str,
    refresh_token: &str,
    expires_at: &str,
    workspace_id: &str,
    server_id: &str,
) -> OpenCodeConsoleCred {
    OpenCodeConsoleCred {
        access_token: Some(access_token.to_string()),
        workspace_id: Some(workspace_id.to_string()),
        refresh_token: Some(refresh_token.to_string()),
        expires_at: Some(expires_at.to_string()),
        server_id: Some(server_id.to_string()),
    }
}

/// The fixed `_server billing.get` success body used by scenarios 1,
/// 3, and 4. Pinned to the documented fixture shape from issue #957
/// so a wire-shape drift fails here, not just in the `wiremock`
/// server assertion.
const LIVE_BODY_OK: &str = r#"{
    "windows": [
        {"label": "5-hour",  "usedPercent": 25.0, "resetsAt": "2026-07-20T22:00:00Z"},
        {"label": "Weekly",  "usedPercent": 12.0, "resetsAt": "2026-07-22T00:00:00Z"},
        {"label": "Monthly", "usedPercent":  4.5, "resetsAt": "2026-08-01T00:00:00Z"}
    ]
}"#;

/// The fixed refresh success body — every TokenResponse field is
/// required by `parse_token_response` (issue #956).
const REFRESH_BODY_OK: &str = r#"{
    "access_token": "new_tok",
    "refresh_token": "new_rt",
    "token_type": "Bearer",
    "expires_in": 3600
}"#;

// ------- Scenario 1: expired → refresh succeeds → live probe succeeds

#[test]
fn opencode_usage_impl_expired_credential_refresh_succeeds_live_probe_succeeds() {
    // Headline scenario 1: the cache is empty, the credential's
    // expires_at is in the past, so the pre-emptive refresh gate
    // fires. Refresh returns a fresh bundle; the live probe runs
    // ONCE with the new bearer and returns the real windows.
    clear_usage_cache();
    let temp = make_opencode_home("s1");

    let refresh_count = Arc::new(AtomicUsize::new(0));
    let live_count = Arc::new(AtomicUsize::new(0));
    let refresh_count_t = refresh_count.clone();
    let live_count_t = live_count.clone();

    let port = spawn_loopback(2, move |req| match req.url() {
        "/auth/device/token" => {
            refresh_count_t.fetch_add(1, Ordering::SeqCst);
            let _ = req.respond(tiny_http::Response::from_string(REFRESH_BODY_OK));
        }
        "/_server" => {
            live_count_t.fetch_add(1, Ordering::SeqCst);
            let _ = req.respond(tiny_http::Response::from_string(LIVE_BODY_OK));
        }
        _ => {
            let _ =
                req.respond(tiny_http::Response::from_string("not found").with_status_code(404));
        }
    });
    let live_url = format!("http://127.0.0.1:{port}/_server");
    let refresh_url = format!("http://127.0.0.1:{port}/auth/device/token");

    let cred = build_cred(
        "old_tok",
        "rt_old",
        "2020-01-01T00:00:00Z",
        "wrk_q",
        "srv_v1",
    );
    let outcome = opencode_usage_impl_with_hosts(&temp, &live_url, &refresh_url, Some(&cred));

    assert_eq!(
        refresh_count.load(Ordering::SeqCst),
        1,
        "pre-emptive refresh fires exactly once for an expired credential"
    );
    assert_eq!(
        live_count.load(Ordering::SeqCst),
        1,
        "live probe is called once with the refreshed bearer"
    );
    match outcome {
        UsageOutcome::Reading { windows, .. } => {
            assert_eq!(windows.len(), 3);
            assert_eq!(windows[0].label, "5-hour");
            assert_eq!(windows[0].used_percent, Some(25.0));
        }
        other => panic!("expected live Reading outcome, got: {other:?}"),
    }

    let _ = fs::remove_dir_all(&temp);
}

// ------- Scenario 2: expired → refresh fails → SQLite fallback

#[test]
fn opencode_usage_impl_expired_credential_refresh_fails_falls_back_to_sqlite() {
    // Headline scenario 2: refresh returns 500. The seam logs the
    // failure and proceeds — the live probe is called with the
    // OLD (still expired) bearer, returns 401, and the SQLite
    // fallback takes over. The pre-emptive refresh having fired
    // means the reactive retry is suppressed (see
    // `opencode_usage_impl_with_hosts`), so the SQLite fallback is
    // the user's answer even though the live probe had a 401 to
    // offer.
    clear_usage_cache();
    let temp = make_opencode_home("s2");

    let refresh_count = Arc::new(AtomicUsize::new(0));
    let live_count = Arc::new(AtomicUsize::new(0));
    let refresh_count_t = refresh_count.clone();
    let live_count_t = live_count.clone();

    let port = spawn_loopback(2, move |req| match req.url() {
        "/auth/device/token" => {
            refresh_count_t.fetch_add(1, Ordering::SeqCst);
            let _ = req.respond(
                tiny_http::Response::from_string(r#"{"error":"server boom"}"#)
                    .with_status_code(500),
            );
        }
        "/_server" => {
            live_count_t.fetch_add(1, Ordering::SeqCst);
            let _ = req.respond(
                tiny_http::Response::from_string(r#"{"error":"unauthorized"}"#)
                    .with_status_code(401),
            );
        }
        _ => {
            let _ =
                req.respond(tiny_http::Response::from_string("not found").with_status_code(404));
        }
    });
    let live_url = format!("http://127.0.0.1:{port}/_server");
    let refresh_url = format!("http://127.0.0.1:{port}/auth/device/token");

    let cred = build_cred(
        "old_tok",
        "rt_old",
        "2020-01-01T00:00:00Z",
        "wrk_q",
        "srv_v1",
    );
    let usage = opencode_usage_impl_with_hosts(&temp, &live_url, &refresh_url, Some(&cred));

    assert_eq!(
        refresh_count.load(Ordering::SeqCst),
        1,
        "pre-emptive refresh fires once; reactive is suppressed because pre-emptive fired"
    );
    assert_eq!(
        live_count.load(Ordering::SeqCst),
        1,
        "live probe runs once with the old (expired) bearer when refresh fails"
    );
    // SQLite fallback wins — the live path's 401 is suppressed,
    // and the seeded `$6 of $12` 5-hour window shows through at 50%.
    match usage {
        UsageOutcome::Reading { windows, .. } => {
            assert_eq!(windows.len(), 3);
            assert_eq!(windows[0].label, "5-hour");
            assert_eq!(
                windows[0].used_percent,
                Some(50.0),
                "sqlite fallback returns the seeded 50% 5-hour window"
            );
        }
        other => panic!("expected sqlite Reading outcome, got: {other:?}"),
    }

    let _ = fs::remove_dir_all(&temp);
}

// ------- Scenario 3: fresh → no refresh → live probe succeeds

#[test]
fn opencode_usage_impl_fresh_credential_no_refresh_live_probe_succeeds() {
    // Headline scenario 3: the credential is fresh (expires_at far
    // in the future), the cache is empty, so neither pre-emptive
    // gate fires. The live probe is called ONCE with the existing
    // bearer and returns the real windows. Refresh count is zero —
    // the test pins this so a future refactor that adds a stale
    // cache short-circuit still respects the "no refresh on fresh
    // cred" contract.
    clear_usage_cache();
    let temp = make_opencode_home("s3");

    let refresh_count = Arc::new(AtomicUsize::new(0));
    let live_count = Arc::new(AtomicUsize::new(0));
    let refresh_count_t = refresh_count.clone();
    let live_count_t = live_count.clone();

    let port = spawn_loopback(1, move |req| match req.url() {
        "/auth/device/token" => {
            refresh_count_t.fetch_add(1, Ordering::SeqCst);
            let _ = req.respond(tiny_http::Response::from_string(REFRESH_BODY_OK));
        }
        "/_server" => {
            live_count_t.fetch_add(1, Ordering::SeqCst);
            let _ = req.respond(tiny_http::Response::from_string(LIVE_BODY_OK));
        }
        _ => {
            let _ =
                req.respond(tiny_http::Response::from_string("not found").with_status_code(404));
        }
    });
    let live_url = format!("http://127.0.0.1:{port}/_server");
    let refresh_url = format!("http://127.0.0.1:{port}/auth/device/token");

    let cred = build_cred(
        "fresh_tok",
        "rt_fresh",
        "2099-01-01T00:00:00Z",
        "wrk_q",
        "srv_v1",
    );
    let usage = opencode_usage_impl_with_hosts(&temp, &live_url, &refresh_url, Some(&cred));

    assert_eq!(
        refresh_count.load(Ordering::SeqCst),
        0,
        "no refresh on a fresh credential — neither gate fires"
    );
    assert_eq!(
        live_count.load(Ordering::SeqCst),
        1,
        "live probe runs once with the existing bearer"
    );
    match usage {
        UsageOutcome::Reading { windows, .. } => {
            assert_eq!(windows.len(), 3);
            assert_eq!(windows[0].used_percent, Some(25.0));
        }
        other => panic!("expected live Reading outcome, got: {other:?}"),
    }

    let _ = fs::remove_dir_all(&temp);
}

// ------- Scenario 4: fresh → no refresh → live 401 → refresh-on-spot → live succeeds

#[test]
fn opencode_usage_impl_fresh_credential_live_401_triggers_reactive_refresh_and_retry() {
    // Headline scenario 4: credential is fresh (no pre-emptive
    // refresh), but the live probe returns 401 — the server
    // revoked the token under us. The reactive refresh-on-401
    // branch fires, refreshes the bearer, and the live probe is
    // called AGAIN with the new bearer. The second call succeeds,
    // so the final envelope is the live windows.
    //
    // The test pins every leg of the orchestration so a future
    // refactor that drops the reactive retry — or that forgets to
    // suppress the retry when pre-emptive fires — fails loudly.
    clear_usage_cache();
    let temp = make_opencode_home("s4");

    let refresh_count = Arc::new(AtomicUsize::new(0));
    let live_count = Arc::new(AtomicUsize::new(0));
    let refresh_count_t = refresh_count.clone();
    let live_count_t = live_count.clone();

    // 3 requests total: 1 live (401) + 1 refresh + 1 live (200).
    let port = spawn_loopback(3, move |req| match req.url() {
        "/auth/device/token" => {
            refresh_count_t.fetch_add(1, Ordering::SeqCst);
            let _ = req.respond(tiny_http::Response::from_string(REFRESH_BODY_OK));
        }
        "/_server" => {
            // First call: 401. Second call: real windows.
            let prior = live_count_t.fetch_add(1, Ordering::SeqCst);
            if prior == 0 {
                let _ = req.respond(
                    tiny_http::Response::from_string(r#"{"error":"unauthorized"}"#)
                        .with_status_code(401),
                );
            } else {
                let _ = req.respond(tiny_http::Response::from_string(LIVE_BODY_OK));
            }
        }
        _ => {
            let _ =
                req.respond(tiny_http::Response::from_string("not found").with_status_code(404));
        }
    });
    let live_url = format!("http://127.0.0.1:{port}/_server");
    let refresh_url = format!("http://127.0.0.1:{port}/auth/device/token");

    let cred = build_cred(
        "fresh_tok",
        "rt_fresh",
        "2099-01-01T00:00:00Z",
        "wrk_q",
        "srv_v1",
    );
    let usage = opencode_usage_impl_with_hosts(&temp, &live_url, &refresh_url, Some(&cred));

    assert_eq!(
        refresh_count.load(Ordering::SeqCst),
        1,
        "reactive refresh fires ONCE after the live 401"
    );
    assert_eq!(
        live_count.load(Ordering::SeqCst),
        2,
        "live probe is called twice: first 401, then 200 after refresh"
    );
    // Three windows from the retry's success body — proves the
    // second live call's token was accepted (the seeded SQLite
    // fallback would have been 50%/...).
    match usage {
        UsageOutcome::Reading { windows, .. } => {
            assert_eq!(windows.len(), 3);
            assert_eq!(windows[0].label, "5-hour");
            assert_eq!(windows[0].used_percent, Some(25.0));
        }
        other => panic!("expected live Reading outcome, got: {other:?}"),
    }

    let _ = fs::remove_dir_all(&temp);
}

/// The rendered meter output is pinned (issue #2528).
///
/// The `billing.get` reply and the exact wire triple it must project to
/// are both fixtures: any change to opencode's window labels,
/// percentages or reset times shows up here as a byte difference rather
/// than as a quietly different meter.
#[test]
fn opencode_rendered_output_matches_the_pinned_fixture() {
    const RESPONSE: &str = include_str!("fixtures/opencode-billing-windows.json");
    const RENDERED: &str = include_str!("fixtures/opencode-billing-windows-rendered.json");

    let (windows, detail) = parse_opencode_billing_response(RESPONSE).unwrap();
    let usage = UsageReading {
        windows,
        balance: None,
        meters: Vec::new(),
        detail,
    }
    .into_outcome()
    .into_usage("opencode");
    assert_eq!(
        serde_json::to_string(&usage).unwrap(),
        // The fixture file ends with a newline; the payload does not.
        RENDERED.trim_end(),
        "opencode's rendered meter output must stay byte-identical"
    );
}
