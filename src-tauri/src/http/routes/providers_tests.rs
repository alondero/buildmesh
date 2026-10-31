use super::*;

#[test]
fn usage_payload_keeps_enabled_named_meters_without_credentials() {
    let account = |id: &str, enabled: bool| {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": "My provider", "enabled": enabled,
            "billing_mode": "plan", "claude_compatible": true, "api_key": "private-secret"
        }))
        .unwrap()
    };
    let meter = |id: &str| {
        serde_json::from_value(serde_json::json!({
            "provider": id, "usageTracked": true, "usage": null, "cachedAt": 123
        }))
        .unwrap()
    };
    let rows = usage_rows(
        &[account("enabled", true), account("disabled", false)],
        vec![meter("enabled"), meter("disabled"), meter("removed")],
    );
    let value = serde_json::to_value(rows).unwrap();
    assert_eq!(value.as_array().unwrap().len(), 1);
    assert_eq!(value[0]["name"], "My provider");
    assert_eq!(value[0]["meter"]["cachedAt"], 123);
    assert_eq!(value[0]["meter"]["provider"], "enabled");
    assert!(!value.to_string().contains("private-secret"));
    assert!(value[0].get("api_key").is_none());
}

#[tokio::test]
async fn usage_rejects_invalid_refresh_and_reports_missing_runtime() {
    assert_eq!(
        usage(&ParsedRequest::test_get("/api/usage?refresh=invalid"))
            .await
            .status_code(),
        400
    );
    assert_eq!(
        usage(&ParsedRequest::test_get("/api/usage"))
            .await
            .status_code(),
        503
    );
}

/// Two Codex logins are two rows on the phone too, so each row names its
/// login instead of showing the provider name twice.
#[test]
fn usage_rows_name_the_login_of_a_multi_login_provider() {
    let account: ProviderAccount = serde_json::from_value(serde_json::json!({
        "id": "codex", "name": "Codex", "enabled": true,
        "billing_mode": "plan", "claude_compatible": false
    }))
    .unwrap();
    let meter = |login: Option<serde_json::Value>| {
        let mut value = serde_json::json!({
            "provider": "codex", "usageTracked": true, "usage": null
        });
        if let Some(login) = login {
            value["login"] = login;
        }
        serde_json::from_value(value).unwrap()
    };
    let rows = usage_rows(
        &[account],
        vec![
            meter(Some(
                serde_json::json!({ "id": "default", "label": "Default" }),
            )),
            meter(Some(
                serde_json::json!({ "id": "work", "label": "Work", "email": "w@example.com" }),
            )),
            meter(None),
        ],
    );
    let names: Vec<_> = rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(names, ["Codex · Default", "Codex · Work", "Codex"]);
}
