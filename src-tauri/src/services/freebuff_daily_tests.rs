use super::*;
use crate::services::usage::spawn_loopback;

// Minimal redacted shape observed on the website and replayed using CLI auth.
const DAILY_RESPONSE: &str = r#"{
    "status": "none",
    "freebucks": {
        "daily": {
            "limit": 20, "spent": 15, "remaining": 5,
            "resetAt": "2026-10-22T23:00:00.000Z", "resetTimeZone": "Europe/London"
        },
        "wallet": { "balance": 100 }
    },
    "rateLimitsByModel": { "example-model": { "limit": 6, "recentCount": 1 } }
}"#;

#[test]
fn daily_meter_matches_website_even_without_an_active_session() {
    let windows = parse_freebuff_response(DAILY_RESPONSE).unwrap();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].label, "Daily");
    assert_eq!(windows[0].used_percent, Some(75.0));
    assert_eq!(
        windows[0].resets_at.as_deref(),
        Some("2026-10-22T23:00:00.000Z")
    );
}

#[test]
fn daily_percentage_handles_zero_fractional_and_over_limit_usage() {
    for (spent, limit, expected) in [(0.0, 20.0, 0.0), (1.0, 8.0, 12.5), (30.0, 20.0, 100.0)] {
        let body = serde_json::json!({"freebucks":{"daily":{"spent":spent,"limit":limit}}});
        let windows = parse_freebuff_response(&body.to_string()).unwrap();
        assert_eq!(windows[0].used_percent, Some(expected));
        assert_eq!(windows[0].resets_at, None);
    }
}

#[test]
fn missing_or_invalid_allowance_is_not_a_healthy_empty_reading() {
    for body in [
        "not-json",
        "{}",
        r#"{"status":"none"}"#,
        r#"{"freebucks":null}"#,
        r#"{"freebucks":{"daily":{"spent":0,"limit":0}}}"#,
        r#"{"freebucks":{"daily":{"spent":0,"limit":-1}}}"#,
        r#"{"freebucks":{"daily":{"spent":-1,"limit":20}}}"#,
        r#"{"freebucks":{"daily":{"limit":20}}}"#,
        r#"{"freebucks":{"daily":{"spent":null,"limit":20}}}"#,
        r#"{"windows":[],"earnedSessions":5}"#,
    ] {
        assert!(parse_freebuff_response(body).is_err(), "accepted {body}");
    }
}

#[test]
fn credentials_accept_current_and_legacy_bearers_without_device_metadata() {
    for body in [
        r#"{"default":{"authToken":"  current-token  "}}"#,
        r#"{"authToken":"  current-token  "}"#,
        r#"{"authToken":"inactive-token","default":{"authToken":"current-token"}}"#,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        fs::write(&path, body).unwrap();
        assert_eq!(read_freebuff_credentials(&[path]).unwrap(), "current-token");
    }
}

#[test]
fn missing_or_blank_default_does_not_use_an_inactive_top_level_token() {
    for body in [
        r#"{"default":{},"authToken":"inactive"}"#,
        r#"{"default":{"authToken":"  "},"authToken":"inactive"}"#,
        r#"{"authToken":""}"#,
        "{}",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        fs::write(&path, body).unwrap();
        assert!(matches!(
            read_freebuff_credentials(&[path]),
            Err(UsageError::NoCredential(_))
        ));
    }
}

#[test]
fn malformed_credential_is_unavailable_and_does_not_fetch() {
    for body in ["not-json", r#"{"default":null,"authToken":"inactive"}"#] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        fs::write(&path, body).unwrap();
        assert!(matches!(
            freebuff_usage_with(&[path], "http://127.0.0.1:1/unused"),
            UsageOutcome::Unavailable { .. }
        ));
    }
}

#[test]
fn missing_candidate_falls_through_to_current_login() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.json");
    fs::write(&path, r#"{"default":{"authToken":"current"}}"#).unwrap();
    assert_eq!(
        read_freebuff_credentials(&[dir.path().join("missing"), path]).unwrap(),
        "current"
    );
}

#[test]
fn fetches_daily_meter_with_current_cli_bearer_and_no_browser_cookies() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.json");
    fs::write(&path, r#"{"default":{"authToken":"current-token"}}"#).unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let port = spawn_loopback(1, move |request| {
        send.send((
            request.method().to_string(),
            request.url().to_string(),
            request.headers().to_vec(),
        ))
        .unwrap();
        request
            .respond(tiny_http::Response::from_string(DAILY_RESPONSE))
            .unwrap();
    });
    let outcome = freebuff_usage_with(
        &[path],
        &format!("http://127.0.0.1:{port}/api/web/freebuff-session"),
    );
    let UsageOutcome::Reading {
        windows,
        balance,
        meters,
        detail,
    } = outcome
    else {
        panic!("expected daily reading, got {outcome:?}");
    };
    assert_eq!(windows[0].used_percent, Some(75.0));
    assert!(balance.is_none(), "allowance credits are not USD");
    assert!(meters.is_empty());
    assert_eq!(detail, None);
    let (method, url, headers) = receive.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(method, "GET");
    assert_eq!(url, "/api/web/freebuff-session");
    let header = |name: &str| {
        headers
            .iter()
            .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
            .map(|h| h.value.as_str())
    };
    assert_eq!(header("Authorization"), Some("Bearer current-token"));
    assert_eq!(header("User-Agent"), Some(FREEBUFF_USER_AGENT));
    assert_eq!(header("Cookie"), None);
    assert_eq!(header("X-User-Email"), None);
    assert_eq!(header("X-Fingerprint-Id"), None);
}

#[test]
fn unknown_success_shape_is_unavailable_in_production_fetch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.json");
    fs::write(&path, r#"{"default":{"authToken":"current-token"}}"#).unwrap();
    let port = spawn_loopback(1, |request| {
        request
            .respond(tiny_http::Response::from_string("{}"))
            .unwrap();
    });
    assert!(matches!(
        freebuff_usage_with(&[path], &format!("http://127.0.0.1:{port}/usage")),
        UsageOutcome::Unavailable { .. }
    ));
}

#[test]
fn missing_route_is_unavailable_even_with_an_empty_or_inactive_session_body() {
    for body in ["", r#"{"status":"none"}"#] {
        assert!(matches!(
            classify_status(404, body),
            StatusOutcome::Unavailable(_)
        ));
    }
}
