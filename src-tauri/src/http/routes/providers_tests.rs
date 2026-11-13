use super::*;

#[test]
fn phone_menu_exposes_selection_metadata_without_private_login_details() {
    let codex: crate::agent::provider::ProviderInfo = serde_json::from_value(serde_json::json!({
        "id":"codex", "label":"Codex", "harness_id":"codex", "group_key":"codex",
        "color":"#ffffff", "icon":"C", "resumable":true, "provider_id":null, "is_proxied":false,
        "capabilities": crate::agent::capabilities::capabilities_for(crate::models::Provider::Codex.adapter())
    }))
    .unwrap();
    let mut routed = codex.clone();
    routed.is_proxied = true;
    let mut saved = codex.clone();
    saved.configuration = Some(Default::default());
    let mut other = codex.clone();
    other.harness_id = "claude".into();
    let rows = vec![codex, routed, saved, other];
    let payload =
        serde_json::to_value(phone_menu(rows, crate::http::mobile_wire::sample_logins())).unwrap();
    assert_eq!(payload[0]["logins"][1]["id"], "work");
    assert_eq!(payload[0]["logins"][2]["signed_in"], false);
    assert!(!payload.to_string().contains("private-home"));
    assert!(!payload.to_string().contains("private@example.test"));
    assert!(payload
        .as_array()
        .unwrap()
        .iter()
        .skip(1)
        .all(|row| row.get("logins").is_none()));
}

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
