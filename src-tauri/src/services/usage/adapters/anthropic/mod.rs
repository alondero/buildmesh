//! Anthropic (Claude Code) native adapter.
//!
//! Resolves Claude's active authentication source, then either reports an
//! externally managed cloud/API state or queries `GET /api/oauth/usage`
//! directly. The Claude CLI is never spawned for the meter.

mod auth;
mod parse;

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{shared_client, UsageAdapter, UsageIdentityFingerprint};
use crate::services::usage::types::{logged_out, unavailable, ProviderUsage, UsageMeter};

use auth::{
    active_user_oauth, anthropic_config_dir_for, cache_identity_from, resolve_claude_auth,
    AuthLookup, ClaudeAuthSource, OauthOrigin, ProductionLookup,
};
use parse::{parse_anthropic_usage, parse_oauth_profile_plan};

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

    fn fetch(&self, _accounts: &[ProviderAccount]) -> ProviderUsage {
        anthropic_usage_with(&ProductionLookup, USAGE_URL)
    }
}

fn anthropic_usage_with(lookup: &impl AuthLookup, usage_url: &str) -> ProviderUsage {
    anthropic_usage_with_urls(lookup, usage_url, &profile_url_for(usage_url))
}

fn anthropic_usage_with_urls(
    lookup: &impl AuthLookup,
    usage_url: &str,
    profile_url: &str,
) -> ProviderUsage {
    match resolve_claude_auth(lookup) {
        ClaudeAuthSource::Managed { platform, .. } => ProviderUsage {
            provider: PROVIDER.to_string(),
            logged_in: true,
            windows: vec![],
            balance: None,
            plan: None,
            meters: vec![UsageMeter::ManagedExternally {
                platform: platform.to_string(),
            }],
            detail: None,
            error: None,
        },
        ClaudeAuthSource::Missing { error } => logged_out(PROVIDER, error.to_string()),
        ClaudeAuthSource::Oauth {
            token,
            plan,
            origin,
        } => {
            let usage = fetch_oauth_usage(usage_url, profile_url, &token, plan, origin.clone());
            if !usage.logged_in && origin == OauthOrigin::Login {
                if let Some(ClaudeAuthSource::Oauth {
                    token: fallback_token,
                    plan: fallback_plan,
                    origin: fallback_origin,
                }) = active_user_oauth(lookup, &anthropic_config_dir_for(lookup))
                {
                    if fallback_token != token {
                        return fetch_oauth_usage(
                            usage_url,
                            profile_url,
                            &fallback_token,
                            fallback_plan,
                            fallback_origin,
                        );
                    }
                }
            }
            usage
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
    known_plan: Option<String>,
    origin: OauthOrigin,
) -> ProviderUsage {
    let client = match shared_client() {
        Ok(client) => client,
        Err(error) => return unavailable(PROVIDER, error),
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
        Ok(response) if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS => unavailable(
            PROVIDER,
            "Rate limited — usage data temporarily unavailable".to_string(),
        ),
        Ok(response) if !response.status().is_success() => {
            let code = response.status().as_u16();
            unavailable(
                PROVIDER,
                format!(
                    "API error {}: {}",
                    code,
                    response.text().unwrap_or_default()
                ),
            )
        }
        Ok(response) => match parse_anthropic_usage(&response.text().unwrap_or_default()) {
            Ok(parsed) => {
                let mut plan = known_plan.or(parsed.plan);
                let mut detail = None;
                if plan.is_none() {
                    match fetch_oauth_plan(&client, profile_url, token) {
                        ProfilePlan::Found(label) => plan = Some(label),
                        ProfilePlan::Forbidden => {
                            detail = Some(scope_limitation_detail(&origin));
                        }
                        ProfilePlan::Unavailable => {}
                    }
                }
                ProviderUsage {
                    provider: PROVIDER.to_string(),
                    logged_in: true,
                    windows: parsed.windows,
                    balance: None,
                    plan,
                    meters: parsed.meters,
                    detail,
                    error: None,
                }
            }
            Err(error) => unavailable(PROVIDER, format!("Failed to parse response: {error}")),
        },
        Err(error) => unavailable(PROVIDER, format!("Request failed: {error}")),
    }
}

enum ProfilePlan {
    Found(String),
    Forbidden,
    Unavailable,
}

/// Distinguish expired/revoked credentials from missing `user:profile` scope.
/// Scope failures are HTTP 403 with the explicit scope message and keep the
/// account logged in; 401 (even with similar text) remains `logged_out` so
/// active-profile retry can run.
fn classify_oauth_auth_failure(status: u16, body: &str, origin: &OauthOrigin) -> ProviderUsage {
    if is_oauth_scope_failure(status, body) {
        let detail = scope_limitation_detail(origin);
        let mut usage = unavailable(PROVIDER, detail.clone());
        usage.detail = Some(detail);
        return usage;
    }
    logged_out(
        PROVIDER,
        "Claude login expired — run /login in the Claude CLI".to_string(),
    )
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

fn fetch_oauth_plan(
    client: &reqwest::blocking::Client,
    profile_url: &str,
    token: &str,
) -> ProfilePlan {
    let Ok(response) = client
        .get(profile_url)
        .header("Authorization", format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
    else {
        return ProfilePlan::Unavailable;
    };
    let status = response.status().as_u16();
    let body = response.text().unwrap_or_default();
    if status == 401 || status == 403 {
        return if is_oauth_scope_failure(status, &body) {
            ProfilePlan::Forbidden
        } else {
            ProfilePlan::Unavailable
        };
    }
    if !(200..300).contains(&status) {
        return ProfilePlan::Unavailable;
    }
    match parse_oauth_profile_plan(&body) {
        Some(label) => ProfilePlan::Found(label),
        None => ProfilePlan::Unavailable,
    }
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

    #[test]
    fn consumer_oauth_fetch_keeps_windows_and_plan() {
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_thread = Arc::clone(&hits);
        let port = spawn_loopback(1, move |request| {
            hits_thread.fetch_add(1, Ordering::SeqCst);
            let auth = request
                .headers()
                .iter()
                .find(|header| header.field.equiv("Authorization"))
                .map(|header| header.value.as_str().to_string())
                .unwrap_or_default();
            assert_eq!(auth, "Bearer sk-ant-oat01-secret");
            let _ = request.respond(
                tiny_http::Response::from_string(CONSUMER_BODY)
                    .with_status_code(200)
                    .with_header(
                        tiny_http::Header::from_bytes(
                            &b"Content-Type"[..],
                            &b"application/json"[..],
                        )
                        .unwrap(),
                    ),
            );
        });

        let usage = anthropic_usage_with(&with_file(OAUTH_JSON), &loopback_url(port));
        assert!(usage.logged_in);
        assert_eq!(usage.plan.as_deref(), Some("Pro"));
        assert_eq!(usage.windows.len(), 2);
        assert!(usage.meters.is_empty());
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert!(!format!("{usage:?}").contains("sk-ant-"));
    }

    #[test]
    fn enterprise_oauth_fetch_shows_plan_and_spend() {
        let port = spawn_loopback(1, move |request| {
            let _ = request.respond(
                tiny_http::Response::from_string(ENTERPRISE_SPEND_BODY).with_status_code(200),
            );
        });
        let usage = anthropic_usage_with(&with_file(ENTERPRISE_JSON), &loopback_url(port));
        assert_eq!(usage.plan.as_deref(), Some("Enterprise"));
        assert!(usage.windows.is_empty());
        match &usage.meters[..] {
            [UsageMeter::Metered { amount }] => {
                assert_eq!(amount.used, 25.0);
                assert_eq!(amount.limit, Some(100.0));
                assert_eq!(amount.remaining, Some(75.0));
            }
            other => panic!("expected enterprise spend, got {other:?}"),
        }
    }

    #[test]
    fn env_oauth_token_fetch_gets_plan_from_profile_not_stale_file() {
        // Stale local login is Pro; the env token's profile is Enterprise.
        let port = spawn_loopback(2, move |request| {
            let auth = request
                .headers()
                .iter()
                .find(|header| header.field.equiv("Authorization"))
                .map(|header| header.value.as_str().to_string())
                .unwrap_or_default();
            assert_eq!(auth, "Bearer sk-ant-oat01-env");
            let path = request.url().to_string();
            let body = if path.contains("/profile") {
                r#"{"organization":{"organization_type":"claude_enterprise"}}"#
            } else {
                ENTERPRISE_SPEND_BODY
            };
            let _ = request.respond(tiny_http::Response::from_string(body).with_status_code(200));
        });
        let mut lookup = with_file(OAUTH_JSON);
        lookup
            .env
            .insert("CLAUDE_CODE_OAUTH_TOKEN".into(), "sk-ant-oat01-env".into());
        let usage = anthropic_usage_with(&lookup, &loopback_url(port));
        assert_eq!(
            usage.plan.as_deref(),
            Some("Enterprise"),
            "plan must come from the env token profile, not the stale Pro login file"
        );
        match &usage.meters[..] {
            [UsageMeter::Metered { amount }] => {
                assert_eq!(amount.used, 25.0);
                assert_eq!(amount.limit, Some(100.0));
            }
            other => panic!("expected enterprise spend for env token, got {other:?}"),
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
        let usage = anthropic_usage_with(&lookup, &loopback_url(port));
        assert!(usage.logged_in);
        assert!(usage.plan.is_none(), "must not invent or borrow a plan");
        assert!(
            usage
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("user:profile"),
            "detail={:?}",
            usage.detail
        );
        assert!(matches!(
            usage.meters.first(),
            Some(UsageMeter::Metered { .. })
        ));
    }

    #[test]
    fn rejected_login_retries_active_user_oauth_profile() {
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
                assert_eq!(auth, "Bearer sk-ant-oat01-expired-login");
                let _ = request
                    .respond(tiny_http::Response::from_string("denied").with_status_code(401));
            } else {
                assert_eq!(auth, "Bearer sk-ant-oat01-profile");
                let _ = request.respond(
                    tiny_http::Response::from_string(ENTERPRISE_SPEND_BODY).with_status_code(200),
                );
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

        let usage = anthropic_usage_with(&lookup, &loopback_url(port));
        assert!(usage.logged_in);
        assert_eq!(usage.plan.as_deref(), Some("Enterprise"));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert!(matches!(
            usage.meters.first(),
            Some(UsageMeter::Metered { .. })
        ));
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
        let port = spawn_loopback(1, move |request| {
            let _ = request.respond(tiny_http::Response::from_string(body).with_status_code(200));
        });
        let usage = anthropic_usage_with(&with_file(ENTERPRISE_JSON), &loopback_url(port));
        match &usage.meters[..] {
            [UsageMeter::Metered { amount }] => {
                assert_eq!(amount.used, 3.25);
                assert_eq!(amount.limit, Some(20.5));
            }
            other => panic!("expected extra_usage fallback, got {other:?}"),
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
        let usage = anthropic_usage_with(&lookup, &loopback_url(port));
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        assert_eq!(
            usage.meters,
            vec![UsageMeter::ManagedExternally {
                platform: "AWS Bedrock".to_string()
            }]
        );
        assert!(usage.windows.is_empty());
        assert!(usage.error.is_none());
    }

    #[test]
    fn vertex_and_foundry_are_externally_managed() {
        for (flag, platform) in [
            ("CLAUDE_CODE_USE_VERTEX", "Google Vertex AI"),
            ("CLAUDE_CODE_USE_FOUNDRY", "Microsoft Foundry"),
        ] {
            let mut lookup = with_file(OAUTH_JSON);
            lookup.env.insert(flag.into(), "1".into());
            let usage = anthropic_usage_with(&lookup, "http://127.0.0.1:1/unused");
            assert_eq!(
                usage.meters,
                vec![UsageMeter::ManagedExternally {
                    platform: platform.to_string()
                }]
            );
        }
    }

    #[test]
    fn authentication_failures_are_logged_out() {
        for status in [401_u16, 403] {
            let port = spawn_loopback(1, move |request| {
                let _ = request
                    .respond(tiny_http::Response::from_string("denied").with_status_code(status));
            });
            let usage = anthropic_usage_with(&with_file(OAUTH_JSON), &loopback_url(port));
            assert!(!usage.logged_in, "status {status} should log out");
            assert!(
                usage
                    .error
                    .as_deref()
                    .unwrap_or_default()
                    .contains("login expired"),
                "{status}: {:?}",
                usage.error
            );
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
        let usage = anthropic_usage_with(&lookup, &loopback_url(port));
        assert!(
            usage.logged_in,
            "scope failure must not look like a logged-out/expired credential"
        );
        assert!(usage.plan.is_none());
        let detail = usage.detail.as_deref().unwrap_or_default();
        let error = usage.error.as_deref().unwrap_or_default();
        assert!(
            detail.contains("setup-token") || error.contains("setup-token"),
            "env-token scope failure should mention setup-token; detail={detail:?} error={error:?}"
        );
        assert!(!error.contains("login expired"));
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
        let usage = anthropic_usage_with(&with_file(OAUTH_JSON), &loopback_url(port));
        assert!(usage.logged_in);
        let detail = usage.detail.as_deref().unwrap_or_default();
        let error = usage.error.as_deref().unwrap_or_default();
        assert!(
            detail.contains("run /login") || error.contains("run /login"),
            "stored login scope failure should recommend /login; detail={detail:?} error={error:?}"
        );
        assert!(
            !detail.contains("setup-token") && !error.contains("setup-token"),
            "stored login must not claim the setup-token limitation"
        );
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
        let usage = anthropic_usage_with(&lookup, &loopback_url(port));
        assert!(usage.logged_in);
        let detail = usage.detail.as_deref().unwrap_or_default();
        let error = usage.error.as_deref().unwrap_or_default();
        assert!(
            detail.contains("ant auth login --profile work")
                || error.contains("ant auth login --profile work"),
            "named profile must include the actual profile name; detail={detail:?} error={error:?}"
        );
        assert!(
            !detail.contains("<name>") && !error.contains("<name>"),
            "must not leave a placeholder profile name"
        );
        assert!(
            !detail.contains("run /login") && !error.contains("run /login"),
            "/login cannot repair ANTHROPIC_PROFILE credentials"
        );
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
        let usage = anthropic_usage_with(&lookup, &loopback_url(port));
        assert!(usage.logged_in);
        let detail = usage.detail.as_deref().unwrap_or_default();
        let error = usage.error.as_deref().unwrap_or_default();
        assert!(
            detail.contains("ant auth login --profile agents")
                || error.contains("ant auth login --profile agents"),
            "active non-default profile must name itself; detail={detail:?} error={error:?}"
        );
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

        let usage = anthropic_usage_with(&lookup, &loopback_url(port));
        assert!(usage.logged_in);
        assert_eq!(usage.plan.as_deref(), Some("Enterprise"));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert!(!usage
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("user:profile"));
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

        let usage = anthropic_usage_with(&lookup, &loopback_url(port));
        assert!(usage.logged_in);
        assert_eq!(usage.plan.as_deref(), Some("Enterprise"));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert!(!usage
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("setup-token"));
        assert!(matches!(
            usage.meters.first(),
            Some(UsageMeter::Metered { .. })
        ));
    }

    #[test]
    fn rate_limit_and_parse_failures_stay_unavailable() {
        let port_429 = spawn_loopback(1, |request| {
            let _ = request
                .respond(tiny_http::Response::from_string("slow down").with_status_code(429));
        });
        let limited = anthropic_usage_with(&with_file(OAUTH_JSON), &loopback_url(port_429));
        assert!(limited.logged_in);
        assert!(limited
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("Rate limited"));

        let port_bad = spawn_loopback(1, |request| {
            let _ =
                request.respond(tiny_http::Response::from_string("{nope").with_status_code(200));
        });
        let bad = anthropic_usage_with(&with_file(OAUTH_JSON), &loopback_url(port_bad));
        assert!(bad.logged_in);
        assert!(bad
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("Failed to parse"));
    }

    #[test]
    fn missing_credential_does_not_hit_the_network() {
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_thread = Arc::clone(&hits);
        let port = spawn_loopback(1, move |request| {
            hits_thread.fetch_add(1, Ordering::SeqCst);
            let _ = request.respond(tiny_http::Response::from_string("{}").with_status_code(200));
        });
        let usage = anthropic_usage_with(&FakeLookup::default(), &loopback_url(port));
        assert!(!usage.logged_in);
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
