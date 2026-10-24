// Sibling test suite for the owning module. The suite lives in its own
// file so the production module keeps only the declaration below — the
// repo's rust-test-layout gate requires it.

use super::*;
use crate::services::usage::adapter::spawn_loopback;
use std::sync::Arc;

/// Unwraps the reading a commandcode outcome carries.
///
/// Command Code runs a second, best-effort read — the subscription
/// lookup — so its outcome is an *enriched* reading. The values are the
/// same either way; only the wrapper differs.
fn reading_of(
    outcome: UsageOutcome,
    context: &str,
) -> (Vec<UsageWindow>, Option<BillingBalance>, Option<String>) {
    match outcome {
        UsageOutcome::Reading {
            windows,
            balance,
            detail,
            ..
        } => (windows, balance, detail),
        UsageOutcome::Enriched { reading } => reading_of(*reading, context),
        other => panic!("{context}expected Reading outcome, got: {other:?}"),
    }
}

#[test]
fn commandcode_usage_reads_auth_file_and_maps_credit_windows_and_balance() {
    let auth_dir = tempfile::tempdir().unwrap();
    let auth_path = auth_dir.path().join("auth.json");
    fs::write(
        &auth_path,
        r#"{"apiKey":"","access_token":"  cmd_live_test \n"}"#,
    )
    .unwrap();

    let observed = Arc::new(std::sync::Mutex::new(String::new()));
    let observed_t = observed.clone();
    let port = spawn_loopback(1, move |req| {
        let auth = req
            .headers()
            .iter()
            .find(|h| h.field.equiv("Authorization"))
            .map(|h| h.value.as_str().to_string())
            .unwrap_or_default();
        *observed_t.lock().unwrap() = auth;
        let _ = req.respond(tiny_http::Response::from_string(
            r#"{"windows":[{"label":"5-hour","used_percent":25,"resets_at":"2026-01-01T00:00:00+00:00"},{"label":"Weekly","used_percent":20,"resets_at":"2026-01-08T00:00:00+00:00"}],"monthly_credits":10,"extra_credits":5}"#,
        ));
    });

    let outcome = commandcode_usage_with_path(
        &auth_path,
        &format!("http://127.0.0.1:{port}/alpha/billing/credits"),
    );

    let (windows, balance, _detail) = reading_of(outcome, "");
    assert_eq!(*observed.lock().unwrap(), "Bearer cmd_live_test");
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].label, "5-hour");
    assert_eq!(windows[0].used_percent, Some(25.0));
    assert_eq!(
        windows[0].resets_at.as_deref(),
        Some("2026-01-01T00:00:00+00:00")
    );
    assert_eq!(windows[1].label, "Weekly");
    assert_eq!(windows[1].used_percent, Some(20.0));
    let balance = balance.expect("credits must surface as a balance");
    assert_eq!(balance.remaining, 15.0);
    assert_eq!(balance.monthly_spend, None);
    assert_eq!(balance.currency, "USD");
}

#[test]
fn parse_commandcode_nested_response_maps_observed_api_shape() {
    let body = r#"{
        "windowLimits": {
            "fiveHour": {"cap": 20, "used": 5, "resetAt": 1767225600000},
            "weekly": {"cap": 100, "used": 20, "resetAt": 1767830400000}
        },
        "credits": {"monthlyCredits": 10, "purchasedCredits": 3, "freeCredits": 2}
    }"#;

    let (windows, credits) = parse_commandcode_credits_response(body).unwrap();
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].label, "5-hour");
    assert_eq!(windows[0].used_percent, Some(25.0));
    assert_eq!(
        windows[0].resets_at.as_deref(),
        Some("2026-01-01T00:00:00+00:00")
    );
    assert_eq!(windows[1].label, "Weekly");
    assert_eq!(windows[1].used_percent, Some(20.0));
    assert_eq!(credits.balance().remaining, 15.0);
}

#[test]
fn parse_commandcode_reported_response_accepts_camel_case_aliases() {
    let body = r#"{
        "windows": [
            {"label": "5-hour", "usedPercent": 12.5, "resetsAt": "2026-01-01T00:00:00Z"}
        ],
        "monthlyCredits": 8,
        "extraCredits": 2
    }"#;

    let (windows, credits) = parse_commandcode_credits_response(body).unwrap();
    assert_eq!(windows[0].used_percent, Some(12.5));
    assert_eq!(
        windows[0].resets_at.as_deref(),
        Some("2026-01-01T00:00:00Z")
    );
    assert_eq!(credits.balance().remaining, 10.0);
}

#[test]
fn commandcode_monthly_matches_studio_tiers_grants_and_seats() {
    for (plan, allowance) in [
        ("individual-go", 10.0),
        ("individual-goat", 70.0),
        ("individual-pro", 30.0),
        ("individual-pro-v1", 80.0),
        ("individual-provider", 15.0),
        ("individual-max", 150.0),
        ("individual-ultra", 300.0),
        ("teams-pro", 40.0),
    ] {
        let credits: CommandCodeCredits = serde_json::from_value(serde_json::json!({
            "monthlyCredits": allowance / 2.0, "purchasedCredits": 100, "freeCredits": 50
        }))
        .unwrap();
        let subscription = serde_json::from_value(serde_json::json!({
            "success": true, "data": {"planId": plan, "status": "active",
            "currentPeriodEnd": "2026-09-30T13:13:19.000Z"}
        }))
        .unwrap();
        let monthly = commandcode_monthly_window(&credits, subscription).unwrap();
        assert_eq!(monthly.used_percent, Some(50.0), "{plan}");
        assert_eq!(
            monthly.resets_at.as_deref(),
            Some("2026-09-30T13:13:19+00:00")
        );
    }
    for (plan, grant, seats, remaining, expected) in [
        ("individual-goat", serde_json::json!(100), 1.0, 25.0, 75.0),
        ("individual-goat", serde_json::json!(20), 1.0, 35.0, 50.0),
        ("individual-go", serde_json::json!(10.25), 1.0, 5.125, 50.0),
        ("teams-pro", serde_json::Value::Null, 3.9, 60.0, 50.0),
        ("teams-pro", serde_json::json!(80), 3.0, 60.0, 25.0),
        ("teams-pro", serde_json::json!(0), 0.0, 20.0, 50.0),
        (
            "individual-goat",
            serde_json::json!("unknown"),
            1.0,
            35.0,
            50.0,
        ),
        ("individual-goat", serde_json::json!(-10), 1.0, 70.0, 0.0),
        ("individual-goat", serde_json::Value::Null, 1.0, 80.0, 0.0),
        ("individual-goat", serde_json::Value::Null, 1.0, 0.0, 100.0),
        ("individual-goat", serde_json::Value::Null, 1.0, -5.0, 100.0),
    ] {
        let credits = serde_json::from_value(serde_json::json!({
            "monthlyCredits": remaining, "monthlyCreditsGranted": grant
        }))
        .unwrap();
        let subscription = serde_json::from_value(serde_json::json!({
            "success": true, "data": {"planId": plan, "status": "active", "quantity": seats}
        }))
        .unwrap();
        let monthly = commandcode_monthly_window(&credits, subscription).unwrap();
        assert_eq!(
            monthly.used_percent,
            Some(expected),
            "{plan}, {grant}, {seats}, {remaining}"
        );
        assert!(monthly.resets_at.is_none());
    }
    assert_eq!(commandcode_positive_finite(Some(0.25)), Some(0.25));
    assert_eq!(commandcode_positive_finite(Some(0.0)), None);
    assert_eq!(commandcode_positive_finite(Some(-0.25)), None);
}

#[test]
fn commandcode_monthly_rejects_unavailable_subscriptions_and_invalid_dates() {
    let credits = serde_json::from_str(r#"{"monthlyCredits":35}"#).unwrap();
    for body in [
        r#"{"success":true,"data":null}"#,
        r#"{"success":false,"data":{"planId":"individual-goat","status":"active"}}"#,
        r#"{"success":true,"data":{"planId":"new-plan","status":"active"}}"#,
        r#"{"success":true,"data":{"planId":"individual-goat","status":"past_due"}}"#,
    ] {
        assert!(
            commandcode_monthly_window(&credits, serde_json::from_str(body).unwrap()).is_none(),
            "{body}"
        );
    }
    let subscription = serde_json::from_str(
        r#"{"success":true,"data":{
        "planId":"individual-goat","status":"active","currentPeriodEnd":"not-a-date"
    }}"#,
    )
    .unwrap();
    let monthly = commandcode_monthly_window(&credits, subscription).unwrap();
    assert_eq!(monthly.used_percent, Some(50.0));
    assert!(monthly.resets_at.is_none());
}

const COMMANDCODE_MONTHLY_CREDITS: &str = r#"{
    "windowLimits":{"fiveHour":{"cap":14,"used":0,"resetAt":0},
    "weekly":{"cap":35,"used":7,"resetAt":1789547227218}},
    "credits":{"monthlyCredits":35,"purchasedCredits":3,"freeCredits":2}
}"#;

#[test]
fn commandcode_usage_enriches_monthly_and_separates_extra_credits() {
    let auth_dir = tempfile::tempdir().unwrap();
    let auth_path = auth_dir.path().join("auth.json");
    fs::write(&auth_path, r#"{"apiKey":"cmd_live_test"}"#).unwrap();
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed_t = observed.clone();
    let port = spawn_loopback(2, move |req| {
        let auth = req
            .headers()
            .iter()
            .find(|h| h.field.equiv("Authorization"))
            .map(|h| h.value.as_str().to_string())
            .unwrap_or_default();
        observed_t
            .lock()
            .unwrap()
            .push((req.url().to_string(), auth));
        let body = if req.url() == "/alpha/billing/credits" {
            COMMANDCODE_MONTHLY_CREDITS
        } else {
            r#"{"success":true,"data":{"planId":"individual-goat","status":"active",
            "currentPeriodEnd":"2026-09-30T13:13:19.000Z"}}"#
        };
        req.respond(tiny_http::Response::from_string(body)).unwrap();
    });
    let outcome = commandcode_usage_with_path(
        &auth_path,
        &format!("http://127.0.0.1:{port}/alpha/billing/credits"),
    );
    assert_eq!(
        *observed.lock().unwrap(),
        vec![
            (
                "/alpha/billing/credits".to_string(),
                "Bearer cmd_live_test".to_string()
            ),
            (
                "/alpha/billing/subscriptions?withPending=true".to_string(),
                "Bearer cmd_live_test".to_string()
            ),
        ]
    );
    let (windows, balance, detail) = reading_of(outcome, "");
    assert_eq!(
        windows.iter().map(|w| w.label.as_str()).collect::<Vec<_>>(),
        vec!["5-hour", "Weekly", "Monthly"]
    );
    assert_eq!(windows[0].used_percent, Some(0.0));
    assert_eq!(windows[1].used_percent, Some(20.0));
    assert_eq!(windows[2].used_percent, Some(50.0));
    assert_eq!(
        windows[2].resets_at.as_deref(),
        Some("2026-09-30T13:13:19+00:00")
    );
    assert!(balance.is_none());
    assert_eq!(detail.as_deref(), Some("Additional credits: USD 5.00"));
}

#[test]
fn commandcode_reported_monthly_window_replaces_combined_balance() {
    let auth_dir = tempfile::tempdir().unwrap();
    let auth_path = auth_dir.path().join("auth.json");
    fs::write(&auth_path, r#"{"apiKey":"cmd_live_test"}"#).unwrap();
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed_t = observed.clone();
    let port = spawn_loopback(1, move |req| {
        observed_t.lock().unwrap().push(req.url().to_string());
        req.respond(tiny_http::Response::from_string(
            r#"{
            "windows":[{"label":"Monthly Limit","used_percent":39,
            "resets_at":"2026-09-30T13:13:19+00:00"}],
            "monthly_credits":42.7,"extra_credits":5
        }"#,
        ))
        .unwrap();
    });

    let outcome =
        commandcode_usage_with_path(&auth_path, &format!("http://127.0.0.1:{port}/credits"));
    assert_eq!(*observed.lock().unwrap(), vec!["/credits"]);
    let (windows, balance, detail) = reading_of(outcome, "");
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].label, "Monthly Limit");
    assert!(balance.is_none());
    assert_eq!(detail.as_deref(), Some("Additional credits: USD 5.00"));
}

#[test]
fn commandcode_subscription_failures_preserve_live_windows_and_balance() {
    let auth_dir = tempfile::tempdir().unwrap();
    let auth_path = auth_dir.path().join("auth.json");
    fs::write(&auth_path, r#"{"apiKey":"cmd_live_test"}"#).unwrap();
    for (status, body) in [
        (401, ""),
        (403, ""),
        (429, ""),
        (500, ""),
        (200, "not-json"),
        (200, r#"{"success":true,"data":null}"#),
        (
            200,
            r#"{"success":true,"data":{"planId":"unknown","status":"active"}}"#,
        ),
        (
            200,
            r#"{"success":true,"data":{"planId":"individual-goat","status":"past_due"}}"#,
        ),
    ] {
        let port = spawn_loopback(2, move |req| {
            let response = if req.url() == "/credits" {
                tiny_http::Response::from_string(COMMANDCODE_MONTHLY_CREDITS)
            } else {
                tiny_http::Response::from_string(body).with_status_code(status)
            };
            req.respond(response).unwrap();
        });
        let outcome =
            commandcode_usage_with_path(&auth_path, &format!("http://127.0.0.1:{port}/credits"));
        let (windows, balance, detail) = reading_of(outcome, &format!("{status}: {body}: "));
        assert_eq!(windows.len(), 2, "{status}: {body}");
        assert_eq!(windows[1].used_percent, Some(20.0), "{status}: {body}");
        assert_eq!(balance.unwrap().remaining, 40.0, "{status}: {body}");
        assert!(detail.is_none(), "{status}: {body}");
    }
}

#[test]
fn commandcode_window_rejects_invalid_caps_and_bounds_usage() {
    assert!(commandcode_window(
        "invalid",
        CommandCodeQuotaWindow {
            cap: 0.0,
            used: 1.0,
            reset_at: None,
        },
    )
    .is_none());
    assert!(commandcode_window(
        "nan-cap",
        CommandCodeQuotaWindow {
            cap: f64::NAN,
            used: 1.0,
            reset_at: None,
        },
    )
    .is_none());
    assert!(commandcode_window(
        "nan-used",
        CommandCodeQuotaWindow {
            cap: 1.0,
            used: f64::NAN,
            reset_at: None,
        },
    )
    .is_none());

    let capped = commandcode_window(
        "capped",
        CommandCodeQuotaWindow {
            cap: 2.0,
            used: 3.0,
            reset_at: None,
        },
    )
    .unwrap();
    assert_eq!(capped.used_percent, Some(100.0));
    let floor = commandcode_window(
        "floor",
        CommandCodeQuotaWindow {
            cap: 2.0,
            used: -1.0,
            reset_at: None,
        },
    )
    .unwrap();
    assert_eq!(floor.used_percent, Some(0.0));
    assert!(floor.resets_at.is_none());
}

#[test]
fn commandcode_auth_parser_reports_missing_invalid_and_blank_credentials() {
    let auth_dir = tempfile::tempdir().unwrap();
    let missing = auth_dir.path().join("missing.json");
    assert!(matches!(
        read_commandcode_token(&missing),
        Err(UsageError::NoCredential(_))
    ));

    let auth_path = auth_dir.path().join("auth.json");
    fs::write(&auth_path, "not-json").unwrap();
    assert!(matches!(
        read_commandcode_token(&auth_path),
        Err(UsageError::Shape(_))
    ));

    fs::write(&auth_path, r#"{"apiKey":"  ","access_token":"\t"}"#).unwrap();
    assert!(matches!(
        read_commandcode_token(&auth_path),
        Err(UsageError::NoCredential(_))
    ));
}

#[test]
fn commandcode_usage_http_failures_preserve_auth_and_error_states() {
    let auth_dir = tempfile::tempdir().unwrap();
    let auth_path = auth_dir.path().join("auth.json");
    fs::write(&auth_path, r#"{"apiKey":"cmd_live_test"}"#).unwrap();

    let port_401 = spawn_loopback(1, |req| {
        let _ = req.respond(tiny_http::Response::empty(401));
    });
    let outcome_401 =
        commandcode_usage_with_path(&auth_path, &format!("http://127.0.0.1:{port_401}/credits"));
    let hint_401 = match outcome_401 {
        UsageOutcome::Rejected { hint } => {
            assert!(
                hint.contains("session expired") && hint.contains("—"),
                "session-expired remediation must be preserved, got: {hint:?}"
            );
            hint
        }
        other => panic!("expected Rejected outcome, got: {other:?}"),
    };

    let port_403 = spawn_loopback(1, |req| {
        let _ = req.respond(tiny_http::Response::empty(403));
    });
    let outcome_403 =
        commandcode_usage_with_path(&auth_path, &format!("http://127.0.0.1:{port_403}/credits"));
    match outcome_403 {
        UsageOutcome::Rejected { hint } => assert_eq!(hint, hint_401),
        other => panic!("expected Rejected outcome, got: {other:?}"),
    }

    let port_429 = spawn_loopback(1, |req| {
        let _ = req.respond(tiny_http::Response::empty(429));
    });
    let outcome_429 =
        commandcode_usage_with_path(&auth_path, &format!("http://127.0.0.1:{port_429}/credits"));
    match outcome_429 {
        UsageOutcome::RateLimited { reason } => {
            assert_eq!(reason, "Rate limited — usage data temporarily unavailable")
        }
        other => panic!("expected RateLimited outcome, got: {other:?}"),
    }

    let port_500 = spawn_loopback(1, |req| {
        let _ = req
            .respond(tiny_http::Response::from_string("backend unavailable").with_status_code(500));
    });
    let outcome_500 =
        commandcode_usage_with_path(&auth_path, &format!("http://127.0.0.1:{port_500}/credits"));
    match outcome_500 {
        UsageOutcome::Unavailable { reason } => {
            assert_eq!(reason, "API error 500: backend unavailable")
        }
        other => panic!("expected Unavailable outcome, got: {other:?}"),
    }

    let port_malformed = spawn_loopback(1, |req| {
        let _ = req.respond(tiny_http::Response::from_string("not-json"));
    });
    let outcome_malformed = commandcode_usage_with_path(
        &auth_path,
        &format!("http://127.0.0.1:{port_malformed}/credits"),
    );
    match outcome_malformed {
        UsageOutcome::Unavailable { reason } => assert!(
            reason.contains("Failed to parse response"),
            "parse envelope must be preserved, got: {reason:?}"
        ),
        other => panic!("expected Unavailable outcome, got: {other:?}"),
    }
}

#[test]
fn adapter_seam_contract_commandcode_no_credential_without_network() {
    // A missing CLI credential must report `NoCredential` without
    // touching the network — the unreachable loopback URL below would
    // fail loudly if hit. (`dispatch("commandcode").fetch` is not
    // hermetic here: it reads the real `~/.commandcode/auth.json`.)
    let auth_dir = tempfile::tempdir().unwrap();
    let missing = auth_dir.path().join("missing.json");
    match commandcode_usage_with_path(&missing, "http://127.0.0.1:1/credits") {
        UsageOutcome::NoCredential { .. } => {}
        other => panic!("expected NoCredential outcome, got: {other:?}"),
    }
}

/// The rendered meter output is pinned (issue #2528).
///
/// The credits response, the subscription response and the exact wire
/// triple they must project to are all fixtures. The subscription lookup
/// is the enrichment, so this also pins that the monthly meter replaces
/// the balance and that the extra-credits line survives it.
#[test]
fn commandcode_rendered_output_matches_the_pinned_fixture() {
    const CREDITS: &str = include_str!("fixtures/commandcode-credits.json");
    const SUBSCRIPTION: &str = include_str!("fixtures/commandcode-subscription.json");
    const RENDERED: &str = include_str!("fixtures/commandcode-credits-rendered.json");

    let auth_dir = tempfile::tempdir().unwrap();
    let auth_path = auth_dir.path().join("auth.json");
    fs::write(&auth_path, r#"{"apiKey":"cmd_live_test"}"#).unwrap();
    let port = spawn_loopback(2, move |req| {
        let body = if req.url() == "/alpha/billing/credits" {
            CREDITS
        } else {
            SUBSCRIPTION
        };
        req.respond(tiny_http::Response::from_string(body)).unwrap();
    });

    let outcome = commandcode_usage_with_path(
        &auth_path,
        &format!("http://127.0.0.1:{port}/alpha/billing/credits"),
    );
    let usage = outcome.into_usage("commandcode");
    assert_eq!(
        serde_json::to_string(&usage).unwrap(),
        // The fixture file ends with a newline; the payload does not.
        RENDERED.trim_end(),
        "commandcode's rendered meter output must stay byte-identical"
    );
}
