//! Provider usage fetching — piggybacks on CLI credentials stored by each provider.
//!
//! Endpoints are undocumented / reverse-engineered; treat non-200 responses or
//! shape mismatches as "usage unavailable", never as hard errors.
//!
//! After issue #1745, the module shape is:
//! - [`types`] — wire shapes (`ProviderUsage`, `UsageWindow`, …). `ts-rs`-derived.
//! - [`outcome`] — internal failure taxonomy + the only place that mints the
//!   wire triple from a [`outcome::UsageOutcome`] (the seam that prevents
//!   per-adapter drift). Visibility-fenced so adapters cannot bypass it.
//! - [`cache`] — 5-minute in-process TTL cache keyed on account identity.
//! - [`last_known`] — 7-day *durable* store of the last reading each provider
//!   reported, used to keep a meter visible when a fetch cannot produce a fresh
//!   reading (ADR-0037). Distinct from [`cache`]: that one keeps a reading
//!   fresh within a process, this one survives restarts.
//! - [`adapter`] — the [`adapter::UsageAdapter`] seam and shared HTTP driver.
//! - [`adapters`] — per-provider drop-in adapters.
//! - [`catalog`] — registry + dispatch + cache lookup.

pub mod types;
pub(crate) mod cache;
pub(crate) mod adapter;
pub(crate) mod adapters;
pub(crate) mod catalog;
pub(crate) mod last_known;
pub(crate) mod outcome;

// Re-export the wire types so existing `crate::services::usage::{...}`
// paths keep working while adapters import from `usage::types` directly.
pub use types::{
    BillingBalance, ProviderMeters, ProviderUsage, UsageError, UsageWindow,
};
pub(crate) use outcome::UsageOutcome;
// Cache stays behind the same `usage::` paths callers already use.
pub use cache::{invalidate_cache, invalidate_provider_cache};
// `fetch_usage` is a fetcher-only driver: internal call sites in this
// module import it directly via `crate::services::usage::adapter::fetch_usage`
// so the `usage::` namespace stops advertising it (issue #1657 step 1:
// stop exporting helpers used only by fetchers). Adapters go through
// `catalog::dispatch(id).fetch` instead.
// Internal fetcher helpers are NOT re-exported: `usage::home_dir`,
// `usage::logged_out`, `usage::unavailable`, `usage::cached_age` were
// fetcher-only and the issue (#1657) requires this module to stop
// exporting helpers only fetchers use. Internal callers go through
// `super::types::...` or `super::cache::...` directly.

use reqwest::blocking::Client;
use serde::Deserialize;
// Internal fetcher-only helpers: imported by their defining module so the
// `usage::` namespace stays clean for the seam surface (issue #1657).
use crate::services::usage::adapter::{fetch_usage, shared_client};
use crate::services::usage::cache::cached_age;
use crate::services::usage::types::home_dir;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
// `tracing` is the codebase-wide diagnostic log channel (`warm_pool`,
// `agent_node`, `autopilot` all use `tracing::warn!` for non-fatal side
// channels); `eprintln!` would surface to the user's terminal instead.
use tracing;
// Credential Manager surface — Windows-only. Re-exported at the top so the
// existing `windows_cred::read(...)` call sites below stay unchanged after
// the inline module was extracted to `services::windows_cred` for #956.
// On non-Windows the path doesn't exist; the `cfg(windows)` `read_*` helpers
// catch the gap with `NoCredential`, so callers stay one-statement-uniform.
#[cfg(windows)]
use crate::services::windows_cred;
// The OpenCode OAuth DTO + parser were extracted to `services::opencode_oauth`
// for #956 so the OAuth dance and the live fetcher don't share a private
// helper. The constant `OPENCODE_CONSOLE_CRED_TARGET` was lifted along with
// it; the parser stays qualified (call sites read
// `opencode_oauth::parse_opencode_console_credential(...)`) to make the
// module boundary obvious at every read site.
use crate::services::opencode_oauth::OpenCodeConsoleCred;
use crate::services::opencode_oauth::OPENCODE_CONSOLE_CRED_TARGET;
use crate::services::opencode_oauth::OPENCODE_CONSOLE_HOST;
use crate::services::opencode_oauth::device_flow;

// Wire types + envelope helpers moved to `usage::types` (issue #1657).
// Re-exported at the top of this file so existing paths keep working.


#[derive(Deserialize)]
struct OpenCodeAuthEntry {
    key: Option<String>,
}

fn read_opencode_token(path: PathBuf) -> Result<String, UsageError> {
    let content = fs::read_to_string(&path).map_err(|_| UsageError::NoCredential(path.clone().to_string_lossy().to_string()))?;
    let entries: HashMap<String, OpenCodeAuthEntry> = serde_json::from_str(&content)
        .map_err(|e| UsageError::Shape(e.to_string()))?;
    if let Some(entry) = entries.get("opencode-go") {
        if let Some(ref key) = entry.key {
            if !key.is_empty() {
                return Ok(key.clone());
            }
        }
    }
    Err(UsageError::NoCredential(path.to_string_lossy().to_string()))
}

// `logged_out` / `unavailable` / `fetch_usage` moved to the seam
// (`usage::types` + `usage::adapter`, issue #1657) — imported at the top.
// Anthropic lives in `services/usage/adapters/anthropic` (issue #1673).

// Codex CLI usage lives in `services::usage::adapters::codex`.

// MiniMax lives in `services/usage/adapters/minimax` (ARCH-1).


// Kimi lives in `services/usage/adapters/kimi` (ARCH-1).


// OpenAI lives in `services/usage/adapters/openai` (ARCH-1).


// OpenRouter lives in `services/usage/adapters/openrouter` (ARCH-1).


// DeepSeek lives in `services/usage/adapters/deepseek` (ARCH-1).


// Command Code stores its CLI-owned credential in `~/.commandcode/auth.json`.
// Credits supply rolling windows; subscription enrichment adds the monthly meter.
fn commandcode_auth_path() -> PathBuf {
    crate::env::commandcode_dir().join("auth.json")
}

#[derive(Deserialize, Debug)]
struct CommandCodeAuth {
    #[serde(rename = "apiKey")]
    api_key: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
}

fn read_commandcode_token(path: &Path) -> Result<String, UsageError> {
    let content = fs::read_to_string(path)
        .map_err(|_| UsageError::NoCredential(path.to_string_lossy().to_string()))?;
    let auth: CommandCodeAuth =
        serde_json::from_str(&content).map_err(|e| UsageError::Shape(e.to_string()))?;
    auth.api_key
        .filter(|token| !token.trim().is_empty())
        .or_else(|| auth.access_token.filter(|token| !token.trim().is_empty()))
        .map(|token| token.trim().to_string())
        .ok_or_else(|| UsageError::NoCredential(path.to_string_lossy().to_string()))
}

#[derive(Deserialize, Debug)]
struct CommandCodeQuotaWindow {
    cap: f64,
    used: f64,
    #[serde(rename = "resetAt", alias = "reset_at", default)]
    reset_at: Option<i64>,
}

#[derive(Deserialize, Debug)]
struct CommandCodeWindowLimits {
    #[serde(rename = "fiveHour", alias = "five_hour")]
    five_hour: CommandCodeQuotaWindow,
    weekly: CommandCodeQuotaWindow,
}

#[derive(Deserialize, Debug)]
struct CommandCodeCredits {
    #[serde(rename = "monthlyCredits", alias = "monthly_credits")]
    monthly_credits: f64,
    #[serde(
        rename = "purchasedCredits",
        alias = "purchased_credits",
        alias = "extraCredits",
        alias = "extra_credits",
        default
    )]
    purchased_credits: f64,
    #[serde(rename = "freeCredits", alias = "free_credits", default)]
    free_credits: f64,
    // Studio treats non-numeric grants as absent, preserving its tier fallback.
    #[serde(rename = "monthlyCreditsGranted", alias = "monthly_credits_granted", default)]
    monthly_credits_granted: serde_json::Value,
}

impl CommandCodeCredits {
    fn balance(&self) -> BillingBalance {
        BillingBalance {
            remaining: self.monthly_credits + self.purchased_credits + self.free_credits,
            monthly_spend: None,
            currency: "USD".to_string(),
        }
    }
}

fn commandcode_positive_finite(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value > 0.0)
}

#[derive(Deserialize)]
struct CommandCodeSubscriptionResponse {
    success: bool,
    data: Option<CommandCodeSubscription>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommandCodeSubscription {
    plan_id: String,
    status: String,
    quantity: Option<f64>,
    current_period_end: Option<String>,
}

fn commandcode_monthly_window(
    credits: &CommandCodeCredits,
    subscription: CommandCodeSubscriptionResponse,
) -> Option<UsageWindow> {
    let subscription = subscription.data.filter(|_| subscription.success)?;
    if subscription.status == "past_due" || !credits.monthly_credits.is_finite() {
        return None;
    }
    // Matches Studio's deployed tiers and CLI 1.53.1 (2026-09-12).
    // The alpha API does not supply the allowance for every plan; preserve legacy IDs.
    // Evidence and update sources: docs/research/command-code-monthly-usage.md.
    let base: f64 = match subscription.plan_id.as_str() {
        "individual-go" => 10.0,
        "individual-goat" => 70.0,
        "individual-pro" => 30.0,
        "individual-pro-v1" => 80.0,
        "individual-provider" => 15.0,
        "individual-max" => 150.0,
        "individual-ultra" => 300.0,
        "teams-pro" => 40.0,
        _ => return None,
    };
    let grant = commandcode_positive_finite(credits.monthly_credits_granted.as_f64());
    // Studio floors organization quantity before checking its minimum. A
    // positive fractional quantity below one therefore falls back to one
    // base allowance, rather than becoming a zero-credit organization plan.
    let seats = subscription.quantity.map(f64::floor).unwrap_or(1.0);
    let seats = if seats.is_finite() && seats >= 1.0 { seats } else { 1.0 };
    let total = match grant {
        Some(grant) => grant.max(base),
        None if subscription.plan_id == "teams-pro" => base * seats,
        None => base,
    };
    if !total.is_finite() || total <= 0.0 {
        return None;
    }
    let resets_at = subscription.current_period_end
        .and_then(|date| chrono::DateTime::parse_from_rfc3339(&date).ok())
        .map(|date| date.to_rfc3339());
    Some(UsageWindow {
        label: "Monthly".to_string(),
        used_percent: Some(((total - credits.monthly_credits.max(0.0)) / total * 100.0).clamp(0.0, 100.0)),
        resets_at,
    })
}

#[derive(Deserialize, Debug)]
struct CommandCodeNestedCreditsResponse {
    #[serde(rename = "windowLimits")]
    window_limits: CommandCodeWindowLimits,
    credits: CommandCodeCredits,
}

#[derive(Deserialize, Debug)]
struct CommandCodeReportedWindow {
    label: String,
    #[serde(default, alias = "usedPercent")]
    used_percent: Option<f64>,
    #[serde(default, alias = "resetsAt")]
    resets_at: Option<String>,
}

#[derive(Deserialize, Debug)]
struct CommandCodeReportedCreditsResponse {
    windows: Vec<CommandCodeReportedWindow>,
    #[serde(rename = "monthly_credits", alias = "monthlyCredits")]
    monthly_credits: f64,
    #[serde(
        rename = "extra_credits",
        alias = "extraCredits",
        alias = "purchasedCredits",
        alias = "purchased_credits",
        default
    )]
    extra_credits: f64,
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum CommandCodeCreditsResponse {
    Nested(CommandCodeNestedCreditsResponse),
    Reported(CommandCodeReportedCreditsResponse),
}

fn commandcode_window(label: &str, window: CommandCodeQuotaWindow) -> Option<UsageWindow> {
    if !window.cap.is_finite() || !window.used.is_finite() || window.cap <= 0.0 {
        return None;
    }
    let resets_at = window
        .reset_at
        .filter(|timestamp| *timestamp > 0)
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|datetime| datetime.to_rfc3339());
    Some(UsageWindow {
        label: label.to_string(),
        used_percent: Some(((window.used / window.cap) * 100.0).clamp(0.0, 100.0)),
        resets_at,
    })
}

fn parse_commandcode_credits_response(
    body: &str,
) -> Result<(Vec<UsageWindow>, CommandCodeCredits), UsageError> {
    let response: CommandCodeCreditsResponse =
        serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;
    match response {
        CommandCodeCreditsResponse::Nested(response) => {
            let windows = [
                commandcode_window("5-hour", response.window_limits.five_hour),
                commandcode_window("Weekly", response.window_limits.weekly),
            ]
            .into_iter()
            .flatten()
            .collect();
            Ok((windows, response.credits))
        }
        CommandCodeCreditsResponse::Reported(response) => Ok((
            response
                .windows
                .into_iter()
                .map(|window| UsageWindow {
                    label: window.label,
                    used_percent: window
                        .used_percent
                        .filter(|percent| percent.is_finite())
                        .map(|percent| percent.clamp(0.0, 100.0)),
                    resets_at: window.resets_at,
                })
                .collect(),
            CommandCodeCredits {
                monthly_credits: response.monthly_credits,
                purchased_credits: response.extra_credits,
                free_credits: 0.0,
                monthly_credits_granted: serde_json::Value::Null,
            },
        )),
    }
}

/// Public Command Code fetcher. Reads the CLI-managed credential and queries
/// its billing API directly; spawning the CLI would make a usage refresh wait
/// on an interactive process startup.
///
/// Issue #1745 phase 2 step 14: migrated to the outcome seam. Missing
/// credential → `NoCredential`. 401/403 → `Rejected` with the session-expired
/// remediation. 429 → `RateLimited`. Other → `Unavailable`. The ladder stays
/// hand-rolled (the kimi precedent): the shared `fetch_usage` driver cannot
/// carry the dual-fetch quota + subscription-enrichment reading.
pub fn commandcode_usage() -> UsageOutcome {
    commandcode_usage_with_path(
        &commandcode_auth_path(),
        "https://api.commandcode.ai/alpha/billing/credits",
    )
}

/// Test seam for the CLI-owned credential path and the HTTP endpoint.
fn commandcode_usage_with_path(auth_path: &Path, live_url: &str) -> UsageOutcome {
    let token = match read_commandcode_token(auth_path) {
        Ok(token) => token,
        Err(error) => {
            return UsageOutcome::NoCredential {
                hint: error.to_string(),
            }
        }
    };
    let client = match Client::builder().timeout(Duration::from_secs(15)).build() {
        Ok(client) => client,
        Err(error) => {
            return UsageOutcome::Unavailable {
                reason: format!("Client error: {error}"),
            }
        }
    };
    let response = match client
        .get(live_url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
    {
        Ok(response) if response.status().as_u16() == 401 || response.status().as_u16() == 403 => {
            return UsageOutcome::Rejected {
                hint: "Command Code session expired — run 'cmdc login' to log in".to_string(),
            };
        }
        Ok(response) if response.status().as_u16() == 429 => {
            return UsageOutcome::RateLimited {
                reason: "Rate limited — usage data temporarily unavailable".to_string(),
            };
        }
        Ok(response) if !response.status().is_success() => {
            let code = response.status().as_u16();
            let body = response.text().unwrap_or_default();
            let detail = if body.trim().is_empty() {
                "usage endpoint failed"
            } else {
                body.trim()
            };
            return UsageOutcome::Unavailable {
                reason: format!("API error {code}: {detail}"),
            };
        }
        Ok(response) => response,
        Err(error) => {
            return UsageOutcome::Unavailable {
                reason: format!("Request failed: {error}"),
            }
        }
    };
    let body = match response.text() {
        Ok(body) => body,
        Err(error) => {
            return UsageOutcome::Unavailable {
                reason: format!("Failed to read response: {error}"),
            }
        }
    };
    match parse_commandcode_credits_response(&body) {
        Ok((mut windows, credits)) => {
            let mut balance = Some(credits.balance());
            let mut detail = None;
            // Enrichment is optional: a failed subscription request must not discard
            // successfully fetched windows or credits. Keep the extra wait bounded.
            let reported_monthly = windows.iter().any(|window| {
                window.used_percent.is_some()
                    && window.label.trim().to_ascii_lowercase().starts_with("monthly")
            });
            if reported_monthly {
                balance = None;
                detail = commandcode_extra_credits_detail(&credits);
            } else {
                let monthly = commandcode_fetch_monthly_window(&client, &token, live_url, &credits);
                let monthly = match monthly {
                    Ok(Some(monthly)) => Some(monthly),
                    Ok(None) => {
                        tracing::warn!(
                            provider = "commandcode",
                            "Command Code subscription has no usable monthly meter"
                        );
                        None
                    }
                    Err(error) => {
                        tracing::warn!(
                            provider = "commandcode",
                            error = %error,
                            "Command Code monthly usage enrichment failed"
                        );
                        None
                    }
                };
                if let Some(monthly) = monthly {
                    windows.push(monthly);
                    balance = None;
                    detail = commandcode_extra_credits_detail(&credits);
                }
            }
            UsageOutcome::Reading {
                windows,
                balance,
                meters: vec![],
                detail,
            }
        }
        Err(error) => UsageOutcome::Unavailable {
            reason: format!("Failed to parse response: {error}"),
        },
    }
}

fn commandcode_extra_credits_detail(credits: &CommandCodeCredits) -> Option<String> {
    let extra = credits.purchased_credits + credits.free_credits;
    (extra.is_finite() && extra != 0.0)
        .then(|| format!("Additional credits: USD {extra:.2}"))
}

fn commandcode_fetch_monthly_window(
    client: &Client,
    token: &str,
    credits_url: &str,
    credits: &CommandCodeCredits,
) -> Result<Option<UsageWindow>, String> {
    let mut subscription_url = reqwest::Url::parse(credits_url)
        .map_err(|error| format!("invalid credits URL: {error}"))?
        .join("subscriptions")
        .map_err(|error| format!("invalid subscription URL: {error}"))?;
    subscription_url
        .query_pairs_mut()
        .append_pair("withPending", "true");
    let response = client
        .get(subscription_url)
        .bearer_auth(token)
        .timeout(Duration::from_secs(5))
        .send()
        .map_err(|error| format!("request failed: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("API error {}", status.as_u16()));
    }
    let subscription = response
        .json::<CommandCodeSubscriptionResponse>()
        .map_err(|error| format!("invalid response: {error}"))?;
    Ok(commandcode_monthly_window(credits, subscription))
}


// ─── xAI / Grok (`grok`) ───────────────────────────────────────────────────
//
// The Grok Build CLI (`grok`) stores OIDC session credentials in
// `~/.grok/auth.json`. The OIDC access token resides inside the nested "key"
// field, and the user ID is in "user_id".
//
// To retrieve billing / usage, we query `GET /v1/billing?format=credits` on
// the Grok proxy `cli-chat-proxy.grok.com`. To authorize and request the weekly
// unified billing format (the rolling consumer pool), we must pass special headers:
// `X-XAI-Token-Auth: xai-grok-cli`, `x-userid`, and `x-grok-client-*`.

fn grok_auth_path() -> PathBuf {
    home_dir().join(".grok").join("auth.json")
}

#[derive(Deserialize, Debug)]
struct GrokAuthEntry {
    key: Option<String>,
    user_id: Option<String>,
}

fn read_grok_token(path: PathBuf) -> Result<(String, String), UsageError> {
    let content = fs::read_to_string(&path)
        .map_err(|_| UsageError::NoCredential(path.clone().to_string_lossy().to_string()))?;
    let entries: HashMap<String, GrokAuthEntry> = serde_json::from_str(&content)
        .map_err(|e| UsageError::Shape(e.to_string()))?;
    for (k, v) in entries {
        if k.starts_with("https://auth.x.ai::") {
            if let (Some(key), Some(user_id)) = (v.key, v.user_id) {
                if !key.is_empty() && !user_id.is_empty() {
                    return Ok((key, user_id));
                }
            }
        }
    }
    Err(UsageError::NoCredential(path.to_string_lossy().to_string()))
}

#[derive(Deserialize, Debug)]
struct GrokVal {
    val: f64,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)] // fields are populated by serde during JSON deserialization
                    // but the grok billing parser (parse_grok_response_*) doesn't
                    // surface them yet — kept in the struct so a future ticket
                    // can render "resets <end>" without re-parsing the response.
struct GrokPeriod {
    #[serde(rename = "type")]
    period_type: Option<String>,
    start: Option<String>,
    end: Option<String>,
}

#[derive(Deserialize, Debug)]
struct GrokBillingConfig {
    #[serde(rename = "currentPeriod")]
    current_period: Option<GrokPeriod>,
    #[serde(rename = "creditUsagePercent")]
    credit_usage_percent: Option<f64>,
    #[serde(rename = "onDemandCap")]
    on_demand_cap: Option<GrokVal>,
    #[serde(rename = "onDemandUsed")]
    on_demand_used: Option<GrokVal>,
    #[serde(rename = "isUnifiedBillingUser")]
    is_unified_billing_user: Option<bool>,
    #[serde(rename = "prepaidBalance")]
    prepaid_balance: Option<GrokVal>,
    #[serde(rename = "billingPeriodEnd")]
    billing_period_end: Option<String>,
    #[serde(rename = "monthlyLimit")]
    monthly_limit: Option<GrokVal>,
    used: Option<GrokVal>,
}

#[derive(Deserialize, Debug)]
struct GrokBillingResp {
    config: GrokBillingConfig,
}

fn parse_grok_response(
    body: &str,
) -> Result<(Vec<UsageWindow>, Option<BillingBalance>), UsageError> {
    let resp: GrokBillingResp = serde_json::from_str(body)
        .map_err(|e| UsageError::Shape(e.to_string()))?;
    let config = resp.config;

    let mut windows = Vec::new();
    let is_unified = config.is_unified_billing_user.unwrap_or(false);

    let balance = if let Some(ref prepaid) = config.prepaid_balance {
        if prepaid.val > 0.0 {
            Some(BillingBalance {
                remaining: prepaid.val,
                monthly_spend: config.on_demand_used.as_ref().map(|v| v.val),
                currency: "USD".to_string(),
            })
        } else {
            None
        }
    } else {
        None
    };

    if is_unified {
        let used_percent = config.credit_usage_percent
            .map(|percent| percent.clamp(0.0, 100.0))
            .or_else(|| config.on_demand_cap.as_ref().map(|cap| {
                if cap.val > 0.0 {
                    config.on_demand_used.as_ref()
                        .map(|used| ((used.val / cap.val) * 100.0).clamp(0.0, 100.0))
                        .unwrap_or(0.0)
                } else {
                    0.0
                }
            }));

        if let Some(used_percent) = used_percent {
            let label = config.current_period.as_ref()
                .and_then(|p| p.period_type.as_ref())
                .map(|t| {
                    if t.contains("WEEKLY") {
                        "Weekly Pool".to_string()
                    } else {
                        "Grok Build Quota".to_string()
                    }
                })
                .unwrap_or_else(|| "Weekly Pool".to_string());

            windows.push(UsageWindow {
                label,
                used_percent: Some(used_percent),
                resets_at: config.billing_period_end.clone(),
            });
        }
    } else if let Some(ref limit) = config.monthly_limit {
        if limit.val > 0.0 {
            let used_percent = config.used.as_ref().map(|u| (u.val / limit.val) * 100.0);
            windows.push(UsageWindow {
                label: "Monthly Limit".to_string(),
                used_percent,
                resets_at: config.billing_period_end.clone(),
            });
        }
    }

    Ok((windows, balance))
}

/// Public Grok fetcher. Reads the CLI-managed OIDC credential and queries
/// the Grok proxy billing endpoint with the `xai-grok-cli` headers.
///
/// Issue #1745 phase 2 step 17: migrated to the outcome seam. Missing
/// credential → `NoCredential` (as before). 401/403 → `Rejected` with
/// the "Invalid API key" affordance. 429 → `RateLimited`. Client-build /
/// transport / non-2xx / parse → `Unavailable`. The ladder stays
/// hand-rolled (the kimi precedent): the shared `fetch_usage` driver
/// cannot carry grok's prepaid-balance reading (the custom headers
/// would fit the driver's request builder; the balance does not fit
/// its `(windows, detail)` parse shape).
pub fn grok_usage() -> UsageOutcome {
    let base_url = env::var("GROK_CLI_CHAT_PROXY_BASE_URL")
        .unwrap_or_else(|_| "https://cli-chat-proxy.grok.com".to_string());
    grok_usage_with(&grok_auth_path(), &base_url)
}

/// Test seam for the CLI-owned credential path and the proxy base URL.
fn grok_usage_with(auth_path: &Path, base_url: &str) -> UsageOutcome {
    let (token, user_id) = match read_grok_token(auth_path.to_path_buf()) {
        Ok(t) => t,
        Err(e) => {
            return UsageOutcome::NoCredential {
                hint: e.to_string(),
            }
        }
    };

    let client = match shared_client() {
        Ok(c) => c,
        Err(e) => {
            return UsageOutcome::Unavailable { reason: e };
        }
    };

    let url = format!("{base_url}/v1/billing?format=credits");

    let resp = match client
        .get(&url)
        .header("Authorization", format!("Bearer {}", token))
        .header("X-XAI-Token-Auth", "xai-grok-cli")
        .header("x-userid", user_id)
        .header("x-grok-client-mode", "grok-build")
        .header("x-grok-client-version", "0.2.103")
        .header("x-grok-client-identifier", "grok-shell")
        .send()
    {
        Ok(r) if !r.status().is_success() => {
            let code = r.status().as_u16();
            if code == 401 || code == 403 {
                return UsageOutcome::Rejected {
                    hint: "Invalid API key".to_string(),
                };
            }
            if code == 429 {
                return UsageOutcome::RateLimited {
                    reason: "Rate limited — usage data temporarily unavailable".to_string(),
                };
            }
            let body = r.text().unwrap_or_default();
            return UsageOutcome::Unavailable {
                reason: format!("API error {code}: {body}"),
            };
        }
        Ok(r) => r,
        Err(e) => {
            return UsageOutcome::Unavailable {
                reason: format!("Request failed: {e}"),
            }
        }
    };

    let body = match resp.text() {
        Ok(b) => b,
        Err(e) => {
            return UsageOutcome::Unavailable {
                reason: format!("Failed to read response body: {e}"),
            }
        }
    };

    match parse_grok_response(&body) {
        Ok((windows, balance)) => UsageOutcome::Reading {
            windows,
            balance,
            meters: vec![],
            detail: None,
        },
        Err(e) => UsageOutcome::Unavailable {
            reason: format!("Failed to parse response: {e}"),
        },
    }
}

fn calculate_opencode_windows_impl(conn: &rusqlite::Connection) -> Result<Vec<UsageWindow>, String> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    
    let sum_cost_since = |since_ms: i64| -> Result<f64, rusqlite::Error> {
        let mut stmt = conn.prepare("SELECT SUM(cost) FROM session WHERE time_created >= ?")?;
        let cost: Option<f64> = stmt.query_row([since_ms], |row| row.get(0))?;
        Ok(cost.unwrap_or(0.0))
    };

    // Rolling limits:
    // - 5-Hour Rolling Limit: $12.00
    // - Weekly Limit: $30.00
    // - Monthly Limit: $60.00
    let five_hours_ms = 5 * 60 * 60 * 1000;
    let weekly_ms = 7 * 24 * 60 * 60 * 1000;
    let monthly_ms = 30 * 24 * 60 * 60 * 1000;

    let cost_5h = sum_cost_since(now_ms - five_hours_ms).map_err(|e| e.to_string())?;
    let cost_weekly = sum_cost_since(now_ms - weekly_ms).map_err(|e| e.to_string())?;
    let cost_monthly = sum_cost_since(now_ms - monthly_ms).map_err(|e| e.to_string())?;

    let limit_5h = 12.0;
    let limit_weekly = 30.0;
    let limit_monthly = 60.0;

    let pct_5h = (cost_5h / limit_5h) * 100.0;
    let pct_weekly = (cost_weekly / limit_weekly) * 100.0;
    let pct_monthly = (cost_monthly / limit_monthly) * 100.0;

    Ok(vec![
        UsageWindow {
            label: "5-hour".to_string(),
            used_percent: Some(pct_5h.clamp(0.0, 100.0)),
            resets_at: None,
        },
        UsageWindow {
            label: "Weekly".to_string(),
            used_percent: Some(pct_weekly.clamp(0.0, 100.0)),
            resets_at: None,
        },
        UsageWindow {
            label: "Monthly".to_string(),
            used_percent: Some(pct_monthly.clamp(0.0, 100.0)),
            resets_at: None,
        },
    ])
}

fn calculate_opencode_windows(db_path: &std::path::Path) -> Result<Vec<UsageWindow>, String> {
    if !db_path.exists() {
        return Ok(vec![
            UsageWindow {
                label: "5-hour".to_string(),
                used_percent: Some(0.0),
                resets_at: None,
            },
            UsageWindow {
                label: "Weekly".to_string(),
                used_percent: Some(0.0),
                resets_at: None,
            },
            UsageWindow {
                label: "Monthly".to_string(),
                used_percent: Some(0.0),
                resets_at: None,
            },
        ]);
    }

    let conn = rusqlite::Connection::open(db_path)
        .map_err(|e| format!("Failed to open DB: {}", e))?;

    calculate_opencode_windows_impl(&conn)
}

// ─── Live `_server billing.get` parser (issue #957) ────────────────────────
//
// Response shape (pinned fixture in `parse_opencode_billing_response_full`):
//   {
//     "windows": [
//       { "label": "5-hour",  "usedPercent": 25.0, "resetsAt": "2026-07-20T22:00:00Z" },
//       { "label": "Weekly",  "usedPercent": 12.0, "resetsAt": "2026-07-22T00:00:00Z" },
//       { "label": "Monthly", "usedPercent":  4.5, "resetsAt": "2026-08-01T00:00:00Z" }
//     ]
//   }
//
// OpenCode Go is a Plan account (#957 sub-spec point 2) so `balance` stays
// `None` — only `windows` is populated. The fetcher treats a body without a
// `windows` array as a `Shape` error so the degradation chain falls through
// to SQLite (#953) rather than silently zero-windowing.

#[derive(Deserialize, Debug)]
struct OpenCodeBillingWindow {
    label: Option<String>,
    #[serde(rename = "usedPercent")]
    used_percent: Option<f64>,
    #[serde(rename = "resetsAt")]
    resets_at: Option<String>,
}

#[derive(Deserialize, Debug)]
struct OpenCodeBillingResp {
    windows: Vec<OpenCodeBillingWindow>,
}

fn parse_opencode_billing_response(
    body: &str,
) -> Result<(Vec<UsageWindow>, Option<String>), UsageError> {
    let resp: OpenCodeBillingResp =
        serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;
    // A real window carries both `label` and `usedPercent` — a missing
    // `usedPercent` is the "shape failure" the degradation chain (#957
    // sub-spec point 4) routes to the SQLite fallback. We filter rather
    // than error out so a single malformed entry doesn't poison the whole
    // reply; if nothing survives the filter, the empty-windows detail below
    // surfaces to the user.
    let windows: Vec<UsageWindow> = resp
        .windows
        .into_iter()
        .filter_map(|w| match (w.label, w.used_percent) {
            (Some(label), Some(used_percent)) => Some(UsageWindow {
                label,
                used_percent: Some(used_percent),
                resets_at: w.resets_at,
            }),
            _ => None,
        })
        .collect();
    let detail = if windows.is_empty() {
        Some("No active OpenCode Go quotas found".to_string())
    } else {
        None
    };
    Ok((windows, detail))
}

/// Pure assembly: combines a live fetch outcome with the SQLite fallback
/// to produce the final [`UsageOutcome`]. The live path wins when it
/// returns `Reading`; any failure — `None` = no credential, or `Some`
/// carrying a non-`Reading` outcome — falls through to the SQLite result
/// so a user mid-OAuth always sees SOMETHING (issue #957 sub-spec
/// point 4).
fn choose_opencode_usage(live: Option<UsageOutcome>, sqlite: UsageOutcome) -> UsageOutcome {
    match live {
        Some(outcome @ UsageOutcome::Reading { .. }) => outcome,
        _ => sqlite,
    }
}

fn opencode_usage_impl(home: &std::path::Path) -> UsageOutcome {
    opencode_usage_impl_with_hosts(
        home,
        "https://opencode.ai/_server",
        &format!("{}{}", OPENCODE_CONSOLE_HOST, device_flow::TOKEN_PATH),
        None,
    )
}

/// Pure orchestration for the opencode usage pipeline, parameterized on
/// the network endpoints and the credential source. Production callers
/// ([`opencode_usage_impl`]) pin the production URLs and pass `None` for
/// `cred` so the live probe reads the Buildmesh-owned OAuth credential
/// from Windows Credential Manager. Tests inject a credential directly
/// and point both URLs at a loopback `tiny_http` listener so the full
/// refresh-on-401 round-trip is exercised without hitting the live
/// server (issue #971).
///
/// Pipeline:
///   1. **Pre-emptive refresh** (issue #970): if the credential is
///      expired OR the cached live-fetch result is older than
///      `REFRESH_TTL`, mint a fresh bearer BEFORE the live probe.
///   2. **Live probe**: POST `billing.get` to the configured `live_url`.
///   3. **Reactive refresh-on-401** (issue #971): if the pre-emptive
///      refresh did NOT fire AND the live probe returned 401, refresh
///      and retry the live probe ONCE. Handles the case where the
///      credential's `expires_at` claimed validity but the server
///      revoked the token (e.g., user signed out elsewhere).
///   4. **SQLite fallback** (#953): if the live path never produced a
///      usable envelope, fall back to the local `opencode.db` rolls.
///
/// The reactive retry is gated on "pre-emptive didn't fire" so a
/// credential that already failed pre-emptive refresh doesn't get
/// retried — the offline SQLite fallback is the right place for that
/// degraded state.
fn opencode_usage_impl_with_hosts(
    home: &std::path::Path,
    live_url: &str,
    refresh_url: &str,
    cred: Option<&OpenCodeConsoleCred>,
) -> UsageOutcome {
    let opencode_dir = home.join(".local").join("share").join("opencode");
    let auth_path = opencode_dir.join("auth.json");
    let db_path = opencode_dir.join("opencode.db");

    // Resolve the credential: test injects via `cred`; production
    // reads from Windows Credential Manager. Same shape either way.
    let initial_cred = cred
        .cloned()
        .or_else(|| read_opencode_console_credential_full().ok());

    // ── Pre-emptive refresh (issue #970) ─────────────────────────────
    //
    // If the cached credential is expired OR the cached live-fetch
    // result is older than REFRESH_TTL, mint a fresh bearer BEFORE the
    // `_server billing.get` HTTP call so a near-expiry token doesn't
    // 401 the fetch. Failure is logged and the seam continues — the
    // existing live path still runs and may surface a 401 the reactive
    // retry below handles, and the SQLite fallback (#953) catches the
    // worst case.
    let mut current_cred = initial_cred.clone();
    let mut pre_emptive_refresh_fired = false;
    if let Some(c) = &current_cred {
        let age = cached_age("opencode");
        let now_unix = chrono::Utc::now().timestamp();
        if opencode_needs_refresh(c, age, now_unix) {
            pre_emptive_refresh_fired = true;
            if let Some(refresh_token) = c.refresh_token.clone() {
                match crate::services::opencode_oauth::try_refresh_against(
                    refresh_url,
                    &refresh_token,
                ) {
                    Ok(token) => {
                        invalidate_provider_cache("opencode");
                        current_cred = Some(cred_from_token(
                            &token,
                            c.workspace_id.as_deref(),
                            c.server_id.as_deref(),
                        ));
                    }
                    Err(e) => tracing::warn!("opencode refresh failed: {e}"),
                }
            }
        }
    }

    // ── Live probe (first attempt) ──────────────────────────────────
    //
    // Reads the Buildmesh-owned OAuth credential (#956) and POSTs
    // `billing.get` to SolidStart. A missing credential part collapses
    // to `None` via `opencode_live_request_parts`, and HTTP-level
    // failures (401, 5xx, shape mismatch) surface as non-`Reading`
    // outcomes — either way `choose_opencode_usage` falls through to
    // the SQLite path below. The `X-Server-Id` header is sourced from
    // the persisted credential's `server_id` field (issue #972);
    // pre-#956 blobs fall through to the legacy default and trigger a
    // process-wide warn-once.
    let mut live = current_cred
        .as_ref()
        .and_then(|c| opencode_live_request_at(live_url, c));

    // ── Reactive refresh-on-401 (issue #971) ────────────────────────
    //
    // If the pre-emptive refresh did NOT fire (credential was fresh)
    // AND the live probe returned 401, the server revoked the token
    // under us (e.g., user signed out elsewhere, password reset).
    // Refresh and retry the live probe ONCE. The single-retry policy
    // bounds the worst case to 2 live + 1 refresh round-trips.
    if !pre_emptive_refresh_fired && needs_retry_on_401(live.as_ref()) {
        if let Some(c) = &current_cred {
            if let Some(refresh_token) = c.refresh_token.clone() {
                if let Ok(token) = crate::services::opencode_oauth::try_refresh_against(
                    refresh_url,
                    &refresh_token,
                ) {
                    let new_cred = cred_from_token(
                        &token,
                        c.workspace_id.as_deref(),
                        c.server_id.as_deref(),
                    );
                    live = opencode_live_request_at(live_url, &new_cred);
                }
            }
        }
    }

    // ── Offline SQLite fallback (#953) ──────────────────────────────
    //
    // Same auth.json gate as before — a user mid-OAuth (live path
    // failed but auth.json present) still gets real numbers; a user
    // who hasn't run any auth reports `NoCredential` here (the gate
    // drops the row, as the old `logged_out` envelope did).
    let _token = match read_opencode_token(auth_path) {
        Ok(t) => t,
        Err(e) => {
            return UsageOutcome::NoCredential {
                hint: e.to_string(),
            }
        }
    };

    let sqlite = match calculate_opencode_windows(&db_path) {
        Ok(windows) => UsageOutcome::Reading {
            windows,
            balance: None,
            meters: vec![],
            detail: None,
        },
        Err(e) => UsageOutcome::Unavailable {
            reason: format!("Failed to query opencode.db: {e}"),
        },
    };
    // Pure assembly pins the degradation contract: live wins when it returns
    // `Reading`, anything else falls through to SQLite.
    choose_opencode_usage(live, sqlite)
}

/// Fires the live `_server billing.get` probe against a parameterized
/// `live_url`. Extracted from `opencode_usage_impl_with_hosts` so the
/// pre-emptive + reactive retry paths share the same wire-binding
/// closure (header set, JSON body, parser) without duplicating the
/// `opencode_live_request_parts` + `fetch_usage` composition.
fn opencode_live_request_at(
    live_url: &str,
    cred: &OpenCodeConsoleCred,
) -> Option<UsageOutcome> {
    let (token, workspace_id, server_id) = opencode_live_request_parts(cred)?;
    let live_url_owned = live_url.to_string();
    Some(fetch_usage(
        crate::services::usage::outcome::AuthPolicy::Rejected,
        move |client| {
            client
                .post(&live_url_owned)
                .header("X-Server-Id", server_id)
                .header("Authorization", format!("Bearer {}", token))
                .json(&[workspace_id])
        },
        parse_opencode_billing_response,
    ))
}

/// True when the live probe's credential was rejected (the
/// "refresh-on-the-spot" trigger). Issue #1758: matches on the
/// `Rejected` outcome instead of substring-matching the wire error
/// string for `"401"` — the outcome is set by the shared driver's
/// 401/403 arm, so a body that merely mentions "401" can no longer
/// trigger a spurious refresh round-trip.
fn needs_retry_on_401(live: Option<&UsageOutcome>) -> bool {
    matches!(live, Some(UsageOutcome::Rejected { .. }))
}

/// Composes a fresh [`OpenCodeConsoleCred`] from a refresh response.
/// Mirrors the field shape of [`persist_token_response`] but skips the
/// Windows Credential Manager write — the test path uses this to
/// thread the new bundle into the reactive retry's live probe.
///
/// The token response (verified 2026-07-23) no longer carries
/// `workspace_id` or `server_id` — the OAuth scope is stable across
/// refreshes, so we read the prior `workspace_id` AND `server_id` from
/// the existing credential. The `server_id` fallback to the legacy
/// `OPENCODE_SERVER_ID` constant kicks in only when the prior credential
/// had no server_id (a pre-#956 blob), per the
/// `resolve_opencode_server_id` contract. New flows that bind a custom
/// `server_id` (issue #972 forwarded the OAuth dance's response) keep
/// it across refreshes — a custom-then-default flip would silently
/// degrade the live probe's `X-Server-Id` header.
///
/// Test-only: the call site in `opencode_usage_impl_with_hosts` passes
/// the prior credential's fields; the `try_refresh` path in
/// `services::opencode_oauth` writes the same shape via
/// `persist_token_response` so the live probe sees the same wiring
/// whether the credential was just refreshed or freshly issued.
fn cred_from_token(
    token: &crate::services::opencode_oauth::TokenResponse,
    prior_workspace_id: Option<&str>,
    prior_server_id: Option<&str>,
) -> OpenCodeConsoleCred {
    let expires_at = (chrono::Utc::now()
        + chrono::Duration::seconds(token.expires_in.as_secs() as i64))
    .to_rfc3339();
    OpenCodeConsoleCred {
        access_token: Some(token.access_token.clone()),
        workspace_id: prior_workspace_id.map(str::to_owned),
        refresh_token: Some(token.refresh_token.clone()),
        expires_at: Some(expires_at),
        server_id: prior_server_id.map(str::to_owned),
    }
}

/// Public OpenCode fetcher.
///
/// Issue #1745 phase 2 step 18: migrated to the outcome seam. The live
/// `_server billing.get` probe already classified through the shared
/// driver; the SQLite fallback now builds `Reading` directly, and the
/// reactive refresh-on-401 gate matches on the `Rejected` outcome
/// instead of substring-matching the wire error string.
pub fn opencode_usage() -> UsageOutcome {
    opencode_usage_impl(&home_dir())
}

// ─── Google / Antigravity (`agy`) ───────────────────────────────────────────
//
// The Antigravity CLI surfaces a per-model quota that is a DIFFERENT product
// from Gemini Code Assist: it lives on the `daily-cloudcode-pa` staging host,
// behind `retrieveUserQuotaSummary` (weekly + 5-hour shared buckets) with
// `fetchAvailableModels` as a five-hour-only fallback, and is gated purely
// by the client User-Agent.
// Auth is separate from `~/.gemini/oauth_creds.json`. Credential discovery
// (CLI oauth file + keyring fallback, Shape-vs-NoCredential contract, 401
// retry across sources) lives in `usage::adapters::agy` so new token-source
// lore does not accrue in this module (#1657).
//
// This path is deliberately best-effort and FRAGILE (staging host, User-Agent
// gate, no token refresh since the Antigravity OAuth client isn't recoverable).
// Per the module contract, any failure degrades to "unavailable", never errors.

const AGY_HOST: &str = "https://daily-cloudcode-pa.googleapis.com";
/// The Antigravity CLI identifies with this User-Agent and the Cloud Code private
/// API allowlists it. Load-bearing: without it the API returns 403 PERMISSION_DENIED.
const AGY_USER_AGENT: &str = "antigravity/cli/1.0.3 windows/amd64";

// ─── OpenCode Go (live `_server billing.get` probe) ────────────────────────
//
// OpenCode Go ships a SolidStart server-function RPC at
// `POST https://opencode.ai/_server` (function name `billing.get`) that returns
// the user's server-authoritative 5-hour / weekly / monthly usage windows
// (issue #957). The probe falls through to the offline SQLite path (#953) on
// any failure so a user mid-OAuth-flow keeps seeing SOMETHING instead of a
// silent blank gauge. The credential blob lives at this target, written by
// #956's Buildmesh-owned device-flow dance.

/// Legacy default for the SolidStart deployment id the `_server
/// billing.get` probe sends in the `X-Server-Id` header. Captured from the
/// opencode-cli binary's outbound traffic (issue #944 / research ticket).
/// Stable per deployment; not per-user.
///
/// After issue #956 ships the device-flow dance, fresh credentials persist
/// the same deployment id into [`OpenCodeConsoleCred::server_id`]
/// (`services::opencode_oauth`). The live probe — see
/// [`resolve_opencode_server_id`] — reads the persisted value first and
/// falls back to this constant when a blob predates the field (e.g. a
/// credential written by an older build, or a developer-only fixture). The
/// constant stays as the documented legacy default for at least one
/// release (#963/#972); remove it after re-authentication has rolled out
/// everywhere.
const OPENCODE_SERVER_ID: &str =
    "c83b78a614689c38ebee981f9b39a8b377716db85c1fd7dbab604adc02d3313d";

/// Reads the Buildmesh-owned OpenCode Console credential as the full DTO so
/// callers can consume the optional `server_id` (issue #972) — and so the
/// refresh seam (#970) can re-use the same read path to inspect
/// `refresh_token` + `expires_at`. The full DTO is now the only read
/// path: the previous narrow tuple-returning helper was retired when the
/// live fetch moved to [`opencode_live_request_parts`] for #972.
#[cfg(windows)]
fn read_opencode_console_credential_full() -> Result<OpenCodeConsoleCred, UsageError> {
    crate::services::opencode_oauth::parse_opencode_console_full_credential(&windows_cred::read(
        OPENCODE_CONSOLE_CRED_TARGET,
    )?)
}

#[cfg(not(windows))]
fn read_opencode_console_credential_full() -> Result<OpenCodeConsoleCred, UsageError> {
    Err(UsageError::NoCredential(
        "OpenCode Console usage is only available on Windows".to_string(),
    ))
}

/// Refresh-seam gate (issue #970): pure function deciding whether
/// `opencode_usage_impl` should call `try_refresh()` before its live `_server
/// billing.get` HTTP fetch. True when EITHER:
///
///   1. The credential's `expires_at` is in the past — a 401 is imminent,
///      so mint a new bearer proactively. Delegates to
///      `opencode_oauth::cred_is_expired`, which treats missing/malformed
///      `expires_at` as `false` so the live fetch still gets a chance.
///   2. The cached live-fetch result is older than
///      `opencode_oauth::REFRESH_TTL` — the credential was fresh at fetch
///      time but is plausibly near expiry by now (defense-in-depth against
///      the credential blob's `expires_at` drifting from the server's view).
///
/// Pure (no I/O) so the seam is unit-testable without a Windows Credential
/// Manager fixture. Extracted from `opencode_usage_impl` for that reason —
/// the seam itself is mostly I/O orchestration around this decision.
fn opencode_needs_refresh(
    cred: &OpenCodeConsoleCred,
    cached_age: Option<Duration>,
    now_unix: i64,
) -> bool {
    use crate::services::opencode_oauth;
    opencode_oauth::cred_is_expired(cred, now_unix)
        || cached_age.is_some_and(|age| age > opencode_oauth::REFRESH_TTL)
}

/// Resolves the value the live `_server billing.get` probe should send in
/// the `X-Server-Id` header (issue #972).
///
/// Primary source is `OpenCodeConsoleCred.server_id` — the value the OAuth
/// device-flow exchange returned and `persist_token_response` wrote into
/// the persisted blob. Fallback is [`OPENCODE_SERVER_ID`] for blobs written
/// before #956 added the field; the fallback fires a single process-wide
/// `tracing::warn!` so a user who re-authenticates sees the warning stop.
///
/// Empty-string `server_id` is treated as missing — a hand-edited blob
/// with `"server_id": ""` must not produce a useless `X-Server-Id: ` header.
fn resolve_opencode_server_id(cred: &OpenCodeConsoleCred) -> &str {
    if let Some(id) = cred.server_id.as_deref() {
        if !id.is_empty() {
            return id;
        }
    }
    warn_legacy_opencode_server_id_once();
    OPENCODE_SERVER_ID
}

/// Process-wide once-cell for the legacy-server-id warning. The cell lives
/// for the lifetime of the buildmesh process; re-authenticating writes a
/// fresh `server_id` into the blob and the resolver takes the
/// `cred.server_id` branch on subsequent probes, so the warning never
/// fires again even though the `Once` itself never resets.
fn warn_legacy_opencode_server_id_once() {
    use std::sync::Once;
    static WARNED: Once = Once::new();
    WARNED.call_once(|| {
        tracing::warn!(
            target: "services::opencode_oauth",
            "OpenCode Console credential predates the `server_id` field; \
             falling back to the legacy OPENCODE_SERVER_ID constant. \
             Re-authenticating will persist a fresh `server_id` into the \
             credential blob and silence this warning. (issue #972)"
        );
    });
}

/// Pure pipeline that produces the three strings the live `_server
/// billing.get` probe needs to bind into its HTTP request: the bearer
/// token, the workspace id (JSON body), and the `X-Server-Id` header
/// value. Extracted so the binding contract (issue #972 acceptance #5)
/// is unit-testable without standing up an HTTP mock.
///
/// Returns `None` when the credential lacks a non-empty `access_token`
/// or `workspace_id` — the caller (`opencode_usage_impl`) treats that as
/// "no credential" and falls through to the SQLite path identically to a
/// `NoCredential` read.
fn opencode_live_request_parts(cred: &OpenCodeConsoleCred) -> Option<(String, String, String)> {
    let token = cred.access_token.clone().filter(|s| !s.is_empty())?;
    let workspace_id = cred.workspace_id.clone().filter(|s| !s.is_empty())?;
    let server_id = resolve_opencode_server_id(cred).to_owned();
    Some((token, workspace_id, server_id))
}

/// Minimal FFI to the Windows Credential Manager (`advapi32!CredReadW` /
///
/// `CredWriteW` / `CredDeleteW`) was extracted out of this module for #956
/// and now lives at [`crate::services::windows_cred`]; the local
/// `cfg(windows)` `use` at the top of `usage` keeps the call sites
/// (`adapters::agy` keyring read, `read_opencode_console_credential`) reading naturally.
///
/// [`crate::services::windows_cred`]: crate::services::windows_cred

#[derive(Deserialize)]
struct AgyLoadResp {
    #[serde(rename = "cloudaicompanionProject")]
    cloudaicompanion_project: Option<String>,
}

/// `loadCodeAssist` bootstraps the session and returns the user's auto-managed
/// cloudaicompanion project, which `fetchAvailableModels` then requires.
fn agy_load_project(client: &Client, token: &str) -> Result<String, UsageError> {
    let resp = client
        .post(format!("{AGY_HOST}/v1internal:loadCodeAssist"))
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", AGY_USER_AGENT)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "metadata": {} }))
        .send()
        .map_err(|e| UsageError::Shape(format!("loadCodeAssist failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(UsageError::Shape(format!(
            "loadCodeAssist HTTP {} — try re-authenticating via the Antigravity CLI",
            resp.status().as_u16()
        )));
    }
    let parsed: AgyLoadResp = serde_json::from_str(&resp.text().unwrap_or_default())
        .map_err(|e| UsageError::Shape(e.to_string()))?;
    parsed
        .cloudaicompanion_project
        .filter(|s| !s.is_empty())
        .ok_or_else(|| UsageError::Shape("loadCodeAssist returned no project".into()))
}

#[derive(Deserialize)]
struct AgyQuotaInfo {
    #[serde(rename = "remainingFraction")]
    remaining_fraction: Option<f64>,
    #[serde(rename = "resetTime")]
    reset_time: Option<String>,
}

#[derive(Deserialize)]
struct AgyModel {
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    #[serde(rename = "quotaInfo")]
    quota_info: Option<AgyQuotaInfo>,
}

#[derive(Deserialize)]
struct AgyGroup {
    #[serde(rename = "modelIds", default)]
    model_ids: Vec<String>,
}

#[derive(Deserialize)]
struct AgySort {
    #[serde(default)]
    groups: Vec<AgyGroup>,
}

#[derive(Deserialize)]
struct AgyModelsResp {
    #[serde(default)]
    models: HashMap<String, AgyModel>,
    #[serde(rename = "agentModelSorts", default)]
    agent_model_sorts: Vec<AgySort>,
}

/// Wire shape of `POST /v1internal:retrieveUserQuotaSummary`. Unlike
/// `fetchAvailableModels` (five-hour remainingFraction only), this is the
/// endpoint the CLI `/usage` command wraps: per-group shared weekly + 5-hour
/// buckets. Calling it over HTTP avoids a ~6s `agy` process spawn that stalled
/// the whole Usage Probe after #1324.
#[derive(Deserialize)]
struct AgyQuotaSummaryResp {
    #[serde(default)]
    groups: Vec<AgyQuotaGroup>,
}

#[derive(Deserialize)]
struct AgyQuotaGroup {
    #[serde(rename = "displayName")]
    display_name: String,
    #[serde(default)]
    buckets: Vec<AgyQuotaBucket>,
}

#[derive(Deserialize)]
struct AgyQuotaBucket {
    window: String,
    #[serde(rename = "remainingFraction")]
    remaining_fraction: Option<f64>,
    #[serde(rename = "resetTime")]
    reset_time: Option<String>,
}

fn agy_window_label(window: &str) -> String {
    match window {
        "weekly" => "Weekly".to_string(),
        "5h" => "5-hour".to_string(),
        other => other.to_string(),
    }
}

/// Rank used to put 5-hour before weekly inside each group, matching Anthropic /
/// Codex / OpenCode meter order. Unknown windows keep their relative position
/// after the two known ones.
fn agy_window_rank(window: &str) -> u8 {
    match window {
        "5h" => 0,
        "weekly" => 1,
        _ => 2,
    }
}

fn parse_agy_quota_summary(
    body: &str,
) -> Result<(Vec<UsageWindow>, Option<String>), UsageError> {
    let resp: AgyQuotaSummaryResp =
        serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;

    let mut windows = Vec::new();
    for group in resp.groups {
        let mut buckets = group.buckets;
        buckets.sort_by_key(|bucket| agy_window_rank(&bucket.window));
        for bucket in buckets {
            if let Some(fraction) = bucket.remaining_fraction {
                windows.push(UsageWindow {
                    label: format!(
                        "{} — {}",
                        group.display_name,
                        agy_window_label(&bucket.window)
                    ),
                    used_percent: Some((1.0 - fraction) * 100.0),
                    resets_at: bucket.reset_time,
                });
            }
        }
    }
    if windows.is_empty() {
        return Err(UsageError::Shape(
            "retrieveUserQuotaSummary returned no quota buckets".to_string(),
        ));
    }
    Ok((windows, None))
}

fn agy_quota_summary(
    client: &Client,
    token: &str,
) -> Result<(Vec<UsageWindow>, Option<String>), UsageError> {
    let resp = client
        .post(format!("{AGY_HOST}/v1internal:retrieveUserQuotaSummary"))
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", AGY_USER_AGENT)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({}))
        .send()
        .map_err(|e| UsageError::Shape(format!("retrieveUserQuotaSummary failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(UsageError::Shape(format!(
            "retrieveUserQuotaSummary HTTP {}",
            resp.status().as_u16()
        )));
    }
    parse_agy_quota_summary(&resp.text().unwrap_or_default())
}

/// Models whose `displayName` starts with this prefix all draw from one shared
/// Gemini bucket on Google's side (verified live 2026-06-04), even though the
/// API reports them as separate effort-level entries (Flash Low/Medium/High,
/// Pro Low/High). We collapse them into a single row to avoid five identical bars.
const GEMINI_DISPLAY_PREFIX: &str = "Gemini";
const GEMINI_COLLAPSED_LABEL: &str = "Gemini (all models)";

/// Appends one window per model that carries a quota. `used_percent` is the
/// inverse of the remaining fraction, matching how the other providers report.
/// Gemini-prefixed models are emitted at most once (relabeled to
/// `GEMINI_COLLAPSED_LABEL`) — see the prefix constant for the rationale.
fn push_agy_window(windows: &mut Vec<UsageWindow>, model: &AgyModel, seen_gemini: &mut bool) {
    if let (Some(name), Some(q)) = (&model.display_name, &model.quota_info) {
        if let Some(fraction) = q.remaining_fraction {
            let is_gemini = name.starts_with(GEMINI_DISPLAY_PREFIX);
            if is_gemini && *seen_gemini {
                return;
            }
            let label = if is_gemini {
                *seen_gemini = true;
                GEMINI_COLLAPSED_LABEL.to_string()
            } else {
                name.clone()
            };
            windows.push(UsageWindow {
                label,
                used_percent: Some((1.0 - fraction) * 100.0),
                resets_at: q.reset_time.clone(),
            });
        }
    }
}

/// Builds usage windows from `fetchAvailableModels`. The first `agentModelSorts`
/// entry dictates which models (and in what order) the Antigravity UI surfaces;
/// we mirror it, falling back to every quota-bearing model if it's absent.
fn parse_agy_models(body: &str) -> Result<(Vec<UsageWindow>, Option<String>), UsageError> {
    let resp: AgyModelsResp =
        serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;

    let ordered_ids: Vec<&String> = resp
        .agent_model_sorts
        .first()
        .map(|sort| sort.groups.iter().flat_map(|g| g.model_ids.iter()).collect())
        .unwrap_or_default();

    let mut windows = vec![];
    let mut seen_gemini = false;
    if ordered_ids.is_empty() {
        for model in resp.models.values() {
            push_agy_window(&mut windows, model, &mut seen_gemini);
        }
    } else {
        for id in ordered_ids {
            if let Some(model) = resp.models.get(id) {
                push_agy_window(&mut windows, model, &mut seen_gemini);
            }
        }
    }

    let detail = if windows.is_empty() {
        Some("No active model quotas found".to_string())
    } else {
        None
    };
    Ok((windows, detail))
}

/// True when an Antigravity HTTP helper reported 401/403 — the bearer was
/// rejected, so a different credential source may still succeed.
fn agy_http_auth_failure(err: &UsageError) -> bool {
    match err {
        UsageError::Shape(msg) => msg.contains("HTTP 401") || msg.contains("HTTP 403"),
        UsageError::NoCredential(_) => false,
    }
}

/// One-token attempt: quota summary, then model-API fallback. Returns
/// `Err` on auth rejection or hard failure so the caller can try the next
/// credential source; success and model-API outcomes return directly.
fn try_agy_usage_with_token(client: &Client, token: &str) -> Result<UsageOutcome, UsageError> {
    // retrieveUserQuotaSummary is the HTTP surface behind `agy /usage`: both
    // the 5-hour and weekly shared buckets, without booting the CLI. Empty
    // body is enough (project is optional). Fall back to fetchAvailableModels
    // when the summary RPC is missing or empty so older backends still show
    // the five-hour meter.
    match agy_quota_summary(client, token) {
        Ok((windows, detail)) => {
            return Ok(UsageOutcome::Reading {
                windows,
                balance: None,
                meters: vec![],
                detail,
            });
        }
        Err(error) if agy_http_auth_failure(&error) => return Err(error),
        Err(error) => {
            tracing::debug!(
                "Antigravity quota summary unavailable; falling back to model API: {error}"
            );
        }
    }
    // fetchAvailableModels needs the user's cloudaicompanion project, which
    // loadCodeAssist hands back.
    let project = agy_load_project(client, token)?;

    Ok(fetch_usage(
        crate::services::usage::outcome::AuthPolicy::Rejected,
        |c| {
            c.post(format!("{AGY_HOST}/v1internal:fetchAvailableModels"))
                .header("Authorization", format!("Bearer {token}"))
                .header("User-Agent", AGY_USER_AGENT)
                .header("Content-Type", "application/json")
                .json(&serde_json::json!({ "project": project }))
        },
        parse_agy_models,
    ))
}

/// Classify a per-source fetch failure into the outcome taxonomy (issue
/// #1758): an auth rejection means the Bearer [REDACTED] gone/bad (`Rejected`, so the
/// gate drops the native row); anything else is transport-class
/// (`Unavailable`, so the row stays visible with red error copy).
fn agy_outcome_from_source_error(error: UsageError) -> UsageOutcome {
    if agy_http_auth_failure(&error) {
        UsageOutcome::Rejected {
            hint: error.to_string(),
        }
    } else {
        UsageOutcome::Unavailable {
            reason: error.to_string(),
        }
    }
}

/// Public Antigravity fetcher. Collects the CLI oauth-file and keyring
/// tokens, then tries each in order until one succeeds.
///
/// Issue #1745 phase 2 step 12: migrated to the outcome seam. Missing
/// tokens → `NoCredential` (the gate drops the row, as before). The
/// client-build failure and non-auth source failures are transport-class →
/// `Unavailable` (previously `logged_out`, which silently dropped the row).
pub fn agy_usage() -> UsageOutcome {
    use crate::services::usage::adapters::agy::collect_agy_access_tokens_default;

    let tokens = match collect_agy_access_tokens_default() {
        Ok(t) => t,
        Err(e) => {
            return UsageOutcome::NoCredential {
                hint: e.to_string(),
            }
        }
    };
    let client = match Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return UsageOutcome::Unavailable {
                reason: format!("Client error: {e}"),
            }
        }
    };

    let last = tokens.len().saturating_sub(1);
    for (idx, cred) in tokens.iter().enumerate() {
        match try_agy_usage_with_token(&client, &cred.access_token) {
            Ok(outcome) => {
                tracing::debug!(
                    target: "services::usage::agy",
                    source = cred.source.as_str(),
                    "Antigravity usage fetch succeeded"
                );
                return outcome;
            }
            Err(error) if agy_http_auth_failure(&error) && idx < last => {
                tracing::debug!(
                    target: "services::usage::agy",
                    source = cred.source.as_str(),
                    next = tokens[idx + 1].source.as_str(),
                    error = %error,
                    "Antigravity auth rejected; trying next credential source"
                );
                continue;
            }
            Err(error) => {
                // Auth rejection on the last source, or loadCodeAssist failure
                // after quota-summary fell through — classified by
                // `agy_outcome_from_source_error` so rejections drop the row
                // and transport failures stay visible.
                return agy_outcome_from_source_error(error);
            }
        }
    }

    UsageOutcome::NoCredential {
        hint: "Antigravity OAuth token not found (antigravity-oauth-token or gemini:antigravity)"
            .to_string(),
    }
}

// Freebuff (`freebuff`) implementation lives in `services::freebuff_usage`
// (issue #1438 review) and dispatches through
// `services::usage::adapters::FreebuffAdapter` (issue #1657). The catalog no
// longer re-exports the fetcher through this module.
//
// Detection-gating and dispatch wiring live in `services::usage::catalog`.

// Cache moved to `usage::cache` (issue #1657) — re-exported at the top.

/// Re-export the loopback HTTP fixture at the parent module level so
/// `services::freebuff_usage::tests` (a sibling test mod) can share it
/// instead of duplicating the implementation.
#[cfg(test)]
pub(crate) use crate::services::usage::adapter::spawn_loopback;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;


    #[test]
    fn agy_http_auth_failure_detects_401_and_403() {
        assert!(agy_http_auth_failure(&UsageError::Shape(
            "retrieveUserQuotaSummary HTTP 401".into()
        )));
        assert!(agy_http_auth_failure(&UsageError::Shape(
            "loadCodeAssist HTTP 403 — try re-authenticating via the Antigravity CLI".into()
        )));
        assert!(!agy_http_auth_failure(&UsageError::Shape(
            "retrieveUserQuotaSummary HTTP 500".into()
        )));
        assert!(!agy_http_auth_failure(&UsageError::NoCredential(
            "missing".into()
        )));
    }

    #[test]
    fn agy_source_error_classifies_auth_rejection_as_rejected() {
        // Issue #1758: auth rejection on the last source means the Bearer [REDACTED]
        // gone/bad — `Rejected` so the gate drops the native row (same
        // user-visible result as the old `logged_out` envelope).
        match agy_outcome_from_source_error(UsageError::Shape(
            "retrieveUserQuotaSummary HTTP 401".into(),
        )) {
            UsageOutcome::Rejected { hint } => {
                assert!(hint.contains("401"), "hint must carry the status, got: {hint:?}");
            }
            other => panic!("expected Rejected outcome, got: {other:?}"),
        }
    }

    #[test]
    fn agy_source_error_classifies_transport_failure_as_unavailable() {
        // Issue #1758: a non-auth source failure (e.g. loadCodeAssist
        // transport error) is transport-class — `Unavailable` keeps the row
        // visible with red error copy instead of silently dropping it.
        match agy_outcome_from_source_error(UsageError::Shape(
            "loadCodeAssist failed: connection refused".into(),
        )) {
            UsageOutcome::Unavailable { reason } => {
                assert!(
                    reason.contains("loadCodeAssist"),
                    "reason must carry the cause, got: {reason:?}"
                );
            }
            other => panic!("expected Unavailable outcome, got: {other:?}"),
        }
    }

    #[test]
    fn parse_agy_quota_summary_puts_five_hour_before_weekly_in_each_group() {
        // retrieveUserQuotaSummary returns weekly then 5-hour in each group.
        // Every other Usage Meter (Anthropic, Codex, OpenCode) lists the 5-hour
        // window first; keep Antigravity in that order.
        let json = r#"{
            "groups": [
                {
                    "displayName": "Gemini Models",
                    "buckets": [
                        {"bucketId":"gemini-weekly","displayName":"Weekly Limit Remaining","window":"weekly","remainingFraction":0.5,"resetTime":"2026-08-29T17:48:59Z"},
                        {"bucketId":"gemini-5h","displayName":"Five Hour Limit Remaining","window":"5h","remainingFraction":0.75,"resetTime":"2026-08-28T00:03:11Z"}
                    ]
                },
                {
                    "displayName": "Claude and GPT models",
                    "buckets": [
                        {"bucketId":"3p-weekly","displayName":"Weekly Limit Remaining","window":"weekly","remainingFraction":0.25,"resetTime":"2026-09-03T16:17:33Z"},
                        {"bucketId":"3p-5h","displayName":"Five Hour Limit Remaining","window":"5h","remainingFraction":0,"resetTime":"2026-08-27T21:17:33Z"}
                    ]
                }
            ]
        }"#;

        let (windows, detail) = parse_agy_quota_summary(json).unwrap();

        assert_eq!(windows.len(), 4);
        assert_eq!(windows[0].label, "Gemini Models — 5-hour");
        assert_eq!(windows[0].used_percent, Some(25.0));
        assert_eq!(windows[0].resets_at.as_deref(), Some("2026-08-28T00:03:11Z"));
        assert_eq!(windows[1].label, "Gemini Models — Weekly");
        assert_eq!(windows[1].used_percent, Some(50.0));
        assert_eq!(windows[1].resets_at.as_deref(), Some("2026-08-29T17:48:59Z"));
        assert_eq!(windows[2].label, "Claude and GPT models — 5-hour");
        // remainingFraction 0 is a real exhausted window, not "missing".
        assert_eq!(windows[2].used_percent, Some(100.0));
        assert_eq!(windows[3].label, "Claude and GPT models — Weekly");
        assert_eq!(windows[3].used_percent, Some(75.0));
        assert_eq!(detail, None);
    }

    #[test]
    fn parse_agy_quota_summary_empty_result_falls_back_to_model_api() {
        // An empty but successful response is transient upstream state, not a
        // meaningful zero-quota reading. Returning an error lets `agy_usage`
        // retain the older model-API meter instead of caching a blank card.
        let json = r#"{"groups":[]}"#;
        assert!(parse_agy_quota_summary(json).is_err());
    }

    #[test]
    fn parse_agy_quota_summary_missing_group_name_is_shape() {
        // Fail loudly: a missing displayName is not an unnamed meter.
        let json = r#"{"groups":[{"buckets":[{"window":"5h","remainingFraction":0.5}]}]}"#;
        assert!(parse_agy_quota_summary(json).is_err());
    }

    #[test]
    fn agy_usage_does_not_spawn_cli() {
        // #1324 spawned `agy --print /usage` (≈6s CLI boot) to reach the weekly
        // Gemini bucket. retrieveUserQuotaSummary is the same payload over HTTP
        // (~250ms). Spawning the CLI stalls the whole Usage Probe.
        let src = include_str!("usage.rs");
        assert!(
            !src.contains("command_no_window(\"agy\")"),
            "do not spawn the agy CLI for usage; call retrieveUserQuotaSummary instead"
        );
        assert!(
            !src.contains("\"/usage\""),
            "do not spawn agy --print /usage; call retrieveUserQuotaSummary instead"
        );
    }

    #[test]
    fn parse_agy_models_follows_sort_order_and_inverts_fraction() {
        // Two ranked models + one with no quota (must be skipped). The sort order
        // (claude before flash) must be preserved regardless of map iteration.
        let json = r#"{
            "models": {
                "m-flash": {"displayName":"Gemini 3.5 Flash (Medium)","quotaInfo":{"remainingFraction":0.8,"resetTime":"2026-05-31T12:22:46Z"}},
                "m-claude": {"displayName":"Claude Sonnet 4.6 (Thinking)","quotaInfo":{"remainingFraction":1.0,"resetTime":"2026-05-31T16:51:02Z"}},
                "m-hidden": {"displayName":"No Quota Model"}
            },
            "agentModelSorts": [{"displayName":"Recommended","groups":[{"modelIds":["m-claude","m-flash"]}]}]
        }"#;
        let (windows, detail) = parse_agy_models(json).unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].label, "Claude Sonnet 4.6 (Thinking)");
        assert_eq!(windows[0].used_percent, Some(0.0));
        // Single Gemini entry is still relabeled — the collapsed label applies
        // unconditionally so the row always reads as "this is your Gemini budget".
        assert_eq!(windows[1].label, "Gemini (all models)");
        // Same float expression the parser uses (0.8 remaining → ~20% used).
        assert_eq!(windows[1].used_percent, Some((1.0 - 0.8) * 100.0));
        assert_eq!(windows[1].resets_at.as_deref(), Some("2026-05-31T12:22:46Z"));
        assert_eq!(detail, None);
    }

    #[test]
    fn parse_agy_models_falls_back_to_all_when_no_sorts() {
        let json = r#"{"models":{"a":{"displayName":"Model A","quotaInfo":{"remainingFraction":0.5}}}}"#;
        let (windows, _) = parse_agy_models(json).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].used_percent, Some(50.0));
    }

    #[test]
    fn parse_agy_models_empty_reports_detail() {
        let (windows, detail) = parse_agy_models(r#"{"models":{}}"#).unwrap();
        assert!(windows.is_empty());
        assert_eq!(detail.as_deref(), Some("No active model quotas found"));
    }

    #[test]
    fn parse_agy_models_collapses_gemini_effort_levels() {
        // Antigravity returns one quota row per Gemini effort level (Low / Medium /
        // High, Pro Low / High) but they all read off one shared bucket on
        // Google's side, so we render them as a single "Gemini (all models)" row
        // at the first Gemini entry's position in the sort. Non-Gemini models
        // pass through unchanged. In production all Gemini entries carry the
        // SAME remainingFraction (verified live 2026-06-04).
        let json = r#"{
            "models": {
                "m-flash-low":   {"displayName":"Gemini 3.5 Flash (Low)",   "quotaInfo":{"remainingFraction":0.7,"resetTime":"2026-06-04T12:00:00Z"}},
                "m-flash-med":   {"displayName":"Gemini 3.5 Flash (Medium)","quotaInfo":{"remainingFraction":0.7,"resetTime":"2026-06-04T12:00:00Z"}},
                "m-flash-high":  {"displayName":"Gemini 3.5 Flash (High)",  "quotaInfo":{"remainingFraction":0.7,"resetTime":"2026-06-04T12:00:00Z"}},
                "m-claude":      {"displayName":"Claude Sonnet 4.6 (Thinking)","quotaInfo":{"remainingFraction":1.0,"resetTime":"2026-06-04T16:00:00Z"}},
                "m-gpt":         {"displayName":"GPT-OSS 120B","quotaInfo":{"remainingFraction":0.5,"resetTime":"2026-06-04T18:00:00Z"}}
            },
            "agentModelSorts":[{"groups":[{"modelIds":["m-claude","m-flash-med","m-flash-low","m-flash-high","m-gpt"]}]}]
        }"#;
        let (windows, detail) = parse_agy_models(json).unwrap();
        // 5 input models → 3 windows out (the 3 Gemini rows collapse to 1).
        assert_eq!(windows.len(), 3, "Gemini effort levels should collapse to a single row");
        // Sort order preserved: claude → (collapsed gemini at first gemini's slot) → gpt.
        assert_eq!(windows[0].label, "Claude Sonnet 4.6 (Thinking)");
        assert_eq!(windows[1].label, "Gemini (all models)");
        assert_eq!(windows[1].used_percent, Some((1.0 - 0.7) * 100.0));
        assert_eq!(windows[1].resets_at.as_deref(), Some("2026-06-04T12:00:00Z"));
        assert_eq!(windows[2].label, "GPT-OSS 120B");
        assert_eq!(detail, None);
    }

    #[test]
    fn agy_load_project_extracts_companion_project() {
        let resp: AgyLoadResp = serde_json::from_str(
            r#"{"currentTier":{"id":"standard-tier"},"cloudaicompanionProject":"sinuous-strategy-j3z18"}"#,
        )
        .unwrap();
        assert_eq!(resp.cloudaicompanion_project.as_deref(), Some("sinuous-strategy-j3z18"));
    }

    // ─── Grok ─────────────────────────────────────────────────────────────

    #[test]
    fn parse_grok_response_unified_billing_valid() {
        let json = r#"{
            "config": {
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_WEEKLY",
                    "start": "2026-07-15T00:00:00+00:00",
                    "end": "2026-07-22T00:00:00+00:00"
                },
                "onDemandCap": { "val": 10.0 },
                "onDemandUsed": { "val": 2.5 },
                "isUnifiedBillingUser": true,
                "prepaidBalance": { "val": 0.0 },
                "billingPeriodEnd": "2026-07-22T00:00:00+00:00"
            }
        }"#;
        let (windows, balance) = parse_grok_response(json).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "Weekly Pool");
        assert_eq!(windows[0].used_percent, Some(25.0));
        assert_eq!(windows[0].resets_at.as_deref(), Some("2026-07-22T00:00:00+00:00"));
        assert!(balance.is_none());
    }

    #[test]
    fn parse_grok_response_supergrok_token_plan_uses_credit_percentage() {
        let json = r#"{
            "config": {
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_WEEKLY",
                    "start": "2026-08-24T00:00:00+00:00",
                    "end": "2026-08-31T00:00:00+00:00"
                },
                "creditUsagePercent": 37.0,
                "onDemandCap": { "val": 0.0 },
                "onDemandUsed": { "val": 0.0 },
                "productUsage": [
                    { "product": "grok-build", "usagePercent": 37.0 }
                ],
                "isUnifiedBillingUser": true,
                "prepaidBalance": { "val": 0.0 },
                "billingPeriodEnd": "2026-08-31T00:00:00+00:00"
            }
        }"#;
        let (windows, _) = parse_grok_response(json).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "Weekly Pool");
        assert_eq!(windows[0].used_percent, Some(37.0));
    }

    #[test]
    fn parse_grok_response_monthly_limit_valid() {
        let json = r#"{
            "config": {
                "monthlyLimit": { "val": 50.0 },
                "used": { "val": 10.0 },
                "isUnifiedBillingUser": false,
                "billingPeriodEnd": "2026-08-01T00:00:00+00:00"
            }
        }"#;
        let (windows, _) = parse_grok_response(json).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "Monthly Limit");
        assert_eq!(windows[0].used_percent, Some(20.0));
        assert_eq!(windows[0].resets_at.as_deref(), Some("2026-08-01T00:00:00+00:00"));
    }

    #[test]
    fn parse_grok_response_prepaid_balance() {
        let json = r#"{
            "config": {
                "prepaidBalance": { "val": 15.75 },
                "onDemandUsed": { "val": 4.25 }
            }
        }"#;
        let (_, balance) = parse_grok_response(json).unwrap();
        let balance = balance.expect("prepaid balance present");
        assert_eq!(balance.remaining, 15.75);
        assert_eq!(balance.monthly_spend, Some(4.25));
        assert_eq!(balance.currency, "USD");
    }

    #[test]
    fn grok_usage_with_empty_key_returns_logged_out() {
        // Force an empty path to trigger logged_out path
        let usage = read_grok_token(PathBuf::from("")).map(|(t, _u)| t).unwrap_or_else(|e| e.to_string());
        assert!(usage.contains("No credential found"));
    }

    /// Helper: write a `~/.grok/auth.json`-shaped credential file into a
    /// tempdir so `grok_usage_with` tests stay hermetic.
    fn write_grok_auth(dir: &tempfile::TempDir) -> PathBuf {
        let path = dir.path().join("auth.json");
        fs::write(
            &path,
            r#"{"https://auth.x.ai::test": {"key": "sk-grok-test", "user_id": "user-1"}}"#,
        )
        .unwrap();
        path
    }

    #[test]
    fn adapter_seam_contract_grok_no_credential_without_network() {
        // A missing credential file must report `NoCredential` without
        // touching the network — the unreachable base URL below would
        // fail loudly if hit. (`dispatch("grok").fetch` is not hermetic
        // here: it reads the real `~/.grok/auth.json`.)
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("auth.json");
        match grok_usage_with(&missing, "http://127.0.0.1:1") {
            UsageOutcome::NoCredential { .. } => {}
            other => panic!("expected NoCredential outcome, got: {other:?}"),
        }
    }

    #[test]
    fn grok_usage_with_401_returns_rejected_with_invalid_key_hint() {
        let dir = tempfile::tempdir().unwrap();
        let auth_path = write_grok_auth(&dir);
        let port = spawn_loopback(1, |req| {
            let _ = req.respond(tiny_http::Response::empty(401));
        });
        match grok_usage_with(&auth_path, &format!("http://127.0.0.1:{port}")) {
            UsageOutcome::Rejected { hint } => {
                assert_eq!(hint, "Invalid API key");
            }
            other => panic!("expected Rejected outcome, got: {other:?}"),
        }
    }

    #[test]
    fn grok_usage_with_429_returns_rate_limited() {
        let dir = tempfile::tempdir().unwrap();
        let auth_path = write_grok_auth(&dir);
        let port = spawn_loopback(1, |req| {
            let _ = req.respond(tiny_http::Response::empty(429));
        });
        match grok_usage_with(&auth_path, &format!("http://127.0.0.1:{port}")) {
            UsageOutcome::RateLimited { reason } => {
                assert!(
                    reason.contains("Rate limited"),
                    "rate-limit copy must be preserved, got: {reason:?}"
                );
            }
            other => panic!("expected RateLimited outcome, got: {other:?}"),
        }
    }

    #[test]
    fn grok_usage_with_transport_failure_returns_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let auth_path = write_grok_auth(&dir);
        match grok_usage_with(&auth_path, "http://127.0.0.1:1") {
            UsageOutcome::Unavailable { reason } => {
                assert!(
                    reason.contains("Request failed"),
                    "transport envelope must be preserved, got: {reason:?}"
                );
            }
            other => panic!("expected Unavailable outcome, got: {other:?}"),
        }
    }

    #[test]
    fn grok_usage_with_live_loopback_returns_reading() {
        let dir = tempfile::tempdir().unwrap();
        let auth_path = write_grok_auth(&dir);
        let port = spawn_loopback(1, |req| {
            let _ = req.respond(tiny_http::Response::from_string(
                r#"{
                    "config": {
                        "currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY"},
                        "creditUsagePercent": 37.0,
                        "isUnifiedBillingUser": true,
                        "billingPeriodEnd": "2026-08-31T00:00:00+00:00"
                    }
                }"#,
            ));
        });
        match grok_usage_with(&auth_path, &format!("http://127.0.0.1:{port}")) {
            UsageOutcome::Reading { windows, .. } => {
                assert_eq!(windows.len(), 1);
                assert_eq!(windows[0].label, "Weekly Pool");
                assert_eq!(windows[0].used_percent, Some(37.0));
            }
            other => panic!("expected Reading outcome, got: {other:?}"),
        }
    }

    #[test]
    fn grok_usage_with_malformed_body_returns_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let auth_path = write_grok_auth(&dir);
        let port = spawn_loopback(1, |req| {
            let _ = req.respond(tiny_http::Response::from_string("not-json"));
        });
        match grok_usage_with(&auth_path, &format!("http://127.0.0.1:{port}")) {
            UsageOutcome::Unavailable { reason } => {
                assert!(
                    reason.contains("Failed to parse response"),
                    "parse envelope must be preserved, got: {reason:?}"
                );
            }
            other => panic!("expected Unavailable outcome, got: {other:?}"),
        }
    }

    // ─── OpenCode ─────────────────────────────────────────────────────────

    #[test]
    fn parse_opencode_auth_json_valid() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("opencode_auth.json");
        let json = r#"{
            "google": { "type": "api", "key": "AIzaSy..." },
            "opencode-go": { "type": "api", "key": "sk-D54t4e3..." }
        }"#;
        fs::write(&path, json).unwrap();
        let key = read_opencode_token(path.clone()).unwrap();
        assert_eq!(key, "sk-D54t4e3...");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn parse_opencode_auth_json_missing_or_empty_key() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("opencode_auth_empty.json");
        let json = r#"{
            "opencode-go": { "type": "api", "key": "" }
        }"#;
        fs::write(&path, json).unwrap();
        let err = read_opencode_token(path.clone()).unwrap_err();
        assert!(matches!(err, UsageError::NoCredential(_)));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_calculate_opencode_windows_impl() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY,
                time_created INTEGER NOT NULL,
                cost REAL DEFAULT 0 NOT NULL
            )",
            [],
        )
        .unwrap();

        let now_ms = chrono::Utc::now().timestamp_millis();

        // 5 hours limit = $12.00, we put $3.00 -> 25.0%
        // Weekly limit = $30.00, we put $3.00 + $6.00 = $9.00 -> 30.0%
        // Monthly limit = $60.00, we put $9.00 + $15.00 = $24.00 -> 40.0%
        conn.execute(
            "INSERT INTO session (id, time_created, cost) VALUES (?, ?, ?)",
            rusqlite::params!["ses1", now_ms - 2 * 60 * 60 * 1000, 3.0],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session (id, time_created, cost) VALUES (?, ?, ?)",
            rusqlite::params!["ses2", now_ms - 2 * 24 * 60 * 60 * 1000, 6.0],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session (id, time_created, cost) VALUES (?, ?, ?)",
            rusqlite::params!["ses3", now_ms - 15 * 24 * 60 * 60 * 1000, 15.0],
        )
        .unwrap();
        // Outside 30 days, should not be included
        conn.execute(
            "INSERT INTO session (id, time_created, cost) VALUES (?, ?, ?)",
            rusqlite::params!["ses4", now_ms - 40 * 24 * 60 * 60 * 1000, 100.0],
        )
        .unwrap();

        let windows = calculate_opencode_windows_impl(&conn).unwrap();
        assert_eq!(windows.len(), 3);

        assert_eq!(windows[0].label, "5-hour");
        assert_eq!(windows[0].used_percent, Some(25.0));

        assert_eq!(windows[1].label, "Weekly");
        assert_eq!(windows[1].used_percent, Some(30.0));

        assert_eq!(windows[2].label, "Monthly");
        assert_eq!(windows[2].used_percent, Some(40.0));
    }

    // ── OpenCode Go live `_server billing.get` probe (issue #957) ────────

    #[test]
    fn parse_opencode_billing_response_full() {
        // Pinned fixture: the documented `billing.get` reply shape (issue #957
        // sub-spec point 5). All three windows + their reset countdowns are
        // present and must round-trip through `UsageWindow` byte-for-byte.
        let json = r#"{
            "windows": [
                {"label": "5-hour",  "usedPercent": 25.0, "resetsAt": "2026-07-20T22:00:00Z"},
                {"label": "Weekly",  "usedPercent": 12.0, "resetsAt": "2026-07-22T00:00:00Z"},
                {"label": "Monthly", "usedPercent":  4.5, "resetsAt": "2026-08-01T00:00:00Z"}
            ]
        }"#;
        let (windows, detail) = parse_opencode_billing_response(json).unwrap();
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].label, "5-hour");
        assert_eq!(windows[0].used_percent, Some(25.0));
        assert_eq!(windows[0].resets_at.as_deref(), Some("2026-07-20T22:00:00Z"));
        assert_eq!(windows[1].label, "Weekly");
        assert_eq!(windows[1].used_percent, Some(12.0));
        assert_eq!(windows[2].label, "Monthly");
        assert_eq!(windows[2].used_percent, Some(4.5));
        assert_eq!(detail, None);
    }

    #[test]
    fn parse_opencode_billing_response_partial_5hour_only() {
        // Sub-spec point 5: a partial reply that carries only the 5-hour
        // window must parse cleanly (one row out, no error) rather than
        // failing closed. This matches how SolidStart server functions can
        // early-return the most-pressed window before the others.
        let json = r#"{
            "windows": [
                {"label": "5-hour", "usedPercent": 80.0, "resetsAt": "2026-07-20T22:00:00Z"}
            ]
        }"#;
        let (windows, detail) = parse_opencode_billing_response(json).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "5-hour");
        assert_eq!(windows[0].used_percent, Some(80.0));
        assert_eq!(windows[0].resets_at.as_deref(), Some("2026-07-20T22:00:00Z"));
        assert_eq!(detail, None);
    }

    #[test]
    fn parse_opencode_billing_response_missing_windows_array_is_shape_error() {
        // Required field — a body without `windows` is malformed, not "all
        // quotas are 0". This is the silent-zero-windowing trap the parser
        // MUST fail loudly against so the live fetch returns unavailable
        // and the degradation chain falls through to SQLite.
        let json = r#"{"foo": "bar"}"#;
        let err = parse_opencode_billing_response(json).unwrap_err();
        assert!(matches!(err, UsageError::Shape(_)), "expected Shape error, got {err:?}");
    }

    #[test]
    fn parse_opencode_billing_response_empty_windows_reports_detail() {
        // A well-formed reply with an empty `windows` array surfaces the
        // user-facing "no active quotas" detail so the UI doesn't render a
        // mysteriously empty meter. Mirrors the `parse_agy_models_empty_reports_detail`
        // contract so the Usage tab copy stays consistent across providers.
        let json = r#"{"windows": []}"#;
        let (windows, detail) = parse_opencode_billing_response(json).unwrap();
        assert!(windows.is_empty());
        assert_eq!(detail.as_deref(), Some("No active OpenCode Go quotas found"));
    }

    #[test]
    fn parse_opencode_billing_response_filters_malformed_windows() {
        // Per the stricter shape contract (issue #957 sub-spec point 4), a
        // window missing `usedPercent` is a shape failure — it MUST NOT
        // render as a "5-hour: (no data)" row, which is the silent-blank
        // gauge the spec sought to prevent. Such windows are filtered out
        // and any surviving valid windows still parse.
        let json = r#"{
            "windows": [
                {"label": "5-hour", "usedPercent": 25.0, "resetsAt": "2026-07-20T22:00:00Z"},
                {"label": "no-data-window"},
                {"usedPercent": 50.0, "resetsAt": "2026-07-22T00:00:00Z"},
                {}
            ]
        }"#;
        let (windows, detail) = parse_opencode_billing_response(json).unwrap();
        assert_eq!(windows.len(), 1, "only the fully-formed window survives");
        assert_eq!(windows[0].label, "5-hour");
        assert_eq!(windows[0].used_percent, Some(25.0));
        assert_eq!(detail, None);
    }

    #[test]
    fn parse_opencode_billing_response_all_windows_malformed_reports_detail() {
        // Edge case: every window is malformed. The parser succeeds (the
        // shape is well-formed), the empty-filtered result surfaces the
        // "no active quotas" detail. The SQLite fallback is NOT triggered
        // here because the shape contract is satisfied — empty + detail is
        // a valid "no quotas configured" reply, distinct from "shape
        // failure" (which would be `{"foo":"bar"}`).
        let json = r#"{"windows": [{"label": "junk"}, {"usedPercent": null}]}"#;
        let (windows, detail) = parse_opencode_billing_response(json).unwrap();
        assert!(windows.is_empty());
        assert_eq!(detail.as_deref(), Some("No active OpenCode Go quotas found"));
    }

    // The OpenCode Console credential parser tests (formerly
    // `parse_opencode_console_credential_*`) were relocated to
    // `services::opencode_oauth` alongside the DTO + parser itself for #956.

    // ── choose_opencode_usage — the heart of the degradation chain ────────

    fn fake_reading(used_percent: f64) -> UsageOutcome {
        UsageOutcome::Reading {
            windows: vec![UsageWindow {
                label: "5-hour".to_string(),
                used_percent: Some(used_percent),
                resets_at: None,
            }],
            balance: None,
            meters: vec![],
            detail: None,
        }
    }

    fn fake_unavailable(msg: &str) -> UsageOutcome {
        UsageOutcome::Unavailable {
            reason: msg.to_string(),
        }
    }

    #[test]
    fn choose_opencode_usage_live_success_wins() {
        // Live returned real numbers → SQLite is ignored. The 75% figure is
        // the live value; the 50% figure is the SQLite value — neither
        // matches the other's source, so the assertion is unambiguous.
        let live = fake_reading(75.0);
        let sqlite = fake_reading(50.0);
        match choose_opencode_usage(Some(live), sqlite) {
            UsageOutcome::Reading { windows, .. } => {
                assert_eq!(windows[0].used_percent, Some(75.0));
            }
            other => panic!("expected live Reading outcome, got: {other:?}"),
        }
    }

    #[test]
    fn choose_opencode_usage_live_error_falls_back_to_sqlite() {
        // THE pin: live attempted AND failed (HTTP 401 / 5xx / shape) →
        // SQLite is returned. A future refactor that drops the
        // `Reading`-only guard would surface the failure in the Probe UI
        // instead of the SQLite windows; this test catches it.
        let live = fake_unavailable("API error 401: Unauthorized");
        let sqlite = fake_reading(50.0);
        match choose_opencode_usage(Some(live), sqlite) {
            UsageOutcome::Reading { windows, .. } => {
                assert_eq!(windows[0].used_percent, Some(50.0));
            }
            other => panic!("expected sqlite Reading outcome, got: {other:?}"),
        }
    }

    #[test]
    fn choose_opencode_usage_no_credential_falls_back_to_sqlite() {
        // No `opencode:console` credential at all (read returned
        // NoCredential, collapsed to None) → SQLite is returned. The user
        // who has run `opencode auth login` but not yet finished #956's
        // device flow lands here.
        let sqlite = fake_reading(50.0);
        match choose_opencode_usage(None, sqlite) {
            UsageOutcome::Reading { windows, .. } => {
                assert_eq!(windows[0].used_percent, Some(50.0));
            }
            other => panic!("expected sqlite Reading outcome, got: {other:?}"),
        }
    }

    #[test]
    fn needs_retry_on_401_matches_rejected_outcome_not_error_text() {
        // Issue #1758: the reactive retry gate matches on the `Rejected`
        // outcome. A `Rejected` 403 (revocation by another name) retries;
        // an `Unavailable` whose body merely mentions "401" does not —
        // the old substring match fired a spurious refresh there.
        assert!(needs_retry_on_401(Some(
            &UsageOutcome::Rejected {
                hint: "API error 401: Unauthorized".to_string(),
            }
        )));
        assert!(needs_retry_on_401(Some(
            &UsageOutcome::Rejected {
                hint: "API error 403: Forbidden".to_string(),
            }
        )));
        assert!(!needs_retry_on_401(Some(&fake_unavailable(
            "API error 500: error 401 in body"
        ))));
        assert!(!needs_retry_on_401(Some(&fake_reading(25.0))));
        assert!(!needs_retry_on_401(None));
    }

    // ── opencode_usage_impl — end-to-end fallback integration ──────────────

    // ── resolve_opencode_server_id — issue #972 ─────────────────────────────
    //
    // The live `_server billing.get` probe must read its `X-Server-Id`
    // header from the persisted `OpenCodeConsoleCred.server_id`, falling
    // back to the legacy `OPENCODE_SERVER_ID` constant for credentials
    // written before #956 added the field. These pure tests pin the
    // resolver contract; the empty-string case is hand-edit safety.

    fn cred_with_server_id(server_id: Option<&str>) -> OpenCodeConsoleCred {
        OpenCodeConsoleCred {
            access_token: Some("tok".to_string()),
            workspace_id: Some("wrk".to_string()),
            refresh_token: None,
            expires_at: None,
            server_id: server_id.map(str::to_owned),
        }
    }

    #[test]
    fn resolve_opencode_server_id_prefers_persisted_value() {
        // Issue #972 acceptance #1: when `server_id` is present and
        // non-empty, the resolver returns it verbatim — the live probe
        // sends that value in the `X-Server-Id` header.
        let cred = cred_with_server_id(Some("custom-deployment-id-abc123"));
        assert_eq!(
            resolve_opencode_server_id(&cred),
            "custom-deployment-id-abc123"
        );
    }

    #[test]
    fn resolve_opencode_server_id_falls_back_to_constant_when_missing() {
        // Issue #972 acceptance #2: a blob without `server_id` (the
        // pre-#956 shape) keeps emitting the legacy constant so existing
        // users don't lose their live probe.
        let cred = cred_with_server_id(None);
        assert_eq!(resolve_opencode_server_id(&cred), OPENCODE_SERVER_ID);
    }

    #[test]
    fn resolve_opencode_server_id_falls_back_when_empty_string() {
        // A hand-edited blob with `"server_id": ""` is treated as missing
        // so the resolver never returns a useless empty `X-Server-Id`
        // header. The fallback branch's warn-once still fires.
        let cred = cred_with_server_id(Some(""));
        assert_eq!(resolve_opencode_server_id(&cred), OPENCODE_SERVER_ID);
    }

    #[test]
    fn opencode_live_request_parts_returns_persisted_server_id_for_header() {
        // Issue #972 acceptance #5: a credential with a non-default
        // `server_id` causes the live probe to send THAT value (not the
        // legacy constant) in the `X-Server-Id` header. The token /
        // workspace round-trip is pinned in the same assertion so a
        // future refactor that drops the header binding still fails.
        let cred = cred_with_server_id(Some("custom-deployment-id-xyz"));
        let (token, workspace_id, server_id) =
            opencode_live_request_parts(&cred).expect("credential is complete");
        assert_eq!(token, "tok");
        assert_eq!(workspace_id, "wrk");
        assert_eq!(
            server_id, "custom-deployment-id-xyz",
            "header must read from cred.server_id, not OPENCODE_SERVER_ID"
        );
        assert_ne!(
            server_id, OPENCODE_SERVER_ID,
            "must not silently fall back to the constant"
        );
    }

    #[test]
    fn opencode_live_request_parts_uses_legacy_constant_when_persisted_missing() {
        // The matching legacy-default branch: when the credential has no
        // `server_id`, the header value IS the constant — that's how
        // pre-#956 blobs continue to probe SolidStart without an
        // immediate re-auth.
        let cred = cred_with_server_id(None);
        let (_token, _workspace_id, server_id) =
            opencode_live_request_parts(&cred).expect("credential is complete");
        assert_eq!(server_id, OPENCODE_SERVER_ID);
    }

    #[test]
    fn opencode_live_request_parts_returns_none_when_token_missing() {
        // A blob missing `access_token` (e.g. mid-flow) must collapse to
        // `None` so the SQLite fallback runs. Mirrors how the live path
        // originally responded to a `NoCredential` read.
        let mut cred = cred_with_server_id(Some("custom"));
        cred.access_token = None;
        assert!(opencode_live_request_parts(&cred).is_none());
    }

    #[test]
    fn opencode_live_request_parts_returns_none_when_workspace_missing() {
        // Same invariant for `workspace_id` — the body needs a value
        // even when the token is present.
        let mut cred = cred_with_server_id(Some("custom"));
        cred.workspace_id = None;
        assert!(opencode_live_request_parts(&cred).is_none());
    }

    #[test]
    fn opencode_usage_impl_with_sqlite_db_only_returns_sqlite_windows() {
        // Pin the degradation chain (issue #957 sub-spec point 4): with a
        // fake home containing a valid auth.json + opencode.db but NO
        // `opencode:console` credential in the OS store, the fetcher MUST
        // run the SQLite path and return its windows. We exercise this via
        // the testable `opencode_usage_impl(home)` seam (the public
        // `opencode_usage()` reads `home_dir()` — we can't override that
        // without mutating USERPROFILE globally across the test suite).
        let unique = format!(
            "opencode_test_fallback_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let temp = std::env::temp_dir().join(unique);
        let _ = fs::remove_dir_all(&temp);
        let opencode_dir = temp.join(".local").join("share").join("opencode");
        fs::create_dir_all(&opencode_dir).unwrap();

        // Auth.json present (SQLite path's logged-in gate) with a non-empty
        // `opencode-go` key. This is the same shape `opencode auth login`
        // produces on a real workstation.
        fs::write(
            opencode_dir.join("auth.json"),
            r#"{"opencode-go": {"type": "api", "key": "sk-test-abc"}}"#,
        )
        .unwrap();

        // Seed an opencode.db with one recent session whose cost puts us at
        // 50% of the 5-hour $12 limit. The weekly/monthly rows are computed
        // from the same session so they'll report small percentages too.
        let db_path = opencode_dir.join("opencode.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY,
                time_created INTEGER NOT NULL,
                cost REAL DEFAULT 0 NOT NULL
            )",
            [],
        )
        .unwrap();
        let now_ms = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "INSERT INTO session (id, time_created, cost) VALUES (?, ?, ?)",
            rusqlite::params!["ses_fallback", now_ms - 30 * 60 * 1000, 6.0],
        )
        .unwrap();
        drop(conn);

        // Live path will fail with NoCredential (no Windows Credential
        // Manager entry exists in this test) — but the SQLite fallback
        // MUST still produce real windows. Pin that.
        match opencode_usage_impl(&temp) {
            UsageOutcome::Reading { windows, .. } => {
                assert_eq!(windows.len(), 3);
                assert_eq!(windows[0].label, "5-hour");
                assert_eq!(
                    windows[0].used_percent,
                    Some(50.0),
                    "$6 of $12 5-hour limit should yield 50%"
                );
            }
            other => panic!("expected sqlite Reading outcome, got: {other:?}"),
        }

        let _ = fs::remove_dir_all(&temp);
    }

    // ── Refresh seam gate (issue #970) ──────────────────────────────────
    //
    // The seam in `opencode_usage_impl` runs `try_refresh()` before the live
    // `_server billing.get` HTTP call when EITHER:
    //   - the cached credential's `expires_at` is in the past, OR
    //   - the cached live-fetch result is older than REFRESH_TTL (the credential
    //     was fresh at fetch time but is plausibly near expiry by now).
    //
    // These tests pin both halves of the gate without needing to mock
    // Windows Credential Manager — `opencode_needs_refresh` is a pure
    // function over the inputs we already have.

    #[test]
    fn opencode_needs_refresh_when_credential_is_expired() {
        // The primary trigger: `expires_at` is in the past → MUST refresh
        // regardless of cache age. A credential that's already past expiry
        // is going to 401 the next live fetch, so we mint a new bearer
        // proactively.
        let cred = OpenCodeConsoleCred {
            expires_at: Some("2020-01-01T00:00:00Z".to_string()),
            ..Default::default()
        };
        assert!(opencode_needs_refresh(&cred, None, 1_700_000_000));
        assert!(opencode_needs_refresh(&cred, Some(Duration::from_secs(0)), 1_700_000_000));
    }

    #[test]
    fn opencode_needs_refresh_when_cache_is_stale_but_credential_claims_fresh() {
        // The belt-and-braces trigger: `expires_at` claims the token is
        // still valid, but the cached live fetch is older than REFRESH_TL.
        // Token might have been near expiry at fetch time and now IS
        // expired; refreshing proactively avoids the 401 round-trip.
        let cred = OpenCodeConsoleCred {
            expires_at: Some("2099-01-01T00:00:00Z".to_string()),
            ..Default::default()
        };
        let now = 1_700_000_000;
        assert!(
            opencode_needs_refresh(&cred, Some(Duration::from_secs(301)), now),
            "cache older than REFRESH_TL must trigger refresh"
        );
        assert!(
            !opencode_needs_refresh(&cred, Some(Duration::from_secs(299)), now),
            "cache within REFRESH_TL must NOT trigger refresh"
        );
    }

    #[test]
    fn opencode_needs_refresh_no_op_when_fresh_and_no_cache() {
        // Two ways to skip the refresh: a fresh credential with no cached
        // fetch yet (first call to opencode_usage_impl this process), AND
        // a fresh credential with a recent cached fetch. Both must NOT
        // trigger refresh so we don't burn a /auth/device/token round-trip
        // on every usage panel poll.
        let cred = OpenCodeConsoleCred {
            expires_at: Some("2099-01-01T00:00:00Z".to_string()),
            ..Default::default()
        };
        assert!(!opencode_needs_refresh(&cred, None, 1_700_000_000));
        assert!(!opencode_needs_refresh(&cred, Some(Duration::from_secs(0)), 1_700_000_000));
        assert!(!opencode_needs_refresh(&cred, Some(Duration::from_secs(300)), 1_700_000_000));
    }

    #[test]
    fn opencode_needs_refresh_handles_missing_or_malformed_expires_at() {
        // A credential without `expires_at` (legacy or pre-#956 blob) is
        // treated as "unknown" by `cred_is_expired` (returns false) so the
        // cache-age half of the gate is the only refresh trigger. Same for
        // a malformed timestamp — we don't want a parsing error to fire a
        // refresh that we then can't use.
        let missing = OpenCodeConsoleCred::default();
        let malformed = OpenCodeConsoleCred {
            expires_at: Some("not a date".to_string()),
            ..Default::default()
        };
        let now = 1_700_000_000;
        // Missing expires_at + no cache → no refresh (let the live fetch try).
        assert!(!opencode_needs_refresh(&missing, None, now));
        // Missing expires_at + stale cache → refresh (cache age wins).
        assert!(opencode_needs_refresh(&missing, Some(Duration::from_secs(600)), now));
        // Malformed expires_at + no cache → no refresh.
        assert!(!opencode_needs_refresh(&malformed, None, now));
    }

    // ── Refresh-on-401 — mocked HTTP integration (issue #971) ─────────
    //
    // The four headline scenarios from #971's Verification section:
    //   1. Credential expired → refresh succeeds → live probe succeeds.
    //   2. Credential expired → refresh fails → SQLite fallback.
    //   3. Credential fresh → no refresh → live probe succeeds.
    //   4. Credential fresh → no refresh → live 401 → refresh-on-the-spot
    //      → live probe succeeds.
    //
    // `spawn_loopback` stands up a `tiny_http` server on `127.0.0.1:0`
    // that dispatches on `req.url()`. The two URLs mirror the production
    // paths: `/_server` for the live probe, `/auth/device/token` for the
    // refresh. Each test seeds a credential with the shape produced by
    // `persist_token_response` (issue #956) and counts calls per path so
    // we can assert the orchestration, not just the final envelope.
    //
    // Pattern lifted from `services::opencode_oauth::tests::spawn_loopback`
    // (issue #967) — the `tiny_http` crate is already a regular
    // `[dependencies]` entry, so this adds no new crate.


    /// Builds a temp home with a `auth.json` + `opencode.db` so the
    /// SQLite fallback has something to render. The session row seeds
    /// the 5-hour window at 50% — the SQLite fallback tests assert this
    /// number verbatim so a regression in the roll-up math surfaces
    /// here rather than in the live probe path.
    fn make_opencode_home(label: &str) -> std::path::PathBuf {
        let unique = format!(
            "opencode_test_refresh_{}_{}_{:?}",
            label,
            std::process::id(),
            std::thread::current().id()
        );
        let temp = std::env::temp_dir().join(unique);
        let _ = fs::remove_dir_all(&temp);
        let opencode_dir = temp.join(".local").join("share").join("opencode");
        fs::create_dir_all(&opencode_dir).unwrap();
        // auth.json gates the SQLite fallback's `logged_in` branch.
        fs::write(
            opencode_dir.join("auth.json"),
            r#"{"opencode-go": {"type": "api", "key": "sk-test-abc"}}"#,
        )
        .unwrap();
        // One session row, $6 spent → 50% of the $12 5-hour limit.
        let db_path = opencode_dir.join("opencode.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY,
                time_created INTEGER NOT NULL,
                cost REAL DEFAULT 0 NOT NULL
            )",
            [],
        )
        .unwrap();
        let now_ms = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "INSERT INTO session (id, time_created, cost) VALUES (?, ?, ?)",
            rusqlite::params!["ses_refresh", now_ms - 30 * 60 * 1000, 6.0],
        )
        .unwrap();
        drop(conn);
        temp
    }

    /// Wipes the process-wide `USAGE_CACHE` so a previous test's cached
    /// envelope can't short-circuit the live path. Issue #970's cache-age
    /// gate (cached_age > REFRESH_TTL) would otherwise leak state across
    /// tests in the same process.
    fn clear_usage_cache() {
        invalidate_cache();
    }

    /// Constructs a credential with the shape produced by
    /// `services::opencode_oauth::persist_token_response`.
    fn build_cred(
        access_token: &str,
        refresh_token: &str,
        expires_at: &str,
        workspace_id: &str,
        server_id: &str,
    ) -> OpenCodeConsoleCred {
        OpenCodeConsoleCred {
            access_token: Some(access_token.to_string()),
            workspace_id: Some(workspace_id.to_string()),
            refresh_token: Some(refresh_token.to_string()),
            expires_at: Some(expires_at.to_string()),
            server_id: Some(server_id.to_string()),
        }
    }

    /// The fixed `_server billing.get` success body used by scenarios 1,
    /// 3, and 4. Pinned to the documented fixture shape from issue #957
    /// so a wire-shape drift fails here, not just in the `wiremock`
    /// server assertion.
    const LIVE_BODY_OK: &str = r#"{
        "windows": [
            {"label": "5-hour",  "usedPercent": 25.0, "resetsAt": "2026-07-20T22:00:00Z"},
            {"label": "Weekly",  "usedPercent": 12.0, "resetsAt": "2026-07-22T00:00:00Z"},
            {"label": "Monthly", "usedPercent":  4.5, "resetsAt": "2026-08-01T00:00:00Z"}
        ]
    }"#;

    /// The fixed refresh success body — every TokenResponse field is
    /// required by `parse_token_response` (issue #956).
    const REFRESH_BODY_OK: &str = r#"{
        "access_token": "new_tok",
        "refresh_token": "new_rt",
        "token_type": "Bearer",
        "expires_in": 3600
    }"#;

    // ------- Scenario 1: expired → refresh succeeds → live probe succeeds

    #[test]
    fn opencode_usage_impl_expired_credential_refresh_succeeds_live_probe_succeeds() {
        // Headline scenario 1: the cache is empty, the credential's
        // expires_at is in the past, so the pre-emptive refresh gate
        // fires. Refresh returns a fresh bundle; the live probe runs
        // ONCE with the new bearer and returns the real windows.
        clear_usage_cache();
        let temp = make_opencode_home("s1");

        let refresh_count = Arc::new(AtomicUsize::new(0));
        let live_count = Arc::new(AtomicUsize::new(0));
        let refresh_count_t = refresh_count.clone();
        let live_count_t = live_count.clone();

        let port = spawn_loopback(2, move |req| match req.url() {
            "/auth/device/token" => {
                refresh_count_t.fetch_add(1, Ordering::SeqCst);
                let _ = req.respond(tiny_http::Response::from_string(REFRESH_BODY_OK));
            }
            "/_server" => {
                live_count_t.fetch_add(1, Ordering::SeqCst);
                let _ = req.respond(tiny_http::Response::from_string(LIVE_BODY_OK));
            }
            _ => {
                let _ = req.respond(
                    tiny_http::Response::from_string("not found").with_status_code(404),
                );
            }
        });
        let live_url = format!("http://127.0.0.1:{port}/_server");
        let refresh_url = format!("http://127.0.0.1:{port}/auth/device/token");

        let cred = build_cred(
            "old_tok",
            "rt_old",
            "2020-01-01T00:00:00Z",
            "wrk_q",
            "srv_v1",
        );
        let outcome = opencode_usage_impl_with_hosts(
            &temp,
            &live_url,
            &refresh_url,
            Some(&cred),
        );

        assert_eq!(
            refresh_count.load(Ordering::SeqCst),
            1,
            "pre-emptive refresh fires exactly once for an expired credential"
        );
        assert_eq!(
            live_count.load(Ordering::SeqCst),
            1,
            "live probe is called once with the refreshed bearer"
        );
        match outcome {
            UsageOutcome::Reading { windows, .. } => {
                assert_eq!(windows.len(), 3);
                assert_eq!(windows[0].label, "5-hour");
                assert_eq!(windows[0].used_percent, Some(25.0));
            }
            other => panic!("expected live Reading outcome, got: {other:?}"),
        }

        let _ = fs::remove_dir_all(&temp);
    }

    // ------- Scenario 2: expired → refresh fails → SQLite fallback

    #[test]
    fn opencode_usage_impl_expired_credential_refresh_fails_falls_back_to_sqlite() {
        // Headline scenario 2: refresh returns 500. The seam logs the
        // failure and proceeds — the live probe is called with the
        // OLD (still expired) bearer, returns 401, and the SQLite
        // fallback takes over. The pre-emptive refresh having fired
        // means the reactive retry is suppressed (see
        // `opencode_usage_impl_with_hosts`), so the SQLite fallback is
        // the user's answer even though the live probe had a 401 to
        // offer.
        clear_usage_cache();
        let temp = make_opencode_home("s2");

        let refresh_count = Arc::new(AtomicUsize::new(0));
        let live_count = Arc::new(AtomicUsize::new(0));
        let refresh_count_t = refresh_count.clone();
        let live_count_t = live_count.clone();

        let port = spawn_loopback(2, move |req| match req.url() {
            "/auth/device/token" => {
                refresh_count_t.fetch_add(1, Ordering::SeqCst);
                let _ = req.respond(
                    tiny_http::Response::from_string(r#"{"error":"server boom"}"#)
                        .with_status_code(500),
                );
            }
            "/_server" => {
                live_count_t.fetch_add(1, Ordering::SeqCst);
                let _ = req.respond(
                    tiny_http::Response::from_string(r#"{"error":"unauthorized"}"#)
                        .with_status_code(401),
                );
            }
            _ => {
                let _ = req.respond(
                    tiny_http::Response::from_string("not found").with_status_code(404),
                );
            }
        });
        let live_url = format!("http://127.0.0.1:{port}/_server");
        let refresh_url = format!("http://127.0.0.1:{port}/auth/device/token");

        let cred = build_cred(
            "old_tok",
            "rt_old",
            "2020-01-01T00:00:00Z",
            "wrk_q",
            "srv_v1",
        );
        let usage = opencode_usage_impl_with_hosts(
            &temp,
            &live_url,
            &refresh_url,
            Some(&cred),
        );

        assert_eq!(
            refresh_count.load(Ordering::SeqCst),
            1,
            "pre-emptive refresh fires once; reactive is suppressed because pre-emptive fired"
        );
        assert_eq!(
            live_count.load(Ordering::SeqCst),
            1,
            "live probe runs once with the old (expired) bearer when refresh fails"
        );
        // SQLite fallback wins — the live path's 401 is suppressed,
        // and the seeded `$6 of $12` 5-hour window shows through at 50%.
        match usage {
            UsageOutcome::Reading { windows, .. } => {
                assert_eq!(windows.len(), 3);
                assert_eq!(windows[0].label, "5-hour");
                assert_eq!(
                    windows[0].used_percent,
                    Some(50.0),
                    "sqlite fallback returns the seeded 50% 5-hour window"
                );
            }
            other => panic!("expected sqlite Reading outcome, got: {other:?}"),
        }

        let _ = fs::remove_dir_all(&temp);
    }

    // ------- Scenario 3: fresh → no refresh → live probe succeeds

    #[test]
    fn opencode_usage_impl_fresh_credential_no_refresh_live_probe_succeeds() {
        // Headline scenario 3: the credential is fresh (expires_at far
        // in the future), the cache is empty, so neither pre-emptive
        // gate fires. The live probe is called ONCE with the existing
        // bearer and returns the real windows. Refresh count is zero —
        // the test pins this so a future refactor that adds a stale
        // cache short-circuit still respects the "no refresh on fresh
        // cred" contract.
        clear_usage_cache();
        let temp = make_opencode_home("s3");

        let refresh_count = Arc::new(AtomicUsize::new(0));
        let live_count = Arc::new(AtomicUsize::new(0));
        let refresh_count_t = refresh_count.clone();
        let live_count_t = live_count.clone();

        let port = spawn_loopback(1, move |req| match req.url() {
            "/auth/device/token" => {
                refresh_count_t.fetch_add(1, Ordering::SeqCst);
                let _ = req.respond(tiny_http::Response::from_string(REFRESH_BODY_OK));
            }
            "/_server" => {
                live_count_t.fetch_add(1, Ordering::SeqCst);
                let _ = req.respond(tiny_http::Response::from_string(LIVE_BODY_OK));
            }
            _ => {
                let _ = req.respond(
                    tiny_http::Response::from_string("not found").with_status_code(404),
                );
            }
        });
        let live_url = format!("http://127.0.0.1:{port}/_server");
        let refresh_url = format!("http://127.0.0.1:{port}/auth/device/token");

        let cred = build_cred(
            "fresh_tok",
            "rt_fresh",
            "2099-01-01T00:00:00Z",
            "wrk_q",
            "srv_v1",
        );
        let usage = opencode_usage_impl_with_hosts(
            &temp,
            &live_url,
            &refresh_url,
            Some(&cred),
        );

        assert_eq!(
            refresh_count.load(Ordering::SeqCst),
            0,
            "no refresh on a fresh credential — neither gate fires"
        );
        assert_eq!(
            live_count.load(Ordering::SeqCst),
            1,
            "live probe runs once with the existing bearer"
        );
        match usage {
            UsageOutcome::Reading { windows, .. } => {
                assert_eq!(windows.len(), 3);
                assert_eq!(windows[0].used_percent, Some(25.0));
            }
            other => panic!("expected live Reading outcome, got: {other:?}"),
        }

        let _ = fs::remove_dir_all(&temp);
    }

    // ------- Scenario 4: fresh → no refresh → live 401 → refresh-on-spot → live succeeds

    #[test]
    fn opencode_usage_impl_fresh_credential_live_401_triggers_reactive_refresh_and_retry() {
        // Headline scenario 4: credential is fresh (no pre-emptive
        // refresh), but the live probe returns 401 — the server
        // revoked the token under us. The reactive refresh-on-401
        // branch fires, refreshes the bearer, and the live probe is
        // called AGAIN with the new bearer. The second call succeeds,
        // so the final envelope is the live windows.
        //
        // The test pins every leg of the orchestration so a future
        // refactor that drops the reactive retry — or that forgets to
        // suppress the retry when pre-emptive fires — fails loudly.
        clear_usage_cache();
        let temp = make_opencode_home("s4");

        let refresh_count = Arc::new(AtomicUsize::new(0));
        let live_count = Arc::new(AtomicUsize::new(0));
        let refresh_count_t = refresh_count.clone();
        let live_count_t = live_count.clone();

        // 3 requests total: 1 live (401) + 1 refresh + 1 live (200).
        let port = spawn_loopback(3, move |req| match req.url() {
            "/auth/device/token" => {
                refresh_count_t.fetch_add(1, Ordering::SeqCst);
                let _ = req.respond(tiny_http::Response::from_string(REFRESH_BODY_OK));
            }
            "/_server" => {
                // First call: 401. Second call: real windows.
                let prior = live_count_t.fetch_add(1, Ordering::SeqCst);
                if prior == 0 {
                    let _ = req.respond(
                        tiny_http::Response::from_string(r#"{"error":"unauthorized"}"#)
                            .with_status_code(401),
                    );
                } else {
                    let _ = req.respond(tiny_http::Response::from_string(LIVE_BODY_OK));
                }
            }
            _ => {
                let _ = req.respond(
                    tiny_http::Response::from_string("not found").with_status_code(404),
                );
            }
        });
        let live_url = format!("http://127.0.0.1:{port}/_server");
        let refresh_url = format!("http://127.0.0.1:{port}/auth/device/token");

        let cred = build_cred(
            "fresh_tok",
            "rt_fresh",
            "2099-01-01T00:00:00Z",
            "wrk_q",
            "srv_v1",
        );
        let usage = opencode_usage_impl_with_hosts(
            &temp,
            &live_url,
            &refresh_url,
            Some(&cred),
        );

        assert_eq!(
            refresh_count.load(Ordering::SeqCst),
            1,
            "reactive refresh fires ONCE after the live 401"
        );
        assert_eq!(
            live_count.load(Ordering::SeqCst),
            2,
            "live probe is called twice: first 401, then 200 after refresh"
        );
        // Three windows from the retry's success body — proves the
        // second live call's token was accepted (the seeded SQLite
        // fallback would have been 50%/...).
        match usage {
            UsageOutcome::Reading { windows, .. } => {
                assert_eq!(windows.len(), 3);
                assert_eq!(windows[0].label, "5-hour");
                assert_eq!(windows[0].used_percent, Some(25.0));
            }
            other => panic!("expected live Reading outcome, got: {other:?}"),
        }

        let _ = fs::remove_dir_all(&temp);
    }


    #[test]
    fn commandcode_usage_reads_auth_file_and_maps_credit_windows_and_balance() {
        let auth_dir = tempfile::tempdir().unwrap();
        let auth_path = auth_dir.path().join("auth.json");
        fs::write(
            &auth_path,
            r#"{"apiKey":"","access_token":"  cmd_live_test \n"}"#,
        )
        .unwrap();

        let observed = Arc::new(std::sync::Mutex::new(String::new()));
        let observed_t = observed.clone();
        let port = spawn_loopback(1, move |req| {
            let auth = req
                .headers()
                .iter()
                .find(|h| h.field.equiv("Authorization"))
                .map(|h| h.value.as_str().to_string())
                .unwrap_or_default();
            *observed_t.lock().unwrap() = auth;
            let _ = req.respond(tiny_http::Response::from_string(
                r#"{"windows":[{"label":"5-hour","used_percent":25,"resets_at":"2026-01-01T00:00:00+00:00"},{"label":"Weekly","used_percent":20,"resets_at":"2026-01-08T00:00:00+00:00"}],"monthly_credits":10,"extra_credits":5}"#,
            ));
        });

        let outcome = commandcode_usage_with_path(
            &auth_path,
            &format!("http://127.0.0.1:{port}/alpha/billing/credits"),
        );

        let (windows, balance) = match outcome {
            UsageOutcome::Reading {
                windows, balance, ..
            } => (windows, balance),
            other => panic!("expected Reading outcome, got: {other:?}"),
        };
        assert_eq!(*observed.lock().unwrap(), "Bearer cmd_live_test");
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].label, "5-hour");
        assert_eq!(windows[0].used_percent, Some(25.0));
        assert_eq!(
            windows[0].resets_at.as_deref(),
            Some("2026-01-01T00:00:00+00:00")
        );
        assert_eq!(windows[1].label, "Weekly");
        assert_eq!(windows[1].used_percent, Some(20.0));
        let balance = balance.expect("credits must surface as a balance");
        assert_eq!(balance.remaining, 15.0);
        assert_eq!(balance.monthly_spend, None);
        assert_eq!(balance.currency, "USD");
    }

    #[test]
    fn parse_commandcode_nested_response_maps_observed_api_shape() {
        let body = r#"{
            "windowLimits": {
                "fiveHour": {"cap": 20, "used": 5, "resetAt": 1767225600000},
                "weekly": {"cap": 100, "used": 20, "resetAt": 1767830400000}
            },
            "credits": {"monthlyCredits": 10, "purchasedCredits": 3, "freeCredits": 2}
        }"#;

        let (windows, credits) = parse_commandcode_credits_response(body).unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].label, "5-hour");
        assert_eq!(windows[0].used_percent, Some(25.0));
        assert_eq!(windows[0].resets_at.as_deref(), Some("2026-01-01T00:00:00+00:00"));
        assert_eq!(windows[1].label, "Weekly");
        assert_eq!(windows[1].used_percent, Some(20.0));
        assert_eq!(credits.balance().remaining, 15.0);
    }

    #[test]
    fn parse_commandcode_reported_response_accepts_camel_case_aliases() {
        let body = r#"{
            "windows": [
                {"label": "5-hour", "usedPercent": 12.5, "resetsAt": "2026-01-01T00:00:00Z"}
            ],
            "monthlyCredits": 8,
            "extraCredits": 2
        }"#;

        let (windows, credits) = parse_commandcode_credits_response(body).unwrap();
        assert_eq!(windows[0].used_percent, Some(12.5));
        assert_eq!(windows[0].resets_at.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(credits.balance().remaining, 10.0);
    }

    #[test]
    fn commandcode_monthly_matches_studio_tiers_grants_and_seats() {
        for (plan, allowance) in [
            ("individual-go", 10.0), ("individual-goat", 70.0),
            ("individual-pro", 30.0), ("individual-pro-v1", 80.0),
            ("individual-provider", 15.0), ("individual-max", 150.0),
            ("individual-ultra", 300.0), ("teams-pro", 40.0),
        ] {
            let credits: CommandCodeCredits = serde_json::from_value(serde_json::json!({
                "monthlyCredits": allowance / 2.0, "purchasedCredits": 100, "freeCredits": 50
            })).unwrap();
            let subscription = serde_json::from_value(serde_json::json!({
                "success": true, "data": {"planId": plan, "status": "active",
                "currentPeriodEnd": "2026-09-30T13:13:19.000Z"}
            })).unwrap();
            let monthly = commandcode_monthly_window(&credits, subscription).unwrap();
            assert_eq!(monthly.used_percent, Some(50.0), "{plan}");
            assert_eq!(monthly.resets_at.as_deref(), Some("2026-09-30T13:13:19+00:00"));
        }
        for (plan, grant, seats, remaining, expected) in [
            ("individual-goat", serde_json::json!(100), 1.0, 25.0, 75.0),
            ("individual-goat", serde_json::json!(20), 1.0, 35.0, 50.0),
            ("individual-go", serde_json::json!(10.25), 1.0, 5.125, 50.0),
            ("teams-pro", serde_json::Value::Null, 3.9, 60.0, 50.0),
            ("teams-pro", serde_json::json!(80), 3.0, 60.0, 25.0),
            ("teams-pro", serde_json::json!(0), 0.0, 20.0, 50.0),
            ("individual-goat", serde_json::json!("unknown"), 1.0, 35.0, 50.0),
            ("individual-goat", serde_json::json!(-10), 1.0, 70.0, 0.0),
            ("individual-goat", serde_json::Value::Null, 1.0, 80.0, 0.0),
            ("individual-goat", serde_json::Value::Null, 1.0, 0.0, 100.0),
            ("individual-goat", serde_json::Value::Null, 1.0, -5.0, 100.0),
        ] {
            let credits = serde_json::from_value(serde_json::json!({
                "monthlyCredits": remaining, "monthlyCreditsGranted": grant
            })).unwrap();
            let subscription = serde_json::from_value(serde_json::json!({
                "success": true, "data": {"planId": plan, "status": "active", "quantity": seats}
            })).unwrap();
            let monthly = commandcode_monthly_window(&credits, subscription).unwrap();
            assert_eq!(monthly.used_percent, Some(expected), "{plan}, {grant}, {seats}, {remaining}");
            assert!(monthly.resets_at.is_none());
        }
        assert_eq!(commandcode_positive_finite(Some(0.25)), Some(0.25));
        assert_eq!(commandcode_positive_finite(Some(0.0)), None);
        assert_eq!(commandcode_positive_finite(Some(-0.25)), None);
    }

    #[test]
    fn commandcode_monthly_rejects_unavailable_subscriptions_and_invalid_dates() {
        let credits = serde_json::from_str(r#"{"monthlyCredits":35}"#).unwrap();
        for body in [
            r#"{"success":true,"data":null}"#,
            r#"{"success":false,"data":{"planId":"individual-goat","status":"active"}}"#,
            r#"{"success":true,"data":{"planId":"new-plan","status":"active"}}"#,
            r#"{"success":true,"data":{"planId":"individual-goat","status":"past_due"}}"#,
        ] {
            assert!(commandcode_monthly_window(&credits, serde_json::from_str(body).unwrap()).is_none(), "{body}");
        }
        let subscription = serde_json::from_str(r#"{"success":true,"data":{
            "planId":"individual-goat","status":"active","currentPeriodEnd":"not-a-date"
        }}"#).unwrap();
        let monthly = commandcode_monthly_window(&credits, subscription).unwrap();
        assert_eq!(monthly.used_percent, Some(50.0));
        assert!(monthly.resets_at.is_none());
    }

    const COMMANDCODE_MONTHLY_CREDITS: &str = r#"{
        "windowLimits":{"fiveHour":{"cap":14,"used":0,"resetAt":0},
        "weekly":{"cap":35,"used":7,"resetAt":1789547227218}},
        "credits":{"monthlyCredits":35,"purchasedCredits":3,"freeCredits":2}
    }"#;

    #[test]
    fn commandcode_usage_enriches_monthly_and_separates_extra_credits() {
        let auth_dir = tempfile::tempdir().unwrap();
        let auth_path = auth_dir.path().join("auth.json");
        fs::write(&auth_path, r#"{"apiKey":"cmd_live_test"}"#).unwrap();
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed_t = observed.clone();
        let port = spawn_loopback(2, move |req| {
            let auth = req.headers().iter().find(|h| h.field.equiv("Authorization"))
                .map(|h| h.value.as_str().to_string()).unwrap_or_default();
            observed_t.lock().unwrap().push((req.url().to_string(), auth));
            let body = if req.url() == "/alpha/billing/credits" {
                COMMANDCODE_MONTHLY_CREDITS
            } else {
                r#"{"success":true,"data":{"planId":"individual-goat","status":"active",
                "currentPeriodEnd":"2026-09-30T13:13:19.000Z"}}"#
            };
            req.respond(tiny_http::Response::from_string(body)).unwrap();
        });
        let outcome = commandcode_usage_with_path(&auth_path, &format!("http://127.0.0.1:{port}/alpha/billing/credits"));
        assert_eq!(*observed.lock().unwrap(), vec![
            ("/alpha/billing/credits".to_string(), "Bearer cmd_live_test".to_string()),
            ("/alpha/billing/subscriptions?withPending=true".to_string(), "Bearer cmd_live_test".to_string()),
        ]);
        match outcome {
            UsageOutcome::Reading {
                windows, balance, detail, ..
            } => {
                assert_eq!(windows.iter().map(|w| w.label.as_str()).collect::<Vec<_>>(), vec!["5-hour", "Weekly", "Monthly"]);
                assert_eq!(windows[0].used_percent, Some(0.0));
                assert_eq!(windows[1].used_percent, Some(20.0));
                assert_eq!(windows[2].used_percent, Some(50.0));
                assert_eq!(windows[2].resets_at.as_deref(), Some("2026-09-30T13:13:19+00:00"));
                assert!(balance.is_none());
                assert_eq!(detail.as_deref(), Some("Additional credits: USD 5.00"));
            }
            other => panic!("expected Reading outcome, got: {other:?}"),
        }
    }

    #[test]
    fn commandcode_reported_monthly_window_replaces_combined_balance() {
        let auth_dir = tempfile::tempdir().unwrap();
        let auth_path = auth_dir.path().join("auth.json");
        fs::write(&auth_path, r#"{"apiKey":"cmd_live_test"}"#).unwrap();
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed_t = observed.clone();
        let port = spawn_loopback(1, move |req| {
            observed_t.lock().unwrap().push(req.url().to_string());
            req.respond(tiny_http::Response::from_string(r#"{
                "windows":[{"label":"Monthly Limit","used_percent":39,
                "resets_at":"2026-09-30T13:13:19+00:00"}],
                "monthly_credits":42.7,"extra_credits":5
            }"#)).unwrap();
        });

        let outcome = commandcode_usage_with_path(&auth_path, &format!("http://127.0.0.1:{port}/credits"));
        assert_eq!(*observed.lock().unwrap(), vec!["/credits"]);
        match outcome {
            UsageOutcome::Reading {
                windows, balance, detail, ..
            } => {
                assert_eq!(windows.len(), 1);
                assert_eq!(windows[0].label, "Monthly Limit");
                assert!(balance.is_none());
                assert_eq!(detail.as_deref(), Some("Additional credits: USD 5.00"));
            }
            other => panic!("expected Reading outcome, got: {other:?}"),
        }
    }

    #[test]
    fn commandcode_subscription_failures_preserve_live_windows_and_balance() {
        let auth_dir = tempfile::tempdir().unwrap();
        let auth_path = auth_dir.path().join("auth.json");
        fs::write(&auth_path, r#"{"apiKey":"cmd_live_test"}"#).unwrap();
        for (status, body) in [
            (401, ""), (403, ""), (429, ""), (500, ""), (200, "not-json"),
            (200, r#"{"success":true,"data":null}"#),
            (200, r#"{"success":true,"data":{"planId":"unknown","status":"active"}}"#),
            (200, r#"{"success":true,"data":{"planId":"individual-goat","status":"past_due"}}"#),
        ] {
            let port = spawn_loopback(2, move |req| {
                let response = if req.url() == "/credits" {
                    tiny_http::Response::from_string(COMMANDCODE_MONTHLY_CREDITS)
                } else {
                    tiny_http::Response::from_string(body).with_status_code(status)
                };
                req.respond(response).unwrap();
            });
            let outcome = commandcode_usage_with_path(&auth_path, &format!("http://127.0.0.1:{port}/credits"));
            match outcome {
                UsageOutcome::Reading {
                    windows, balance, detail, ..
                } => {
                    assert_eq!(windows.len(), 2, "{status}: {body}");
                    assert_eq!(windows[1].used_percent, Some(20.0), "{status}: {body}");
                    assert_eq!(balance.unwrap().remaining, 40.0, "{status}: {body}");
                    assert!(detail.is_none(), "{status}: {body}");
                }
                other => panic!("{status}: {body}: expected Reading outcome, got: {other:?}"),
            }
        }
    }

    #[test]
    fn commandcode_window_rejects_invalid_caps_and_bounds_usage() {
        assert!(commandcode_window(
            "invalid",
            CommandCodeQuotaWindow {
                cap: 0.0,
                used: 1.0,
                reset_at: None,
            },
        )
        .is_none());
        assert!(commandcode_window(
            "nan-cap",
            CommandCodeQuotaWindow {
                cap: f64::NAN,
                used: 1.0,
                reset_at: None,
            },
        )
        .is_none());
        assert!(commandcode_window(
            "nan-used",
            CommandCodeQuotaWindow {
                cap: 1.0,
                used: f64::NAN,
                reset_at: None,
            },
        )
        .is_none());

        let capped = commandcode_window(
            "capped",
            CommandCodeQuotaWindow {
                cap: 2.0,
                used: 3.0,
                reset_at: None,
            },
        )
        .unwrap();
        assert_eq!(capped.used_percent, Some(100.0));
        let floor = commandcode_window(
            "floor",
            CommandCodeQuotaWindow {
                cap: 2.0,
                used: -1.0,
                reset_at: None,
            },
        )
        .unwrap();
        assert_eq!(floor.used_percent, Some(0.0));
        assert!(floor.resets_at.is_none());
    }

    #[test]
    fn commandcode_auth_parser_reports_missing_invalid_and_blank_credentials() {
        let auth_dir = tempfile::tempdir().unwrap();
        let missing = auth_dir.path().join("missing.json");
        assert!(matches!(
            read_commandcode_token(&missing),
            Err(UsageError::NoCredential(_))
        ));

        let auth_path = auth_dir.path().join("auth.json");
        fs::write(&auth_path, "not-json").unwrap();
        assert!(matches!(
            read_commandcode_token(&auth_path),
            Err(UsageError::Shape(_))
        ));

        fs::write(
            &auth_path,
            r#"{"apiKey":"  ","access_token":"\t"}"#,
        )
        .unwrap();
        assert!(matches!(
            read_commandcode_token(&auth_path),
            Err(UsageError::NoCredential(_))
        ));
    }

    #[test]
    fn commandcode_usage_http_failures_preserve_auth_and_error_states() {
        let auth_dir = tempfile::tempdir().unwrap();
        let auth_path = auth_dir.path().join("auth.json");
        fs::write(&auth_path, r#"{"apiKey":"cmd_live_test"}"#).unwrap();

        let port_401 = spawn_loopback(1, |req| {
            let _ = req.respond(tiny_http::Response::empty(401));
        });
        let outcome_401 = commandcode_usage_with_path(
            &auth_path,
            &format!("http://127.0.0.1:{port_401}/credits"),
        );
        let hint_401 = match outcome_401 {
            UsageOutcome::Rejected { hint } => {
                assert!(
                    hint.contains("session expired") && hint.contains("—"),
                    "session-expired remediation must be preserved, got: {hint:?}"
                );
                hint
            }
            other => panic!("expected Rejected outcome, got: {other:?}"),
        };

        let port_403 = spawn_loopback(1, |req| {
            let _ = req.respond(tiny_http::Response::empty(403));
        });
        let outcome_403 = commandcode_usage_with_path(
            &auth_path,
            &format!("http://127.0.0.1:{port_403}/credits"),
        );
        match outcome_403 {
            UsageOutcome::Rejected { hint } => assert_eq!(hint, hint_401),
            other => panic!("expected Rejected outcome, got: {other:?}"),
        }

        let port_429 = spawn_loopback(1, |req| {
            let _ = req.respond(tiny_http::Response::empty(429));
        });
        let outcome_429 = commandcode_usage_with_path(
            &auth_path,
            &format!("http://127.0.0.1:{port_429}/credits"),
        );
        match outcome_429 {
            UsageOutcome::RateLimited { reason } => assert_eq!(
                reason,
                "Rate limited — usage data temporarily unavailable"
            ),
            other => panic!("expected RateLimited outcome, got: {other:?}"),
        }

        let port_500 = spawn_loopback(1, |req| {
            let _ = req.respond(
                tiny_http::Response::from_string("backend unavailable").with_status_code(500),
            );
        });
        let outcome_500 = commandcode_usage_with_path(
            &auth_path,
            &format!("http://127.0.0.1:{port_500}/credits"),
        );
        match outcome_500 {
            UsageOutcome::Unavailable { reason } => assert_eq!(
                reason,
                "API error 500: backend unavailable"
            ),
            other => panic!("expected Unavailable outcome, got: {other:?}"),
        }

        let port_malformed = spawn_loopback(1, |req| {
            let _ = req.respond(tiny_http::Response::from_string("not-json"));
        });
        let outcome_malformed = commandcode_usage_with_path(
            &auth_path,
            &format!("http://127.0.0.1:{port_malformed}/credits"),
        );
        match outcome_malformed {
            UsageOutcome::Unavailable { reason } => assert!(
                reason.contains("Failed to parse response"),
                "parse envelope must be preserved, got: {reason:?}"
            ),
            other => panic!("expected Unavailable outcome, got: {other:?}"),
        }
    }

    #[test]
    fn adapter_seam_contract_commandcode_no_credential_without_network() {
        // A missing CLI credential must report `NoCredential` without
        // touching the network — the unreachable loopback URL below would
        // fail loudly if hit. (`dispatch("commandcode").fetch` is not
        // hermetic here: it reads the real `~/.commandcode/auth.json`.)
        let auth_dir = tempfile::tempdir().unwrap();
        let missing = auth_dir.path().join("missing.json");
        match commandcode_usage_with_path(&missing, "http://127.0.0.1:1/credits") {
            UsageOutcome::NoCredential { .. } => {}
            other => panic!("expected NoCredential outcome, got: {other:?}"),
        }
    }
}
