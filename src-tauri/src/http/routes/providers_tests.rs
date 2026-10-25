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
