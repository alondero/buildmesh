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
use crate::services::usage::types::UsagePage;
use std::collections::HashMap;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProfileScopeProbe {
    Limited,
    Available,
}

static PROFILE_SCOPE_PROBES: once_cell::sync::Lazy<
    std::sync::Mutex<HashMap<UsageIdentityFingerprint, ProfileScopeProbe>>,
> = once_cell::sync::Lazy::new(|| std::sync::Mutex::new(HashMap::new()));

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
    fn usage_page(&self) -> Option<UsagePage> {
        Some(UsagePage::new(
            "https://claude.ai/settings/usage",
            "View usage",
        ))
    }

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
        ClaudeAuthSource::Oauth { token, origin, .. } => {
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
                reason: format!(
                    "API error {}: {}",
                    code,
                    response.text().unwrap_or_default()
                ),
            }
        }
        Ok(response) => {
            let fallback_reset = overage_reset_header(&response);
            match parse_anthropic_usage(&response.text().unwrap_or_default(), fallback_reset) {
                Ok(parsed) => {
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
            }
        }
        Err(error) => UsageOutcome::Unavailable {
            reason: format!("Request failed: {error}"),
        },
    }
}

fn overage_reset_header(response: &reqwest::blocking::Response) -> Option<String> {
    let value = response
        .headers()
        .get("anthropic-ratelimit-unified-overage-reset")?;
    let text = value.to_str().ok()?.trim();
    let seconds = text
        .parse::<i64>()
        .ok()
        .or_else(|| text.parse::<f64>().ok().map(|value| value as i64))?;
    if seconds <= 0 {
        return None;
    }
    chrono::DateTime::from_timestamp(seconds, 0).map(|instant| instant.to_rfc3339())
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
    let identity_material = format!("{profile_url}\0{token}");
    let identity =
        UsageIdentityFingerprint::new("anthropic_profile_scope", identity_material.as_bytes());
    if let Some(probe) = PROFILE_SCOPE_PROBES.lock().unwrap().get(&identity) {
        return *probe == ProfileScopeProbe::Limited;
    }

    let response = match client
        .get(profile_url)
        .header("Authorization", format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
    {
        Ok(response) => response,
        Err(_) => return false,
    };
    if response.status().is_success() {
        PROFILE_SCOPE_PROBES
            .lock()
            .unwrap()
            .insert(identity, ProfileScopeProbe::Available);
        return false;
    }
    if response.status().as_u16() != 403 {
        return false;
    }
    let body = response.text().unwrap_or_default();
    let limited = is_oauth_scope_failure(403, &body);
    if limited {
        PROFILE_SCOPE_PROBES
            .lock()
            .unwrap()
            .insert(identity, ProfileScopeProbe::Limited);
    }
    limited
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
#[path = "anthropic_tests.rs"]
mod tests;
