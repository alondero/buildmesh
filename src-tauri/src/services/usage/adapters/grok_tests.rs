// Sibling test suite for the owning module. The suite lives in its own
// file so the production module keeps only the declaration below — the
// repo's rust-test-layout gate requires it.

use super::*;
use crate::services::usage::adapter::spawn_loopback;
// ─── Grok ─────────────────────────────────────────────────────────────

#[test]
fn parse_grok_response_unified_billing_valid() {
    let json = r#"{
        "config": {
            "currentPeriod": {
                "type": "USAGE_PERIOD_TYPE_WEEKLY",
                "start": "2026-07-15T00:00:00+00:00",
                "end": "2026-07-22T00:00:00+00:00"
            },
            "onDemandCap": { "val": 10.0 },
            "onDemandUsed": { "val": 2.5 },
            "isUnifiedBillingUser": true,
            "prepaidBalance": { "val": 0.0 },
            "billingPeriodEnd": "2026-07-22T00:00:00+00:00"
        }
    }"#;
    let (windows, balance) = parse_grok_response(json).unwrap();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].label, "Weekly Pool");
    assert_eq!(windows[0].used_percent, Some(25.0));
    assert_eq!(
        windows[0].resets_at.as_deref(),
        Some("2026-07-22T00:00:00+00:00")
    );
    assert!(balance.is_none());
}

#[test]
fn parse_grok_response_supergrok_token_plan_uses_credit_percentage() {
    let json = r#"{
        "config": {
            "currentPeriod": {
                "type": "USAGE_PERIOD_TYPE_WEEKLY",
                "start": "2026-08-24T00:00:00+00:00",
                "end": "2026-08-31T00:00:00+00:00"
            },
            "creditUsagePercent": 37.0,
            "onDemandCap": { "val": 0.0 },
            "onDemandUsed": { "val": 0.0 },
            "productUsage": [
                { "product": "grok-build", "usagePercent": 37.0 }
            ],
            "isUnifiedBillingUser": true,
            "prepaidBalance": { "val": 0.0 },
            "billingPeriodEnd": "2026-08-31T00:00:00+00:00"
        }
    }"#;
    let (windows, _) = parse_grok_response(json).unwrap();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].label, "Weekly Pool");
    assert_eq!(windows[0].used_percent, Some(37.0));
}

#[test]
fn parse_grok_response_monthly_limit_valid() {
    let json = r#"{
        "config": {
            "monthlyLimit": { "val": 50.0 },
            "used": { "val": 10.0 },
            "isUnifiedBillingUser": false,
            "billingPeriodEnd": "2026-08-01T00:00:00+00:00"
        }
    }"#;
    let (windows, _) = parse_grok_response(json).unwrap();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].label, "Monthly Limit");
    assert_eq!(windows[0].used_percent, Some(20.0));
    assert_eq!(
        windows[0].resets_at.as_deref(),
        Some("2026-08-01T00:00:00+00:00")
    );
}

#[test]
fn parse_grok_response_prepaid_balance() {
    let json = r#"{
        "config": {
            "prepaidBalance": { "val": 15.75 },
            "onDemandUsed": { "val": 4.25 }
        }
    }"#;
    let (_, balance) = parse_grok_response(json).unwrap();
    let balance = balance.expect("prepaid balance present");
    assert_eq!(balance.remaining, 15.75);
    assert_eq!(balance.monthly_spend, Some(4.25));
    assert_eq!(balance.currency, "USD");
}

#[test]
fn grok_usage_with_empty_key_returns_logged_out() {
    // Force an empty path to trigger logged_out path
    let usage = read_grok_token(PathBuf::from(""))
        .map(|(t, _u)| t)
        .unwrap_or_else(|e| e.to_string());
    assert!(usage.contains("No credential found"));
}

/// Helper: write a `~/.grok/auth.json`-shaped credential file into a
/// tempdir so `grok_usage_with` tests stay hermetic.
fn write_grok_auth(dir: &tempfile::TempDir) -> PathBuf {
    let path = dir.path().join("auth.json");
    fs::write(
        &path,
        r#"{"https://auth.x.ai::test": {"key": "sk-grok-test", "user_id": "user-1"}}"#,
    )
    .unwrap();
    path
}

#[test]
fn adapter_seam_contract_grok_no_credential_without_network() {
    // A missing credential file must report `NoCredential` without
    // touching the network — the unreachable base URL below would
    // fail loudly if hit. (`dispatch("grok").fetch` is not hermetic
    // here: it reads the real `~/.grok/auth.json`.)
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("auth.json");
    match grok_usage_with(&missing, "http://127.0.0.1:1") {
        UsageOutcome::NoCredential { .. } => {}
        other => panic!("expected NoCredential outcome, got: {other:?}"),
    }
}

#[test]
fn grok_usage_with_401_returns_rejected_with_invalid_key_hint() {
    let dir = tempfile::tempdir().unwrap();
    let auth_path = write_grok_auth(&dir);
    let port = spawn_loopback(1, |req| {
        let _ = req.respond(tiny_http::Response::empty(401));
    });
    match grok_usage_with(&auth_path, &format!("http://127.0.0.1:{port}")) {
        UsageOutcome::Rejected { hint } => {
            assert_eq!(hint, "Invalid API key");
        }
        other => panic!("expected Rejected outcome, got: {other:?}"),
    }
}

#[test]
fn grok_usage_with_429_returns_rate_limited() {
    let dir = tempfile::tempdir().unwrap();
    let auth_path = write_grok_auth(&dir);
    let port = spawn_loopback(1, |req| {
        let _ = req.respond(tiny_http::Response::empty(429));
    });
    match grok_usage_with(&auth_path, &format!("http://127.0.0.1:{port}")) {
        UsageOutcome::RateLimited { reason } => {
            assert!(
                reason.contains("Rate limited"),
                "rate-limit copy must be preserved, got: {reason:?}"
            );
        }
        other => panic!("expected RateLimited outcome, got: {other:?}"),
    }
}

#[test]
fn grok_usage_with_transport_failure_returns_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let auth_path = write_grok_auth(&dir);
    match grok_usage_with(&auth_path, "http://127.0.0.1:1") {
        UsageOutcome::Unavailable { reason } => {
            assert!(
                reason.contains("Request failed"),
                "transport envelope must be preserved, got: {reason:?}"
            );
        }
        other => panic!("expected Unavailable outcome, got: {other:?}"),
    }
}

#[test]
fn grok_usage_with_live_loopback_returns_reading() {
    let dir = tempfile::tempdir().unwrap();
    let auth_path = write_grok_auth(&dir);
    let port = spawn_loopback(1, |req| {
        let _ = req.respond(tiny_http::Response::from_string(
            r#"{
                "config": {
                    "currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY"},
                    "creditUsagePercent": 37.0,
                    "isUnifiedBillingUser": true,
                    "billingPeriodEnd": "2026-08-31T00:00:00+00:00"
                }
            }"#,
        ));
    });
    match grok_usage_with(&auth_path, &format!("http://127.0.0.1:{port}")) {
        UsageOutcome::Reading { windows, .. } => {
            assert_eq!(windows.len(), 1);
            assert_eq!(windows[0].label, "Weekly Pool");
            assert_eq!(windows[0].used_percent, Some(37.0));
        }
        other => panic!("expected Reading outcome, got: {other:?}"),
    }
}

#[test]
fn grok_usage_with_malformed_body_returns_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let auth_path = write_grok_auth(&dir);
    let port = spawn_loopback(1, |req| {
        let _ = req.respond(tiny_http::Response::from_string("not-json"));
    });
    match grok_usage_with(&auth_path, &format!("http://127.0.0.1:{port}")) {
        UsageOutcome::Unavailable { reason } => {
            assert!(
                reason.contains("Failed to parse response"),
                "parse envelope must be preserved, got: {reason:?}"
            );
        }
        other => panic!("expected Unavailable outcome, got: {other:?}"),
    }
}

/// The rendered meter output is pinned (issue #2528).
///
/// The provider's response and the exact wire triple it must project to
/// are both fixtures: a change to the windows, percentages, reset times or
/// balance grok's parse produces shows up here as a byte difference rather
/// than as a quietly different meter.
#[test]
fn grok_rendered_output_matches_the_pinned_fixture() {
    const RESPONSE: &str = include_str!("fixtures/grok-unified-billing.json");
    const RENDERED: &str = include_str!("fixtures/grok-unified-billing-rendered.json");

    let (windows, balance) = parse_grok_response(RESPONSE).unwrap();
    let usage = UsageReading {
        windows,
        balance,
        meters: Vec::new(),
        detail: None,
    }
    .into_outcome()
    .into_usage("grok");
    assert_eq!(
        serde_json::to_string(&usage).unwrap(),
        // The fixture file ends with a newline; the payload does not.
        RENDERED.trim_end(),
        "grok's rendered meter output must stay byte-identical"
    );
}
