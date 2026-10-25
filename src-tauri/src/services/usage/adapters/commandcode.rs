//! CommandCode native adapter — self-authenticates via its CLI credential,
//! detection-gated on the `commandcode` harness.
//!
//! The fetch lives here rather than in the `usage.rs` facade (issue #2528):
//! commandcode owns its credential read, its two endpoints and its response
//! parses. The quota read and the subscription read are one shared-driver
//! flow: the subscription read is the *enrichment*, and the driver carries
//! it as a first-class, best-effort part of the outcome — a failed
//! subscription lookup leaves the quota reading standing.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{fetch_usage_with_enrichment, UsageAdapter};
use crate::services::usage::outcome::{
    ErrorBody, ResponsePolicy, UsageEnrichmentPatch, UsageOutcome, UsageReading,
};
use crate::services::usage::types::{BillingBalance, UsageError, UsagePage, UsageWindow};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Drop-in [`UsageAdapter`] for `commandcode`.
pub(crate) struct CommandcodeAdapter;

impl UsageAdapter for CommandcodeAdapter {
    fn usage_page(&self) -> Option<UsagePage> {
        Some(UsagePage::new("https://commandcode.ai/usage", "View usage"))
    }

    fn id(&self) -> &'static str {
        "commandcode"
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("commandcode")
    }

    // Issue #1745 phase 2 step 14: commandcode migrated to the outcome
    // seam. Missing credential → `NoCredential`. 401/403 → `Rejected` with
    // the session-expired remediation. 429 → `RateLimited`. Other →
    // `Unavailable`. Enrichment failures stay best-effort.
    //
    // Issue #2528 step 3a: the quota + subscription reads are the shared
    // driver's flow now, with the subscription read as the driver's
    // enrichment and commandcode's own wording as a `ResponsePolicy`.
    fn fetch(&self, _accounts: &[ProviderAccount]) -> UsageOutcome {
        commandcode_usage()
    }
}

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

#[derive(Deserialize, Debug, Clone)]
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
    #[serde(
        rename = "monthlyCreditsGranted",
        alias = "monthly_credits_granted",
        default
    )]
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
    let seats = if seats.is_finite() && seats >= 1.0 {
        seats
    } else {
        1.0
    };
    let total = match grant {
        Some(grant) => grant.max(base),
        None if subscription.plan_id == "teams-pro" => base * seats,
        None => base,
    };
    if !total.is_finite() || total <= 0.0 {
        return None;
    }
    let resets_at = subscription
        .current_period_end
        .and_then(|date| chrono::DateTime::parse_from_rfc3339(&date).ok())
        .map(|date| date.to_rfc3339());
    Some(UsageWindow {
        label: "Monthly".to_string(),
        used_percent: Some(
            ((total - credits.monthly_credits.max(0.0)) / total * 100.0).clamp(0.0, 100.0),
        ),
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
/// remediation. 429 → `RateLimited`. Other → `Unavailable`. Enrichment is
/// optional: a failed subscription request must not discard successfully
/// fetched windows or credits.
///
/// Issue #2528 step 3a: the ladder is the shared driver now, and the
/// subscription lookup is the driver's *enrichment* rather than an ad-hoc
/// second HTTP call. The enrichment is best-effort — a failed subscription
/// lookup leaves the quota reading as the credits endpoint produced it.
pub fn commandcode_usage() -> UsageOutcome {
    commandcode_usage_with_path(
        &commandcode_auth_path(),
        "https://api.commandcode.ai/alpha/billing/credits",
    )
}

/// Command Code's failure wording, as the shared driver spells it.
///
/// The session expired is Command Code's own remediation ("run `cmdc
/// login`"), the non-2xx body is trimmed with a fallback so an empty body
/// still says something useful, and the body-read failure copy is
/// Command Code's.
fn commandcode_response_policy() -> ResponsePolicy {
    ResponsePolicy {
        rejected_hint: Some("Command Code session expired — run 'cmdc login' to log in"),
        body_read_error: Some("Failed to read response"),
        error_body: ErrorBody::TrimmedOr("usage endpoint failed"),
    }
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

    // The quota parse produces the credits, and the subscription lookup needs
    // those credits to work out the monthly window. The driver runs the two
    // steps in order, so the quota travels between them through this cell.
    let quota: RefCell<Option<CommandCodeQuota>> = RefCell::new(None);

    fetch_usage_with_enrichment(
        commandcode_response_policy(),
        |client| {
            client
                .get(live_url)
                .header("Authorization", format!("Bearer {token}"))
        },
        |body| {
            let (windows, credits) = parse_commandcode_credits_response(body)?;
            let reported_monthly = windows.iter().any(|window| {
                window.used_percent.is_some()
                    && window
                        .label
                        .trim()
                        .to_ascii_lowercase()
                        .starts_with("monthly")
            });
            // A credits response that already reports a monthly meter shows
            // the meter, not the balance — same rule the subscription lookup
            // applies below.
            let (balance, detail) = if reported_monthly {
                (None, commandcode_extra_credits_detail(&credits))
            } else {
                (Some(credits.balance()), None)
            };
            *quota.borrow_mut() = Some(CommandCodeQuota {
                credits,
                reported_monthly,
            });
            Ok(UsageReading {
                windows,
                balance,
                meters: Vec::new(),
                detail,
            })
        },
        |client| {
            let quota = match quota.borrow().clone() {
                Some(quota) => quota,
                None => return Err(UsageError::Shape("missing credits".to_string())),
            };
            // The credits response already carried a monthly meter, so there
            // is no contribution to make — and no request to make.
            if quota.reported_monthly {
                return Ok(None);
            }
            let credits = &quota.credits;
            match commandcode_fetch_monthly_window(client, &token, live_url, credits) {
                Ok(Some(monthly)) => Ok(Some(UsageEnrichmentPatch {
                    windows: vec![monthly],
                    balance: None,
                    detail: commandcode_extra_credits_detail(credits),
                })),
                Ok(None) => {
                    tracing::warn!(
                        provider = "commandcode",
                        "Command Code subscription has no usable monthly meter"
                    );
                    Ok(None)
                }
                Err(error) => {
                    tracing::warn!(
                        provider = "commandcode",
                        error = %error,
                        "Command Code monthly usage enrichment failed"
                    );
                    Err(UsageError::Shape(error))
                }
            }
        },
    )
}

/// What the credits read produced, handed on to the subscription lookup.
#[derive(Clone)]
struct CommandCodeQuota {
    /// The credits the quota response reported.
    credits: CommandCodeCredits,
    /// The credits response already carried a monthly meter, so the
    /// subscription lookup has no contribution to make.
    reported_monthly: bool,
}

fn commandcode_extra_credits_detail(credits: &CommandCodeCredits) -> Option<String> {
    let extra = credits.purchased_credits + credits.free_credits;
    (extra.is_finite() && extra != 0.0).then(|| format!("Additional credits: USD {extra:.2}"))
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

#[cfg(test)]
#[path = "commandcode_tests.rs"]
mod tests;
