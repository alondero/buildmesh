use super::*;
use crate::services::usage::adapters::anthropic::auth::FakeLookup;
use crate::services::usage::spawn_loopback;
use crate::services::usage::types::UsageMeter;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const CONSUMER_BODY: &str = r#"{
    "five_hour":{"utilization":41.0,"resets_at":"2026-05-30T21:30:00Z"},
    "seven_day":{"utilization":33.0,"resets_at":"2026-06-05T04:00:00Z"}
}"#;
const ENTERPRISE_SPEND_BODY: &str = r#"{
    "spend": {
        "used": {"amount_minor": 2500, "currency": "USD", "exponent": 2},
        "limit": {"amount_minor": 10000, "currency": "USD", "exponent": 2},
        "enabled": true
    }
}"#;
const OAUTH_JSON: &str =
    r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-secret","subscriptionType":"pro"}}"#;
const ENTERPRISE_JSON: &str =
    r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-ent","subscriptionType":"enterprise"}}"#;

fn with_file(json: &str) -> FakeLookup {
    let mut lookup = FakeLookup::default();
    lookup.files.insert(
        lookup.home.join(".claude").join(".credentials.json"),
        json.to_string(),
    );
    lookup
}

fn loopback_url(port: u16) -> String {
    static NEXT_PATH: AtomicUsize = AtomicUsize::new(0);
    let path = NEXT_PATH.fetch_add(1, Ordering::SeqCst);
    format!("http://127.0.0.1:{port}/test{path}/oauth/usage")
}

#[test]
fn consumer_oauth_fetch_keeps_windows() {
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_thread = Arc::clone(&hits);
    let port = spawn_loopback(2, move |request| {
        hits_thread.fetch_add(1, Ordering::SeqCst);
        let auth = request
            .headers()
            .iter()
            .find(|header| header.field.equiv("Authorization"))
            .map(|header| header.value.as_str().to_string())
            .unwrap_or_default();
        assert_eq!(auth, "Bearer sk-ant-oat01-secret");
        let body = if request.url().to_string().contains("/profile") {
            "{}"
        } else {
            CONSUMER_BODY
        };
        let _ = request.respond(tiny_http::Response::from_string(body).with_status_code(200));
    });

    let outcome = anthropic_usage_with(&with_file(OAUTH_JSON), &loopback_url(port));
    match &outcome {
        UsageOutcome::Reading {
            windows,
            meters,
            detail,
            ..
        } => {
            assert_eq!(windows.len(), 2);
            assert!(meters.is_empty());

            assert!(detail.is_none());
        }
        other => panic!("expected Reading outcome, got: {other:?}"),
    }

    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert!(!format!("{outcome:?}").contains("sk-ant-"));
}

#[test]
fn repeat_refresh_reuses_the_profile_scope_result() {
    let usage_hits = Arc::new(AtomicUsize::new(0));
    let profile_hits = Arc::new(AtomicUsize::new(0));
    let usage_hits_thread = Arc::clone(&usage_hits);
    let profile_hits_thread = Arc::clone(&profile_hits);
    let port = spawn_loopback(3, move |request| {
        if request.url().to_string().contains("/profile") {
            profile_hits_thread.fetch_add(1, Ordering::SeqCst);
            let _ = request.respond(tiny_http::Response::from_string("{}").with_status_code(200));
        } else {
            usage_hits_thread.fetch_add(1, Ordering::SeqCst);
            let _ = request
                .respond(tiny_http::Response::from_string(CONSUMER_BODY).with_status_code(200));
        }
    });

    let usage_url = loopback_url(port);
    for _ in 0..2 {
        let outcome = anthropic_usage_with(&with_file(OAUTH_JSON), &usage_url);
        assert!(matches!(outcome, UsageOutcome::Reading { .. }));
    }

    assert_eq!(usage_hits.load(Ordering::SeqCst), 2);
    assert_eq!(
        profile_hits.load(Ordering::SeqCst),
        1,
        "the token's profile-scope result must be reused rather than probed on every refresh"
    );
}

#[test]
fn rotated_tokens_do_not_reuse_the_profile_scope_result() {
    let usage_hits = Arc::new(AtomicUsize::new(0));
    let profile_hits = Arc::new(AtomicUsize::new(0));
    let usage_hits_thread = Arc::clone(&usage_hits);
    let profile_hits_thread = Arc::clone(&profile_hits);
    let port = spawn_loopback(4, move |request| {
        if request.url().to_string().contains("/profile") {
            profile_hits_thread.fetch_add(1, Ordering::SeqCst);
            let auth = request
                .headers()
                .iter()
                .find(|header| header.field.equiv("Authorization"))
                .map(|header| header.value.as_str().to_string())
                .unwrap_or_default();
            if auth == "Bearer sk-ant-oat01-rotated" {
                let _ = request.respond(
                    tiny_http::Response::from_string(
                        r#"{"type":"error","error":{"type":"permission_error","message":"OAuth token does not meet scope requirement user:profile"}}"#,
                    )
                    .with_status_code(403),
                );
            } else {
                let _ =
                    request.respond(tiny_http::Response::from_string("{}").with_status_code(200));
            }
        } else {
            usage_hits_thread.fetch_add(1, Ordering::SeqCst);
            let _ = request
                .respond(tiny_http::Response::from_string(CONSUMER_BODY).with_status_code(200));
        }
    });

    let usage_url = loopback_url(port);
    let first = anthropic_usage_with(&with_file(OAUTH_JSON), &usage_url);
    assert!(matches!(first, UsageOutcome::Reading { detail: None, .. }));

    let rotated_json = OAUTH_JSON.replace("sk-ant-oat01-secret", "sk-ant-oat01-rotated");
    let second = anthropic_usage_with(&with_file(&rotated_json), &usage_url);
    match second {
        UsageOutcome::Reading {
            detail: Some(detail),
            ..
        } => assert!(
            detail.contains("user:profile"),
            "rotated token must use its own profile-scope result; detail={detail:?}"
        ),
        other => panic!("expected scope-limited Reading outcome, got: {other:?}"),
    }

    assert_eq!(usage_hits.load(Ordering::SeqCst), 2);
    assert_eq!(profile_hits.load(Ordering::SeqCst), 2);
}

#[test]
fn enterprise_oauth_fetch_shows_spend() {
    let port = spawn_loopback(2, move |request| {
        let body = if request.url().to_string().contains("/profile") {
            "{}"
        } else {
            ENTERPRISE_SPEND_BODY
        };
        let _ = request.respond(tiny_http::Response::from_string(body).with_status_code(200));
    });
    let outcome = anthropic_usage_with(&with_file(ENTERPRISE_JSON), &loopback_url(port));
    match &outcome {
        UsageOutcome::Reading {
            windows,
            meters,
            detail,
            ..
        } => {
            assert!(windows.is_empty());
            match &meters[..] {
                [UsageMeter::Metered { amount }] => {
                    assert_eq!(amount.used, 25.0);
                    assert_eq!(amount.limit, Some(100.0));
                    assert_eq!(amount.remaining, Some(75.0));
                }
                other => panic!("expected enterprise spend, got {other:?}"),
            }
            assert!(detail.is_none());
        }
        other => panic!("expected Reading outcome, got: {other:?}"),
    }
}

#[test]
fn enterprise_oauth_fetch_uses_the_overage_reset_header() {
    let port = spawn_loopback(2, move |request| {
        let body = if request.url().to_string().contains("/profile") {
            "{}"
        } else {
            ENTERPRISE_SPEND_BODY
        };
        let header = tiny_http::Header::from_bytes(
            &b"anthropic-ratelimit-unified-overage-reset"[..],
            &b"1793491200"[..],
        )
        .expect("valid test header");
        let _ = request.respond(
            tiny_http::Response::from_string(body)
                .with_status_code(200)
                .with_header(header),
        );
    });
    let outcome = anthropic_usage_with(&with_file(ENTERPRISE_JSON), &loopback_url(port));
    match &outcome {
        UsageOutcome::Reading { meters, .. } => match &meters[..] {
            [UsageMeter::Metered { amount }] => {
                assert_eq!(
                    amount.resets_at.as_deref(),
                    Some("2026-11-01T00:00:00+00:00")
                );
            }
            other => panic!("expected enterprise spend, got {other:?}"),
        },
        other => panic!("expected Reading outcome, got: {other:?}"),
    }
}

#[test]
fn env_oauth_token_does_not_use_stale_login_file_plan() {
    let port = spawn_loopback(2, move |request| {
        let auth = request
            .headers()
            .iter()
            .find(|header| header.field.equiv("Authorization"))
            .map(|header| header.value.as_str().to_string())
            .unwrap_or_default();
        assert_eq!(auth, "Bearer sk-ant-oat01-env");
        let body = if request.url().to_string().contains("/profile") {
            "{}"
        } else {
            ENTERPRISE_SPEND_BODY
        };
        let _ = request.respond(tiny_http::Response::from_string(body).with_status_code(200));
    });
    let mut lookup = with_file(OAUTH_JSON);
    lookup
        .env
        .insert("CLAUDE_CODE_OAUTH_TOKEN".into(), "sk-ant-oat01-env".into());
    let outcome = anthropic_usage_with(&lookup, &loopback_url(port));
    match &outcome {
        UsageOutcome::Reading { meters, .. } => match &meters[..] {
            [UsageMeter::Metered { amount }] => {
                assert_eq!(amount.used, 25.0);
                assert_eq!(amount.limit, Some(100.0));
            }
            other => panic!("expected enterprise spend for env token, got {other:?}"),
        },
        other => panic!("expected Reading outcome, got: {other:?}"),
    }
}

#[test]
fn env_oauth_token_forbidden_profile_documents_setup_token_limit() {
    let port = spawn_loopback(2, move |request| {
        let path = request.url().to_string();
        if path.contains("/profile") {
            let _ = request.respond(
                tiny_http::Response::from_string(
                    r#"{"type":"error","error":{"type":"permission_error","message":"OAuth token does not meet scope requirement user:profile"}}"#,
                )
                .with_status_code(403),
            );
        } else {
            let _ = request.respond(
                tiny_http::Response::from_string(ENTERPRISE_SPEND_BODY).with_status_code(200),
            );
        }
    });
    let mut lookup = with_file(OAUTH_JSON);
    lookup.env.insert(
        "CLAUDE_CODE_OAUTH_TOKEN".into(),
        "sk-ant-oat01-setup".into(),
    );
    let outcome = anthropic_usage_with(&lookup, &loopback_url(port));
    match &outcome {
        UsageOutcome::Reading { meters, detail, .. } => {
            assert!(
                detail
                    .as_deref()
                    .unwrap_or_default()
                    .contains("user:profile"),
                "detail={detail:?}"
            );
            assert!(matches!(meters.first(), Some(UsageMeter::Metered { .. })));
        }
        other => panic!("expected Reading outcome, got: {other:?}"),
    }
}

#[test]
fn rejected_login_retries_active_user_oauth() {
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_thread = Arc::clone(&hits);
    let port = spawn_loopback(3, move |request| {
        let auth = request
            .headers()
            .iter()
            .find(|header| header.field.equiv("Authorization"))
            .map(|header| header.value.as_str().to_string())
            .unwrap_or_default();
        let n = hits_thread.fetch_add(1, Ordering::SeqCst);
        match n {
            0 => {
                assert_eq!(auth, "Bearer sk-ant-oat01-expired-login");
                let _ = request
                    .respond(tiny_http::Response::from_string("denied").with_status_code(401));
            }
            1 => {
                assert_eq!(auth, "Bearer sk-ant-oat01-profile");
                let _ = request.respond(
                    tiny_http::Response::from_string(ENTERPRISE_SPEND_BODY).with_status_code(200),
                );
            }
            _ => {
                assert_eq!(auth, "Bearer sk-ant-oat01-profile");
                let _ =
                    request.respond(tiny_http::Response::from_string("{}").with_status_code(200));
            }
        }
    });

    let mut lookup = FakeLookup::default();
    lookup.files.insert(
        lookup.home.join(".claude").join(".credentials.json"),
        r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-expired-login","subscriptionType":"pro"}}"#
            .into(),
    );
    let cfg = lookup.home.join(".config").join("anthropic");
    lookup.files.insert(
        cfg.join("configs").join("default.json"),
        r#"{"authentication":{"type":"user_oauth"}}"#.into(),
    );
    lookup.files.insert(
        cfg.join("credentials").join("default.json"),
        r#"{"access_token":"sk-ant-oat01-profile","subscriptionType":"enterprise"}"#.into(),
    );

    let outcome = anthropic_usage_with(&lookup, &loopback_url(port));
    match &outcome {
        UsageOutcome::Reading { meters, .. } => {
            assert!(matches!(meters.first(), Some(UsageMeter::Metered { .. })));
        }
        other => panic!("expected Reading outcome, got: {other:?}"),
    }
    assert_eq!(hits.load(Ordering::SeqCst), 3);
}

#[test]
fn usage_ok_profile_scope_failure_surfaces_limitation_detail() {
    let port = spawn_loopback(2, move |request| {
        let path = request.url().to_string();
        if path.contains("/profile") {
            let _ = request.respond(
                tiny_http::Response::from_string(
                    r#"{"type":"error","error":{"type":"permission_error","message":"OAuth token does not meet scope requirement user:profile"}}"#,
                )
                .with_status_code(403),
            );
        } else {
            let _ = request
                .respond(tiny_http::Response::from_string(CONSUMER_BODY).with_status_code(200));
        }
    });
    let outcome = anthropic_usage_with(&with_file(OAUTH_JSON), &loopback_url(port));
    match &outcome {
        UsageOutcome::Reading {
            windows, detail, ..
        } => {
            let detail = detail.as_deref().unwrap_or_default();
            assert!(
                detail.contains("user:profile"),
                "detail should name the missing scope; got detail={detail:?}"
            );

            assert_eq!(windows.len(), 2);
        }
        other => panic!("expected Reading outcome, got: {other:?}"),
    }
}

#[test]
fn usage_ok_profile_200_leaves_detail_unset() {
    let port = spawn_loopback(2, move |request| {
        let body = if request.url().to_string().contains("/profile") {
            r#"{"organization":{"organization_type":"claude_enterprise"}}"#
        } else {
            CONSUMER_BODY
        };
        let _ = request.respond(tiny_http::Response::from_string(body).with_status_code(200));
    });
    let outcome = anthropic_usage_with(&with_file(OAUTH_JSON), &loopback_url(port));
    match &outcome {
        UsageOutcome::Reading {
            windows, detail, ..
        } => {
            assert!(detail.is_none());
            assert_eq!(windows.len(), 2);
        }
        other => panic!("expected Reading outcome, got: {other:?}"),
    }
}

#[test]
fn extra_usage_fallback_is_fetched_when_spend_absent() {
    let body = r#"{
        "extra_usage": {
            "is_enabled": true,
            "monthly_limit": 2050,
            "used_credits": 325
        }
    }"#;
    let port = spawn_loopback(2, move |request| {
        let b = if request.url().to_string().contains("/profile") {
            "{}"
        } else {
            body
        };
        let _ = request.respond(tiny_http::Response::from_string(b).with_status_code(200));
    });
    let outcome = anthropic_usage_with(&with_file(ENTERPRISE_JSON), &loopback_url(port));
    match &outcome {
        UsageOutcome::Reading { meters, .. } => match &meters[..] {
            [UsageMeter::Metered { amount }] => {
                assert_eq!(amount.used, 3.25);
                assert_eq!(amount.limit, Some(20.5));
            }
            other => panic!("expected extra_usage fallback, got {other:?}"),
        },
        other => panic!("expected Reading outcome, got: {other:?}"),
    }
}

#[test]
fn bedrock_does_not_query_oauth_usage() {
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_thread = Arc::clone(&hits);
    let port = spawn_loopback(1, move |request| {
        hits_thread.fetch_add(1, Ordering::SeqCst);
        let _ = request
            .respond(tiny_http::Response::from_string("should not run").with_status_code(200));
    });
    let mut lookup = with_file(OAUTH_JSON);
    lookup
        .env
        .insert("CLAUDE_CODE_USE_BEDROCK".into(), "1".into());
    let outcome = anthropic_usage_with(&lookup, &loopback_url(port));
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    match &outcome {
        UsageOutcome::ManagedExternally { platform } => {
            assert_eq!(platform, "AWS Bedrock");
        }
        other => panic!("expected ManagedExternally outcome, got: {other:?}"),
    }
}

#[test]
fn vertex_and_foundry_are_externally_managed() {
    for (flag, platform) in [
        ("CLAUDE_CODE_USE_VERTEX", "Google Vertex AI"),
        ("CLAUDE_CODE_USE_FOUNDRY", "Microsoft Foundry"),
    ] {
        let mut lookup = with_file(OAUTH_JSON);
        lookup.env.insert(flag.into(), "1".into());
        let outcome = anthropic_usage_with(&lookup, "http://127.0.0.1:1/unused");
        match &outcome {
            UsageOutcome::ManagedExternally { platform: actual } => {
                assert_eq!(actual, platform);
            }
            other => panic!("expected ManagedExternally outcome, got: {other:?}"),
        }
    }
}

#[test]
fn authentication_failures_are_rejected() {
    for status in [401_u16, 403] {
        let port = spawn_loopback(1, move |request| {
            let _ = request
                .respond(tiny_http::Response::from_string("denied").with_status_code(status));
        });
        let outcome = anthropic_usage_with(&with_file(OAUTH_JSON), &loopback_url(port));
        match &outcome {
            UsageOutcome::Rejected { hint } => {
                assert!(hint.contains("login expired"), "{status}: {hint:?}");
            }
            other => panic!("status {status}: expected Rejected outcome, got: {other:?}"),
        }
    }
}

#[test]
fn env_token_401_guides_fresh_token_not_login() {
    let port = spawn_loopback(1, move |request| {
        let _ = request.respond(tiny_http::Response::from_string("denied").with_status_code(401));
    });
    let mut lookup = with_file(OAUTH_JSON);
    lookup
        .env
        .insert("CLAUDE_CODE_OAUTH_TOKEN".into(), "sk-ant-oat01-env".into());
    let outcome = anthropic_usage_with(&lookup, &loopback_url(port));
    match &outcome {
        UsageOutcome::Rejected { hint } => {
            assert!(
                hint.contains("CLAUDE_CODE_OAUTH_TOKEN"),
                "env-token 401 must mention the env var; hint={hint:?}"
            );
            assert!(
                !hint.contains("run /login"),
                "env-token 401 must not claim /login repairs CLAUDE_CODE_OAUTH_TOKEN"
            );
        }
        other => panic!("expected Rejected outcome, got: {other:?}"),
    }
}

#[test]
fn named_profile_401_guides_ant_auth_login_with_profile_name() {
    let port = spawn_loopback(1, move |request| {
        let _ = request.respond(tiny_http::Response::from_string("denied").with_status_code(401));
    });
    let mut lookup = FakeLookup::default();
    lookup.env.insert("ANTHROPIC_PROFILE".into(), "work".into());
    let cfg = lookup.home.join(".config").join("anthropic");
    lookup.files.insert(
        cfg.join("configs").join("work.json"),
        r#"{"authentication":{"type":"user_oauth"}}"#.into(),
    );
    lookup.files.insert(
        cfg.join("credentials").join("work.json"),
        r#"{"access_token":"sk-ant-oat01-profile","subscriptionType":"enterprise"}"#.into(),
    );
    let outcome = anthropic_usage_with(&lookup, &loopback_url(port));
    match &outcome {
        UsageOutcome::Rejected { hint } => {
            assert!(
                hint.contains("ant auth login --profile work"),
                "named-profile 401 must name the profile; hint={hint:?}"
            );
            assert!(
                !hint.contains("run /login"),
                "/login cannot repair ANTHROPIC_PROFILE credentials"
            );
        }
        other => panic!("expected Rejected outcome, got: {other:?}"),
    }
}

#[test]
fn usage_scope_forbidden_stays_logged_in_with_limitation() {
    let port = spawn_loopback(1, move |request| {
        assert!(!request.url().contains("/profile"));
        let _ = request.respond(
            tiny_http::Response::from_string(
                r#"{"type":"error","error":{"type":"permission_error","message":"OAuth token does not meet scope requirement user:profile"}}"#,
            )
            .with_status_code(403),
        );
    });
    let mut lookup = with_file(OAUTH_JSON);
    lookup.env.insert(
        "CLAUDE_CODE_OAUTH_TOKEN".into(),
        "sk-ant-oat01-setup".into(),
    );
    let outcome = anthropic_usage_with(&lookup, &loopback_url(port));
    match &outcome {
        UsageOutcome::Reading { detail, .. } => {
            let detail = detail.as_deref().unwrap_or_default();
            assert!(
                detail.contains("setup-token"),
                "env-token scope failure should mention setup-token; detail={detail:?}"
            );
            assert!(!detail.contains("login expired"));
        }
        other => panic!("expected Reading outcome, got: {other:?}"),
    }
}

#[test]
fn stored_login_scope_failure_guides_relogin_not_setup_token() {
    let port = spawn_loopback(1, move |request| {
        let _ = request.respond(
            tiny_http::Response::from_string(
                r#"{"type":"error","error":{"type":"permission_error","message":"OAuth token does not meet scope requirement user:profile"}}"#,
            )
            .with_status_code(403),
        );
    });
    let outcome = anthropic_usage_with(&with_file(OAUTH_JSON), &loopback_url(port));
    match &outcome {
        UsageOutcome::Reading { detail, .. } => {
            let detail = detail.as_deref().unwrap_or_default();
            assert!(
                detail.contains("run /login"),
                "stored login scope failure should recommend /login; detail={detail:?}"
            );
            assert!(
                !detail.contains("setup-token"),
                "stored login must not claim the setup-token limitation"
            );
        }
        other => panic!("expected Reading outcome, got: {other:?}"),
    }
}

#[test]
fn named_profile_scope_failure_guides_ant_auth_login() {
    let port = spawn_loopback(1, move |request| {
        let _ = request.respond(
            tiny_http::Response::from_string(
                r#"{"type":"error","error":{"type":"permission_error","message":"OAuth token does not meet scope requirement user:profile"}}"#,
            )
            .with_status_code(403),
        );
    });
    let mut lookup = FakeLookup::default();
    lookup.env.insert("ANTHROPIC_PROFILE".into(), "work".into());
    let cfg = lookup.home.join(".config").join("anthropic");
    lookup.files.insert(
        cfg.join("configs").join("work.json"),
        r#"{"authentication":{"type":"user_oauth"}}"#.into(),
    );
    lookup.files.insert(
        cfg.join("credentials").join("work.json"),
        r#"{"access_token":"sk-ant-oat01-profile","subscriptionType":"enterprise"}"#.into(),
    );
    let outcome = anthropic_usage_with(&lookup, &loopback_url(port));
    match &outcome {
        UsageOutcome::Reading { detail, .. } => {
            let detail = detail.as_deref().unwrap_or_default();
            assert!(
                detail.contains("ant auth login --profile work"),
                "named profile must include the actual profile name; detail={detail:?}"
            );
            assert!(
                !detail.contains("<name>"),
                "must not leave a placeholder profile name"
            );
            assert!(
                !detail.contains("run /login"),
                "/login cannot repair ANTHROPIC_PROFILE credentials"
            );
        }
        other => panic!("expected Reading outcome, got: {other:?}"),
    }
}

#[test]
fn active_named_profile_scope_failure_includes_actual_profile_name() {
    let port = spawn_loopback(1, move |request| {
        let _ = request.respond(
            tiny_http::Response::from_string(
                r#"{"type":"error","error":{"type":"permission_error","message":"OAuth token does not meet scope requirement user:profile"}}"#,
            )
            .with_status_code(403),
        );
    });
    let mut lookup = FakeLookup::default();
    let cfg = lookup.home.join(".config").join("anthropic");
    lookup
        .files
        .insert(cfg.join("active_config"), "agents".into());
    lookup.files.insert(
        cfg.join("configs").join("agents.json"),
        r#"{"authentication":{"type":"user_oauth"}}"#.into(),
    );
    lookup.files.insert(
        cfg.join("credentials").join("agents.json"),
        r#"{"access_token":"sk-ant-oat01-agents","subscriptionType":"enterprise"}"#.into(),
    );
    let outcome = anthropic_usage_with(&lookup, &loopback_url(port));
    match &outcome {
        UsageOutcome::Reading { detail, .. } => {
            let detail = detail.as_deref().unwrap_or_default();
            assert!(
                detail.contains("ant auth login --profile agents"),
                "active non-default profile must name itself; detail={detail:?}"
            );
        }
        other => panic!("expected Reading outcome, got: {other:?}"),
    }
}

#[test]
fn usage_401_with_scope_text_is_expired_not_scope_limitation() {
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_thread = Arc::clone(&hits);
    let port = spawn_loopback(2, move |request| {
        let auth = request
            .headers()
            .iter()
            .find(|header| header.field.equiv("Authorization"))
            .map(|header| header.value.as_str().to_string())
            .unwrap_or_default();
        let n = hits_thread.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            assert_eq!(auth, "Bearer sk-ant-oat01-secret");
            let _ = request.respond(
                tiny_http::Response::from_string(
                    r#"{"type":"error","error":{"type":"authentication_error","message":"OAuth token does not meet scope requirement user:profile"}}"#,
                )
                .with_status_code(401),
            );
        } else {
            assert_eq!(auth, "Bearer sk-ant-oat01-profile");
            let _ = request.respond(
                tiny_http::Response::from_string(ENTERPRISE_SPEND_BODY).with_status_code(200),
            );
        }
    });

    let mut lookup = with_file(OAUTH_JSON);
    let cfg = lookup.home.join(".config").join("anthropic");
    lookup.files.insert(
        cfg.join("configs").join("default.json"),
        r#"{"authentication":{"type":"user_oauth"}}"#.into(),
    );
    lookup.files.insert(
        cfg.join("credentials").join("default.json"),
        r#"{"access_token":"sk-ant-oat01-profile","subscriptionType":"enterprise"}"#.into(),
    );

    let outcome = anthropic_usage_with(&lookup, &loopback_url(port));
    match &outcome {
        UsageOutcome::Reading { detail, .. } => {
            assert!(
                !detail
                    .as_deref()
                    .unwrap_or_default()
                    .contains("user:profile"),
                "401-with-scope-text must retry as expired, not surface a scope limitation"
            );
        }
        other => panic!("expected Reading outcome, got: {other:?}"),
    }
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[test]
fn generic_permission_error_is_not_a_scope_failure() {
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_thread = Arc::clone(&hits);
    let port = spawn_loopback(2, move |request| {
        let auth = request
            .headers()
            .iter()
            .find(|header| header.field.equiv("Authorization"))
            .map(|header| header.value.as_str().to_string())
            .unwrap_or_default();
        let n = hits_thread.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            assert_eq!(auth, "Bearer sk-ant-oat01-secret");
            let _ = request.respond(
                tiny_http::Response::from_string(
                    r#"{"type":"error","error":{"type":"permission_error","message":"Request not allowed"}}"#,
                )
                .with_status_code(403),
            );
        } else {
            assert_eq!(auth, "Bearer sk-ant-oat01-profile");
            let _ = request.respond(
                tiny_http::Response::from_string(ENTERPRISE_SPEND_BODY).with_status_code(200),
            );
        }
    });

    let mut lookup = with_file(OAUTH_JSON);
    let cfg = lookup.home.join(".config").join("anthropic");
    lookup.files.insert(
        cfg.join("configs").join("default.json"),
        r#"{"authentication":{"type":"user_oauth"}}"#.into(),
    );
    lookup.files.insert(
        cfg.join("credentials").join("default.json"),
        r#"{"access_token":"sk-ant-oat01-profile","subscriptionType":"enterprise"}"#.into(),
    );

    let outcome = anthropic_usage_with(&lookup, &loopback_url(port));
    match &outcome {
        UsageOutcome::Reading { meters, detail, .. } => {
            assert!(
                !detail
                    .as_deref()
                    .unwrap_or_default()
                    .contains("setup-token"),
                "generic permission error must not surface a scope limitation"
            );
            assert!(matches!(meters.first(), Some(UsageMeter::Metered { .. })));
        }
        other => panic!("expected Reading outcome, got: {other:?}"),
    }
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[test]
fn rate_limit_returns_rate_limited_parse_failure_returns_unavailable() {
    let port_429 = spawn_loopback(1, |request| {
        let _ =
            request.respond(tiny_http::Response::from_string("slow down").with_status_code(429));
    });
    match anthropic_usage_with(&with_file(OAUTH_JSON), &loopback_url(port_429)) {
        UsageOutcome::RateLimited { reason } => {
            assert!(
                reason.contains("Rate limited"),
                "rate-limit copy must be preserved, got: {reason:?}"
            );
        }
        other => panic!("expected RateLimited outcome, got: {other:?}"),
    }

    let port_bad = spawn_loopback(1, |request| {
        let _ = request.respond(tiny_http::Response::from_string("{nope").with_status_code(200));
    });
    match anthropic_usage_with(&with_file(OAUTH_JSON), &loopback_url(port_bad)) {
        UsageOutcome::Unavailable { reason } => {
            assert!(
                reason.contains("Failed to parse"),
                "parse envelope must be preserved, got: {reason:?}"
            );
        }
        other => panic!("expected Unavailable outcome, got: {other:?}"),
    }
}

#[test]
fn missing_credential_does_not_hit_the_network() {
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_thread = Arc::clone(&hits);
    let port = spawn_loopback(1, move |request| {
        hits_thread.fetch_add(1, Ordering::SeqCst);
        let _ = request.respond(tiny_http::Response::from_string("{}").with_status_code(200));
    });

    match anthropic_usage_with(&FakeLookup::default(), &loopback_url(port)) {
        UsageOutcome::NoCredential { .. } => {}
        other => panic!("expected NoCredential outcome, got: {other:?}"),
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[test]
fn adapter_does_not_spawn_claude_cli() {
    let src = include_str!("mod.rs");
    assert!(
        !src.contains("command_no_window(\"claude\")"),
        "do not spawn the Claude CLI for usage"
    );
    assert!(
        !src.contains("Command::new(\"claude\")"),
        "do not spawn the Claude CLI for usage"
    );
}
