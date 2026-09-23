//! DeepSeek keyed adapter -- keyed cash-balance endpoint (issue #1127).
//! Card always visible.
//!
//! Owns the full fetch (ARCH-1): credential gate, live request (plus the
//! loopback-URL test seam), and response parsing. `usage.rs` keeps
//! orchestration only.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{api_key_for, UsageAdapter};
use crate::services::usage::outcome::UsageOutcome;
use crate::services::usage::types::{BillingBalance, UsageError};
use reqwest::blocking::Client;
use serde::Deserialize;

/// Drop-in [`UsageAdapter`] for `deepseek`.
pub(crate) struct DeepseekAdapter;

impl UsageAdapter for DeepseekAdapter {
    fn id(&self) -> &'static str {
        "deepseek"
    }

    // Issue #1745 phase 2 step 11: deepseek migrated to the outcome seam.
    // Empty key → `NoCredential`. 401/403 → `Rejected`. 429 → `RateLimited`.
    // Other → `Unavailable`.
    fn fetch(&self, accounts: &[ProviderAccount]) -> UsageOutcome {
        deepseek_usage(api_key_for(accounts, "deepseek").unwrap_or(""))
    }
}

// ─── DeepSeek Platform API (`deepseek`) ──────────────────────────────────────
//
// DeepSeek exposes exactly one Bearer-authenticated public endpoint for billing:
// `GET https://api.deepseek.com/user/balance` (issue #1125 / #1127). The auth
// scheme is `Authorization: Bearer <sk-…>` (same scheme the chat API uses).
// The response shape is documented as:
//
//   {
//     "is_available": true,
//     "balance_infos": [
//       {
//         "currency": "CNY",
//         "total_balance": "110.00",
//         "granted_balance": "10.00",
//         "topped_up_balance": "100.00"
//       }
//     ]
//   }
//
// DeepSeek's wallet is **pay-as-you-go** (no subscription tier) and the
// `total_balance` field is the headline figure (sum of granted + topped-up).
// We surface it as `BillingBalance.remaining` in the user's reported currency
// (CNY for the canonical DeepSeek Platform account; future region-specific
// currencies are honoured verbatim). `monthly_spend` is `None` — the balance
// endpoint does not return period spend and the chat-usage endpoint requires
// the same Bearer auth but reports token counts (not dollars), which would
// need a separate fetcher.
//
// `deepseek_usage` mirrors `kimi_usage` / `openrouter_usage`: a direct HTTP
// fetch (not via the shared `fetch_usage` driver) because it populates
// `balance`, not `windows` + `detail`. The `fetch_usage` driver could be
// generalised to support both shapes; that work is deferred behind
// `parse_minimax_balance`'s follow-up (see comment near `parse_kimi_response`).

#[derive(Deserialize, Debug)]
struct DeepSeekBalanceInfo {
    /// ISO 4217 currency code (e.g. `"CNY"`). Required (no
    /// `#[serde(default)]`) so a malformed entry is a hard parse error
    /// rather than silently defaulting — currency is what makes the
    /// balance card meaningful.
    currency: String,
    /// Total wallet balance as a **string** (DeepSeek's documented contract
    /// is `"110.00"` rather than `110.0`). Parsed to `f64` below.
    total_balance: String,
}

/// `balance_infos` is documented as a single-element array in the DeepSeek
/// Platform API; we treat it as `Vec` so a future multi-currency response
/// (e.g. CNY + USD) doesn't break the parser — but we collapse to the first
/// entry for the headline `BillingBalance` (the Usage panel renders one
/// balance per account; multi-currency would need its own UX).
#[derive(Deserialize, Debug)]
struct DeepSeekBalanceResp {
    /// Required (no `#[serde(default)]`) so a body without `balance_infos`
    /// fails loudly as a `Shape` error rather than silently zero-balance.
    balance_infos: Vec<DeepSeekBalanceInfo>,
}

/// Parse the DeepSeek `/user/balance` body into a `BillingBalance`. The
/// balance values are documented as **decimal strings** (e.g. `"110.00"`)
/// — `f64::from_str` parses them directly. `total_balance` parses
/// independently from the granted/topped-up split (which the wire contract
/// doesn't surface); if a future DeepSeek response breaks the field into
/// separate `granted_balance` + `topped_up_balance`, a follow-up sum lands
/// here.
///
/// `monthly_spend` is unconditionally `None` — the balance endpoint does not
/// return period spend. A logged-in DeepSeek account therefore renders the
/// `BillingBalance` with `remaining` populated and `monthlySpend = null`
/// (the React side already handles this shape — see UsageRender.tsx).
///
/// `Result<BillingBalance, UsageError>` (NOT `Option`) because every
/// well-formed response has a balance; there is no "no wallet configured"
/// state to model. Using `Result<Option<_>>` would invite a future caller to
/// add an `Ok(None)` indistinguishable from the "no PAYG billing_mode"
/// `ProviderUsage.balance = None` case.
fn parse_deepseek_response(body: &str) -> Result<BillingBalance, UsageError> {
    let resp: DeepSeekBalanceResp =
        serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;
    let info = resp.balance_infos.into_iter().next().ok_or_else(|| {
        UsageError::Shape("DeepSeek balance response had no balance_infos entry".to_string())
    })?;
    let remaining = info
        .total_balance
        .trim()
        .parse::<f64>()
        .map_err(|e| UsageError::Shape(format!("DeepSeek total_balance not a number: {}", e)))?;
    Ok(BillingBalance {
        remaining,
        monthly_spend: None,
        currency: info.currency,
    })
}

/// Public DeepSeek fetcher. Reads the user's DeepSeek API key from the
/// provider account, hits `https://api.deepseek.com/user/balance`, and
/// reports the wire contract. Registered in [`catalog`] so the keyed-provider
/// panel polls it on the same cadence as Kimi / OpenRouter. Thin wrapper around
/// [`deepseek_usage_with_url`] with the production endpoint pinned — the
/// test seam keeps the loopback HTTP integration tests (issue #971
/// pattern) runnable without hitting the real DeepSeek API.
pub(crate) fn deepseek_usage(api_key: &str) -> UsageOutcome {
    // Issue #1745 phase 2: migrated to the outcome seam. Empty key →
    // `NoCredential`. 401/403 → `Rejected`. 429 → `RateLimited`. Other →
    // `Unavailable`.
    deepseek_usage_with_url(api_key, "https://api.deepseek.com/user/balance")
}

/// Test seam: pass an explicit loopback URL so the live-fetcher tests can
/// stand in a `tiny_http` server for the production endpoint without
/// hitting the real DeepSeek API. Mirrors the
/// `openai_usage_with_base_url` pattern —
/// keeps the public fetcher a one-line wrapper around this with the
/// production URL. (Note: `kimi_usage` / `openrouter_usage` don't carry
/// this seam and have no loopback tests; their balance parsers are
/// pinned by pure fixtures only. Adding the seam here is the more
/// thorough contract — leaving room for either kimi/openrouter to gain
/// the same seam in a follow-up, or for this seam to be dropped once
/// parity across balance fetchers is the chosen design.)
fn deepseek_usage_with_url(api_key: &str, live_url: &str) -> UsageOutcome {
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
    let resp = match client.get(live_url).header("Authorization", auth).send() {
        Ok(r) if r.status() == 429 => {
            return UsageOutcome::RateLimited {
                reason: "Rate limited — usage data temporarily unavailable".to_string(),
            }
        }
        Ok(r) if !r.status().is_success() => {
            let code = r.status().as_u16();
            if code == 401 || code == 403 {
                return UsageOutcome::Rejected {
                    hint: "Invalid API key".to_string(),
                };
            }
            return UsageOutcome::Unavailable {
                reason: format!("API error {}: {}", code, r.text().unwrap_or_default()),
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
                reason: format!("Failed to read response: {}", e),
            }
        }
    };
    match parse_deepseek_response(&body) {
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
    use crate::services::usage::adapter::spawn_loopback;
    use std::sync::Arc;

    // ─── DeepSeek Platform API (`deepseek`) ──────────────────────────────────
    //
    // Pin the wire contract from issue #1125's documented response, plus the
    // 401/403/429/200 degradation branches via loopback HTTP. The fixture is
    // the canonical CNY wallet shape; USD-shaped responses (future) parse
    // identically because the currency field is propagated verbatim.

    /// Pinned fixture from the documented DeepSeek Platform contract (issue
    /// #1125). `total_balance` is a **string** (not a number) per the
    /// documented contract — that's the spec we're pinning.
    const DEEPSEEK_BALANCE_BODY: &str = r#"{
        "is_available": true,
        "balance_infos": [
            {
                "currency": "CNY",
                "total_balance": "110.00",
                "granted_balance": "10.00",
                "topped_up_balance": "100.00"
            }
        ]
    }"#;

    #[test]
    fn parse_deepseek_response_extracts_total_balance_and_currency() {
        // Headline happy path: 200 with the documented CNY wallet shape.
        // `monthly_spend` is unconditionally None — the balance endpoint
        // does not return period spend. `granted_balance` / `topped_up_balance`
        // are ignored (we surface only `total_balance`); the split is
        // bookkeeping detail the Usage panel doesn't render.
        let b = parse_deepseek_response(DEEPSEEK_BALANCE_BODY).unwrap();
        assert_eq!(b.remaining, 110.00);
        assert_eq!(b.monthly_spend, None);
        assert_eq!(b.currency, "CNY");
    }

    #[test]
    fn parse_deepseek_response_handles_decimal_string_with_leading_zero() {
        // DeepSeek's documented examples use `"110.00"`; verify the parser
        // also handles `" 110.00 "` (leading/trailing whitespace) and the
        // edge case of `"0"` (zero-wallet is not a shape error).
        let trimmed = parse_deepseek_response(
            r#"{
                "is_available": true,
                "balance_infos": [
                    {"currency": "CNY", "total_balance": " 110.00 ", "granted_balance": "10.00", "topped_up_balance": "100.00"}
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(trimmed.remaining, 110.00);

        let zero = parse_deepseek_response(
            r#"{
                "is_available": false,
                "balance_infos": [
                    {"currency": "CNY", "total_balance": "0", "granted_balance": "0", "topped_up_balance": "0"}
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(zero.remaining, 0.0);
        assert_eq!(zero.currency, "CNY");
    }

    #[test]
    fn parse_deepseek_response_rejects_missing_balance_infos() {
        // Required field — a body without `balance_infos` is malformed, not
        // "empty wallet". Mirrors the `parse_kimi_response_rejects_missing_data`
        // / `parse_openrouter_response_rejects_missing_data` regression nets.
        let err = parse_deepseek_response(r#"{"is_available":true}"#).unwrap_err();
        assert!(
            matches!(err, UsageError::Shape(_)),
            "expected Shape error, got {err:?}"
        );
    }

    #[test]
    fn parse_deepseek_response_rejects_empty_balance_infos_array() {
        // An empty array is documented-impossible (DeepSeek returns exactly
        // one entry per the wire contract). Treat as a shape error rather
        // than silently zero-balance — a future API that drops the entry
        // would otherwise look like a fresh user with no wallet.
        let err = parse_deepseek_response(r#"{"is_available":true,"balance_infos":[]}"#)
            .unwrap_err();
        assert!(
            matches!(err, UsageError::Shape(_)),
            "expected Shape error, got {err:?}"
        );
    }

    #[test]
    fn parse_deepseek_response_rejects_non_numeric_total_balance() {
        // The field is documented as a decimal string. A response that puts
        // a raw JSON number there (`"total_balance": 110.0`) is a schema
        // drift and must fail loudly — silently treating it as 0 would hide
        // a real billing misconfiguration.
        let err = parse_deepseek_response(
            r#"{"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":110.0}]}"#,
        )
        .unwrap_err();
        assert!(
            matches!(err, UsageError::Shape(_)),
            "expected Shape error for non-string total_balance, got {err:?}"
        );
    }

    #[test]
    fn deepseek_usage_with_empty_key_returns_logged_out() {
        // Mirrors `kimi_usage_with_empty_key_returns_logged_out` —
        // `cached_or_fetch` is expected to gate on key presence via
        // `configured_keyed_providers`, but the fetcher still defends so a
        // misconfigured call surfaces "no API key" instead of a confusing 401.
        let usage = deepseek_usage("").into_usage("deepseek");
        assert!(!usage.logged_in);
        assert_eq!(usage.provider, "deepseek");
        assert!(usage
            .error
            .as_deref()
            .map(|e| e.contains("No API key"))
            .unwrap_or(false));
        assert!(usage.balance.is_none());
    }

    /// Headline happy-path test: valid key → 200 with the documented
    /// `total_balance` body → `ProviderUsage` carries a `BillingBalance`
    /// with `currency` propagated verbatim. Verifies the full pipeline:
    /// Bearer header shape, JSON parse, currency propagation.
    #[test]
    fn deepseek_usage_with_live_loopback_returns_balance() {
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
            let _ = req.respond(tiny_http::Response::from_string(DEEPSEEK_BALANCE_BODY));
        });
        let url = format!("http://127.0.0.1:{port}/user/balance");
        let usage = deepseek_usage_with_url("sk-test", &url).into_usage("deepseek");
        assert!(usage.logged_in);
        assert_eq!(usage.provider, "deepseek");
        assert!(usage.error.is_none());
        let balance = usage.balance.expect("happy-path must populate balance");
        assert_eq!(balance.remaining, 110.00);
        assert_eq!(balance.monthly_spend, None);
        assert_eq!(balance.currency, "CNY");
        assert!(usage.windows.is_empty());
        // The bearer header carries the user's configured key verbatim
        // (DeepSeek's auth scheme matches OpenAI / Kimi / OpenRouter).
        assert_eq!(*observed.lock().unwrap(), "Bearer sk-test");
    }

    /// 401 collapses to `logged_out` with "Invalid API key" — the keyed
    /// pattern shared with `kimi_usage` / `openrouter_usage`. The re-auth
    /// affordance stays as a single seam so the React `<UsagePanel>` copy
    /// doesn't diverge per-provider.
    #[test]
    fn deepseek_usage_with_live_loopback_401_returns_logged_out() {
        let port = spawn_loopback(1, |req| {
            let _ = req.respond(
                tiny_http::Response::from_string(r#"{"error":"authentication failed"}"#)
                    .with_status_code(401),
            );
        });
        let url = format!("http://127.0.0.1:{port}/user/balance");
        let usage = deepseek_usage_with_url("sk-bad", &url).into_usage("deepseek");
        assert!(!usage.logged_in);
        assert_eq!(usage.provider, "deepseek");
        assert_eq!(usage.error.as_deref(), Some("Invalid API key"));
        assert!(usage.balance.is_none());
    }

    /// 403 (Forbidden / account revoked) collapses with 401 to the same
    /// logged-out branch — an account-revoked token behaves identically to
    /// an expired one from the user's perspective.
    #[test]
    fn deepseek_usage_with_live_loopback_403_returns_logged_out() {
        let port = spawn_loopback(1, |req| {
            let _ = req.respond(tiny_http::Response::empty(403));
        });
        let url = format!("http://127.0.0.1:{port}/user/balance");
        let usage = deepseek_usage_with_url("sk-revoked", &url).into_usage("deepseek");
        assert!(!usage.logged_in);
        assert_eq!(usage.error.as_deref(), Some("Invalid API key"));
    }

    /// 429 preserves `logged_in = true` (key may be valid; rate limit is
    /// transient). The user-facing copy matches Kimi / OpenRouter / Cursor
    /// for a uniform "rate limited" affordance across keyed fetchers.
    #[test]
    fn deepseek_usage_with_live_loopback_429_preserves_logged_in() {
        let port = spawn_loopback(1, |req| {
            let _ = req.respond(tiny_http::Response::empty(429));
        });
        let url = format!("http://127.0.0.1:{port}/user/balance");
        let usage = deepseek_usage_with_url("sk-test", &url).into_usage("deepseek");
        assert!(usage.logged_in, "429 must not flip to logged_out");
        assert!(usage
            .error
            .as_deref()
            .map(|e| e.contains("Rate limited"))
            .unwrap_or(false));
        assert!(usage.balance.is_none());
    }

    /// Malformed body (200 + garbage JSON): shape error → `unavailable`
    /// with `logged_in = true`. Mirrors the codex / kimi / openrouter
    /// shape-failure contract — the fetcher logs the user in (key works
    /// for *some* endpoints) but the response shape is broken.
    #[test]
    fn deepseek_usage_with_live_loopback_malformed_body_returns_unavailable() {
        let port = spawn_loopback(1, |req| {
            let _ = req.respond(tiny_http::Response::from_string("not-json"));
        });
        let url = format!("http://127.0.0.1:{port}/user/balance");
        let usage = deepseek_usage_with_url("sk-test", &url).into_usage("deepseek");
        assert!(usage.logged_in);
        assert!(usage
            .error
            .as_deref()
            .map(|e| e.contains("parse") || e.contains("Shape"))
            .unwrap_or(false));
        assert!(usage.balance.is_none());
    }
}
