use super::*;
use crate::services::usage::adapter::spawn_loopback;
use std::sync::{Arc, Mutex};

fn reading(
    outcome: UsageOutcome,
) -> (
    Vec<UsageWindow>,
    Option<BillingBalance>,
    Vec<UsageMeter>,
    Option<String>,
) {
    match outcome {
        UsageOutcome::Reading {
            windows,
            balance,
            meters,
            detail,
        } => (windows, balance, meters, detail),
        other => panic!("expected a reading, got {other:?}"),
    }
}

fn close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-6,
        "expected {expected}, got {actual}"
    );
}

fn write_credentials(dir: &std::path::Path, body: &str) -> PathBuf {
    let path = dir.join("credentials.toml");
    std::fs::write(&path, body).unwrap();
    path
}

fn inputs(file: Option<PathBuf>, key: Option<&str>, server: Option<&str>) -> Inputs {
    Inputs {
        credentials_file: file,
        env_api_key: key.map(str::to_string),
        env_server_url: server.map(str::to_string),
    }
}

#[test]
fn quota_mode_reports_daily_and_weekly_used_percent_with_resets() {
    let body = serde_json::json!({
        "userStatus": {"planStatus": {
            "dailyQuotaRemainingPercent": 80,
            "weeklyQuotaRemainingPercent": 35.5,
            "dailyQuotaResetAtUnix": "1893456000",
            "weeklyQuotaResetAtUnix": 1893888000
        }}
    })
    .to_string();
    let (windows, balance, meters, detail) = reading(outcome_from_body(&body));
    assert!(balance.is_none() && meters.is_empty() && detail.is_none());
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].label, "Daily");
    close(windows[0].used_percent.unwrap(), 20.0);
    assert_eq!(
        windows[0].resets_at.as_deref(),
        Some("2030-01-01T00:00:00+00:00")
    );
    assert_eq!(windows[1].label, "Weekly");
    close(windows[1].used_percent.unwrap(), 64.5);
    assert_eq!(
        windows[1].resets_at.as_deref(),
        Some("2030-01-06T00:00:00+00:00")
    );
}

#[test]
fn snake_case_fields_and_clamping_are_accepted() {
    let body = serde_json::json!({
        "user_status": {"plan_status": {
            "daily_quota_remaining_percent": 140,
            "weekly_quota_remaining_percent": -10
        }}
    })
    .to_string();
    let (windows, ..) = reading(outcome_from_body(&body));
    close(windows[0].used_percent.unwrap(), 0.0);
    close(windows[1].used_percent.unwrap(), 100.0);
    assert!(windows[0].resets_at.is_none());
}

#[test]
fn hidden_quotas_are_not_rendered() {
    let body = serde_json::json!({
        "userStatus": {
            "planInfo": {"hideDailyQuota": true},
            "planStatus": {
                "dailyQuotaRemainingPercent": 10,
                "weeklyQuotaRemainingPercent": 90
            }
        }
    })
    .to_string();
    let (windows, ..) = reading(outcome_from_body(&body));
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].label, "Weekly");
}

#[test]
fn overage_balance_is_a_usd_balance_beside_the_windows() {
    let body = serde_json::json!({
        "userStatus": {"planStatus": {
            "dailyQuotaRemainingPercent": 50,
            "overageBalanceMicros": "12500000"
        }}
    })
    .to_string();
    let (windows, balance, ..) = reading(outcome_from_body(&body));
    assert_eq!(windows.len(), 1);
    let balance = balance.unwrap();
    close(balance.remaining, 12.5);
    assert_eq!(balance.currency, "USD");

    let zero = serde_json::json!({"userStatus": {"planStatus": {"dailyQuotaRemainingPercent": 50, "overageBalanceMicros": 0}}}).to_string();
    assert!(reading(outcome_from_body(&zero)).1.is_none());
}

#[test]
fn acu_billing_cycle_is_one_bar_with_a_detail_and_no_duplicate_meter() {
    let body = serde_json::json!({
        "userStatus": {"planStatus": {
            "acuConsumed": 25,
            "acuLimit": 100,
            "planEnd": "2030-02-01T00:00:00Z"
        }}
    })
    .to_string();
    let (windows, _, meters, detail) = reading(outcome_from_body(&body));
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].label, "Billing cycle");
    close(windows[0].used_percent.unwrap(), 25.0);
    assert_eq!(
        windows[0].resets_at.as_deref(),
        Some("2030-02-01T00:00:00Z")
    );
    assert!(meters.is_empty());
    assert_eq!(detail.as_deref(), Some("25.00 / 100.00 ACUs"));
}

#[test]
fn dollar_billing_cycle_formats_money() {
    let body = serde_json::json!({
        "userStatus": {"planStatus": {"dollarsConsumed": 3.5, "dollarsLimit": 20, "planEnd": {"seconds": "1893456000"}}}
    })
    .to_string();
    let (windows, _, _, detail) = reading(outcome_from_body(&body));
    close(windows[0].used_percent.unwrap(), 17.5);
    assert_eq!(
        windows[0].resets_at.as_deref(),
        Some("2030-01-01T00:00:00+00:00")
    );
    assert_eq!(detail.as_deref(), Some("$3.50 / $20.00"));
}

#[test]
fn consumption_without_a_limit_is_uncapped_and_a_limit_without_consumption_is_detail() {
    let uncapped =
        serde_json::json!({"userStatus": {"planStatus": {"acuConsumed": 7.25}}}).to_string();
    let (windows, _, meters, detail) = reading(outcome_from_body(&uncapped));
    assert!(windows.is_empty() && detail.is_none());
    match &meters[0] {
        UsageMeter::NoIndividualLimit { amount } => {
            close(amount.used, 7.25);
            assert_eq!(amount.unit, "ACUs");
        }
        other => panic!("expected an uncapped meter, got {other:?}"),
    }

    let zero_limit =
        serde_json::json!({"userStatus": {"planStatus": {"acuConsumed": 1, "acuLimit": 0}}})
            .to_string();
    assert!(matches!(
        reading(outcome_from_body(&zero_limit)).2[0],
        UsageMeter::NoIndividualLimit { .. }
    ));

    let limit_only =
        serde_json::json!({"userStatus": {"planStatus": {"acuLimit": 50}}}).to_string();
    assert_eq!(
        reading(outcome_from_body(&limit_only)).3.as_deref(),
        Some("ACUs limit 50.00")
    );
}

#[test]
fn legacy_prompt_credits_become_a_percentage() {
    let body = serde_json::json!({
        "userStatus": {"planStatus": {"availablePromptCredits": 50000, "usedPromptCredits": 12500}}
    })
    .to_string();
    let (windows, ..) = reading(outcome_from_body(&body));
    assert_eq!(windows[0].label, "Credits");
    close(windows[0].used_percent.unwrap(), 25.0);
}

#[test]
fn empty_and_malformed_bodies_are_explicit() {
    let (windows, balance, meters, detail) = reading(outcome_from_body("{}"));
    assert!(windows.is_empty() && balance.is_none() && meters.is_empty());
    assert_eq!(detail.as_deref(), Some("No quota data available"));
    assert!(matches!(
        outcome_from_body("<html>"),
        UsageOutcome::Unavailable { .. }
    ));
}

#[test]
fn credentials_file_wins_and_environment_is_the_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let file = write_credentials(
        temp.path(),
        "windsurf_api_key = \"file-key\"\napi_server_url = \"https://example.test/\"\n",
    );
    let resolved = resolve(&inputs(Some(file.clone()), Some("env-key"), None)).unwrap();
    assert_eq!(resolved.api_key, "file-key");
    assert_eq!(resolved.server_url, "https://example.test");

    let overridden = resolve(&inputs(Some(file), None, Some("https://override.test"))).unwrap();
    assert_eq!(overridden.server_url, "https://override.test");

    let missing = temp.path().join("absent.toml");
    let env_only = resolve(&inputs(Some(missing.clone()), Some("env-key"), None)).unwrap();
    assert_eq!(env_only.api_key, "env-key");
    assert_eq!(env_only.server_url, DEFAULT_SERVER);

    assert!(resolve(&inputs(Some(missing), None, None)).is_none());
}

#[test]
fn an_empty_key_in_the_file_is_not_a_credential() {
    let temp = tempfile::tempdir().unwrap();
    let file = write_credentials(temp.path(), "windsurf_api_key = \"\"\n");
    assert!(resolve(&inputs(Some(file), Some("env-key"), None)).is_none());
}

#[test]
fn fingerprint_changes_with_the_key_and_is_deterministic() {
    let a = fingerprint_for(Some(&Credentials {
        api_key: "secret-a".into(),
        server_url: DEFAULT_SERVER.into(),
    }));
    let b = fingerprint_for(Some(&Credentials {
        api_key: "secret-b".into(),
        server_url: DEFAULT_SERVER.into(),
    }));
    let out = fingerprint_for(None);
    assert!(a != b);
    assert!(a != out);
    assert!(
        a == fingerprint_for(Some(&Credentials {
            api_key: "secret-a".into(),
            server_url: DEFAULT_SERVER.into()
        }))
    );
}

#[test]
fn a_missing_credential_reports_not_signed_in() {
    assert!(matches!(
        fetch_with(None),
        UsageOutcome::NoCredential { .. }
    ));
}

#[test]
fn non_https_non_loopback_servers_are_refused_before_any_request() {
    for url in ["file:///etc/passwd", "http://example.test"] {
        let outcome = fetch_with(Some(Credentials {
            api_key: "k".into(),
            server_url: url.into(),
        }));
        assert!(matches!(outcome, UsageOutcome::Unavailable { .. }), "{url}");
    }
}

type RecordedRequests = Arc<Mutex<Vec<(String, String, Vec<String>)>>>;

fn serve(status: u16, body: &'static str) -> (String, RecordedRequests) {
    let seen: RecordedRequests = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let port = spawn_loopback(1, move |mut request| {
        let mut payload = String::new();
        std::io::Read::read_to_string(request.as_reader(), &mut payload).unwrap();
        let headers = request
            .headers()
            .iter()
            .map(|h| format!("{}: {}", h.field, h.value))
            .collect();
        sink.lock()
            .unwrap()
            .push((request.url().to_string(), payload, headers));
        let _ = request.respond(tiny_http::Response::from_string(body).with_status_code(status));
    });
    (format!("http://127.0.0.1:{port}"), seen)
}

#[test]
fn fetch_posts_the_connect_json_request_and_parses_the_reading() {
    let (server, seen) = serve(
        200,
        "{\"userStatus\":{\"planStatus\":{\"dailyQuotaRemainingPercent\":60}}}",
    );
    let outcome = fetch_with(Some(Credentials {
        api_key: "key-123".into(),
        server_url: server,
    }));
    let (windows, ..) = reading(outcome);
    close(windows[0].used_percent.unwrap(), 40.0);
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].0, USER_STATUS_PATH);
    let request: serde_json::Value = serde_json::from_str(&seen[0].1).unwrap();
    assert_eq!(request["metadata"]["api_key"], "key-123");
    assert_eq!(request["metadata"]["ide_name"], "devin-cli");
    let headers = seen[0].2.join("\n").to_ascii_lowercase();
    assert!(headers.contains("connect-protocol-version: 1"));
    assert!(headers.contains("content-type: application/json"));
}

#[test]
fn rejected_keys_and_throttling_map_to_distinct_outcomes() {
    let creds = |server: String| {
        Some(Credentials {
            api_key: "k".into(),
            server_url: server,
        })
    };
    let (server, _) = serve(401, "{}");
    assert!(matches!(
        fetch_with(creds(server)),
        UsageOutcome::Rejected { .. }
    ));
    let (server, _) = serve(
        400,
        "{\"code\":\"invalid_argument\",\"message\":\"bad key\"}",
    );
    assert!(matches!(
        fetch_with(creds(server)),
        UsageOutcome::Rejected { .. }
    ));
    let (server, _) = serve(429, "{}");
    assert!(matches!(
        fetch_with(creds(server)),
        UsageOutcome::RateLimited { .. }
    ));
    let (server, _) = serve(500, "{}");
    assert!(matches!(
        fetch_with(creds(server)),
        UsageOutcome::Unavailable { .. }
    ));
}

#[test]
fn adapter_metadata_gates_on_the_native_harness_and_links_the_usage_page() {
    let adapter = DevinAdapter;
    assert_eq!(adapter.id(), "devin");
    assert_eq!(adapter.native_harness(), Some("devin"));
    let page = adapter.usage_page().unwrap();
    assert_eq!(page.url, "https://app.devin.ai/settings/usage");
    assert_eq!(page.label, "View usage");
}
