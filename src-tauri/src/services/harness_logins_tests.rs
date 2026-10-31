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

/// `codex login` writes `auth.json` and then exits. The list polls the process
/// before it reads any `auth.json`, so a sign-in that finishes during the call
/// is reported finished *and* signed in rather than finished and signed out
/// (which would stop the Settings poll on a stale "Not signed in"). This test
/// pins the order; the write-before-exit order is Codex's.
#[test]
fn the_list_polls_the_sign_in_before_reading_auth_json() {
    let prefs = crate::preferences::AppPreferences {
        harness_logins: vec![HarnessLogin {
            id: "work".into(),
            harness_id: "codex".into(),
            label: "Work".into(),
            home: "C:/u/.codex-accounts/work".into(),
        }],
        ..Default::default()
    };
    let polled = std::cell::Cell::new(false);
    let reads_before_poll = std::cell::Cell::new(0);
    let list = list_with(
        &prefs,
        std::path::Path::new("C:/u/.codex"),
        || {
            polled.set(true);
            Some((
                "work".to_string(),
                SignInState {
                    running: false,
                    url: None,
                    error: None,
                },
            ))
        },
        |_home| {
            if !polled.get() {
                reads_before_poll.set(reads_before_poll.get() + 1);
            }
            login_identity_from_auth(&auth_json("w@example.com", "user-w"))
        },
    );
    assert_eq!(reads_before_poll.get(), 0, "auth.json read before the poll");
    let work = list.iter().find(|login| login.id == "work").unwrap();
    assert!(work.signed_in);
    assert_eq!(
        work.sign_in.as_ref().map(|state| state.running),
        Some(false)
    );
}

/// A failed sign-in keeps reporting its error (Settings may be closed when it
/// fails) until the user dismisses it; dismissing clears it for good.
#[test]
fn a_finished_sign_in_error_is_reported_until_dismissed() {
    let mut command = if cfg!(windows) {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "exit 1"]);
        command
    } else {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "exit 1"]);
        command
    };
    let mut child = command.spawn().expect("spawn a failing process");
    child.wait().expect("the process exits");
    let id = "dismiss-test";
    *lock() = Some(SignInSession {
        login_id: id.into(),
        child,
        #[cfg(target_os = "windows")]
        job: None,
        output: Arc::new(Mutex::new(SignInOutput {
            url: None,
            last_line: Some("Login failed".into()),
        })),
        started: Instant::now(),
        finished: None,
    });

    for _ in 0..2 {
        let (login, state) = poll_sign_in().expect("the failure is reported");
        assert_eq!(login, id);
        assert!(!state.running);
        assert_eq!(state.error.as_deref(), Some("Login failed"));
    }
    cancel_sign_in_blocking(id).unwrap();
    assert!(poll_sign_in().is_none(), "a dismissed sign-in is gone");
}
