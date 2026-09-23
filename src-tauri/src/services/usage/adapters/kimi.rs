//! Kimi (Moonshot) keyed wallet adapter -- cash-balance endpoint rather
//! than plan windows. Card always visible.
//!
//! Owns the full fetch (ARCH-1): credential gate, live request, and
//! response parsing. `usage.rs` keeps orchestration only.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{api_key_for, UsageAdapter};
use crate::services::usage::outcome::UsageOutcome;
use crate::services::usage::types::{BillingBalance, UsageError};
use reqwest::blocking::Client;
use serde::Deserialize;

/// Drop-in [`UsageAdapter`] for `kimi`.
pub(crate) struct KimiAdapter;

impl UsageAdapter for KimiAdapter {
    fn id(&self) -> &'static str {
        "kimi"
    }

    // Issue #1745 phase 2 step 8: kimi migrated to the outcome seam.
    // Empty key → `NoCredential`. 401/403 → `Rejected` ("Invalid API key"
    // affordance). 429 → `RateLimited`. Other → `Unavailable`.
    fn fetch(&self, accounts: &[ProviderAccount]) -> UsageOutcome {
        kimi_usage(api_key_for(accounts, "kimi").unwrap_or(""))
    }
}

// ─── Kimi (Moonshot) wallet meter ──────────────────────────────────────────
//
// Kimi exposes exactly one Bearer-authenticated public endpoint for billing:
// `GET https://api.moonshot.ai/v1/users/me/balance` — the same auth scheme as
// the chat API (`Authorization: Bearer <sk-…>`). It reports the user's wallet
// total in USD as `available_balance` (cash + voucher). The richer
// per-model/monthly spend data lives on `platform.kimi.ai/api?endpoint=consumes`
// and siblings, but those require an OAuth session JWT (login.moonshot.ai +
// refreshToken), NOT the API key — so `monthly_spend` stays None until a
// dedicated Kimi OAuth flow ships in Buildmesh.
//
// `kimi_usage` is a direct HTTP fetch (not via the shared `fetch_usage`
// driver) because it populates `balance` (not `windows` + `detail`). The
// `fetch_usage` driver could be generalized to support both shapes, which
// would also unlock the existing-but-dead `parse_minimax_balance` (issue
// #537 follow-up) — filed as a follow-up issue.

#[derive(Deserialize, Debug)]
struct KimiData {
    // Required (no #[serde(default)]) so a malformed body — one that lacks the
    // balance field — fails loudly instead of silently reporting a zero wallet.
    available_balance: f64,
}

#[derive(Deserialize, Debug)]
struct KimiResp {
    #[serde(default)]
    code: Option<i64>,
    data: KimiData,
}

/// Parses the Kimi Check Balance response into a `BillingBalance`. `monthly_spend`
/// is unconditionally `None` (no public-auth path to the spend endpoint); a
/// future OAuth-driven fetcher is the only way this gains a spend figure.
///
/// Returns `Result<BillingBalance, UsageError>` (NOT `Option`) because every
/// well-formed response has a balance — there is no "no wallet configured"
/// state to model. Using `Result<Option<_>>` would invite a future caller to
/// add an `Ok(None)` that is indistinguishable from the existing "no PAYG
/// billing_mode" `ProviderUsage.balance = None` case.
fn parse_kimi_response(body: &str) -> Result<BillingBalance, UsageError> {
    let resp: KimiResp =
        serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;

    // Vendor envelope: non-zero `code` is a vendor-level error, not a zero
    // balance. Same shape as Moonshot's chat-API errors.
    if let Some(c) = resp.code {
        if c != 0 {
            return Err(UsageError::Shape(format!(
                "Kimi API returned code {}",
                c
            )));
        }
    }

    Ok(BillingBalance {
        remaining: resp.data.available_balance,
        monthly_spend: None,
        // The wallet is denominated in USD per the documented contract;
        // Moonshot does not return a currency field on this endpoint.
        currency: "USD".to_string(),
    })
}

pub(crate) fn kimi_usage(api_key: &str) -> UsageOutcome {
    // Issue #1745 phase 2: Kimi is migrated to the outcome seam. Empty key
    // → `NoCredential` (gate drops). 401/403 → `Rejected` ("Invalid API
    // key" affordance). 429 → `RateLimited`. Other non-2xx / transport /
    // parse → `Unavailable`.
    if api_key.is_empty() {
        return UsageOutcome::NoCredential {
            hint: "No API key configured".to_string(),
        };
    }

    let client = match Client::builder().build() {
        Ok(c) => c,
        Err(e) => {
            return UsageOutcome::Unavailable {
                reason: format!("Client error: {}", e),
            }
        }
    };

    let auth = format!("Bearer {}", api_key);
    let resp = match client
        .get("https://api.moonshot.ai/v1/users/me/balance")
        .header("Authorization", auth)
        .send()
    {
        Ok(r) if r.status() == 429 => {
            return UsageOutcome::RateLimited {
                reason: "Rate limited — usage data temporarily unavailable".to_string(),
            }
        }
        Ok(r) if !r.status().is_success() => {
            let code = r.status().as_u16();
            // 401 (Unauthorized) AND 403 (Forbidden / account revoked) both signal
            // "the credential is bad" — surface as `Rejected` so the UI can
            // prompt the user to re-enter, matching the `logged_in=false`
            // semantics. Moonshot returns 403 for disabled accounts per their
            // API contract; missing the branch silently degrades to a generic
            // 'API error 403' with no re-enter affordance.
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

    let body = resp.text().unwrap_or_default();
    match parse_kimi_response(&body) {
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


    // ── Kimi (Moonshot) wallet meter ────────────────────────────────────────
    //
    // The Kimi platform's only Bearer-authenticated public endpoint is
    // `GET /v1/users/me/balance` — it returns the user's cash + voucher wallet
    // total in USD. Spend/usage endpoints (consumes, organizationAccountInfo)
    // require an OAuth session JWT, NOT the chat API key, so monthly_spend
    // stays None until a Kimi OAuth flow lands. Fixture is the documented
    // response shape (https://platform.kimi.ai/docs/api/balance.md).

    #[test]
    fn parse_kimi_response_extracts_available_balance_as_usd_wallet() {
        let json = r#"{
            "code": 0,
            "data": {
                "available_balance": 49.58894,
                "voucher_balance": 46.58893,
                "cash_balance": 3.00001
            },
            "scode": "0x0",
            "status": true
        }"#;
        let b = parse_kimi_response(json).unwrap();
        assert_eq!(b.remaining, 49.58894);
        // monthly_spend has no public auth path — pin None so a future
        // OAuth-driven fetcher is the ONLY way this becomes Some.
        assert_eq!(b.monthly_spend, None);
        assert_eq!(b.currency, "USD");
    }

    #[test]
    fn parse_kimi_response_rejects_nonzero_code() {
        // Vendor envelope: non-zero `code` is an error, not a zero balance.
        let json = r#"{"code":401,"data":{"available_balance":0.0},"status":false}"#;
        let err = parse_kimi_response(json).unwrap_err();
        assert!(matches!(err, UsageError::Shape(_)), "expected Shape error, got {err:?}");
    }

    #[test]
    fn parse_kimi_response_rejects_missing_data() {
        // Required field — a body without `data` is malformed, not "empty wallet".
        let json = r#"{"code":0,"scode":"0x0","status":true}"#;
        let err = parse_kimi_response(json).unwrap_err();
        assert!(matches!(err, UsageError::Shape(_)));
    }

    #[test]
    fn parse_kimi_response_rejects_missing_available_balance() {
        // Required field — the response without `available_balance` is malformed.
        let json = r#"{"code":0,"data":{"voucher_balance":0.0,"cash_balance":0.0},"scode":"0x0","status":true}"#;
        let err = parse_kimi_response(json).unwrap_err();
        assert!(matches!(err, UsageError::Shape(_)));
    }

    #[test]
    fn kimi_usage_with_empty_key_returns_logged_out() {
        // Mirrors minimax_usage("") — the upstream caller is expected to gate on
        // the key being present; we still defend here so a misconfigured fetch
        // surfaces as "no API key configured" rather than an HTTP 401.
        let usage = kimi_usage("").into_usage("kimi");
        assert!(!usage.logged_in);
        assert_eq!(usage.provider, "kimi");
        assert!(usage.error.is_some());
        assert!(usage.balance.is_none());
    }

    #[test]
    fn parse_kimi_response_accepts_missing_top_level_code_envelope() {
        // Forward-compat: some endpoints may omit the `code` field entirely on
        // success (Moonshot's documented example includes it, but a future
        // schema change shouldn't break parsing). The Option<i64> default
        // expresses this; absence is NOT treated as an error.
        let json = r#"{
            "data": {
                "available_balance": 12.34,
                "voucher_balance": 0.0,
                "cash_balance": 12.34
            }
        }"#;
        let b = parse_kimi_response(json).unwrap();
        assert_eq!(b.remaining, 12.34);
        assert_eq!(b.currency, "USD");
    }
}
