//! OpenRouter keyed adapter -- cash-balance endpoint. Card always visible
//! so a revoked key keeps its row (the gate keeps `logged_out` envelopes
//! when a key is configured, so the UI can render "Invalid API key").
//!
//! Owns the full fetch (ARCH-1): credential gate, live request, and
//! response parsing. `usage.rs` keeps orchestration only.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{api_key_for, UsageAdapter};
use crate::services::usage::outcome::UsageOutcome;
use crate::services::usage::types::{BillingBalance, UsageError};
use reqwest::blocking::Client;
use serde::Deserialize;

/// Drop-in [`UsageAdapter`] for `openrouter`.
pub(crate) struct OpenrouterAdapter;

impl UsageAdapter for OpenrouterAdapter {
    fn id(&self) -> &'static str {
        "openrouter"
    }

    // Issue #1745 phase 2 step 9: openrouter migrated to the outcome
    // seam. Empty key → `NoCredential`. 401/403 → `Rejected`. 429 →
    // `RateLimited`. Other → `Unavailable`.
    fn fetch(&self, accounts: &[ProviderAccount]) -> UsageOutcome {
        openrouter_usage(api_key_for(accounts, "openrouter").unwrap_or(""))
    }
}

// ─── OpenRouter ──────────────────────────────────────────────────────────────
//
// OpenRouter exposes a simple "Anthropic Skin" for Claude Code (set
// `ANTHROPIC_BASE_URL=https://openrouter.ai/api` + `ANTHROPIC_AUTH_TOKEN=$key`)
// and a separate `GET /api/v1/credits` Bearer-authenticated endpoint that
// reports `total_credits` (all credits purchased) and `total_usage` (lifetime
// spend). Mirrors the `kimi_usage` shape — keyed fetcher, balance-style response,
// no Anthropic-side windows to harvest — so the `ProviderUsage.balance` field is
// the canonical surface.

/// Response envelope from `GET https://openrouter.ai/api/v1/credits`.
/// `total_usage` is required to derive the remaining wallet balance;
/// it is lifetime-cumulative, not current-month, spend.
/// `BalanceCard` would label any value there as "Spent this month", so we
/// leave `monthly_spend` unset.
#[derive(Deserialize, Debug)]
struct OpenRouterResp {
    data: OpenRouterData,
}

#[derive(Deserialize, Debug)]
struct OpenRouterData {
    total_credits: f64,
    total_usage: f64,
}

/// Parses the OpenRouter `/api/v1/credits` body into a `BillingBalance`. The
/// `data.total_credits` is total wallet funding in USD, not remaining balance;
/// `data.total_usage` is lifetime spend, so their difference is the remaining
/// balance.
/// Lifetime cumulative spend is not a current-month figure; labelling it as
/// "Spent this month" would be misleading.
/// We leave `monthly_spend = None` because OpenRouter provides lifetime usage,
/// not a billing-period figure. Missing required fields are hard parse errors
/// rather than silent zeros so response-shape changes are visible.
fn parse_openrouter_response(body: &str) -> Result<BillingBalance, UsageError> {
    let resp: OpenRouterResp =
        serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;
    Ok(BillingBalance {
        remaining: resp.data.total_credits - resp.data.total_usage,
        monthly_spend: None,
        // OpenRouter bills in USD per the platform's published pricing; the
        // endpoint does not return a currency field.
        currency: "USD".to_string(),
    })
}

pub(crate) fn openrouter_usage(api_key: &str) -> UsageOutcome {
    // Issue #1745 phase 2: migrated to the outcome seam. Empty key →
    // `NoCredential`. 401/403 → `Rejected`. 429 → `RateLimited`. Other →
    // `Unavailable`.
    if api_key.is_empty() {
        return UsageOutcome::NoCredential {
            hint: "No API key configured".to_string(),
        };
    }

    let client = match Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return UsageOutcome::Unavailable {
                reason: format!("Client error: {}", e),
            }
        }
    };

    let auth = format!("Bearer {}", api_key);
    let resp = match client
        .get("https://openrouter.ai/api/v1/credits")
        .header("Authorization", auth)
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
            let body = match r.text() {
                Ok(b) => b,
                Err(e) => {
                    return UsageOutcome::Unavailable {
                        reason: format!("API error {}: failed to read error body: {}", code, e),
                    }
                }
            };
            return UsageOutcome::Unavailable {
                reason: format!("API error {}: {}", code, body),
            };
        }
        Ok(r) => r,
        Err(e) => {
            return UsageOutcome::Unavailable {
                reason: format!("Request failed: {}", e),
            }
        }
    };

    let body = match resp.text() {
        Ok(b) => b,
        Err(e) => {
            return UsageOutcome::Unavailable {
                reason: format!("Failed to read response body: {}", e),
            }
        }
    };
    match parse_openrouter_response(&body) {
        Ok(balance) => UsageOutcome::Reading {
            windows: Vec::new(),
            balance: Some(balance),
            meters: Vec::new(),
            detail: None,
        },
        Err(e) => UsageOutcome::Unavailable {
            reason: format!("Failed to parse response: {}", e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    // ─── OpenRouter ───────────────────────────────────────────────────────
    //
    // Mirrors the Kimi test set: the success-path parser, two required-field
    // failure paths, and the empty-key "logged_out" defensive case. OpenRouter
    // has no vendor-envelope (unlike Kimi's `code` field) so the only failure
    // mode is missing required fields.

    #[test]
    fn parse_openrouter_response_calculates_remaining_balance_and_omits_monthly_spend() {
        // `total_credits` is funding, not remaining balance. Pin the
        // subtraction so a future contributor can't regress to displaying the
        // original deposit after it has been spent.
        let json = r#"{
            "data": {
                "total_credits": 10.0,
                "total_usage": 8.5
            }
        }"#;
        let b = parse_openrouter_response(json).unwrap();
        assert_eq!(b.remaining, 1.5);
        assert_eq!(b.monthly_spend, None);
        assert_eq!(b.currency, "USD");
    }

    #[test]
    fn parse_openrouter_response_rejects_missing_data() {
        // Required-field failure — a body without `data` is malformed.
        let json = r#"{}"#;
        let err = parse_openrouter_response(json).unwrap_err();
        assert!(matches!(err, UsageError::Shape(_)), "expected Shape error, got {err:?}");
    }

    #[test]
    fn parse_openrouter_response_rejects_missing_total_credits() {
        // Required-field failure — every successful response carries a balance.
        let json = r#"{"data": {"total_usage": 12.34}}"#;
        let err = parse_openrouter_response(json).unwrap_err();
        assert!(matches!(err, UsageError::Shape(_)));
    }

    #[test]
    fn parse_openrouter_response_rejects_missing_total_usage() {
        // Usage is required because it is needed to derive the balance.
        let json = r#"{"data": {"total_credits": 50.0}}"#;
        let err = parse_openrouter_response(json).unwrap_err();
        assert!(matches!(err, UsageError::Shape(_)));
    }

    #[test]
    fn openrouter_usage_with_empty_key_returns_logged_out() {
        // Mirrors `kimi_usage_with_empty_key_returns_logged_out` — the upstream
        // caller is expected to gate on key presence, but we still defend here
        // so a misconfigured fetch surfaces as "no API key" rather than a 401.
        let usage = openrouter_usage("").into_usage("openrouter");
        assert!(!usage.logged_in);
        assert_eq!(usage.provider, "openrouter");
        assert!(usage.error.is_some());
        assert!(usage.balance.is_none());
    }
}
