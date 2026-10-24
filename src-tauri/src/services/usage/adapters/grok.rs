//! Grok (xAI) native adapter — self-authenticates via its CLI credential,
//! detection-gated on the `grok` harness.
//!
//! The fetch lives here rather than in the `usage.rs` facade (issue #2528):
//! grok owns its credential read, its request shape (the `x-grok-client-*`
//! headers the proxy requires) and its response parse. The status ladder
//! itself is the shared driver — grok supplies only the wording its
//! provider uses.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{fetch_usage, UsageAdapter};
use crate::services::usage::outcome::{ErrorBody, ResponsePolicy, UsageOutcome, UsageReading};
use crate::services::usage::types::{home_dir, BillingBalance, UsageError, UsagePage, UsageWindow};
use serde::Deserialize;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Drop-in [`UsageAdapter`] for `grok`.
pub(crate) struct GrokAdapter;

impl UsageAdapter for GrokAdapter {
    fn usage_page(&self) -> Option<UsagePage> {
        Some(UsagePage::new("https://grok.com/?_s=usage", "View usage"))
    }

    fn id(&self) -> &'static str {
        "grok"
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("grok")
    }

    // Issue #1745 phase 2 step 17: grok migrated to the outcome seam.
    // Missing credential → `NoCredential` (gate drops, as before).
    // 401/403 → `Rejected` ("Invalid API key" affordance). 429 →
    // `RateLimited`. Client-build / transport / non-2xx / parse →
    // `Unavailable`.
    //
    // Issue #2528 step 2: the ladder is the shared driver now. The
    // prepaid-balance reading rides the driver's `UsageReading` shape
    // instead of a hand-rolled HTTP path, and grok's own wording
    // ("Invalid API key") rides the driver's `ResponsePolicy`.
    fn fetch(&self, _accounts: &[ProviderAccount]) -> UsageOutcome {
        grok_usage()
    }
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
    let entries: HashMap<String, GrokAuthEntry> =
        serde_json::from_str(&content).map_err(|e| UsageError::Shape(e.to_string()))?;
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
struct GrokPeriod {
    #[serde(rename = "type")]
    period_type: Option<String>,
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
    let resp: GrokBillingResp =
        serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;
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
        let used_percent = config
            .credit_usage_percent
            .map(|percent| percent.clamp(0.0, 100.0))
            .or_else(|| {
                config.on_demand_cap.as_ref().map(|cap| {
                    if cap.val > 0.0 {
                        config
                            .on_demand_used
                            .as_ref()
                            .map(|used| ((used.val / cap.val) * 100.0).clamp(0.0, 100.0))
                            .unwrap_or(0.0)
                    } else {
                        0.0
                    }
                })
            });

        if let Some(used_percent) = used_percent {
            let label = config
                .current_period
                .as_ref()
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
/// transport / non-2xx / parse → `Unavailable`.
///
/// Issue #2528 step 2: the ladder is the shared driver now. The
/// prepaid-balance reading rides the driver's `UsageReading` shape — the
/// balance and the quota windows are not alternatives, a grok account can
/// report both — and grok's own wording rides the driver's
/// `ResponsePolicy`.
pub fn grok_usage() -> UsageOutcome {
    let base_url = env::var("GROK_CLI_CHAT_PROXY_BASE_URL")
        .unwrap_or_else(|_| "https://cli-chat-proxy.grok.com".to_string());
    grok_usage_with(&grok_auth_path(), &base_url)
}

/// Grok's failure wording, as the shared driver spells it.
///
/// The default driver wording is `"API error {status}: {body}"`, but grok's
/// credential is an OIDC session rather than a pasted API key, so the
/// remediation copy is "Invalid API key". The body-read failure copy is
/// grok's own wording too.
fn grok_response_policy() -> ResponsePolicy {
    ResponsePolicy {
        rejected_hint: Some("Invalid API key"),
        body_read_error: Some("Failed to read response body"),
        error_body: ErrorBody::Raw,
    }
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

    let url = format!("{base_url}/v1/billing?format=credits");

    fetch_usage(
        grok_response_policy(),
        |client| {
            client
                .get(&url)
                .header("Authorization", format!("Bearer {}", token))
                .header("X-XAI-Token-Auth", "xai-grok-cli")
                .header("x-userid", user_id)
                .header("x-grok-client-mode", "grok-build")
                .header("x-grok-client-version", "0.2.103")
                .header("x-grok-client-identifier", "grok-shell")
        },
        |body| {
            let (windows, balance) = parse_grok_response(body)?;
            Ok(UsageReading {
                windows,
                balance,
                meters: Vec::new(),
                detail: None,
            })
        },
    )
}

#[cfg(test)]
#[path = "grok_tests.rs"]
mod tests;
