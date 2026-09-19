//! Anthropic (Claude Code) native adapter.
//!
//! Resolves Claude's active authentication source, then either reports an
//! externally managed cloud/API state or queries `GET /api/oauth/usage`
//! directly. The Claude CLI is never spawned for the meter.

mod auth;
mod parse;

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{shared_client, UsageAdapter, UsageIdentityFingerprint};
use crate::services::usage::outcome::UsageOutcome;

use auth::{
    active_user_oauth, anthropic_config_dir_for, cache_identity_from, resolve_claude_auth,
    AuthLookup, ClaudeAuthSource, OauthOrigin, ProductionLookup,
};
use parse::parse_anthropic_usage;

const PROVIDER: &str = "anthropic";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
/// Env / setup-token path: inference-only tokens lack `user:profile`.
const PLAN_SCOPE_DETAIL_ENV: &str = "Plan unavailable — token lacks user:profile scope (`claude setup-token` is model-request-only; run /login for full OAuth)";
/// Claude Code `/login` store tokens that predate the scope.
const PLAN_SCOPE_DETAIL_RELOGIN: &str =
    "Plan unavailable — OAuth token lacks user:profile scope; run /login to refresh credentials";

fn scope_limitation_detail(origin: &OauthOrigin) -> String {
    match origin {
        OauthOrigin::Env => PLAN_SCOPE_DETAIL_ENV.to_string(),
        OauthOrigin::Login => PLAN_SCOPE_DETAIL_RELOGIN.to_string(),
        OauthOrigin::NamedProfile { name } | OauthOrigin::ActiveProfile { name } => {
            format!(
                "Plan unavailable — OAuth token lacks user:profile scope; re-authenticate the Anthropic profile with `ant auth login --profile {name}`"
            )
        }
    }
}

/// Remediation for expired/revoked credentials (non-scope 401/403).
fn auth_failure_detail(origin: &OauthOrigin) -> String {
    match origin {
        OauthOrigin::Env => {
            "Claude OAuth token rejected — set a fresh CLAUDE_CODE_OAUTH_TOKEN or unset it to use /login credentials"
                .to_string()
        }
        OauthOrigin::Login => {
            "Claude login expired — run /login in the Claude CLI".to_string()
        }
        OauthOrigin::NamedProfile { name } | OauthOrigin::ActiveProfile { name } => {
            format!(
                "Anthropic profile `{name}` authentication failed — run `ant auth login --profile {name}`"
            )
        }
    }
}

pub(crate) struct AnthropicAdapter;

impl UsageAdapter for AnthropicAdapter {
    fn id(&self) -> &'static str {
        PROVIDER
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("anthropic")
    }

    fn cache_identity(&self, _accounts: &[ProviderAccount]) -> UsageIdentityFingerprint {
        cache_identity_from(&resolve_claude_auth(&ProductionLookup))
    }

    // Issue #1745 phase 2 step 19: anthropic migrated to the outcome
    // seam. The bespoke `classify_oauth_auth_failure` scope-403-vs-401
    // distinction stays in the adapter (provider-side body parsing, not
    // a seam concern); it now classifies into `UsageOutcome` variants.
    // Externally managed auth (Bedrock / Vertex / Foundry) reports
    // `ManagedExternally`; missing credential reports `NoCredential`.
    fn fetch(&self, _accounts: &[ProviderAccount]) -> UsageOutcome {
        anthropic_usage_with(&ProductionLookup, USAGE_URL)
    }
}

fn anthropic_usage_with(lookup: &impl AuthLookup, usage_url: &str) -> UsageOutcome {
    anthropic_usage_with_urls(lookup, usage_url, &profile_url_for(usage_url))
}

fn anthropic_usage_with_urls(
    lookup: &impl AuthLookup,
    usage_url: &str,
    profile_url: &str,
) -> UsageOutcome {
    match resolve_claude_auth(lookup) {
        ClaudeAuthSource::Managed { platform, .. } => UsageOutcome::ManagedExternally {
            platform: platform.to_string(),
        },
        ClaudeAuthSource::Missing { error } => UsageOutcome::NoCredential {
            hint: error.to_string(),
        },
        ClaudeAuthSource::Oauth {
            token, origin, ..
        } => {
            let outcome = fetch_oauth_usage(usage_url, profile_url, &token, origin.clone());
            if matches!(outcome, UsageOutcome::Rejected { .. }) && origin == OauthOrigin::Login {
                if let Some(ClaudeAuthSource::Oauth {
                    token: fallback_token,
                    origin: fallback_origin,
                    ..
                }) = active_user_oauth(lookup, &anthropic_config_dir_for(lookup))
                {
                    if fallback_token != token {
                        return fetch_oauth_usage(
                            usage_url,
                            profile_url,
                            &fallback_token,
                            fallback_origin,
                        );
                    }
                }
            }
            outcome
        }
    }
}

fn profile_url_for(usage_url: &str) -> String {
    if usage_url == USAGE_URL {
        return PROFILE_URL.to_string();
    }
    if let Some(base) = usage_url.strip_suffix("/oauth/usage") {
        return format!("{base}/oauth/profile");
    }
    if let Some(base) = usage_url.strip_suffix("/usage") {
        return format!("{base}/profile");
    }
    PROFILE_URL.to_string()
}

fn fetch_oauth_usage(
    usage_url: &str,
    profile_url: &str,
    token: &str,
    origin: OauthOrigin,
) -> UsageOutcome {
    let client = match shared_client() {
        Ok(client) => client,
        Err(error) => return UsageOutcome::Unavailable { reason: error },
    };

    let request = client
        .get(usage_url)
        .header("Authorization", format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20");

    match request.send() {
        Ok(response) if response.status().as_u16() == 401 || response.status().as_u16() == 403 => {
            let status = response.status().as_u16();
            let body = response.text().unwrap_or_default();
            classify_oauth_auth_failure(status, &body, &origin)
        }
        Ok(response) if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS => {
            UsageOutcome::RateLimited {
                reason: "Rate limited — usage data temporarily unavailable".to_string(),
            }
        }
        Ok(response) if !response.status().is_success() => {
            let code = response.status().as_u16();
            UsageOutcome::Unavailable {
                reason: format!("API error {}: {}", code, response.text().unwrap_or_default()),
            }
        }
        Ok(response) => match parse_anthropic_usage(&response.text().unwrap_or_default()) {
            Ok(parsed) => {
                // Always probe /oauth/profile, even when /usage succeeded:
                // a token may lack `user:profile` while still returning a
                // 200 from /usage with whatever default data Anthropic
                // chooses to surface. The profile endpoint catches the
                // missing scope precisely via 403 + scope body. Performance
                // is one extra HTTP round-trip per meter refresh — the
                // trade was made deliberately in #1689 / #1694.
                let detail = if oauth_profile_scope_failure(&client, profile_url, token) {
                    Some(scope_limitation_detail(&origin))
                } else {
                    None
                };
                UsageOutcome::Reading {
                    windows: parsed.windows,
                    balance: None,
                    meters: parsed.meters,
                    detail,
                }
            }
            Err(error) => UsageOutcome::Unavailable {
                reason: format!("Failed to parse response: {error}"),
            },
        },
        Err(error) => UsageOutcome::Unavailable {
            reason: format!("Request failed: {error}"),
        },
    }
}

/// Hit `/oauth/profile` and report whether the response is a 403 with the
/// `user:profile` scope-failure body. Used to surface the OAuth scope
/// limitation message in `usage.detail` even when /usage itself
/// returned 200 (a token can succeed at /usage while missing the
/// profile scope; the profile endpoint is the precise signal).
fn oauth_profile_scope_failure(
    client: &reqwest::blocking::Client,
    profile_url: &str,
    token: &str,
) -> bool {
    let response = match client
        .get(profile_url)
        .header("Authorization", format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
    {
        Ok(response) => response,
        Err(_) => return false,
    };
    if response.status().as_u16() != 403 {
        return false;
    }
    let body = response.text().unwrap_or_default();
    is_oauth_scope_failure(403, &body)
}

/// Distinguish expired/revoked credentials from missing `user:profile` scope.
/// Scope failures are HTTP 403 with the explicit scope message and surface
/// as `Reading` with `detail` (the meter keeps working; only the plan
/// lookup is limited); 401 (even with similar text) is `Rejected` so the
/// active-profile retry can run.
fn classify_oauth_auth_failure(status: u16, body: &str, origin: &OauthOrigin) -> UsageOutcome {
    if is_oauth_scope_failure(status, body) {
        return UsageOutcome::Reading {
            windows: vec![],
            balance: None,
            meters: vec![],
            detail: Some(scope_limitation_detail(origin)),
        };
    }
    UsageOutcome::Rejected {
        hint: auth_failure_detail(origin),
    }
}

fn is_oauth_scope_failure(status: u16, body: &str) -> bool {
    // Anthropic documents the missing-scope case as HTTP 403 with an explicit
    // `user:profile` scope message. 401 is authentication failure / expired.
    if status != 403 {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    lower.contains("scope requirement") && lower.contains("user:profile")
}

#[cfg(test)]
mod tests {
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
        format!("http://127.0.0.1:{port}/oauth/usage")
    }

    // After #1689 the Anthropic adapter always issues a second HTTP
    // request to /oauth/profile for scope-failure detection. Most tests
    // don't care about the profile response; the test loopback answers
    // it with a 200 + empty body (which `oauth_profile_scope_failure`
    // interprets as "no scope failure, no detail to surface"). Tests
    // that need a 403-from-profile path override the closure.

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
                windows, meters, detail, ..
            } => {
                assert_eq!(windows.len(), 2);
                assert!(meters.is_empty());
                // /profile returned 200, so detail stays unset.
                assert!(detail.is_none());
            }
            other => panic!("expected Reading outcome, got: {other:?}"),
        }
        // /usage + /profile both succeed.
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert!(!format!("{outcome:?}").contains("sk-ant-"));
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
                windows, meters, detail, ..
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
    fn env_oauth_token_does_not_use_stale_login_file_plan() {
        // Stale local login file is "pro"; the env token is a separate
        // credential. After #1689 the plan label is no longer surfaced,
        // so this test now only asserts that the env token's own spend
        // body drives the meter (no leakage from the stale login).
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
            UsageOutcome::Reading { meters, .. } => {
                match &meters[..] {
                    [UsageMeter::Metered { amount }] => {
                        assert_eq!(amount.used, 25.0);
                        assert_eq!(amount.limit, Some(100.0));
                    }
                    other => panic!("expected enterprise spend for env token, got {other:?}"),
                }
            }
            other => panic!("expected Reading outcome, got: {other:?}"),
        }
    }

    #[test]
    fn env_oauth_token_forbidden_profile_documents_setup_token_limit() {
        // Realistic setup-token shape: usage/spend may succeed while profile
        // returns 403 (missing user:profile). Plan stays blank with an explicit
        // limitation — we do not invent Enterprise or borrow a stale local plan.
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
                    detail.as_deref().unwrap_or_default().contains("user:profile"),
                    "detail={detail:?}"
                );
                assert!(matches!(
                    meters.first(),
                    Some(UsageMeter::Metered { .. })
                ));
            }
            other => panic!("expected Reading outcome, got: {other:?}"),
        }
    }

    #[test]
    fn rejected_login_retries_active_user_oauth() {
        // First /usage call uses the expired login token (401). The
        // adapter then retries with the active user_oauth profile and
        // succeeds, hitting /usage again. After the successful /usage,
        // /profile is also called for scope-failure detection. Total
        // hits: 3.
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
                        tiny_http::Response::from_string(ENTERPRISE_SPEND_BODY)
                            .with_status_code(200),
                    );
                }
                _ => {
                    // /profile from the retry path
                    assert_eq!(auth, "Bearer sk-ant-oat01-profile");
                    let _ = request
                        .respond(tiny_http::Response::from_string("{}").with_status_code(200));
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
                assert!(matches!(
                    meters.first(),
                    Some(UsageMeter::Metered { .. })
                ));
            }
            other => panic!("expected Reading outcome, got: {other:?}"),
        }
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    // Spec test for #1689: /usage may return 200 with valid data while
    // /oauth/profile returns 403 missing user:profile. The adapter must
    // surface the OAuth scope-limitation message in `detail` even when
    // /usage itself returned success — that's the new behaviour this PR
    // introduces (the unconditional /profile probe).
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
                let _ = request.respond(
                    tiny_http::Response::from_string(CONSUMER_BODY).with_status_code(200),
                );
            }
        });
        let outcome = anthropic_usage_with(&with_file(OAUTH_JSON), &loopback_url(port));
        match &outcome {
            UsageOutcome::Reading { windows, detail, .. } => {
                let detail = detail.as_deref().unwrap_or_default();
                assert!(
                    detail.contains("user:profile"),
                    "detail should name the missing scope; got detail={detail:?}"
                );
                // The windows from the successful /usage call are still surfaced.
                assert_eq!(windows.len(), 2);
            }
            other => panic!("expected Reading outcome, got: {other:?}"),
        }
    }

    // Negative companion: /profile returns 200 with a body that does NOT
    // match the scope-failure predicate. detail stays unset.
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
            UsageOutcome::Reading { windows, detail, .. } => {
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
            UsageOutcome::Reading { meters, .. } => {
                match &meters[..] {
                    [UsageMeter::Metered { amount }] => {
                        assert_eq!(amount.used, 3.25);
                        assert_eq!(amount.limit, Some(20.5));
                    }
                    other => panic!("expected extra_usage fallback, got {other:?}"),
                }
            }
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
                    assert!(
                        hint.contains("login expired"),
                        "{status}: {hint:?}"
                    );
                }
                other => panic!("status {status}: expected Rejected outcome, got: {other:?}"),
            }
        }
    }

    #[test]
    fn env_token_401_guides_fresh_token_not_login() {
        let port = spawn_loopback(1, move |request| {
            let _ =
                request.respond(tiny_http::Response::from_string("denied").with_status_code(401));
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
            let _ =
                request.respond(tiny_http::Response::from_string("denied").with_status_code(401));
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
        // Anthropic returns 403 from /usage when the token lacks user:profile
        // (common for setup-token). That is not an expired login.
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
        // 401 is authentication failure even if the body mentions user:profile.
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
                    !detail.as_deref().unwrap_or_default().contains("user:profile"),
                    "401-with-scope-text must retry as expired, not surface a scope limitation"
                );
            }
            other => panic!("expected Reading outcome, got: {other:?}"),
        }
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn generic_permission_error_is_not_a_scope_failure() {
        // A bare permission_error without the user:profile scope message must
        // stay logged_out so active-profile retry can still run.
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
                    !detail.as_deref().unwrap_or_default().contains("setup-token"),
                    "generic permission error must not surface a scope limitation"
                );
                assert!(matches!(
                    meters.first(),
                    Some(UsageMeter::Metered { .. })
                ));
            }
            other => panic!("expected Reading outcome, got: {other:?}"),
        }
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn rate_limit_returns_rate_limited_parse_failure_returns_unavailable() {
        let port_429 = spawn_loopback(1, |request| {
            let _ = request
                .respond(tiny_http::Response::from_string("slow down").with_status_code(429));
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
            let _ =
                request.respond(tiny_http::Response::from_string("{nope").with_status_code(200));
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
        // (`dispatch("anthropic").fetch` is not hermetic here: it reads
        // the real production credential sources. `anthropic_usage_with`
        // with a `FakeLookup` is the hermetic seam-level equivalent.)
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
}
