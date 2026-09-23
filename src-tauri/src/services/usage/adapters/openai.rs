//! OpenAI keyed adapter -- Organization Costs endpoint (admin-scoped;
//! project keys degrade through the fetcher's normal logged-in/detail
//! envelope). Card always visible.
//!
//! Owns the full fetch (ARCH-1): credential gate, inference-probe +
//! costs-probe live requests, and response parsing. `usage.rs` keeps
//! orchestration only.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{api_key_for, UsageAdapter};
use crate::services::usage::outcome::UsageOutcome;
use crate::services::usage::types::{BillingBalance, UsageError};
use chrono::Datelike;
use reqwest::blocking::Client;
use serde::Deserialize;
use std::time::Duration;

/// Drop-in [`UsageAdapter`] for `openai`.
pub(crate) struct OpenaiAdapter;

impl UsageAdapter for OpenaiAdapter {
    fn id(&self) -> &'static str {
        "openai"
    }

    // Issue #1745 phase 2 step 10: openai migrated to the outcome seam.
    // Empty key → `NoCredential`. 401/403 on inference → `Rejected`. 429 →
    // `RateLimited`. 401/403 on costs (sk-proj- contract per ADR-0026 §2)
    // → `Degraded` (logged_in: true with `detail` hint). Other → `Unavailable`.
    fn fetch(&self, accounts: &[ProviderAccount]) -> UsageOutcome {
        openai_usage(api_key_for(accounts, "openai").unwrap_or(""))
    }
}

// ─── OpenAI Platform API (`openai`) ────────────────────────────────────────────
//
// OpenAI offers no public real-time wallet/credit-balance endpoint for either
// Admin or Project API keys — the only spend data is the **Organization
// Costs** API (`GET /v1/organization/costs`), which is admin-scoped
// (`sk-admin-…` keys only). Standard Project keys (`sk-proj-…`) return
// `401 Unauthorized` / `403 Forbidden` on that endpoint (issue #1109,
// spec §3).
//
// Degradation matrix (spec §3.2):
//
//   | Key Type     | Costs endpoint   | ProviderUsage                              |
//   |--------------|------------------|--------------------------------------------|
//   | Admin Key    | 200 + cost body  | logged_in=true, balance=Some(...)          |
//   | Project Key  | 401/403          | logged_in=true, balance=None, detail=Some  |
//   | Invalid Key  | 401 on /models   | logged_in=false, error="Invalid API key"   |
//
// We probe `/v1/models` first to distinguish the third case (invalid key)
// from the second (project key, which also fails on costs but works on
// inference). The two-round-trip overhead is amortized by the in-process
// 5-min usage cache (the same seam `opencode_usage_impl` uses for #957).

#[derive(Deserialize, Debug)]
struct OpenAiAmount {
    #[serde(default)]
    value: Option<f64>,
    #[serde(default)]
    currency: Option<String>,
}

#[derive(Deserialize, Debug)]
struct OpenAiCostResult {
    #[serde(default)]
    amount: Option<OpenAiAmount>,
}

#[derive(Deserialize, Debug)]
struct OpenAiCostBucket {
    #[serde(default)]
    results: Vec<OpenAiCostResult>,
}

#[derive(Deserialize, Debug)]
struct OpenAiCostResp {
    // Required (no `#[serde(default)]`) so a malformed body — one that lacks
    // the `data` field — fails loudly with `Shape` instead of silently
    // reporting zero monthly spend. Mirrors `MinimaxResp.model_remains`
    // (#537) and `OpenCodeBillingResp.windows` (#957).
    data: Vec<OpenAiCostBucket>,
}

/// Parse the `/v1/organization/costs` body into a [`BillingBalance`]. Sums
/// every USD `amount.value` across all buckets on the page. OpenAI currently
/// bills only in USD so the currency filter is future-proofing — a multi-
/// currency org would surface `monthly_spend` in USD only and ignore other
/// amounts (preserves the wire invariant that `currency` is `"USD"`).
///
/// A missing `data` field is a shape error (the field is required by the
/// documented contract); an empty `data` array is a valid "no spend yet this
/// month" reply and yields `monthly_spend = 0.0`.
fn parse_openai_costs_response(body: &str) -> Result<BillingBalance, UsageError> {
    let resp: OpenAiCostResp =
        serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;
    let mut total: f64 = 0.0;
    for bucket in resp.data {
        for result in bucket.results {
            if let Some(amount) = result.amount {
                let is_usd = amount
                    .currency
                    .as_deref()
                    .map(|c| c.eq_ignore_ascii_case("usd"))
                    .unwrap_or(true); // absent = USD by current OpenAI contract
                if is_usd {
                    if let Some(value) = amount.value {
                        total += value;
                    }
                }
            }
        }
    }
    Ok(BillingBalance {
        // OpenAI has no wallet/balance surface on this endpoint — only spend.
        // `remaining` is set to 0.0 so the JSON shape is well-formed; the
        // <UsagePanel> renders `monthly_spend` as the headline figure.
        remaining: 0.0,
        monthly_spend: Some(total),
        currency: "USD".to_string(),
    })
}

/// Compute the Unix epoch seconds for the start of the current UTC calendar
/// month. Used to bound the `/v1/organization/costs` query to the billing
/// period. Returns 0 as a defensive fallback when the current date can't be
/// normalized — rare, but keeps the URL well-formed rather than producing
/// `?start_time=-1`.
fn current_month_start_epoch() -> i64 {
    let now = chrono::Utc::now();
    let month_start = now
        .date_naive()
        .with_day(1)
        .unwrap_or_else(|| now.date_naive())
        .and_hms_opt(0, 0, 0)
        .unwrap_or_else(|| now.naive_utc());
    month_start.and_utc().timestamp().max(0)
}

/// Public OpenAI fetcher. Registered in [`catalog`] so the keyed-provider
/// panel polls it on the same cadence as the other keyed fetchers.
pub(crate) fn openai_usage(api_key: &str) -> UsageOutcome {
    // Issue #1745 phase 2: migrated to the outcome seam. Empty key →
    // `NoCredential`. 401/403 on inference → `Rejected`. 429 →
    // `RateLimited`. 401/403 on costs → `Degraded` (sk-proj- contract per
    // ADR-0026 §2: key works, admin endpoint doesn't, surface a hint via
    // `detail`). Other failures → `Unavailable`.
    openai_usage_with_base_url(api_key, "https://api.openai.com/v1")
}

/// Test seam: pass an explicit base URL so a loopback `tiny_http` server can
/// stand in for the production endpoint in mocked HTTP tests (issue #971
/// pattern).
fn openai_usage_with_base_url(api_key: &str, base_url: &str) -> UsageOutcome {
    if api_key.is_empty() {
        return UsageOutcome::NoCredential {
            hint: "No API key configured".to_string(),
        };
    }

    let client = match Client::builder().timeout(Duration::from_secs(15)).build() {
        Ok(c) => c,
        Err(e) => {
            return UsageOutcome::Unavailable {
                reason: format!("Client error: {}", e),
            }
        }
    };

    let auth = format!("Bearer {}", api_key);

    // ── Step 1: inference check via `/v1/models` ────────────────────────
    //
    // Validates that the key works AT ALL before attempting the admin-only
    // costs endpoint. This is the discriminator that lets us tell a project
    // key (which fails 401/403 on costs but works on inference) from a
    // truly invalid/revoked key (which fails on both). Spec §3.2.
    match client
        .get(format!("{}/models", base_url))
        .header("Authorization", auth.clone())
        .send()
    {
        Ok(r) if r.status().as_u16() == 401 || r.status().as_u16() == 403 => {
            return UsageOutcome::Rejected {
                hint: "Invalid API key".to_string(),
            };
        }
        Ok(r) if r.status().as_u16() == 429 => {
            return UsageOutcome::RateLimited {
                reason: "Rate limited — usage data temporarily unavailable".to_string(),
            };
        }
        Ok(r) if !r.status().is_success() => {
            let code = r.status().as_u16();
            return UsageOutcome::Unavailable {
                reason: format!("API error {}: inference check failed", code),
            };
        }
        Ok(_) => {} // 2xx — proceed to costs probe
        Err(e) => {
            return UsageOutcome::Unavailable {
                reason: format!("Inference check failed: {}", e),
            }
        }
    }

    // ── Step 2: organization costs (admin-scoped) ───────────────────────
    let start_time = current_month_start_epoch();
    let url = format!(
        "{}/organization/costs?start_time={}&bucket_width=1d",
        base_url, start_time
    );
    let resp = match client
        .get(&url)
        .header("Authorization", auth)
        .send()
    {
        Ok(r) if r.status().as_u16() == 401 || r.status().as_u16() == 403 => {
            // Project key — graceful degradation per ADR-0026 §2. The key
            // is valid for inference; we just can't reach the admin
            // endpoint. Surface as `Degraded` so the panel renders the
            // `detail` hint without painting the row as "fetch failed".
            return UsageOutcome::Degraded {
                detail: "Monthly spend tracking requires an Organization Admin API Key (sk-admin-...)"
                    .to_string(),
            };
        }
        Ok(r) if r.status().as_u16() == 429 => {
            return UsageOutcome::RateLimited {
                reason: "Rate limited — usage data temporarily unavailable".to_string(),
            };
        }
        Ok(r) if !r.status().is_success() => {
            let code = r.status().as_u16();
            return UsageOutcome::Unavailable {
                reason: format!("API error {}: costs query failed", code),
            };
        }
        Ok(r) => r,
        Err(e) => {
            return UsageOutcome::Unavailable {
                reason: format!("Costs query failed: {}", e),
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
    match parse_openai_costs_response(&body) {
        Ok(balance) => UsageOutcome::Reading {
            windows: Vec::new(),
            balance: Some(balance),
            meters: Vec::new(),
            detail: None,
        },
        Err(e) => UsageOutcome::Unavailable {
            reason: format!("Failed to parse costs response: {}", e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    // ── OpenAI costs response parser (spec §3) ────────────────────────────

    #[test]
    fn parse_openai_costs_response_sums_daily_buckets() {
        // Pinned fixture for the OpenAI `/v1/organization/costs` shape.
        // Two days, two results each; sums to 7.25 USD across the month.
        let json = r#"{
            "object": "page",
            "data": [
                {
                    "object": "bucket",
                    "start_time": 1751328000,
                    "end_time": 1751414400,
                    "results": [
                        {"object":"organization.cost.result","amount":{"value":1.5,"currency":"usd"},"line_item":null,"project_id":null,"organization_id":"org-x"},
                        {"object":"organization.cost.result","amount":{"value":2.25,"currency":"usd"},"line_item":null,"project_id":null,"organization_id":"org-x"}
                    ]
                },
                {
                    "object": "bucket",
                    "start_time": 1751414400,
                    "end_time": 1751500800,
                    "results": [
                        {"object":"organization.cost.result","amount":{"value":3.5,"currency":"USD"},"line_item":null,"project_id":null,"organization_id":"org-x"}
                    ]
                }
            ],
            "has_more": false,
            "next_page": null
        }"#;
        let balance = parse_openai_costs_response(json).unwrap();
        assert_eq!(balance.remaining, 0.0);
        assert_eq!(balance.monthly_spend, Some(7.25));
        assert_eq!(balance.currency, "USD");
    }

    #[test]
    fn parse_openai_costs_response_empty_data_is_zero_spend() {
        // No spend this month is a valid response: `data: []` and zero
        // monthly_spend. NOT a shape error.
        let balance = parse_openai_costs_response(r#"{"data":[]}"#).unwrap();
        assert_eq!(balance.monthly_spend, Some(0.0));
        assert_eq!(balance.currency, "USD");
    }

    #[test]
    fn parse_openai_costs_response_filters_non_usd_currency() {
        // Forward-compat: a multi-currency org would have non-USD amounts;
        // we ignore them so the USD headline is consistent.
        let json = r#"{
            "data": [{
                "results": [
                    {"amount": {"value": 5.0, "currency": "usd"}},
                    {"amount": {"value": 99.0, "currency": "eur"}}
                ]
            }]
        }"#;
        let balance = parse_openai_costs_response(json).unwrap();
        assert_eq!(balance.monthly_spend, Some(5.0));
    }

    #[test]
    fn parse_openai_costs_response_missing_data_is_shape_error() {
        // Required field — a body without `data` is malformed. OpenAI
        // always returns the field even on an empty month.
        let err = parse_openai_costs_response(r#"{"object":"page"}"#).unwrap_err();
        assert!(matches!(err, UsageError::Shape(_)), "got {err:?}");
    }

    #[test]
    fn current_month_start_epoch_is_first_of_utc_month() {
        // The start_time query parameter must be the first second of the
        // current UTC month, not a rolling 30-day window. Spot-check the
        // month-day boundary: the value is always day=1 at 00:00:00 UTC.
        let epoch = current_month_start_epoch();
        let dt = chrono::DateTime::from_timestamp(epoch, 0).expect("valid epoch");
        assert_eq!(dt.day(), 1, "month start must be day 1, got day {}", dt.day());
        assert_eq!(dt.hour(), 0);
        assert_eq!(dt.minute(), 0);
        assert_eq!(dt.second(), 0);
    }

    // ── OpenAI live-probe mocked HTTP integration (spec §3.2) ────────────

    const OPENAI_COSTS_BODY: &str = r#"{
        "object": "page",
        "data": [
            {
                "object": "bucket",
                "start_time": 1751328000,
                "end_time": 1751414400,
                "results": [
                    {"amount": {"value": 12.5, "currency": "usd"}}
                ]
            }
        ],
        "has_more": false
    }"#;

    const OPENAI_MODELS_BODY: &str = r#"{
        "object": "list",
        "data": [{"id": "gpt-4o", "object": "model"}]
    }"#;

    #[test]
    fn openai_usage_with_admin_key_returns_monthly_spend() {
        // Headline happy path: inference check (200) → costs (200) →
        // BillingBalance with `monthly_spend`. `logged_in = true`, no
        // error, no degradation detail.
        let port = {
            use std::thread;
            let server = tiny_http::Server::http("127.0.0.1:0").expect("bind loopback");
            let p = match server.server_addr() {
                tiny_http::ListenAddr::IP(std::net::SocketAddr::V4(v4)) => v4.port(),
                other => panic!("expected v4 loopback, got {other:?}"),
            };
            thread::spawn(move || {
                for req in server.incoming_requests() {
                    let body = match req.url() {
                        url if url.ends_with("/models") => OPENAI_MODELS_BODY,
                        url if url.contains("/organization/costs") => OPENAI_COSTS_BODY,
                        _ => "{}",
                    };
                    let _ = req.respond(tiny_http::Response::from_string(body));
                }
            });
            p
        };
        let base = format!("http://127.0.0.1:{port}");

        let outcome = openai_usage_with_base_url("sk-admin-test", &base);
        // Issue #1745: assertions are on the outcome variant, not the wire.
        let usage = outcome.into_usage("openai");
        assert!(usage.logged_in);
        assert!(usage.error.is_none());
        let balance = usage.balance.expect("admin key must populate balance");
        assert_eq!(balance.monthly_spend, Some(12.5));
        assert_eq!(balance.currency, "USD");
        assert!(usage.detail.is_none());
    }

    #[test]
    fn openai_usage_with_project_key_gracefully_degrades() {
        // Spec §3.2 / User Story #11: a `sk-proj-…` key passes the
        // inference check (200 on /v1/models) but 403s on
        // /v1/organization/costs. The result is `logged_in = true` with a
        // detail string explaining the gap — NOT a logged-out state, so
        // the user's agents keep running.
        let port = {
            use std::thread;
            let server = tiny_http::Server::http("127.0.0.1:0").expect("bind loopback");
            let p = match server.server_addr() {
                tiny_http::ListenAddr::IP(std::net::SocketAddr::V4(v4)) => v4.port(),
                other => panic!("expected v4 loopback, got {other:?}"),
            };
            thread::spawn(move || {
                for req in server.incoming_requests() {
                    match req.url() {
                        url if url.ends_with("/models") => {
                            let _ = req.respond(tiny_http::Response::from_string(OPENAI_MODELS_BODY));
                        }
                        url if url.contains("/organization/costs") => {
                            let _ = req.respond(
                                tiny_http::Response::from_string(r#"{"error":"insufficient permissions"}"#)
                                    .with_status_code(403),
                            );
                        }
                        _ => {
                            let _ = req.respond(
                                tiny_http::Response::from_string("{}").with_status_code(404),
                            );
                        }
                    }
                }
            });
            p
        };
        let base = format!("http://127.0.0.1:{port}");

        let outcome = openai_usage_with_base_url("sk-proj-test", &base);
        // Issue #1745: `sk-proj-` graceful degradation lands as
        // `UsageOutcome::Degraded`, which projects to `logged_in: true` with
        // `detail` set and no `error` — preserving the ADR-0026 §2 wire.
        match &outcome {
            UsageOutcome::Degraded { detail } => {
                assert!(
                    detail.contains("Organization Admin") && detail.contains("sk-admin"),
                    "detail must explain the org-admin requirement, got: {detail:?}"
                );
            }
            other => panic!("expected Degraded outcome, got: {other:?}"),
        }
        let usage = outcome.into_usage("openai");
        assert!(usage.logged_in, "project key on org costs must NOT log out");
        assert!(usage.error.is_none(), "degradation must not carry an error");
        assert!(usage.balance.is_none(), "no balance when costs 403");
    }

    #[test]
    fn openai_usage_with_invalid_key_returns_logged_out() {
        // Spec §3.2 / User Story #12: a revoked/invalid key 401s on the
        // inference check. The result is `logged_in = false` so the UI
        // surfaces the re-enter-key affordance. The two-round-trip probe
        // is what distinguishes this from the project-key degradation case.
        let port = {
            use std::thread;
            let server = tiny_http::Server::http("127.0.0.1:0").expect("bind loopback");
            let p = match server.server_addr() {
                tiny_http::ListenAddr::IP(std::net::SocketAddr::V4(v4)) => v4.port(),
                other => panic!("expected v4 loopback, got {other:?}"),
            };
            thread::spawn(move || {
                for req in server.incoming_requests() {
                    let _ = req.respond(
                        tiny_http::Response::from_string(r#"{"error":"invalid_api_key"}"#)
                            .with_status_code(401),
                    );
                }
            });
            p
        };
        let base = format!("http://127.0.0.1:{port}");

        let usage = openai_usage_with_base_url("sk-bad", &base).into_usage("openai");
        assert_eq!(usage.provider, "openai");
        assert!(!usage.logged_in);
        assert_eq!(usage.error.as_deref(), Some("Invalid API key"));
        assert!(usage.balance.is_none());
        assert!(usage.detail.is_none());
    }

    #[test]
    fn openai_usage_with_empty_key_returns_logged_out() {
        // Mirror `kimi_usage("")` / `openrouter_usage("")`: the
        // configured-key gate should catch a missing key, but the fetcher
        // still defends with a logged-out message so a misconfigured call
        // surfaces "no key" instead of a confusing 401.
        let usage = openai_usage_with_base_url("", "http://127.0.0.1:1").into_usage("openai");
        assert!(!usage.logged_in);
        assert_eq!(usage.provider, "openai");
        assert!(usage.error.as_deref().map(|e| e.contains("No API key")).unwrap_or(false));
        assert!(usage.balance.is_none());
    }

    #[test]
    fn openai_usage_with_429_on_inference_returns_unavailable() {
        // Rate-limit on the inference probe preserves `logged_in = true`
        // (we don't know the key is bad — could be transient).
        let port = {
            use std::thread;
            let server = tiny_http::Server::http("127.0.0.1:0").expect("bind loopback");
            let p = match server.server_addr() {
                tiny_http::ListenAddr::IP(std::net::SocketAddr::V4(v4)) => v4.port(),
                other => panic!("expected v4 loopback, got {other:?}"),
            };
            thread::spawn(move || {
                for req in server.incoming_requests() {
                    let _ = req.respond(tiny_http::Response::empty(429));
                }
            });
            p
        };
        let base = format!("http://127.0.0.1:{port}");

        let usage = openai_usage_with_base_url("sk-test", &base).into_usage("openai");
        assert!(usage.logged_in, "429 must not flip to logged_out");
        assert!(usage.error.as_deref().map(|e| e.contains("Rate limited")).unwrap_or(false));
    }
}
