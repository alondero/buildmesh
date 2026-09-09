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
    cache_identity_from, resolve_claude_auth, AuthLookup, ClaudeAuthSource, ProductionLookup,
};
use parse::parse_anthropic_usage;

const PROVIDER: &str = "anthropic";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

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
        ClaudeAuthSource::Oauth { token, plan } => fetch_oauth_usage(usage_url, &token, plan),
    }
}

fn fetch_oauth_usage(usage_url: &str, token: &str, plan: Option<String>) -> ProviderUsage {
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
            logged_out(
                PROVIDER,
                "Claude login expired — run /login in the Claude CLI".to_string(),
            )
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
            Ok(parsed) => ProviderUsage {
                provider: PROVIDER.to_string(),
                logged_in: true,
                windows: parsed.windows,
                balance: None,
                plan,
                meters: parsed.meters,
                detail: None,
                error: None,
            },
            Err(error) => unavailable(PROVIDER, format!("Failed to parse response: {error}")),
        },
        Err(error) => unavailable(PROVIDER, format!("Request failed: {error}")),
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
    const OAUTH_JSON: &str = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-secret","subscriptionType":"pro"}}"#;
    const ENTERPRISE_JSON: &str = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-ent","subscriptionType":"enterprise"}}"#;

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
                        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
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
            let _ = request.respond(tiny_http::Response::from_string("should not run").with_status_code(200));
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
                let _ = request.respond(tiny_http::Response::from_string("denied").with_status_code(status));
            });
            let usage = anthropic_usage_with(&with_file(OAUTH_JSON), &loopback_url(port));
            assert!(!usage.logged_in, "status {status} should log out");
            assert!(
                usage.error.as_deref().unwrap_or_default().contains("login expired"),
                "{status}: {:?}",
                usage.error
            );
        }
    }

    #[test]
    fn rate_limit_and_parse_failures_stay_unavailable() {
        let port_429 = spawn_loopback(1, |request| {
            let _ = request.respond(tiny_http::Response::from_string("slow down").with_status_code(429));
        });
        let limited = anthropic_usage_with(&with_file(OAUTH_JSON), &loopback_url(port_429));
        assert!(limited.logged_in);
        assert!(limited
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("Rate limited"));

        let port_bad = spawn_loopback(1, |request| {
            let _ = request.respond(tiny_http::Response::from_string("{nope").with_status_code(200));
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
