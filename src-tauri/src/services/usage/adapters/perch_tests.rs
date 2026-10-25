// Sibling test suite for the Perch usage adapter. Confirmed against
// perchai-cli 2.4.116 row math (`nyt`, `wGo`, `tyt`, `Xle`, `TPe`).
// These tests pass an explicit directory and never read the real
// `~/.perch` session.

use super::*;
use crate::services::usage::adapter::spawn_loopback;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

fn location(dir: &Path) -> AuthLocation {
    AuthLocation {
        dir: dir.to_path_buf(),
        keychain: false,
    }
}

fn sample_session(expires_at: Option<i64>, access: &str, refresh: &str) -> Session {
    Session {
        app_url: "https://app.perchai.app".into(),
        access_token: access.into(),
        refresh_token: Some(refresh.into()),
        expires_at,
        user_id: Some("user-1".into()),
        email: Some("hidden@example.com".into()),
        updated_at: "2026-10-24T00:00:00Z".into(),
    }
}

fn write_auth(dir: &Path, app_url: &str, access: &str, refresh: &str, expires_at: Option<i64>) {
    let body = serde_json::json!({
        "version": 1,
        "appUrl": app_url,
        "accessToken": access,
        "refreshToken": refresh,
        "expiresAt": expires_at,
        "userId": "user-1",
        "email": "hidden@example.com",
        "updatedAt": "2026-10-24T00:00:00Z",
    });
    std::fs::write(dir.join("cli-auth-session.json"), body.to_string()).unwrap();
}

fn reading(outcome: UsageOutcome) -> (Vec<UsageWindow>, Vec<UsageMeter>, Option<String>) {
    match outcome {
        UsageOutcome::Reading {
            windows,
            meters,
            detail,
            balance,
        } => {
            assert!(balance.is_none());
            (windows, meters, detail)
        }
        other => panic!("expected a reading, got {other:?}"),
    }
}

fn close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-6,
        "expected {expected}, got {actual}"
    );
}

#[test]
fn access_token_usable_treats_the_skew_boundary_as_expired() {
    let now = 1_700_000_000;
    let at_boundary = sample_session(Some(now + 90), "access", "refresh");
    let just_after = sample_session(Some(now + 91), "access", "refresh");
    let open = sample_session(None, "access", "refresh");
    assert!(!access_token_usable(&at_boundary, now));
    assert!(access_token_usable(&just_after, now));
    assert!(access_token_usable(&open, now));
}

#[test]
fn starter_monthly_uses_ceil_points_and_rolling_uses_rounded_points() {
    // dailyPt must not become the 5-hour row. Rolling spend is TPe of the
    // window USD. Monthly spend is always Xle of monthlyUsd. The monthly
    // row has no invented reset.
    let body = r#"{
        "ok": true,
        "session": {"planCode": "starter", "membershipRole": "member", "entitlements": []},
        "usageMeter": {
            "dailyPt": 9999,
            "dailyUsd": 0.25,
            "weeklyPt": null,
            "weeklyUsd": 0.5,
            "monthlyUsd": 1.5,
            "roostRolling": {
                "enabled": true,
                "window5hUsd": 0.5,
                "window5hCapUsd": 5,
                "window5hNextFreedAt": "not-a-date",
                "window7dUsd": 2.5,
                "window7dCapUsd": 20,
                "window7dNextFreedAt": "2026-11-01T00:00:00Z"
            }
        }
    }"#;
    let (windows, meters, detail) = reading(outcome_from_account(body));
    assert_eq!(detail.as_deref(), Some("Starter"));
    assert_eq!(windows.len(), 3);
    assert_eq!(windows[0].label, "Monthly");
    close(windows[0].used_percent.unwrap(), 7.5);
    assert_eq!(windows[0].resets_at, None);
    assert_eq!(windows[1].label, "5-hour");
    close(windows[1].used_percent.unwrap(), 10.0);
    assert_eq!(windows[1].resets_at, None);
    assert_eq!(windows[2].label, "Weekly");
    close(windows[2].used_percent.unwrap(), 12.5);
    assert_eq!(
        windows[2].resets_at.as_deref(),
        Some("2026-11-01T00:00:00Z")
    );

    // A capped row is a percentage window, not also an explicit meter: the
    // "Amount used / Limit / Remaining" block would only repeat the bar.
    // The percentages above pin the points each bar is computed from
    // (1,500 of 20,000 PT; 500 of 5,000 PT; 2,500 of 20,000 PT).
    assert!(meters.is_empty());
}

#[test]
fn a_paid_subscription_supplies_the_monthly_reset_date() {
    // Free plans send `billingSubscription: null`, so the Monthly row keeps
    // no reset. A paid plan's subscription record carries the period end.
    let free = r#"{
        "ok": true,
        "session": {"planCode": "starter"},
        "usageMeter": {"monthlyUsd": 1.5},
        "billingSubscription": null
    }"#;
    let (windows, _, _) = reading(outcome_from_account(free));
    assert_eq!(windows[0].label, "Monthly");
    assert_eq!(windows[0].resets_at, None);

    let epoch = r#"{
        "ok": true,
        "session": {"planCode": "starter"},
        "usageMeter": {"monthlyUsd": 1.5},
        "billingSubscription": {"status": "active", "current_period_end": 1793491200}
    }"#;
    let (windows, _, _) = reading(outcome_from_account(epoch));
    assert_eq!(
        windows[0].resets_at.as_deref(),
        Some("2026-11-01T00:00:00+00:00")
    );

    let iso = r#"{
        "ok": true,
        "session": {"planCode": "starter"},
        "usageMeter": {"monthlyUsd": 1.5},
        "billingSubscription": {"currentPeriodEnd": "2026-12-01T00:00:00Z"}
    }"#;
    let (windows, _, _) = reading(outcome_from_account(iso));
    assert_eq!(
        windows[0].resets_at.as_deref(),
        Some("2026-12-01T00:00:00Z")
    );

    // A shape we do not recognise yields no reset rather than a wrong one.
    let unknown = r#"{
        "ok": true,
        "session": {"planCode": "starter"},
        "usageMeter": {"monthlyUsd": 1.5},
        "billingSubscription": {"status": "active", "invoiceDue": "2026-11-01T00:00:00Z"}
    }"#;
    let (windows, _, _) = reading(outcome_from_account(unknown));
    assert_eq!(windows[0].resets_at, None);
}

#[test]
fn pro_roost_is_a_note_and_the_manual_allowance_is_a_window() {
    let body = r#"{
        "ok": true,
        "session": {"planCode": "pro", "planName": "Pro", "membershipRole": "member"},
        "creditBalancePt": 42.7,
        "billingSubscription": {"status": "active", "current_period_end": 1793491200},
        "usageMeter": {
            "monthlyUsd": 10,
            "monthlyManualPt": 1000,
            "roostRolling": {
                "enabled": true,
                "window5hUsd": 0.5,
                "window5hCapUsd": 5,
                "window5hNextFreedAt": "2026-10-24T12:00:00Z",
                "window7dUsd": 2.5,
                "window7dCapUsd": 20,
                "window7dNextFreedAt": "2026-10-31T00:00:00Z"
            },
            "manualRolling": {
                "enabled": true,
                "window5hUsd": 0.25,
                "window5hCapUsd": 15,
                "window5hNextFreedAt": "2026-10-24T18:00:00Z",
                "window7dUsd": 3,
                "window7dCapUsd": 37.5,
                "window7dNextFreedAt": "2026-10-30T00:00:00Z"
            }
        }
    }"#;
    let (windows, meters, detail) = reading(outcome_from_account(body));
    assert_eq!(
        detail.as_deref(),
        Some("Pro · Roost fair use · 42 PT credits")
    );
    assert_eq!(
        windows
            .iter()
            .map(|window| window.label.as_str())
            .collect::<Vec<_>>(),
        vec![
            "5-hour",
            "Weekly",
            "Manual",
            "Manual 5-hour",
            "Manual weekly"
        ]
    );
    assert!(windows.iter().all(|window| window.used_percent.is_some()));
    // The manual allowance follows the subscription's billing period, so it
    // carries that period's end as its reset.
    assert_eq!(
        windows[2].resets_at.as_deref(),
        Some("2026-11-01T00:00:00+00:00")
    );
    close(windows[2].used_percent.unwrap(), 1.333_333_333_333_333_3);
    close(windows[3].used_percent.unwrap(), 1.666_666_666_666_666_7);
    close(windows[4].used_percent.unwrap(), 8.0);
    assert!(meters.is_empty());
}

#[test]
fn internal_and_admin_accounts_report_spend_without_a_cap() {
    let internal =
        r#"{"ok":true,"session":{"planCode":"internal"},"usageMeter":{"monthlyUsd":2.5}}"#;
    let (windows, meters, detail) = reading(outcome_from_account(internal));
    assert!(windows.is_empty());
    assert_eq!(detail.as_deref(), Some("Internal"));
    match &meters[0] {
        UsageMeter::NoIndividualLimit { amount } => {
            assert_eq!(amount.used, 2500.0);
            assert_eq!(amount.limit, None);
            assert_eq!(amount.used_percent, None);
            assert_eq!(amount.unit, "PT");
        }
        other => panic!("expected no individual limit, got {other:?}"),
    }

    let admin = r#"{"ok":true,"session":{"planCode":"starter","membershipRole":"admin"},"usageMeter":{"monthlyUsd":1.5}}"#;
    let (windows, meters, _) = reading(outcome_from_account(admin));
    assert!(windows.is_empty());
    match &meters[0] {
        UsageMeter::NoIndividualLimit { amount } => assert_eq!(amount.used, 1500.0),
        other => panic!("admin starter is still unlimited, got {other:?}"),
    }
}

#[test]
fn entitlement_limits_override_the_plan_fallback() {
    let body = r#"{
        "ok": true,
        "session": {
            "planCode": "starter",
            "entitlements": [
                {"key": "usage.monthly_pt", "value_json": {"limit": 5000}},
                {"key": "usage.manual_monthly_pt", "value_json": {"limit": null}}
            ]
        },
        "usageMeter": {"monthlyUsd": 1.5}
    }"#;
    let (windows, meters, _) = reading(outcome_from_account(body));
    assert!(meters.is_empty());
    // 1,500 PT of the entitlement's 5,000 PT.
    close(windows[0].used_percent.unwrap(), 30.0);

    let pro = r#"{
        "ok": true,
        "session": {
            "planCode": "pro",
            "entitlements": [
                {"key": "usage.manual_monthly_pt", "value_json": {"limit": 1000}}
            ]
        },
        "usageMeter": {"monthlyUsd": 1, "monthlyManualPt": 250}
    }"#;
    let (windows, meters, _) = reading(outcome_from_account(pro));
    assert!(meters.is_empty());
    let manual = windows
        .iter()
        .find(|window| window.label == "Manual")
        .expect("the manual allowance is a window");
    // 250 PT of the entitlement's 1,000 PT.
    close(manual.used_percent.unwrap(), 25.0);
}

#[test]
fn snake_case_account_fields_parse() {
    let body = r#"{
        "ok": true,
        "session": {"plan_code": "pilot", "plan_name": "pilot"},
        "credit_balance_pt": 8,
        "usage_meter": {
            "monthly_usd": 0.5,
            "monthly_manual_pt": 10,
            "roost_rolling": {
                "enabled": true,
                "window_5h_usd": 0.5,
                "window_5h_cap_usd": 5,
                "window_5h_next_freed_at": "2026-10-24T12:00:00Z",
                "window_7d_usd": 0,
                "window_7d_cap_usd": 0
            }
        }
    }"#;
    let (windows, meters, detail) = reading(outcome_from_account(body));
    assert_eq!(detail.as_deref(), Some("Starter · 8 PT credits"));
    assert_eq!(windows.len(), 2);
    assert!(meters.is_empty());
    // 500 PT of 20,000 PT, then 500 PT of 5,000 PT.
    close(windows[0].used_percent.unwrap(), 2.5);
    close(windows[1].used_percent.unwrap(), 10.0);
    assert_eq!(
        windows[1].resets_at.as_deref(),
        Some("2026-10-24T12:00:00Z")
    );
}

#[test]
fn unknown_plan_with_spend_and_no_limit_has_no_percent_window() {
    let body = r#"{"ok":true,"session":{"planCode":"team"},"usageMeter":{"monthlyUsd":1.5}}"#;
    let (windows, meters, detail) = reading(outcome_from_account(body));
    assert!(windows.is_empty());
    assert_eq!(detail.as_deref(), Some("Team"));
    match &meters[0] {
        UsageMeter::NoIndividualLimit { amount } => assert_eq!(amount.used, 1500.0),
        other => panic!("expected uncapped spend, got {other:?}"),
    }
}

#[test]
fn byo_lane_is_managed_externally() {
    let body = r#"{"ok":true,"session":{"planCode":"pro","billingLane":"byo"},"usageMeter":{"monthlyUsd":10}}"#;
    match outcome_from_account(body) {
        UsageOutcome::ManagedExternally { platform } => {
            assert_eq!(platform, "your own API key");
        }
        other => panic!("expected managed externally, got {other:?}"),
    }
}

#[test]
fn malformed_or_empty_account_is_unavailable() {
    match outcome_from_account("not-json") {
        UsageOutcome::Unavailable { reason } => {
            assert_eq!(reason, "Perch returned an unreadable account.");
        }
        other => panic!("expected unavailable, got {other:?}"),
    }
    match outcome_from_account(r#"{"ok":false,"error":"nope"}"#) {
        UsageOutcome::Unavailable { reason } => {
            assert_eq!(reason, "Perch did not return an account.");
            assert!(!reason.contains("nope"));
        }
        other => panic!("expected unavailable, got {other:?}"),
    }
}

#[test]
fn missing_session_file_is_not_signed_in() {
    let dir = tempfile::tempdir().unwrap();
    match fetch_at(&location(dir.path())) {
        UsageOutcome::NoCredential { hint } => {
            assert_eq!(hint, "Perch is not signed in. Run perch login.");
        }
        other => panic!("expected no credential, got {other:?}"),
    }
}

#[test]
fn fingerprints_follow_the_user_id_and_ignore_a_missing_file() {
    let signed_out = tempfile::tempdir().unwrap();
    let also_signed_out = tempfile::tempdir().unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    write_auth(
        first.path(),
        "https://app.perchai.app",
        "access-a",
        "refresh",
        Some(9_000_000_000),
    );
    write_auth(
        second.path(),
        "https://app.perchai.app",
        "access-b",
        "refresh",
        Some(9_000_000_000),
    );
    assert!(
        fingerprint_at(&location(signed_out.path()))
            == fingerprint_at(&location(also_signed_out.path()))
    );
    assert!(fingerprint_at(&location(first.path())) == fingerprint_at(&location(second.path())));
}

struct Seen {
    path: String,
    auth: String,
    body: String,
}

fn serve(
    max_requests: usize,
    first_account_status: u16,
    token_status: u16,
    token_body: &'static str,
) -> (u16, Arc<Mutex<Vec<Seen>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_thread = seen.clone();
    let bound_port = Arc::new(Mutex::new(0u16));
    let port_for_handler = bound_port.clone();
    let account_hits = Arc::new(AtomicUsize::new(0));
    let port = spawn_loopback(max_requests, move |mut request| {
        let path = request.url().to_string();
        let auth = request
            .headers()
            .iter()
            .find(|header| header.field.equiv("Authorization"))
            .map(|header| header.value.as_str().to_string())
            .unwrap_or_default();
        let mut body = String::new();
        let _ = request.as_reader().read_to_string(&mut body);
        seen_thread.lock().unwrap().push(Seen {
            path: path.clone(),
            auth,
            body: body.clone(),
        });
        let (status, response) = if path.contains("/api/perchai/account") {
            let hit = account_hits.fetch_add(1, Ordering::SeqCst);
            let status = if hit == 0 { first_account_status } else { 200 };
            let response = if status == 200 {
                r#"{"ok":true,"session":{"planCode":"starter"},"usageMeter":{"monthlyUsd":1.5}}"#
            } else {
                r#"{"ok":false,"error":"not_authenticated"}"#
            };
            (status, response.to_string())
        } else if path.contains("/cli-auth/config") {
            let port = *port_for_handler.lock().unwrap();
            (
                200,
                format!(
                    r#"{{"ok":true,"supabaseUrl":"http://127.0.0.1:{port}","supabaseAnonKey":"anon-test"}}"#
                ),
            )
        } else if path.contains("/auth/v1/token") {
            (token_status, token_body.to_string())
        } else {
            (404, "missing".to_string())
        };
        let _ =
            request.respond(tiny_http::Response::from_string(response).with_status_code(status));
    });
    *bound_port.lock().unwrap() = port;
    (port, seen)
}

fn paths(seen: &Mutex<Vec<Seen>>) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .map(|hit| hit.path.clone())
        .collect()
}

const FRESH_TOKEN: &str = r#"{"access_token":"access-new","refresh_token":"refresh-new","expires_at":9000000000,"user":{"id":"user-9","email":"hidden@example.com"}}"#;

#[test]
fn valid_token_reads_the_account_without_refreshing() {
    let dir = tempfile::tempdir().unwrap();
    let (port, seen) = serve(4, 200, 200, FRESH_TOKEN);
    let app = format!("http://127.0.0.1:{port}");
    write_auth(
        dir.path(),
        &app,
        "access-live",
        "refresh-old",
        Some(9_000_000_000),
    );
    let (windows, _, detail) = reading(fetch_at(&location(dir.path())));
    assert_eq!(detail.as_deref(), Some("Starter"));
    close(windows[0].used_percent.unwrap(), 7.5);
    let hits = seen.lock().unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].path.contains("/api/perchai/account"));
    assert_eq!(hits[0].auth, "Bearer access-live");
}

#[test]
fn expired_token_refreshes_persists_rotation_and_clears_a_stale_lock() {
    let dir = tempfile::tempdir().unwrap();
    let lock = dir.path().join("cli-auth-refresh.lock");
    std::fs::write(&lock, r#"{"pid":1,"createdAt":0}"#).unwrap();
    let stale = SystemTime::now() - Duration::from_secs(120);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&lock)
        .unwrap()
        .set_modified(stale)
        .unwrap();
    let (port, seen) = serve(6, 200, 200, FRESH_TOKEN);
    let app = format!("http://127.0.0.1:{port}");
    write_auth(dir.path(), &app, "access-old", "refresh-old", Some(1));
    let _ = reading(fetch_at(&location(dir.path())));
    let hits = paths(&seen);
    assert_eq!(hits.len(), 3, "{hits:?}");
    assert!(hits[0].contains("/cli-auth/config"));
    assert!(hits[1].contains("/auth/v1/token"));
    assert!(hits[2].contains("/api/perchai/account"));
    assert_eq!(seen.lock().unwrap()[1].auth, "Bearer anon-test");
    assert!(seen.lock().unwrap()[1].body.contains("refresh-old"));
    assert_eq!(seen.lock().unwrap()[2].auth, "Bearer access-new");
    let saved: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("cli-auth-session.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["accessToken"], "access-new");
    assert_eq!(saved["refreshToken"], "refresh-new");
    assert_eq!(saved["userId"], "user-9");
    assert_eq!(saved["version"], 1);
    assert!(!dir.path().join("cli-auth-refresh.lock").exists());
    assert!(!dir.path().join("cli-auth-refresh.invalid.json").exists());
}

#[test]
fn account_unauthorized_refreshes_once() {
    let dir = tempfile::tempdir().unwrap();
    let (port, seen) = serve(6, 401, 200, FRESH_TOKEN);
    let app = format!("http://127.0.0.1:{port}");
    write_auth(
        dir.path(),
        &app,
        "access-stale",
        "refresh-old",
        Some(9_000_000_000),
    );
    let (windows, _, _) = reading(fetch_at(&location(dir.path())));
    close(windows[0].used_percent.unwrap(), 7.5);
    let hits = paths(&seen);
    assert_eq!(hits.len(), 4, "{hits:?}");
    assert!(hits[0].contains("/api/perchai/account"));
    assert!(hits[1].contains("/cli-auth/config"));
    assert!(hits[2].contains("/auth/v1/token"));
    assert!(hits[3].contains("/api/perchai/account"));
    assert_eq!(seen.lock().unwrap()[0].auth, "Bearer access-stale");
    assert_eq!(seen.lock().unwrap()[3].auth, "Bearer access-new");
}

#[test]
fn invalid_grant_keeps_the_session_and_does_not_refresh_again() {
    let dir = tempfile::tempdir().unwrap();
    let (port, seen) = serve(
        6,
        200,
        400,
        r#"{"error":"invalid_grant","error_description":"Invalid Refresh Token"}"#,
    );
    let app = format!("http://127.0.0.1:{port}");
    write_auth(dir.path(), &app, "access-old", "refresh-old", Some(1));
    match fetch_at(&location(dir.path())) {
        UsageOutcome::NoCredential { hint } => {
            assert_eq!(hint, "Perch login expired. Run perch login.");
            assert!(!hint.contains("hidden"));
        }
        other => panic!("expected expired login, got {other:?}"),
    }
    let saved = std::fs::read_to_string(dir.path().join("cli-auth-session.json")).unwrap();
    assert!(saved.contains("refresh-old"));
    assert!(saved.contains("access-old"));
    let marker: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("cli-auth-refresh.invalid.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(marker["tokenHash"], token_hash("refresh-old"));
    assert!(marker["expiresAt"].as_i64().unwrap() > chrono::Utc::now().timestamp_millis());
    assert_eq!(paths(&seen).len(), 2);
    match fetch_at(&location(dir.path())) {
        UsageOutcome::NoCredential { hint } => {
            assert_eq!(hint, "Perch login expired. Run perch login.");
        }
        other => panic!("second read should stay signed out, got {other:?}"),
    }
    assert_eq!(paths(&seen).len(), 2);
}
