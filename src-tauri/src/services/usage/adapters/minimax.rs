//! MiniMax keyed adapter -- credential comes from the effective account
//! snapshot, never from disk. Card always visible so the key editor stays
//! reachable.
//!
//! Owns the full fetch (ARCH-1): credential gate, live request via the
//! shared [`fetch_usage`](crate::services::usage::adapter::fetch_usage)
//! driver, and response parsing. `usage.rs` keeps orchestration only.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{api_key_for, fetch_usage, UsageAdapter};
use crate::services::usage::outcome::{AuthPolicy, UsageOutcome};
use crate::services::usage::types::{BillingBalance, UsageError, UsageWindow};
use serde::Deserialize;

/// Drop-in [`UsageAdapter`] for `minimax`.
pub(crate) struct MinimaxAdapter;

impl UsageAdapter for MinimaxAdapter {
    fn id(&self) -> &'static str {
        "minimax"
    }

    // Issue #1745 phase 2 step 1: minimax is the first provider fully
    // migrated to the outcome seam. Empty key -> `NoCredential` (gate
    // drops). Non-empty key -> routes through the shared driver, which
    // classifies 401/403 as `Rejected` (the "Invalid API key"
    // affordance) -- matching MiniMax's keyed siblings
    // (Kimi/OpenRouter/OpenAI/DeepSeek).
    fn fetch(&self, accounts: &[ProviderAccount]) -> UsageOutcome {
        minimax_usage(api_key_for(accounts, "minimax").unwrap_or(""))
    }
}

#[derive(Deserialize, Debug)]
struct MinimaxModelRemain {
    #[serde(default)]
    model_name: String,
    #[serde(default)]
    end_time: i64,
    #[serde(default)]
    current_interval_remaining_percent: Option<f64>,
    #[serde(default)]
    weekly_end_time: i64,
    #[serde(default)]
    current_weekly_remaining_percent: Option<f64>,
}

#[derive(Deserialize, Debug)]
struct MinimaxResp {
    // Required (no #[serde(default)]) so the legacy `category_remains`-shaped
    // response — which the parser used pre-2026-06 — fails loudly instead of
    // silently zero-windowing. See test parse_minimax_response_rejects_legacy_category_remains.
    model_remains: Vec<MinimaxModelRemain>,
}

fn parse_minimax_response(body: &str) -> Result<(Vec<UsageWindow>, Option<String>), UsageError> {
    let resp: MinimaxResp =
        serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;

    // Only the text-generation plan ("general") is surfaced — other entries in
    // `model_remains` (e.g. "video") are intentionally hidden, matching the
    // PR #204 simplification that hid MCP Understand Image / Web Search.
    let mut windows = vec![];

    for model in resp.model_remains {
        if model.model_name != "general" {
            continue;
        }

        let mut push_window = |label: &str, remaining: Option<f64>, end_ms: i64| {
            if let Some(remaining) = remaining {
                let resets_at = if end_ms > 0 {
                    chrono::DateTime::from_timestamp_millis(end_ms).map(|dt| dt.to_rfc3339())
                } else {
                    None
                };
                windows.push(UsageWindow {
                    label: label.to_string(),
                    used_percent: Some(100.0 - remaining),
                    resets_at,
                });
            }
        };

        push_window("5-hour", model.current_interval_remaining_percent, model.end_time);
        push_window("Weekly", model.current_weekly_remaining_percent, model.weekly_end_time);
    }

    let detail = if windows.is_empty() {
        Some("No active token plan quotas found".to_string())
    } else {
        None
    };
    Ok((windows, detail))
}

/// Parses a pay-as-you-go cash-balance response into a [`BillingBalance`]
/// (issue #537). Mirrors the `base_resp`-wrapped envelope MiniMax uses for its
/// other endpoints; the inner field names below are the documented/assumed shape.
///
// TODO(#537 follow-up): the live cash-balance endpoint is unverified — MiniMax's
// known `token_plan/remains` endpoint returns plan percentages, not cash. This
// parser + its mock tests prove the BillingBalance pipeline; wiring an actual
// HTTP fetch is deferred per the agreed slice (real endpoint not yet confirmed).
// `allow(dead_code)`: exercised by the mock tests below, not yet by a live fetch.
#[allow(dead_code)]
fn parse_minimax_balance(body: &str) -> Result<BillingBalance, UsageError> {
    #[derive(Deserialize)]
    struct BalanceInfo {
        // Required so a percentages-only (`token_plan/remains`) body fails loudly
        // rather than silently reporting a zero balance.
        remaining: f64,
        #[serde(default)]
        month_spend: Option<f64>,
        #[serde(default)]
        currency: Option<String>,
    }
    #[derive(Deserialize)]
    struct Resp {
        balance: BalanceInfo,
    }
    let resp: Resp = serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;
    Ok(BillingBalance {
        remaining: resp.balance.remaining,
        monthly_spend: resp.balance.month_spend,
        // Default to USD when the provider omits a currency code.
        currency: resp.balance.currency.unwrap_or_else(|| "USD".to_string()),
    })
}

pub(crate) fn minimax_usage(api_key: &str) -> UsageOutcome {
    // Issue #1745 phase 2 step 1: minimax is the first provider migrated to
    // the outcome seam. Empty key → `NoCredential` (gate drops). Non-empty
    // key routes through the shared driver, which classifies 401/403 as
    // `Rejected` (the "Invalid API key" affordance) — matching MiniMax's
    // keyed siblings (Kimi/OpenRouter/OpenAI/DeepSeek).
    if api_key.is_empty() {
        return UsageOutcome::NoCredential {
            hint: "No API key configured".to_string(),
        };
    }
    let auth = format!("Bearer {}", api_key);
    fetch_usage(
        AuthPolicy::Rejected,
        |c| {
            c.get("https://api.minimax.io/v1/token_plan/remains")
                .header("Authorization", auth)
        },
        parse_minimax_response,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimax_response_valid() {
        // Live response shape as of 2026-06-01: wrapper is `model_remains` (not
        // `category_remains`), items carry `model_name` instead of
        // `category`/`display_name`, and the source of truth is the
        // `*_remaining_percent` fields (0-100) rather than the count pair.
        let json = r#"{
            "model_remains": [
                {
                    "start_time": 1780344000000,
                    "end_time": 1780358400000,
                    "remains_time": 5511238,
                    "current_interval_total_count": 0,
                    "current_interval_usage_count": 0,
                    "model_name": "general",
                    "current_weekly_total_count": 0,
                    "current_weekly_usage_count": 0,
                    "weekly_start_time": 1780272000000,
                    "weekly_end_time": 1780876800000,
                    "weekly_remains_time": 523911238,
                    "current_interval_status": 1,
                    "current_interval_remaining_percent": 89,
                    "current_weekly_status": 1,
                    "current_weekly_remaining_percent": 96
                },
                {
                    "start_time": 1780272000000,
                    "end_time": 1780358400000,
                    "remains_time": 5511238,
                    "current_interval_total_count": 3,
                    "current_interval_usage_count": 0,
                    "model_name": "video",
                    "current_weekly_total_count": 21,
                    "current_weekly_usage_count": 0,
                    "weekly_start_time": 1780272000000,
                    "weekly_end_time": 1780876800000,
                    "weekly_remains_time": 523911238,
                    "current_interval_status": 1,
                    "current_interval_remaining_percent": 100,
                    "current_weekly_status": 1,
                    "current_weekly_remaining_percent": 100
                }
            ],
            "base_resp": {
                "status_code": 0,
                "status_msg": "success"
            }
        }"#;
        let (windows, detail) = parse_minimax_response(json).unwrap();
        // Only the "general" (text-generation) model is surfaced; the "video"
        // entry must be filtered out, leaving 2 windows.
        assert_eq!(windows.len(), 2);
        // First: general 5-hour, 89% remaining → 11% used.
        assert_eq!(windows[0].label, "5-hour");
        assert_eq!(windows[0].used_percent, Some(11.0));
        assert!(windows[0].resets_at.is_some());
        // Second: general weekly, 96% remaining → 4% used.
        assert_eq!(windows[1].label, "Weekly");
        assert_eq!(windows[1].used_percent, Some(4.0));
        assert!(windows[1].resets_at.is_some());
        assert_eq!(detail, None);
    }

    #[test]
    fn parse_minimax_response_empty_model_remains() {
        // When the response shape is current but no models are listed, we still
        // surface the user-facing "no quotas" detail rather than failing.
        let json = r#"{
            "model_remains": [],
            "base_resp": { "status_code": 0, "status_msg": "success" }
        }"#;
        let (windows, detail) = parse_minimax_response(json).unwrap();
        assert!(windows.is_empty());
        assert_eq!(detail.as_deref(), Some("No active token plan quotas found"));
    }

    #[test]
    fn parse_minimax_response_model_without_percent_is_skipped() {
        // Forward-compat: a model entry with neither remaining field should
        // be skipped silently rather than producing a NaN window.
        let json = r#"{
            "model_remains": [
                {
                    "model_name": "unknown",
                    "end_time": 1780358400000,
                    "weekly_end_time": 1780876800000
                }
            ],
            "base_resp": { "status_code": 0, "status_msg": "success" }
        }"#;
        let (windows, detail) = parse_minimax_response(json).unwrap();
        assert!(windows.is_empty());
        assert_eq!(detail.as_deref(), Some("No active token plan quotas found"));
    }

    #[test]
    fn parse_minimax_balance_valid() {
        // Pay-as-you-go cash balance (issue #537): remaining + monthly spend +
        // currency, wrapped in the same base_resp envelope as the other endpoints.
        let json = r#"{
            "balance": { "remaining": 42.5, "month_spend": 7.25, "currency": "USD" },
            "base_resp": { "status_code": 0, "status_msg": "success" }
        }"#;
        let balance = parse_minimax_balance(json).unwrap();
        assert_eq!(balance.remaining, 42.5);
        assert_eq!(balance.monthly_spend, Some(7.25));
        assert_eq!(balance.currency, "USD");
    }

    #[test]
    fn parse_minimax_balance_defaults_currency_and_optional_spend() {
        // Currency + spend are optional; currency falls back to USD, spend to None.
        let json = r#"{ "balance": { "remaining": 100.0 } }"#;
        let balance = parse_minimax_balance(json).unwrap();
        assert_eq!(balance.remaining, 100.0);
        assert_eq!(balance.monthly_spend, None);
        assert_eq!(balance.currency, "USD");
    }

    #[test]
    fn parse_minimax_balance_rejects_percentage_only_body() {
        // A plan-percentage body (no `balance` object) must fail loudly rather
        // than masquerade as a zero balance.
        let json = r#"{
            "model_remains": [],
            "base_resp": { "status_code": 0, "status_msg": "success" }
        }"#;
        assert!(parse_minimax_balance(json).is_err());
    }

    #[test]
    fn parse_minimax_response_rejects_legacy_category_remains() {
        // Regression guard: if Minimax ever reverts the wrapper, the parser must
        // NOT silently return zero windows — the old shape should fail loudly.
        let json = r#"{
            "category_remains": [
                {
                    "category": "text_generation",
                    "display_name": "Text Generation",
                    "end_time": 1780153200000,
                    "current_interval_total_count": 15000,
                    "current_interval_usage_count": 55,
                    "current_weekly_total_count": 150000,
                    "current_weekly_usage_count": 732,
                    "weekly_end_time": 1780272000000
                }
            ],
            "base_resp": { "status_code": 0, "status_msg": "success" }
        }"#;
        let result = parse_minimax_response(json);
        assert!(
            result.is_err(),
            "legacy category_remains shape must be rejected, not silently zero-windowed"
        );
    }

    #[test]
    fn no_credential_returns_error() {
        // Issue #1745: minimax is the first provider migrated to the
        // outcome seam. The no-credential case is `UsageOutcome::NoCredential`,
        // which projects to `logged_in: false, error: Some(hint)`.
        let outcome = minimax_usage("");
        match outcome {
            UsageOutcome::NoCredential { hint } => {
                assert_eq!(hint, "No API key configured");
            }
            other => panic!("expected NoCredential outcome, got: {other:?}"),
        }
    }
}
