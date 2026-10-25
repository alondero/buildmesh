//! MiniMax keyed adapter -- credential comes from the effective account
//! snapshot, never from disk. Card always visible so the key editor stays
//! reachable.
//!
//! Owns the full fetch (ARCH-1): credential gate, live request via the
//! shared [`fetch_usage`](crate::services::usage::adapter::fetch_usage)
//! driver, and response parsing. `usage.rs` keeps orchestration only.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{api_key_for, fetch_usage, UsageAdapter};
use crate::services::usage::outcome::{ResponsePolicy, UsageOutcome, UsageReading};
use crate::services::usage::types::UsagePage;
use crate::services::usage::types::{UsageError, UsageWindow};
// Only the test-only `parse_minimax_balance` below names `BillingBalance`.
#[cfg(test)]
use crate::services::usage::types::BillingBalance;
use serde::Deserialize;

/// Drop-in [`UsageAdapter`] for `minimax`.
pub(crate) struct MinimaxAdapter;

impl UsageAdapter for MinimaxAdapter {
    fn usage_page(&self) -> Option<UsagePage> {
        Some(UsagePage::new(
            "https://platform.minimax.io/console/plan",
            "View usage",
        ))
    }

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

        push_window(
            "5-hour",
            model.current_interval_remaining_percent,
            model.end_time,
        );
        push_window(
            "Weekly",
            model.current_weekly_remaining_percent,
            model.weekly_end_time,
        );
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
// `cfg(test)`: exercised by the mock tests below, not yet by a live fetch
// (issue #2450).
#[cfg(test)]
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
        ResponsePolicy::default(),
        |c| {
            c.get("https://api.minimax.io/v1/token_plan/remains")
                .header("Authorization", auth)
        },
        // The provider reports quota windows only; the balance lives on a
        // second endpoint that no caller wires up yet.
        |body| {
            let (windows, detail) = parse_minimax_response(body)?;
            Ok(UsageReading {
                windows,
                balance: None,
                meters: Vec::new(),
                detail,
            })
        },
    )
}

#[cfg(test)]
#[path = "minimax_tests.rs"]
mod tests;
