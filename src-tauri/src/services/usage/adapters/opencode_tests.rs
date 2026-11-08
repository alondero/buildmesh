use super::*;
use crate::services::usage::adapter::spawn_loopback;

// Live API shape and reset instants from the dated investigation; percentages
// are synthetic nonzero values to exercise the meter projection.
const RESPONSE: &str = include_str!("fixtures/opencode-go-usage.json");

fn legacy_auth(dir: &Path) {
    std::fs::write(
        dir.join("auth.json"),
        r#"{"opencode-go":{"type":"api","key":"sk-legacy-test"}}"#,
    )
    .unwrap();
}

fn database(dir: &Path) -> Connection {
    let conn = Connection::open(dir.join("opencode.db")).unwrap();
    conn.execute_batch("CREATE TABLE credential(id TEXT PRIMARY KEY,integration_id TEXT,value TEXT,active INTEGER,time_created INTEGER)").unwrap();
    conn
}

fn insert_credential(
    conn: &Connection,
    id: &str,
    active: Option<i32>,
    created: i64,
    value: serde_json::Value,
) {
    conn.execute(
        "INSERT INTO credential VALUES(?1,'opencode',?2,?3,?4)",
        rusqlite::params![id, value.to_string(), active, created],
    )
    .unwrap();
}

fn oauth() -> serde_json::Value {
    serde_json::json!({"type":"oauth","methodID":"device","access":"oauth-test-token","refresh":"do-not-refresh","expires":4102444800000_i64,"metadata":{"orgID":"wrk-test","server":"https://opencode.ai/console"}})
}

#[test]
fn go_api_reading_matches_rendered_fixture() {
    let reading = parse_usage(RESPONSE).unwrap();
    let usage = reading.into_outcome().into_usage("opencode");
    assert_eq!(
        serde_json::to_string(&usage).unwrap(),
        include_str!("fixtures/opencode-go-usage-rendered.json").trim_end()
    );
}

#[test]
fn depleted_and_zero_quotas_are_readings_and_offsets_are_normalized() {
    let mut body: serde_json::Value = serde_json::from_str(RESPONSE).unwrap();
    body["usage"]["rolling"]["status"] = "rate-limited".into();
    body["usage"]["rolling"]["percent"] = 100.into();
    body["usage"]["weekly"]["percent"] = 0.into();
    body["usage"]["monthly"]["resetsAt"] = "2026-12-08T15:51:22+05:30".into();
    let reading = parse_usage(&body.to_string()).unwrap();
    assert_eq!(reading.windows[0].used_percent, Some(100.0));
    assert_eq!(reading.windows[1].used_percent, Some(0.0));
    assert_eq!(
        reading.windows[2].resets_at.as_deref(),
        Some("2026-12-08T10:21:22+00:00")
    );
}

#[test]
fn invalid_or_partial_api_data_does_not_invent_windows() {
    for value in [
        serde_json::json!({}),
        serde_json::json!({"windows": []}),
        serde_json::json!({"usage": {}}),
    ] {
        assert!(parse_usage(&value.to_string()).is_err());
    }
    let original: serde_json::Value = serde_json::from_str(RESPONSE).unwrap();
    for invalid in [
        serde_json::Value::Null,
        serde_json::json!(-1),
        serde_json::json!(101),
        serde_json::json!("25"),
        serde_json::json!(1e300),
    ] {
        let mut body = original.clone();
        body["usage"]["rolling"]["percent"] = invalid;
        assert!(parse_usage(&body.to_string()).is_err());
    }
    for invalid in [
        serde_json::Value::Null,
        "".into(),
        "2026-12-08 10:21:22".into(),
        "not-a-date".into(),
    ] {
        let mut body = original.clone();
        body["usage"]["monthly"]["resetsAt"] = invalid;
        assert!(parse_usage(&body.to_string()).is_err());
    }
    let mut body = original;
    body["usage"]["rolling"]["status"] = "unknown".into();
    assert!(parse_usage(&body.to_string()).is_err());
}

#[test]
fn live_v2_oauth_request_uses_active_workspace_and_ignores_expired_legacy_key() {
    let dir = tempfile::tempdir().unwrap();
    legacy_auth(dir.path());
    let conn = database(dir.path());
    insert_credential(&conn, "active", Some(1), 1, oauth());
    let mut inactive = oauth();
    inactive["access"] = "wrong-account".into();
    inactive["metadata"]["orgID"] = "wrong-workspace".into();
    insert_credential(&conn, "newer-inactive", None, 100, inactive);
    let port = spawn_loopback(1, |request| {
        assert_eq!(request.method(), &tiny_http::Method::Get);
        assert_eq!(request.url(), "/inference/go/v1/usage");
        let header = |name: &str| {
            request
                .headers()
                .iter()
                .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
                .map(|h| h.value.as_str())
        };
        assert_eq!(header("authorization"), Some("Bearer oauth-test-token"));
        assert_eq!(header("x-opencode-org-id"), Some("wrk-test"));
        assert_eq!(header("accept"), Some("application/json"));
        assert_eq!(header("user-agent"), Some("Buildmesh-Usage-Probe/1.0"));
        assert_eq!(header("x-server-id"), None);
        request
            .respond(tiny_http::Response::from_string(RESPONSE))
            .unwrap();
    });
    let outcome = usage_at(
        dir.path(),
        &dir.path().join("opencode.db"),
        &format!("http://127.0.0.1:{port}/inference/go/v1/usage"),
    );
    assert!(
        matches!(&outcome, UsageOutcome::Reading { windows, .. } if windows.len() == 3),
        "{outcome:?}"
    );
}

#[test]
fn live_v1_key_works_without_database_or_console_oauth() {
    let dir = tempfile::tempdir().unwrap();
    legacy_auth(dir.path());
    let port =
        spawn_loopback(1, |request| {
            assert_eq!(request.method(), &tiny_http::Method::Get);
            assert!(request
                .headers()
                .iter()
                .any(|h| h.field.equiv("Authorization")
                    && h.value.as_str() == "Bearer sk-legacy-test"));
            assert!(!request
                .headers()
                .iter()
                .any(|h| h.field.equiv("x-opencode-org-id")));
            request
                .respond(tiny_http::Response::from_string(RESPONSE))
                .unwrap();
        });
    assert!(matches!(
        usage_at(
            dir.path(),
            &dir.path().join("opencode.db"),
            &format!("http://127.0.0.1:{port}/")
        ),
        UsageOutcome::Reading { .. }
    ));
    assert!(!dir.path().join("opencode.db").exists());
}

#[test]
fn newest_native_key_and_legacy_configuration_match_cli_selection() {
    let dir = tempfile::tempdir().unwrap();
    let conn = database(dir.path());
    insert_credential(&conn, "a", None, 1, oauth());
    insert_credential(&conn, "b", None, 2, oauth());
    insert_credential(
        &conn,
        "c",
        None,
        2,
        serde_json::json!({"type":"key","key":"native-key","metadata":{"orgID":"old"},"configuration":{"orgID":"selected","server":"https://console.opencode.ai"}}),
    );
    let credential = read_credential(dir.path(), &dir.path().join("opencode.db")).unwrap();
    assert_eq!(credential.token, "native-key");
    assert_eq!(credential.org_id.as_deref(), Some("selected"));
}

#[test]
fn native_credential_errors_do_not_fall_back_to_another_identity() {
    for bad in [
        serde_json::json!({}),
        serde_json::json!({"type":"oauth","access":"secret-do-not-print"}),
        serde_json::json!({"type":"key","key":"secret-do-not-print","metadata":{"orgID":"workspace","server":"https://unrelated.example"}}),
    ] {
        let dir = tempfile::tempdir().unwrap();
        legacy_auth(dir.path());
        let conn = database(dir.path());
        insert_credential(&conn, "selected", Some(1), 1, bad);
        let outcome = usage_at(
            dir.path(),
            &dir.path().join("opencode.db"),
            "http://127.0.0.1:1",
        );
        assert!(matches!(&outcome, UsageOutcome::Unavailable { .. }));
        assert!(!format!("{outcome:?}").contains("secret-do-not-print"));
    }
    let dir = tempfile::tempdir().unwrap();
    legacy_auth(dir.path());
    std::fs::write(dir.path().join("opencode.db"), "not a sqlite database").unwrap();
    assert!(matches!(
        usage_at(
            dir.path(),
            &dir.path().join("opencode.db"),
            "http://127.0.0.1:1"
        ),
        UsageOutcome::Unavailable { .. }
    ));
}

#[test]
fn expired_native_oauth_is_rejected_without_rotating_shared_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let conn = database(dir.path());
    let mut value = oauth();
    value["expires"] = 1.into();
    insert_credential(&conn, "expired", Some(1), 1, value.clone());
    let outcome = usage_at(
        dir.path(),
        &dir.path().join("opencode.db"),
        "http://127.0.0.1:1",
    );
    assert!(matches!(outcome, UsageOutcome::Rejected { hint } if hint.contains("Open OpenCode")));
    let after: String = conn
        .query_row("SELECT value FROM credential", [], |row| row.get(0))
        .unwrap();
    assert_eq!(after, value.to_string());
}

#[test]
fn http_failures_are_preserved_instead_of_estimated_local_usage() {
    for status in [401, 403, 429, 500, 200] {
        let dir = tempfile::tempdir().unwrap();
        legacy_auth(dir.path());
        // Old session costs cannot turn a failed request into a quota reading.
        let conn = Connection::open(dir.path().join("opencode.db")).unwrap();
        conn.execute_batch("CREATE TABLE session(cost REAL,time_created INTEGER); INSERT INTO session VALUES(999,0)").unwrap();
        let port = spawn_loopback(1, move |request| {
            request
                .respond(tiny_http::Response::from_string("{}").with_status_code(status))
                .unwrap();
        });
        let outcome = usage_at(
            dir.path(),
            &dir.path().join("opencode.db"),
            &format!("http://127.0.0.1:{port}/"),
        );
        match status {
            401 | 403 => assert!(matches!(outcome, UsageOutcome::Rejected { .. })),
            429 => assert!(matches!(outcome, UsageOutcome::RateLimited { .. })),
            _ => assert!(matches!(outcome, UsageOutcome::Unavailable { .. })),
        }
    }
}

#[test]
fn missing_auth_and_malformed_auth_remain_distinct() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("opencode.db");
    assert!(matches!(
        usage_at(dir.path(), &db, "http://127.0.0.1:1"),
        UsageOutcome::NoCredential { .. }
    ));
    std::fs::write(dir.path().join("auth.json"), "invalid secret-do-not-print").unwrap();
    let outcome = usage_at(dir.path(), &db, "http://127.0.0.1:1");
    assert!(matches!(&outcome, UsageOutcome::Unavailable { .. }));
    assert!(!format!("{outcome:?}").contains("secret-do-not-print"));
}

#[test]
fn workspace_and_token_changes_invalidate_native_usage_identity() {
    let dir = tempfile::tempdir().unwrap();
    let conn = database(dir.path());
    insert_credential(&conn, "selected", Some(1), 1, oauth());
    let db = dir.path().join("opencode.db");
    let original = read_credential(dir.path(), &db).unwrap().cache_identity();
    let mut value = oauth();
    value["metadata"]["orgID"] = "another-workspace".into();
    conn.execute("UPDATE credential SET value=?1", [value.to_string()])
        .unwrap();
    assert!(original != read_credential(dir.path(), &db).unwrap().cache_identity());
    value = oauth();
    value["access"] = "another-token".into();
    conn.execute("UPDATE credential SET value=?1", [value.to_string()])
        .unwrap();
    assert!(original != read_credential(dir.path(), &db).unwrap().cache_identity());
}

#[test]
fn configured_database_errors_do_not_choose_the_default_database_or_key() {
    let dir = tempfile::tempdir().unwrap();
    legacy_auth(dir.path());
    let conn = database(dir.path());
    insert_credential(&conn, "default", Some(1), 1, oauth());
    let configured = dir.path().join("configured.db");
    assert!(matches!(
        usage_at(dir.path(), &configured, "http://127.0.0.1:1"),
        UsageOutcome::Unavailable { .. }
    ));
    std::fs::write(&configured, "invalid database").unwrap();
    assert!(matches!(
        usage_at(dir.path(), &configured, "http://127.0.0.1:1"),
        UsageOutcome::Unavailable { .. }
    ));
}

#[test]
#[ignore = "Requires the user's native OpenCode Go login and network access"]
fn native_live_go_usage() {
    let outcome = opencode_usage();
    match outcome {
        UsageOutcome::Reading { windows, .. } => {
            assert_eq!(windows.len(), 3);
            for window in windows {
                assert!(window.used_percent.is_some());
                assert!(
                    chrono::DateTime::parse_from_rfc3339(window.resets_at.as_deref().unwrap())
                        .is_ok()
                );
                println!(
                    "{}: {}%, resets {}",
                    window.label,
                    window.used_percent.unwrap(),
                    window.resets_at.unwrap()
                );
            }
        }
        other => panic!("Native live usage did not return a reading: {other:?}"),
    }
}
