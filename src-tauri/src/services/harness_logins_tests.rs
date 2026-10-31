//! Harness Login service: sign-in output parsing, duplicate-account
//! detection, login identity from `auth.json`, and the add/rename/remove
//! preference writes.

use super::*;
use crate::services::usage::adapters::codex::login_identity_from_auth;
use base64::Engine;

fn jwt(claims: serde_json::Value) -> String {
    let encode = |value: &serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
    };
    format!(
        "{}.{}.sig",
        encode(&serde_json::json!({"alg": "none"})),
        encode(&claims)
    )
}

fn auth_json(email: &str, user: &str) -> String {
    auth_json_in(email, user, "acct")
}

fn auth_json_in(email: &str, user: &str, account: &str) -> String {
    serde_json::json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": jwt(serde_json::json!({
                "email": email,
                "sub": "auth0|ignored-when-user-id-present",
                "https://api.openai.com/auth": {
                    "chatgpt_plan_type": "pro",
                    "chatgpt_user_id": user,
                    "chatgpt_account_id": account
                }
            })),
            "access_token": "at",
            "refresh_token": "rt",
            "account_id": account
        }
    })
    .to_string()
}

#[test]
fn the_auth_url_is_found_in_codex_login_output() {
    let line = "If your browser did not open, navigate to this URL to authenticate: https://auth.openai.com/oauth/authorize?client_id=app&state=x";
    assert_eq!(
        auth_url_in(line).as_deref(),
        Some("https://auth.openai.com/oauth/authorize?client_id=app&state=x")
    );
    assert_eq!(
        auth_url_in("Starting local login server on http://localhost:1455"),
        None
    );
    assert_eq!(auth_url_in("see https://example.com/help"), None);
}

#[test]
fn identity_reads_email_plan_and_person_from_the_id_token() {
    let identity = login_identity_from_auth(&auth_json("a@example.com", "user-a"));
    assert!(identity.signed_in);
    assert_eq!(identity.email.as_deref(), Some("a@example.com"));
    assert_eq!(identity.plan.as_deref(), Some("pro"));
    assert_eq!(identity.subscription.as_deref(), Some("user-a|acct"));
}

#[test]
fn a_logged_out_or_unreadable_home_is_not_signed_in() {
    let logged_out = r#"{"tokens":{"access_token":"","id_token":"x.y.z"}}"#;
    assert!(!login_identity_from_auth(logged_out).signed_in);
    assert!(!login_identity_from_auth("not json").signed_in);
    // A token without an id token still signs in; it just has no label.
    let bare = login_identity_from_auth(r#"{"access_token":"at"}"#);
    assert!(bare.signed_in);
    assert_eq!(bare.email, None);
}

#[test]
fn two_logins_signed_in_as_the_same_person_point_at_each_other() {
    let labels = vec![
        (Some("user-a".to_string()), "Default".to_string()),
        (Some("user-b".to_string()), "Work".to_string()),
        (Some("user-a".to_string()), "Personal".to_string()),
        (None, "Not signed in".to_string()),
    ];
    assert_eq!(
        duplicate_of(0, Some("user-a"), &labels).as_deref(),
        Some("Personal")
    );
    assert_eq!(
        duplicate_of(2, Some("user-a"), &labels).as_deref(),
        Some("Default")
    );
    assert_eq!(duplicate_of(1, Some("user-b"), &labels), None);
    assert_eq!(duplicate_of(3, None, &labels), None);
}

#[test]
fn add_rename_and_remove_write_preferences_and_keep_the_folder() {
    let root = tempfile::tempdir().unwrap();
    crate::preferences::init_for_tests(root.path().join("prefs"));
    let user_home = root.path().join("home");
    // `add_blocking` places the folder under the user's home; point it at the
    // temp directory for this thread.
    let default_home = root.path().join(".codex");
    let added = add_in(&user_home, &default_home, "Work").unwrap();
    assert_eq!(added.id, "work");
    assert!(std::path::Path::new(&added.home).is_dir());
    assert!(add_in(&user_home, &default_home, "work")
        .unwrap_err()
        .contains("already exists"));

    let renamed = rename_blocking("work", "Work Pro").unwrap();
    assert_eq!(renamed.label, "Work Pro");
    assert_eq!(renamed.home, added.home, "renaming never moves the home");

    // A configuration pinned to the login is unpinned when it goes.
    crate::preferences::storage::try_update(|prefs| {
        prefs.spawn_configurations.push(
            crate::preferences::spawn_configurations::SpawnConfiguration {
                id: "launch/codex-work".into(),
                name: "Work".into(),
                spawn_option_id: "codex".into(),
                login_id: Some("work".into()),
                ..Default::default()
            },
        );
        Ok(())
    })
    .unwrap();
    remove_blocking("work").unwrap();
    let prefs = crate::preferences::load().unwrap();
    assert!(prefs.harness_logins.is_empty());
    assert_eq!(prefs.spawn_configurations[0].login_id, None);
    assert!(
        std::path::Path::new(&added.home).is_dir(),
        "removing a login keeps its folder and sessions"
    );
    assert!(remove_blocking("work").is_err());
}

/// One person can hold a personal Pro subscription and a seat in a Team
/// workspace: those are two subscriptions, so only the same person in the
/// same workspace counts as the same account.
#[test]
fn the_same_person_in_another_workspace_is_a_different_subscription() {
    let personal =
        login_identity_from_auth(&auth_json_in("a@example.com", "user-a", "acct-personal"));
    let team = login_identity_from_auth(&auth_json_in("a@example.com", "user-a", "acct-team"));
    let again = login_identity_from_auth(&auth_json_in("a@example.com", "user-a", "acct-personal"));
    assert_ne!(personal.subscription, team.subscription);
    assert_eq!(personal.subscription, again.subscription);
    assert!(personal.subscription.is_some());
}
