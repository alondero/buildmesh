//! Provider usage fetching — piggybacks on CLI credentials stored by each provider.
//!
//! Endpoints are undocumented / reverse-engineered; treat non-200 responses or
//! shape mismatches as "usage unavailable", never as hard errors.

use reqwest::blocking::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
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
use crate::services::opencode_oauth::OPENCODE_CONSOLE_CRED_TARGET;

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "UsageWindow.ts")]
/// Generated to src/types/generated/UsageWindow.ts (issue #404). The wire
/// field names (`usedPercent` / `resetsAt`) are camelCase per
/// `#[serde(rename = "...")]` + matching `#[ts(rename = "...")]`.
pub struct UsageWindow {
    pub label: String,
    #[serde(rename = "usedPercent")]
    #[ts(rename = "usedPercent")]
    pub used_percent: Option<f64>,
    #[serde(rename = "resetsAt")]
    #[ts(rename = "resetsAt")]
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "BillingBalance.ts")]
/// Cash-balance view for a pay-as-you-go account (issue #537). The Accounts panel
/// renders this instead of percentage [`UsageWindow`] bars when an account's
/// `billing_mode` is `pay_as_you_go`. Field names are camelCase on the wire.
///
/// Generated to src/types/generated/BillingBalance.ts (issue #537).
pub struct BillingBalance {
    /// Credits / cash remaining, in `currency`.
    pub remaining: f64,
    /// Spend so far in the current billing month, if the provider reports it.
    #[serde(rename = "monthlySpend")]
    #[ts(rename = "monthlySpend")]
    pub monthly_spend: Option<f64>,
    /// ISO 4217 currency code (e.g. "USD", "CNY").
    pub currency: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "ProviderUsage.ts")]
/// Generated to src/types/generated/ProviderUsage.ts (issue #404). `loggedIn`
/// is camelCase on the wire per `#[ts(rename = "loggedIn")]`.
pub struct ProviderUsage {
    pub provider: String,
    #[serde(rename = "loggedIn")]
    #[ts(rename = "loggedIn")]
    pub logged_in: bool,
    pub windows: Vec<UsageWindow>,
    /// Cash balance for pay-as-you-go accounts; `None` for plan accounts, which
    /// report utilization via `windows` instead (issue #537).
    #[serde(default)]
    pub balance: Option<BillingBalance>,
    pub detail: Option<String>,
    pub error: Option<String>,
}

/// One **Model Provider**'s entry on the Providers page (issue #574): its
/// identity plus the **Usage Meters** it exposes on this host, if any.
///
/// The meters themselves reuse the [`ProviderUsage`] shape — its `windows`
/// (subscription quotas) and `balance` (pay-as-you-go wallet) *are* the meters,
/// and a provider may carry several at once. `usage` is `Some` only for a
/// provider Buildmesh has a fetcher for; `usage_tracked` is `false` for a
/// **Generic Model Provider** (no registry entry / no fetcher), which the UI
/// renders as an explicit "usage not tracked" state rather than an empty gauge.
///
/// Only providers relevant to the host appear in the list this wraps
/// (detection-gated): a native harness's subscription meter only when that
/// harness is installed, a keyed provider only when the user has enabled it.
///
/// Generated to src/types/generated/ProviderMeters.ts (issue #574).
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "ProviderMeters.ts")]
pub struct ProviderMeters {
    /// Provider account id ("anthropic", "minimax", a custom slug, …).
    pub provider: String,
    /// Whether Buildmesh ships a usage fetcher for this provider. `false` →
    /// the UI shows "usage not tracked" (camelCase on the wire).
    #[serde(rename = "usageTracked")]
    #[ts(rename = "usageTracked")]
    pub usage_tracked: bool,
    /// The fetched meters; `None` when usage isn't tracked.
    pub usage: Option<ProviderUsage>,
}

/// Failures that happen before we ever reach an endpoint: no credential on disk,
/// or a credential/response body that doesn't deserialize. Transport- and
/// status-level failures are handled inline in [`fetch_usage`].
#[derive(Debug)]
pub enum UsageError {
    NoCredential(String),
    Shape(String),
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UsageError::NoCredential(path) => write!(f, "No credential found at {}", path),
            UsageError::Shape(msg) => write!(f, "Unexpected response shape: {}", msg),
        }
    }
}

fn home_dir() -> PathBuf {
    env::var("USERPROFILE")
        .or_else(|_| env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Codex stores its auth under `$CODEX_HOME` (which points *at* the dir),
/// defaulting to `~/.codex`.
fn codex_home() -> PathBuf {
    env::var("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".codex"))
}

fn anthropic_cred_path() -> PathBuf {
    home_dir().join(".claude").join(".credentials.json")
}

fn codex_auth_path() -> PathBuf {
    codex_home().join("auth.json")
}

#[derive(Deserialize)]
struct OAuthCred {
    access_token: Option<String>,
}

#[derive(Deserialize)]
struct ClaudeAiOauth {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicOAuthCred {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<ClaudeAiOauth>,
}

/// Reads Anthropic's credentials JSON which nests the accessToken inside claudeAiOauth.
fn read_anthropic_token(path: PathBuf) -> Result<String, UsageError> {
    let content = fs::read_to_string(&path).map_err(|_| UsageError::NoCredential(path.clone().to_string_lossy().to_string()))?;
    let cred: AnthropicOAuthCred =
        serde_json::from_str(&content).map_err(|e| UsageError::Shape(e.to_string()))?;
    cred.claude_ai_oauth
        .and_then(|o| o.access_token)
        .ok_or(UsageError::NoCredential(path.to_string_lossy().to_string()))
}

/// Reads Codex's credentials JSON which has access_token at the top level.
fn read_codex_token(path: PathBuf) -> Result<String, UsageError> {
    let content = fs::read_to_string(&path).map_err(|_| UsageError::NoCredential(path.clone().to_string_lossy().to_string()))?;
    let cred: OAuthCred =
        serde_json::from_str(&content).map_err(|e| UsageError::Shape(e.to_string()))?;
    cred.access_token.ok_or(UsageError::NoCredential(path.to_string_lossy().to_string()))
}

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

fn logged_out(provider: &str, error: String) -> ProviderUsage {
    ProviderUsage {
        provider: provider.to_string(),
        logged_in: false,
        windows: vec![],
        balance: None,
        detail: None,
        error: Some(error),
    }
}

/// Builds a `ProviderUsage` for the "logged-in but couldn't fetch" state — the
/// credential is presumed present (so this is NOT the empty-key / no-credential
/// case [`logged_out`] handles), but the fetch failed for a transport, status,
/// or parse reason. Mirrors the `unavailable` closure that lives inside
/// [`fetch_usage`] so per-provider fetchers like [`kimi_usage`] can use the
/// same constructor shape without re-defining it.
fn unavailable(provider: &str, error: String) -> ProviderUsage {
    ProviderUsage {
        provider: provider.to_string(),
        logged_in: true,
        windows: vec![],
        balance: None,
        detail: None,
        error: Some(error),
    }
}

/// Drives the shared request → status-check → parse flow. Callers reach this
/// only once a credential is confirmed present, so any failure here is reported
/// as logged-in-but-unavailable. `parse` maps a 2xx body to `(windows, detail)`.
///
/// Returns a `ProviderUsage` plus the exact HTTP status code, so callers that
/// need to discriminate a specific status (currently just OpenCode for the
/// refresh-on-401 seam, issue #970) can do so without parsing the
/// `error: Some(...)` string. `status` is `None` when the request never reached
/// the server (client builder failure, transport error, TLS handshake fail).
fn fetch_usage_attempt(
    provider: &str,
    build_request: impl FnOnce(&Client) -> RequestBuilder,
    parse: impl FnOnce(&str) -> Result<(Vec<UsageWindow>, Option<String>), UsageError>,
) -> FetchAttempt {
    let client = match Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Err(e) => {
            return FetchAttempt {
                usage: unavailable(provider, format!("Client error: {}", e)),
                status: None,
            };
        }
        Ok(c) => c,
    };

    match build_request(&client).send() {
        Ok(r) if r.status() == 429 => FetchAttempt {
            usage: unavailable(
                provider,
                "Rate limited — usage data temporarily unavailable".to_string(),
            ),
            status: Some(429),
        },
        Ok(r) if !r.status().is_success() => {
            let code = r.status().as_u16();
            FetchAttempt {
                usage: unavailable(
                    provider,
                    format!("API error {}: {}", code, r.text().unwrap_or_default()),
                ),
                status: Some(code),
            }
        }
        Ok(r) => {
            let status = r.status().as_u16();
            match parse(&r.text().unwrap_or_default()) {
                Ok((windows, detail)) => FetchAttempt {
                    usage: ProviderUsage {
                        provider: provider.to_string(),
                        logged_in: true,
                        windows,
                        balance: None,
                        detail,
                        error: None,
                    },
                    status: Some(status),
                },
                Err(e) => FetchAttempt {
                    usage: unavailable(provider, format!("Failed to parse response: {}", e)),
                    status: Some(status),
                },
            }
        }
        Err(e) => FetchAttempt {
            usage: unavailable(provider, format!("Request failed: {}", e)),
            status: None,
        },
    }
}

/// Drives the shared request → status-check → parse flow. Callers reach this
/// only once a credential is confirmed present, so any failure here is reported
/// as logged-in-but-unavailable. `parse` maps a 2xx body to `(windows, detail)`.
fn fetch_usage(
    provider: &str,
    build_request: impl FnOnce(&Client) -> RequestBuilder,
    parse: impl FnOnce(&str) -> Result<(Vec<UsageWindow>, Option<String>), UsageError>,
) -> ProviderUsage {
    fetch_usage_attempt(provider, build_request, parse).usage
}

/// Status-aware fetch result. Carries the exact HTTP status alongside the
/// [`ProviderUsage`] envelope so the OpenCode refresh-on-401 seam (#970) can
/// branch on `status == Some(401)` without parsing the rendered error string.
/// `status` is `None` for transport / client-builder failures.
struct FetchAttempt {
    usage: ProviderUsage,
    status: Option<u16>,
}

fn parse_anthropic_response(body: &str) -> Result<Vec<UsageWindow>, UsageError> {
    #[derive(Deserialize, Debug)]
    struct UsageBucket {
        utilization: Option<f64>,
        #[serde(rename = "resets_at")]
        resets_at: Option<String>,
    }
    #[derive(Deserialize, Debug)]
    struct Resp {
        #[serde(default)]
        five_hour: Option<UsageBucket>,
        #[serde(default)]
        seven_day: Option<UsageBucket>,
        #[serde(default)]
        seven_day_sonnet: Option<UsageBucket>,
    }

    let resp: Resp = serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;

    let mut windows = Vec::new();
    if let Some(bucket) = resp.five_hour {
        if let Some(util) = bucket.utilization {
            windows.push(UsageWindow {
                label: "5-hour".to_string(),
                used_percent: Some(util),
                resets_at: bucket.resets_at,
            });
        }
    }
    if let Some(bucket) = resp.seven_day {
        if let Some(util) = bucket.utilization {
            windows.push(UsageWindow {
                label: "7-day".to_string(),
                used_percent: Some(util),
                resets_at: bucket.resets_at,
            });
        }
    }
    if let Some(bucket) = resp.seven_day_sonnet {
        if let Some(util) = bucket.utilization {
            windows.push(UsageWindow {
                label: "7-day Sonnet".to_string(),
                used_percent: Some(util),
                resets_at: bucket.resets_at,
            });
        }
    }
    Ok(windows)
}

pub fn anthropic_usage() -> ProviderUsage {
    let token = match read_anthropic_token(anthropic_cred_path()) {
        Ok(t) => t,
        Err(e) => return logged_out("anthropic", e.to_string()),
    };
    fetch_usage(
        "anthropic",
        |c| {
            c.get("https://api.anthropic.com/api/oauth/usage")
                .header("Authorization", format!("Bearer {}", token))
                .header("anthropic-beta", "oauth-2025-04-20")
        },
        |body| Ok((parse_anthropic_response(body)?, None)),
    )
}

fn parse_codex_response(body: &str) -> Result<Vec<UsageWindow>, UsageError> {
    #[derive(Deserialize)]
    struct Resp {
        #[serde(default)]
        primary: Option<CodexWindow>,
        #[serde(default)]
        secondary: Option<CodexWindow>,
    }
    #[derive(Deserialize)]
    struct CodexWindow {
        #[serde(rename = "usedPercent")]
        used_percent: Option<f64>,
        #[serde(rename = "resetsAt")]
        resets_at: Option<String>,
        #[serde(default)]
        label: Option<String>,
    }

    let resp: Resp = serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;

    let mut windows = vec![];
    if let Some(p) = resp.primary {
        windows.push(UsageWindow {
            label: p.label.unwrap_or_else(|| "5-hour".to_string()),
            used_percent: p.used_percent,
            resets_at: p.resets_at,
        });
    }
    if let Some(s) = resp.secondary {
        windows.push(UsageWindow {
            label: s.label.unwrap_or_else(|| "Weekly".to_string()),
            used_percent: s.used_percent,
            resets_at: s.resets_at,
        });
    }
    Ok(windows)
}

pub fn codex_usage() -> ProviderUsage {
    let token = match read_codex_token(codex_auth_path()) {
        Ok(t) => t,
        Err(e) => return logged_out("codex", e.to_string()),
    };
    fetch_usage(
        "codex",
        |c| {
            c.get("https://chatgpt.com/backend-api/wham/usage")
                .header("Authorization", format!("Bearer {}", token))
        },
        |body| Ok((parse_codex_response(body)?, None)),
    )
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

pub fn minimax_usage(api_key: &str) -> ProviderUsage {
    if api_key.is_empty() {
        return logged_out("minimax", "No API key configured".to_string());
    }
    let auth = format!("Bearer {}", api_key);
    fetch_usage(
        "minimax",
        |c| {
            c.get("https://api.minimax.io/v1/token_plan/remains")
                .header("Authorization", auth)
        },
        parse_minimax_response,
    )
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

pub fn kimi_usage(api_key: &str) -> ProviderUsage {
    if api_key.is_empty() {
        return logged_out("kimi", "No API key configured".to_string());
    }

    let client = match Client::builder().build() {
        Ok(c) => c,
        Err(e) => return unavailable("kimi", format!("Client error: {}", e)),
    };

    let auth = format!("Bearer {}", api_key);
    let resp = match client
        .get("https://api.moonshot.ai/v1/users/me/balance")
        .header("Authorization", auth)
        .send()
    {
        Ok(r) if r.status() == 429 => {
            return unavailable(
                "kimi",
                "Rate limited — usage data temporarily unavailable".to_string(),
            )
        }
        Ok(r) if !r.status().is_success() => {
            let code = r.status().as_u16();
            // 401 (Unauthorized) AND 403 (Forbidden / account revoked) both signal
            // "the credential is bad" — surface as logged-out so the UI can prompt
            // the user to re-enter, matching the `logged_in=false` semantics in
            // `logged_out()`. Moonshot returns 403 for disabled accounts per their
            // API contract; missing the branch silently degrades to a generic
            // 'API error 403' with no re-enter affordance.
            if code == 401 || code == 403 {
                return logged_out("kimi", "Invalid API key".to_string());
            }
            return unavailable(
                "kimi",
                format!("API error {}: {}", code, r.text().unwrap_or_default()),
            );
        }
        Ok(r) => r,
        Err(e) => return unavailable("kimi", format!("Request failed: {}", e)),
    };

    let body = resp.text().unwrap_or_default();
    match parse_kimi_response(&body) {
        Ok(balance) => ProviderUsage {
            provider: "kimi".to_string(),
            logged_in: true,
            windows: Vec::new(),
            balance: Some(balance),
            detail: None,
            error: None,
        },
        Err(e) => unavailable("kimi", format!("Failed to parse response: {}", e)),
    }
}

// ─── OpenRouter ──────────────────────────────────────────────────────────────
//
// OpenRouter exposes a simple "Anthropic Skin" for Claude Code (set
// `ANTHROPIC_BASE_URL=https://openrouter.ai/api` + `ANTHROPIC_AUTH_TOKEN=$key`)
// and a separate `GET /api/v1/credits` Bearer-authenticated endpoint that
// reports `total_credits` (remaining wallet balance) and `total_usage` (lifetime
// spend). Mirrors the `kimi_usage` shape — keyed fetcher, balance-style response,
// no Anthropic-side windows to harvest — so the `ProviderUsage.balance` field is
// the canonical surface.

/// Response envelope from `GET https://openrouter.ai/api/v1/credits`.
/// `total_usage` is also returned by OpenRouter but we intentionally drop it
/// — it's a lifetime-cumulative figure, not a current-month spend, and the
/// `BalanceCard` would label any number we put in `monthly_spend` as
/// "Spent this month" (misleading). Serde ignores unknown fields by default,
/// so the JSON's `total_usage` key is harmlessly discarded.
#[derive(Deserialize, Debug)]
struct OpenRouterResp {
    data: OpenRouterData,
}

#[derive(Deserialize, Debug)]
struct OpenRouterData {
    total_credits: f64,
}

/// Parses the OpenRouter `/api/v1/credits` body into a `BillingBalance`. The
/// endpoint contract is simple: `data.total_credits` is the remaining wallet
/// balance in USD. The endpoint ALSO returns `data.total_usage`, but it's
/// **lifetime cumulative spend** (not a current-month figure) — labelling
/// that as "Spent this month" in the Usage tab would overstate the user's
/// current-month spend by an order of magnitude. Until OpenRouter exposes a
/// billing-period filter, we leave `monthly_spend = None` and let the
/// "remaining" figure carry the visible signal. A missing required field is
/// a hard parse error rather than a silent zero — OpenRouter might evolve
/// the response and we want the failure to be obvious.
fn parse_openrouter_response(body: &str) -> Result<BillingBalance, UsageError> {
    let resp: OpenRouterResp =
        serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;
    Ok(BillingBalance {
        remaining: resp.data.total_credits,
        monthly_spend: None,
        // OpenRouter bills in USD per the platform's published pricing; the
        // endpoint does not return a currency field.
        currency: "USD".to_string(),
    })
}

pub fn openrouter_usage(api_key: &str) -> ProviderUsage {
    if api_key.is_empty() {
        return logged_out("openrouter", "No API key configured".to_string());
    }

    let client = match Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => return unavailable("openrouter", format!("Client error: {}", e)),
    };

    let auth = format!("Bearer {}", api_key);
    let resp = match client
        .get("https://openrouter.ai/api/v1/credits")
        .header("Authorization", auth)
        .send()
    {
        Ok(r) if !r.status().is_success() => {
            let code = r.status().as_u16();
            // 401/403 → logged-out (bad / revoked key) so the UI prompts for
            // re-entry; 429 → rate-limited "unavailable"; everything else →
            // generic unavailable with the status for debug.
            if code == 401 || code == 403 {
                return logged_out("openrouter", "Invalid API key".to_string());
            }
            if code == 429 {
                return unavailable(
                    "openrouter",
                    "Rate limited — usage data temporarily unavailable".to_string(),
                );
            }
            // Body-read failure here is a transport issue, NOT a malformed
            // payload — surface as "Request failed" rather than collapsing
            // to an empty body that downstream parsing treats as a Shape error.
            let body = match r.text() {
                Ok(b) => b,
                Err(e) => {
                    return unavailable(
                        "openrouter",
                        format!("API error {}: failed to read error body: {}", code, e),
                    )
                }
            };
            return unavailable(
                "openrouter",
                format!("API error {}: {}", code, body),
            );
        }
        Ok(r) => r,
        Err(e) => return unavailable("openrouter", format!("Request failed: {}", e)),
    };

    let body = match resp.text() {
        Ok(b) => b,
        Err(e) => return unavailable("openrouter", format!("Failed to read response body: {}", e)),
    };
    match parse_openrouter_response(&body) {
        Ok(balance) => ProviderUsage {
            provider: "openrouter".to_string(),
            logged_in: true,
            windows: Vec::new(),
            balance: Some(balance),
            detail: None,
            error: None,
        },
        Err(e) => unavailable("openrouter", format!("Failed to parse response: {}", e)),
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

fn parse_grok_response(body: &str) -> Result<ProviderUsage, UsageError> {
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
        if let Some(ref cap) = config.on_demand_cap {
            let used_percent = if cap.val > 0.0 {
                if let Some(ref used) = config.on_demand_used {
                    Some((used.val / cap.val) * 100.0)
                } else {
                    Some(0.0)
                }
            } else {
                Some(0.0)
            };

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
                used_percent,
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

    Ok(ProviderUsage {
        provider: "grok".to_string(),
        logged_in: true,
        windows,
        balance,
        detail: None,
        error: None,
    })
}

pub fn grok_usage() -> ProviderUsage {
    let (token, user_id) = match read_grok_token(grok_auth_path()) {
        Ok(t) => t,
        Err(e) => return logged_out("grok", e.to_string()),
    };

    let client = match Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => return unavailable("grok", format!("Client error: {}", e)),
    };

    let base_url = env::var("GROK_CLI_CHAT_PROXY_BASE_URL")
        .unwrap_or_else(|_| "https://cli-chat-proxy.grok.com".to_string());
    let url = format!("{}/v1/billing?format=credits", base_url);

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
                return logged_out("grok", "Invalid API key".to_string());
            }
            if code == 429 {
                return unavailable(
                    "grok",
                    "Rate limited — usage data temporarily unavailable".to_string(),
                );
            }
            let body = r.text().unwrap_or_default();
            return unavailable("grok", format!("API error {}: {}", code, body));
        }
        Ok(r) => r,
        Err(e) => return unavailable("grok", format!("Request failed: {}", e)),
    };

    let body = match resp.text() {
        Ok(b) => b,
        Err(e) => return unavailable("grok", format!("Failed to read response body: {}", e)),
    };

    match parse_grok_response(&body) {
        Ok(usage) => usage,
        Err(e) => unavailable("grok", format!("Failed to parse response: {}", e)),
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
            used_percent: Some(pct_5h.min(100.0).max(0.0)),
            resets_at: None,
        },
        UsageWindow {
            label: "Weekly".to_string(),
            used_percent: Some(pct_weekly.min(100.0).max(0.0)),
            resets_at: None,
        },
        UsageWindow {
            label: "Monthly".to_string(),
            used_percent: Some(pct_monthly.min(100.0).max(0.0)),
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

/// Pure assembly: combines a live fetch result with the SQLite fallback to
/// produce the final [`ProviderUsage`]. The live path wins when it returns
/// a usable envelope (no `error`); any failure — `None` = no credential,
/// or `Some` carrying an error — falls through to the SQLite result so a
/// user mid-OAuth always sees SOMETHING (issue #957 sub-spec point 4).
fn choose_opencode_usage(
    live: Option<&ProviderUsage>,
    sqlite: ProviderUsage,
) -> ProviderUsage {
    if let Some(usage) = live {
        if usage.error.is_none() {
            return usage.clone();
        }
    }
    sqlite
}

/// Production billing URL for the OpenCode live usage probe (issue #957).
/// `https://opencode.ai/_server` is a SolidStart server-function RPC; the
/// `X-Server-Id` header value is the deployment id pinned at
/// [`OPENCODE_SERVER_ID`] below. Surfaced as a module constant so the
/// parameterized test seam ([`opencode_usage_impl_with_overrides`]) can
/// substitute a wiremock URL while production callers keep the literal.
const OPENCODE_BILLING_URL: &str = "https://opencode.ai/_server";

/// In-memory tracker for credential refresh age. Keyed by credential target
/// so the seam can distinguish "we just refreshed `opencode:console`" from
/// "we just refreshed `buildmesh-test-…-uuid`". Process-local — the age is
/// lost on restart, which is acceptable because a restart is itself a
/// "fresh fetch" opportunity and the 401 retry path still repairs stale
/// credentials.
///
/// **First-seen semantics:** when the seam observes a target for the first
/// time, [`refresh_age`] inserts `Instant::now()` and returns `Duration::ZERO`
/// so a freshly-seeded (i.e. just-persisted-by-the-OAuth-dance) credential
/// is treated as fresh and skips the gate. The same applies on app restart
/// — pre-existing credentials in Credential Manager look "fresh" again, but
/// the live probe's 401 still triggers the refresh-on-the-spot branch.
///
/// **Forget on deletion:** [`forget_refresh_tracking`] drops the entry so a
/// target whose credential is removed (Settings "Sign out", test cleanup)
/// doesn't leave a stale `Instant` that would mask a future `NoCredential`.
static REFRESH_TRACKER: once_cell::sync::Lazy<Arc<Mutex<HashMap<String, Instant>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

/// Returns how long since the seam last successfully refreshed or first
/// observed `target`. `Duration::ZERO` on first observation so a fresh
/// credential skips the gate without a forced refresh on every cold start.
fn refresh_age(target: &str) -> Duration {
    let mut guard = REFRESH_TRACKER.lock().unwrap();
    guard
        .entry(target.to_string())
        .or_insert_with(Instant::now)
        .elapsed()
}

/// Records that the seam just successfully refreshed `target`. Resets the
/// age so the next gate evaluation skips the refresh branch until another
/// `REFRESH_TTL` elapses.
fn mark_refresh_success(target: &str) {
    let mut guard = REFRESH_TRACKER.lock().unwrap();
    guard.insert(target.to_string(), Instant::now());
}

/// Drops `target` from the tracker. Called when Credential Manager reports
/// `NoCredential` for `target` so a removed credential doesn't leave a
/// phantom "freshly refreshed" timestamp that masks the next
/// `NoCredential` read. Tests also call this in their cleanup guard.
#[cfg_attr(not(test), allow(dead_code))] // exercised by tests; production revoke path doesn't yet wire through
fn forget_refresh_tracking(target: &str) {
    let mut guard = REFRESH_TRACKER.lock().unwrap();
    guard.remove(target);
}

/// OpenCode Console live probe with refresh-on-401 (issues #957 + #970).
///
/// `cred_target` is the Windows Credential Manager target — production
/// passes `OPENCODE_CONSOLE_CRED_TARGET` ("opencode:console"), integration
/// tests pass a UUID-suffixed target so cleanup is isolated. `billing_url`
/// is the SolidStart endpoint — production passes [`OPENCODE_BILLING_URL`],
/// tests point at a wiremock on `127.0.0.1:<port>`. `refresh_url` is the
/// OAuth refresh endpoint — production passes `OPENCODE_CONSOLE_TOKEN_URL`
/// from `services::opencode_oauth`, tests pass a wiremock.
///
/// **Algorithm** (matches issue #970's body and the four test scenarios
/// pinned at issue #971):
///
/// 1. Calculate SQLite fallback first so any failure path can short-circuit
///    to it without re-reading the file. The `auth.json` + `opencode.db`
///    shape is unchanged from #953 / #957.
/// 2. Read the FULL credential at `cred_target`. Failure → live = `None`,
///    fall through to SQLite.
/// 3. Gate: `is_expired(cred) || refresh_age(cred_target) > REFRESH_TTL`.
///    If gated, attempt exactly one refresh via
///    `try_refresh_with_target_and_url`. On success: `mark_refresh_success`
///    + `invalidate_cache()` (#970 step 5). On failure: warn and fall
///    through to SQLite WITHOUT retrying the live probe (a stale token is
///    worse than SQLite numbers — see issue #970's refresh-failure branch).
/// 4. Use the (possibly freshly-refreshed) credential for the live probe.
///    `fetch_usage_attempt` returns the exact HTTP status so we can branch
///    on `401` without parsing the rendered error string.
/// 5. **On-the-spot refresh** (scenario 4): if the live probe returns 401
///    AND no refresh was attempted at the gate, attempt one refresh and
///    retry the live probe exactly once. A second 401 or any other failure
///    falls through to SQLite — at most one refresh per call so we never
///    loop.
/// 6. Assemble via [`choose_opencode_usage`]: live wins if it has no error,
///    otherwise SQLite.
fn opencode_usage_impl_with_overrides(
    home: &std::path::Path,
    cred_target: &str,
    billing_url: &str,
    refresh_url: &str,
) -> ProviderUsage {
    // ── SQLite fallback (#953) ──
    let opencode_dir = home.join(".local").join("share").join("opencode");
    let auth_path = opencode_dir.join("auth.json");
    let db_path = opencode_dir.join("opencode.db");

    // `auth.json` is the gate the offline path checks (issue #957 sub-spec
    // point 4): a user mid-OAuth-flow with a valid `auth.json` still gets
    // SQLite numbers when the live path fails. Without it, return
    // `logged_out` — no offline data, no credential, nothing to show.
    if let Err(e) = read_opencode_token(auth_path) {
        return logged_out("opencode", e.to_string());
    }
    let sqlite = match calculate_opencode_windows(&db_path) {
        Ok(windows) => ProviderUsage {
            provider: "opencode".to_string(),
            logged_in: true,
            windows,
            balance: None,
            detail: None,
            error: None,
        },
        Err(e) => unavailable("opencode", format!("Failed to query opencode.db: {}", e)),
    };

    // ── Read full credential (refresh needs refresh_token + expires_at) ──
    let full_cred = match read_opencode_console_full_credential(cred_target) {
        Ok(c) => c,
        Err(_) => {
            // No credential at this target (deleted, never signed in, or
            // non-Windows). Drop the tracker entry so a future successful
            // refresh on a different target can't be masked, then fall
            // through to SQLite.
            forget_refresh_tracking(cred_target);
            return choose_opencode_usage(None, sqlite);
        }
    };

    // ── Gate-driven refresh (#970 step 2) ──
    let now_unix = chrono::Utc::now().timestamp();
    let needs_refresh = crate::services::opencode_oauth::cred_is_expired(&full_cred, now_unix)
        || refresh_age(cred_target) > crate::services::opencode_oauth::REFRESH_TTL;

    // After a successful refresh, the bearer/workspace come from the
    // returned `TokenResponse` (the same blob that was just persisted).
    // `refreshed` flags whether the refresh branch fired so scenario 4's
    // on-the-spot refresh can be skipped if a refresh already happened
    // this call (no loops on a 401-with-stale-token scenario).
    let mut refresh_attempted = false;
    let (token, workspace_id) = if needs_refresh {
        refresh_attempted = true;
        match crate::services::opencode_oauth::try_refresh_with_target_and_url(
            cred_target, refresh_url,
        ) {
            Ok(t) => {
                mark_refresh_success(cred_target);
                invalidate_cache(); // #970 step 5: next read sees fresh numbers
                (t.access_token, t.workspace_id)
            }
            Err(e) => {
                // Refresh failed — fall through to SQLite. DO NOT retry the
                // live probe with the now-stale credential: an expired
                // token that successfully retries the billing endpoint
                // returns 401 again, which would loop forever. SQLite
                // numbers are the safe degradation.
                tracing::warn!(
                    target: "buildmesh.usage",
                    "opencode refresh failed for {cred_target}: {e}"
                );
                return choose_opencode_usage(None, sqlite);
            }
        }
    } else {
        // No refresh needed — use the credential we already read.
        match (
            full_cred.access_token.as_deref(),
            full_cred.workspace_id.as_deref(),
        ) {
            (Some(t), Some(w)) if !t.is_empty() && !w.is_empty() => {
                (t.to_string(), w.to_string())
            }
            _ => return choose_opencode_usage(None, sqlite),
        }
    };

    // ── Live probe ──
    let attempt = fetch_usage_attempt(
        "opencode",
        |c| {
            c.post(billing_url)
                .header("X-Server-Id", OPENCODE_SERVER_ID)
                .header("Authorization", format!("Bearer {token}"))
                .json(&[&workspace_id])
        },
        parse_opencode_billing_response,
    );

    // ── On-the-spot refresh on 401 (scenario 4) ──
    if attempt.status == Some(401) && !refresh_attempted {
        match crate::services::opencode_oauth::try_refresh_with_target_and_url(
            cred_target, refresh_url,
        ) {
            Ok(t) => {
                mark_refresh_success(cred_target);
                invalidate_cache();
                let retry = fetch_usage_attempt(
                    "opencode",
                    |c| {
                        c.post(billing_url)
                            .header("X-Server-Id", OPENCODE_SERVER_ID)
                            .header("Authorization", format!("Bearer {}", t.access_token))
                            .json(&[&t.workspace_id])
                    },
                    parse_opencode_billing_response,
                );
                return choose_opencode_usage(Some(&retry.usage), sqlite);
            }
            Err(e) => {
                tracing::warn!(
                    target: "buildmesh.usage",
                    "opencode on-the-spot refresh failed for {cred_target}: {e}"
                );
                // Fall through to choose_opencode_usage with the failed live
                // attempt — SQLite wins because live.usage.error.is_some().
            }
        }
    }

    choose_opencode_usage(Some(&attempt.usage), sqlite)
}

fn opencode_usage_impl(home: &std::path::Path) -> ProviderUsage {
    opencode_usage_impl_with_overrides(
        home,
        OPENCODE_CONSOLE_CRED_TARGET,
        OPENCODE_BILLING_URL,
        crate::services::opencode_oauth::OPENCODE_CONSOLE_TOKEN_URL,
    )
}

pub fn opencode_usage() -> ProviderUsage {
    opencode_usage_impl(&home_dir())
}

// ─── Google / Antigravity (`agy`) ───────────────────────────────────────────
//
// The Antigravity CLI surfaces a per-model quota that is a DIFFERENT product
// from Gemini Code Assist: it lives on the `daily-cloudcode-pa` staging host,
// behind `fetchAvailableModels`, and is gated purely by the client User-Agent.
// Auth is separate from `~/.gemini/oauth_creds.json` — the token lives in the OS
// credential store under `gemini:antigravity` (written by the agy CLI itself).
//
// This path is deliberately best-effort and FRAGILE (staging host, User-Agent
// gate, no token refresh since the Antigravity OAuth client isn't recoverable).
// Per the module contract, any failure degrades to "unavailable", never errors.

const AGY_HOST: &str = "https://daily-cloudcode-pa.googleapis.com";
/// The Antigravity CLI identifies with this User-Agent and the Cloud Code private
/// API allowlists it. Load-bearing: without it the API returns 403 PERMISSION_DENIED.
const AGY_USER_AGENT: &str = "antigravity/cli/1.0.3 windows/amd64";
/// Credential Manager target the agy CLI stores its OAuth token under.
const AGY_CRED_TARGET: &str = "gemini:antigravity";

// ─── OpenCode Go (live `_server billing.get` probe) ────────────────────────
//
// OpenCode Go ships a SolidStart server-function RPC at
// `POST https://opencode.ai/_server` (function name `billing.get`) that returns
// the user's server-authoritative 5-hour / weekly / monthly usage windows
// (issue #957). The probe falls through to the offline SQLite path (#953) on
// any failure so a user mid-OAuth-flow keeps seeing SOMETHING instead of a
// silent blank gauge. The credential blob lives at this target, written by
// #956's Buildmesh-owned device-flow dance.

/// SolidStart deployment id — the value of the `X-Server-Id` header the
/// OpenCode CLI sends. Captured from the opencode-cli binary's outbound
/// traffic (issue #944 / research ticket). Stable per deployment; not per-user.
///
/// On its way out: the OpenCode Console credential blob (issue #956) is
/// already the canonical home for this id so the OAuth dance can persist
/// the value it received from the device-flow exchange rather than
/// re-emitting it from this binary string. The constant stays until the
/// server-id migration lands; see `services::opencode_oauth` for the
/// candidate replacement.
const OPENCODE_SERVER_ID: &str =
    "c83b78a614689c38ebee981f9b39a8b377716db85c1fd7dbab604adc02d3313d";

#[derive(Deserialize)]
struct AgyTokenField {
    access_token: Option<String>,
}

#[derive(Deserialize)]
struct AgyCred {
    token: Option<AgyTokenField>,
}

/// Parses the agy credential blob (`{ "token": { "access_token": … }, … }`).
fn parse_agy_token(blob: &[u8]) -> Result<String, UsageError> {
    let text = std::str::from_utf8(blob).map_err(|e| UsageError::Shape(e.to_string()))?;
    let cred: AgyCred = serde_json::from_str(text).map_err(|e| UsageError::Shape(e.to_string()))?;
    cred.token
        .and_then(|t| t.access_token)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| UsageError::NoCredential(AGY_CRED_TARGET.to_string()))
}

/// Reads the agy OAuth access token from the OS credential store. Windows-only
/// for now (the agy CLI keyrings differ per platform); elsewhere the provider
/// simply reports logged-out.
#[cfg(windows)]
fn read_agy_token() -> Result<String, UsageError> {
    parse_agy_token(&windows_cred::read(AGY_CRED_TARGET)?)
}

#[cfg(not(windows))]
fn read_agy_token() -> Result<String, UsageError> {
    Err(UsageError::NoCredential(
        "Antigravity usage is only available on Windows".to_string(),
    ))
}

/// Reads the FULL Buildmesh-owned OpenCode Console credential DTO from the
/// Windows Credential Manager at `cred_target`. The refresh seam (#970)
/// needs `expires_at` and `refresh_token` in addition to `access_token` and
/// `workspace_id`, so this returns the parsed struct rather than the
/// `(token, workspace)` tuple [`read_opencode_console_credential`] extracts.
/// `cred_target` is parameterized so integration tests can read from a
/// UUID-suffixed target instead of clobbering the production
/// `opencode:console` slot. On non-Windows returns `NoCredential` to mirror
/// the existing decision (issue #956: non-Windows = agy-style NoCredential).
#[cfg(windows)]
fn read_opencode_console_full_credential(
    cred_target: &str,
) -> Result<crate::services::opencode_oauth::OpenCodeConsoleCred, UsageError> {
    crate::services::opencode_oauth::parse_opencode_console_full_credential(
        &windows_cred::read(cred_target)?,
    )
}

#[cfg(not(windows))]
fn read_opencode_console_full_credential(
    _cred_target: &str,
) -> Result<crate::services::opencode_oauth::OpenCodeConsoleCred, UsageError> {
    Err(UsageError::NoCredential(
        "OpenCode Console usage is only available on Windows".to_string(),
    ))
}

/// Minimal FFI to the Windows Credential Manager (`advapi32!CredReadW` /
///
/// `CredWriteW` / `CredDeleteW`) was extracted out of this module for #956
/// and now lives at [`crate::services::windows_cred`]; the local
/// `cfg(windows)` `use` at the top of `usage` keeps the call sites
/// (`read_agy_token`, `read_opencode_console_credential`) reading naturally.
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

pub fn agy_usage() -> ProviderUsage {
    let token = match read_agy_token() {
        Ok(t) => t,
        Err(e) => return logged_out("agy", e.to_string()),
    };
    let client = match Client::builder().build() {
        Ok(c) => c,
        Err(e) => return logged_out("agy", format!("Client error: {e}")),
    };
    // fetchAvailableModels needs the user's cloudaicompanion project, which
    // loadCodeAssist hands back.
    let project = match agy_load_project(&client, &token) {
        Ok(p) => p,
        Err(e) => return logged_out("agy", e.to_string()),
    };

    fetch_usage(
        "agy",
        |c| {
            c.post(format!("{AGY_HOST}/v1internal:fetchAvailableModels"))
                .header("Authorization", format!("Bearer {token}"))
                .header("User-Agent", AGY_USER_AGENT)
                .header("Content-Type", "application/json")
                .json(&serde_json::json!({ "project": project }))
        },
        parse_agy_models,
    )
}

const CACHE_TTL: Duration = Duration::from_secs(300);

type Cache = HashMap<String, (Instant, ProviderUsage)>;

static USAGE_CACHE: once_cell::sync::Lazy<Arc<Mutex<Cache>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

pub fn get_cached_usage(provider: &str) -> Option<ProviderUsage> {
    let guard = USAGE_CACHE.lock().unwrap();
    guard.get(provider).and_then(|(instant, usage)| {
        if instant.elapsed() < CACHE_TTL {
            Some(usage.clone())
        } else {
            None
        }
    })
}

// ── Cache-age wire gap (issue #857 follow-up — deferred) ─────────────────
//
// The UI's "Refreshed X ago" indicator is currently stamped on the React side
// at the moment `loadMeters` resolves, NOT at the moment each provider's vendor
// endpoint returned. Because [`get_cached_usage`] may short-circuit before any
// HTTP round-trip, the indicator mislabels a pure cache hit as a fresh fetch.
//
// The clean fix is a wire-shape change, deliberately deferred to its own PR
// (issue #857 body flags the cross-cutting consequences — Rust struct +
// ts-rs regen + new React-side cache-vs-fresh semantics — as warranting a
// separate commit). When picked up:
//
//   1. Add `cached_at: Option<i64>` (epoch ms) to [`ProviderMeters`] with
//      `#[ts(rename = "cachedAt")]`. `None` means "freshly fetched on this
//      call"; `Some(_)` means "served from the in-process cache at that instant".
//   2. Change this function's signature to also expose the cache instant, e.g.
//      `Option<(ProviderUsage, Instant)>`, so callers can stamp `cached_at`.
//   3. Have [`commands::usage::cached_or_fetch`] (commands/usage.rs:205) thread
//      the Optional instant through to `assemble_meters`, which sets
//      `cached_at` per row in the returned [`ProviderMeters`].
//   4. Run `cargo test` to regenerate `src/types/generated/ProviderMeters.ts`
//      (the project's ts-rs gate; CLAUDE.md hard rule on wire-type drift).
//   5. The React side (`src/components/Probe/UsageTab.tsx`) then picks the
//      display timestamp: if every row carries `cachedAt`, the oldest one
//      drives a "Cached Xs ago" label; otherwise `Date.now()` keeps the
//      existing "Refreshed Xs ago" semantics for the fresh-row case.

pub fn set_cached_usage(provider: &str, usage: ProviderUsage) {
    let mut guard = USAGE_CACHE.lock().unwrap();
    guard.insert(provider.to_string(), (Instant::now(), usage));
}

pub fn invalidate_cache() {
    let mut guard = USAGE_CACHE.lock().unwrap();
    guard.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    // `super::*` re-exports items from `services::usage`, but stdlib
    // collections like `VecDeque` and `Path` are NOT pulled in by the
    // `use super::*` glob. The refresh-on-401 scenarios need them.
    use std::collections::VecDeque;
    use std::path::Path;

    #[test]
    fn parse_anthropic_response_valid() {
        let json = r#"{"five_hour":{"utilization":41.0,"resets_at":"2026-05-30T21:30:00.379395+00:00"},"seven_day":{"utilization":33.0,"resets_at":"2026-06-05T04:00:00.379418+00:00"}}"#;
        let windows = parse_anthropic_response(json).unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].label, "5-hour");
        assert_eq!(windows[0].used_percent, Some(41.0));
        assert_eq!(windows[1].label, "7-day");
        assert_eq!(windows[1].used_percent, Some(33.0));
    }

    #[test]
    fn parse_anthropic_response_malformed() {
        let json = r#"{"not_usage": []}"#;
        let windows = parse_anthropic_response(json).unwrap();
        assert!(windows.is_empty());
    }

    #[test]
    fn parse_codex_response_valid() {
        let json = r#"{"primary":{"usedPercent":30.0,"resetsAt":"2026-05-30T12:00:00Z","label":"5-hour"},"secondary":{"usedPercent":10.0,"resetsAt":"2026-06-01T00:00:00Z","label":"Weekly"}}"#;
        let windows = parse_codex_response(json).unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].label, "5-hour");
        assert_eq!(windows[1].label, "Weekly");
    }

    #[test]
    fn parse_codex_response_minimal() {
        let json = r#"{"primary":{"usedPercent":50.0}}"#;
        let windows = parse_codex_response(json).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "5-hour");
    }

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
        let usage = minimax_usage("");
        assert!(!usage.logged_in);
        assert!(usage.error.is_some());
    }

    #[test]
    fn test_read_anthropic_token_valid() {
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join("test_anthropic_cred.json");
        let content = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-testtoken123","refreshToken":"sk-ant-ort01-ref123","expiresAt":123456789,"scopes":[],"subscriptionType":"pro","rateLimitTier":"default"}}"#;
        std::fs::write(&file_path, content).unwrap();

        let token = read_anthropic_token(file_path.clone()).unwrap();
        assert_eq!(token, "sk-ant-oat01-testtoken123");

        std::fs::remove_file(file_path).unwrap();
    }

    #[test]
    fn test_read_codex_token_valid() {
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join("test_codex_cred.json");
        let content = r#"{"access_token":"test-codex-access-token-456"}"#;
        std::fs::write(&file_path, content).unwrap();

        let token = read_codex_token(file_path.clone()).unwrap();
        assert_eq!(token, "test-codex-access-token-456");

        std::fs::remove_file(file_path).unwrap();
    }

    #[test]
    fn parse_agy_token_extracts_nested_access_token() {
        let blob = br#"{"token":{"access_token":"ya29.agytok","token_type":"Bearer","refresh_token":"1//ref","expiry":"2026-05-31T12:00:00Z"},"auth_method":"consumer"}"#;
        assert_eq!(parse_agy_token(blob).unwrap(), "ya29.agytok");
    }

    #[test]
    fn parse_agy_token_missing_is_error() {
        assert!(parse_agy_token(br#"{"auth_method":"consumer"}"#).is_err());
        assert!(parse_agy_token(br#"{"token":{"access_token":""}}"#).is_err());
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
        let usage = kimi_usage("");
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

    // ─── OpenRouter ───────────────────────────────────────────────────────
    //
    // Mirrors the Kimi test set: the success-path parser, two required-field
    // failure paths, and the empty-key "logged_out" defensive case. OpenRouter
    // has no vendor-envelope (unlike Kimi's `code` field) so the only failure
    // mode is missing required fields.

    #[test]
    fn parse_openrouter_response_extracts_credits_as_usd_balance_and_omits_monthly_spend() {
        // The endpoint returns `total_credits` (remaining wallet) AND
        // `total_usage` (lifetime cumulative), but lifetime figure can't be
        // rendered as "Spent this month" without misleading the user. Pin
        // `monthly_spend = None` so a future contributor can't silently
        // re-enable the mislabel.
        let json = r#"{
            "data": {
                "total_credits": 50.0,
                "total_usage": 12.34
            }
        }"#;
        let b = parse_openrouter_response(json).unwrap();
        assert_eq!(b.remaining, 50.0);
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
    fn openrouter_usage_with_empty_key_returns_logged_out() {
        // Mirrors `kimi_usage_with_empty_key_returns_logged_out` — the upstream
        // caller is expected to gate on key presence, but we still defend here
        // so a misconfigured fetch surfaces as "no API key" rather than a 401.
        let usage = openrouter_usage("");
        assert!(!usage.logged_in);
        assert_eq!(usage.provider, "openrouter");
        assert!(usage.error.is_some());
        assert!(usage.balance.is_none());
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
        let usage = parse_grok_response(json).unwrap();
        assert_eq!(usage.provider, "grok");
        assert!(usage.logged_in);
        assert_eq!(usage.windows.len(), 1);
        assert_eq!(usage.windows[0].label, "Weekly Pool");
        assert_eq!(usage.windows[0].used_percent, Some(25.0));
        assert_eq!(usage.windows[0].resets_at.as_deref(), Some("2026-07-22T00:00:00+00:00"));
        assert!(usage.balance.is_none());
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
        let usage = parse_grok_response(json).unwrap();
        assert_eq!(usage.provider, "grok");
        assert_eq!(usage.windows.len(), 1);
        assert_eq!(usage.windows[0].label, "Monthly Limit");
        assert_eq!(usage.windows[0].used_percent, Some(20.0));
        assert_eq!(usage.windows[0].resets_at.as_deref(), Some("2026-08-01T00:00:00+00:00"));
    }

    #[test]
    fn parse_grok_response_prepaid_balance() {
        let json = r#"{
            "config": {
                "prepaidBalance": { "val": 15.75 },
                "onDemandUsed": { "val": 4.25 }
            }
        }"#;
        let usage = parse_grok_response(json).unwrap();
        assert!(usage.balance.is_some());
        let balance = usage.balance.unwrap();
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

    fn fake_usage(used_percent: f64) -> ProviderUsage {
        ProviderUsage {
            provider: "opencode".to_string(),
            logged_in: true,
            windows: vec![UsageWindow {
                label: "5-hour".to_string(),
                used_percent: Some(used_percent),
                resets_at: None,
            }],
            balance: None,
            detail: None,
            error: None,
        }
    }

    fn fake_unavailable(msg: &str) -> ProviderUsage {
        ProviderUsage {
            provider: "opencode".to_string(),
            logged_in: true,
            windows: Vec::new(),
            balance: None,
            detail: None,
            error: Some(msg.to_string()),
        }
    }

    #[test]
    fn choose_opencode_usage_live_success_wins() {
        // Live returned real numbers → SQLite is ignored. The 75% figure is
        // the live value; the 50% figure is the SQLite value — neither
        // matches the other's source, so the assertion is unambiguous.
        let live = fake_usage(75.0);
        let sqlite = fake_usage(50.0);
        let result = choose_opencode_usage(Some(&live), sqlite);
        assert_eq!(result.windows[0].used_percent, Some(75.0));
    }

    #[test]
    fn choose_opencode_usage_live_error_falls_back_to_sqlite() {
        // THE pin: live attempted AND failed (HTTP 401 / 5xx / shape) →
        // SQLite is returned. A future refactor that drops the
        // `error.is_none()` guard would surface the 401 in the Probe UI
        // instead of the SQLite windows; this test catches it.
        let live = fake_unavailable("API error 401: Unauthorized");
        let sqlite = fake_usage(50.0);
        let result = choose_opencode_usage(Some(&live), sqlite);
        assert!(result.error.is_none(), "sqlite fallback must clear live error");
        assert_eq!(result.windows[0].used_percent, Some(50.0));
    }

    #[test]
    fn choose_opencode_usage_no_credential_falls_back_to_sqlite() {
        // No `opencode:console` credential at all (read returned
        // NoCredential, collapsed to None) → SQLite is returned. The user
        // who has run `opencode auth login` but not yet finished #956's
        // device flow lands here.
        let sqlite = fake_usage(50.0);
        let result = choose_opencode_usage(None, sqlite);
        assert!(result.error.is_none());
        assert_eq!(result.windows[0].used_percent, Some(50.0));
    }

    // ── opencode_usage_impl — end-to-end fallback integration ──────────────

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
        let usage = opencode_usage_impl(&temp);

        assert_eq!(usage.provider, "opencode");
        assert!(usage.logged_in, "sqlite fallback should be logged_in");
        assert!(usage.error.is_none(), "sqlite fallback must not carry an error: {:?}", usage.error);
        assert_eq!(usage.windows.len(), 3);
        assert_eq!(usage.windows[0].label, "5-hour");
        assert_eq!(
            usage.windows[0].used_percent,
            Some(50.0),
            "$6 of $12 5-hour limit should yield 50%"
        );

        let _ = fs::remove_dir_all(&temp);
    }

    // ── refresh-on-401 contract — issue #971 ────────────────────────────────
    //
    // The four scenarios below pin the seam at
    // `opencode_usage_impl_with_overrides` (issue #970). Each test stands
    // up a stdlib `TcpListener` on `127.0.0.1:0`, enqueues canned HTTP
    // responses, and inspects what the seam actually sent (method, path,
    // headers, body) to confirm the refresh-on-401 contract is honored.

    /// Minimal HTTP/1.1 mock server used only by the refresh-on-401
    /// integration tests. Listens on `127.0.0.1:0` (kernel-assigned port),
    /// routes requests by `(method, path)` to FIFO response queues, and
    /// records every request for assertion. `Connection: close` on every
    /// reply so the seam's reqwest client doesn't try to reuse the socket
    /// mid-test (which would dead-lock the accept loop).
    ///
    /// Thread lifecycle: the accept thread polls a nonblocking listener.
    /// Drop sets stop=true and releases the struct's `Arc<TcpListener>`
    /// reference; the thread holds the other Arc and drops it on exit,
    /// closing the underlying SOCKET. Without `Arc`, `try_clone` would
    /// create an independent Windows SOCKET handle that wouldn't close
    /// when the struct's listener was dropped — and `j.join()` would hang
    /// forever on the accept loop.
    /// Minimal HTTP mock server used only by the refresh-on-401
    /// integration tests. Wraps `tiny_http` (already a dep — see
    /// Cargo.toml:46) so we don't re-implement the accept/read/write
    /// loop ourselves: tiny_http's `Server` runs its own accept loop on
    /// background threads, eliminating the "thread-not-ready" race a
    /// hand-rolled `TcpListener` + thread hit under parallel test load.
    /// Routes requests by `(method, path)` to FIFO response queues and
    /// records every request for assertion.
    struct MockHttpServer {
        base_url: String,
        server: std::sync::Arc<tiny_http::Server>,
        routes: Arc<Mutex<HashMap<(String, String), VecDeque<MockResponse>>>>,
        requests: Arc<Mutex<Vec<RecordedRequest>>>,
        join: Option<std::thread::JoinHandle<()>>,
    }

    #[derive(Clone)]
    struct MockResponse {
        status: u16,
        content_type: String,
        body: Vec<u8>,
    }

    #[derive(Clone, Debug)]
    struct RecordedRequest {
        method: String,
        path: String,
        headers: std::collections::HashMap<String, String>,
        body: Vec<u8>,
    }

    impl MockHttpServer {
        fn new() -> Self {
            let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
            let port = server.server_addr().to_ip().unwrap().port();
            let base_url = format!("http://127.0.0.1:{port}");
            let server_arc = std::sync::Arc::new(server);

            let routes: Arc<Mutex<HashMap<(String, String), VecDeque<MockResponse>>>> =
                Arc::new(Mutex::new(HashMap::new()));
            let requests: Arc<Mutex<Vec<RecordedRequest>>> =
                Arc::new(Mutex::new(Vec::new()));

            let routes_t = routes.clone();
            let requests_t = requests.clone();
            let server_t = server_arc.clone();
            let join = std::thread::spawn(move || {
                for mut request in server_t.incoming_requests() {
                    let method = request.method().as_str().to_string();
                    let path = request.url().to_string();
                    let mut headers = std::collections::HashMap::new();
                    for header in request.headers() {
                        let key = header.field.as_str().to_string().to_lowercase();
                        headers.insert(key, header.value.as_str().to_string());
                    }
                    let mut body = Vec::new();
                    let mut reader = request.as_reader();
                    std::io::Read::read_to_end(&mut reader, &mut body).ok();
                    let key = (method.clone(), path.clone());
                    let response = {
                        let mut routes = routes_t.lock().unwrap();
                        routes.get_mut(&key).and_then(|q| q.pop_front())
                    }
                    .unwrap_or_else(|| MockResponse {
                        status: 500,
                        content_type: "text/plain".to_string(),
                        body: b"no mock response queued".to_vec(),
                    });
                    requests_t.lock().unwrap().push(RecordedRequest {
                        method,
                        path,
                        headers,
                        body,
                    });
                    let resp = tiny_http::Response::from_data(response.body)
                        .with_status_code(tiny_http::StatusCode(response.status))
                        .with_header(
                            tiny_http::Header::from_bytes(
                                &b"Content-Type"[..],
                                response.content_type.as_bytes(),
                            )
                            .unwrap(),
                        );
                    let _ = request.respond(resp);
                }
            });

            Self {
                base_url,
                server: server_arc,
                routes,
                requests,
                join: Some(join),
            }
        }

        fn url(&self, path: &str) -> String {
            format!("{}{}", self.base_url, path)
        }

        fn enqueue(&self, method: &str, path: &str, status: u16, body: &[u8]) {
            let key = (method.to_string(), path.to_string());
            let mut routes = self.routes.lock().unwrap();
            routes
                .entry(key)
                .or_default()
                .push_back(MockResponse {
                    status,
                    content_type: "application/json".to_string(),
                    body: body.to_vec(),
                });
        }

        fn requests_for(&self, method: &str, path: &str) -> Vec<RecordedRequest> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.method == method && r.path == path)
                .cloned()
                .collect()
        }
    }

    impl Drop for MockHttpServer {
        fn drop(&mut self) {
            // Drop the Server Arc so the listener stops accepting new
            // connections. The accept thread's `incoming_requests()`
            // iterator returns None once the Server is fully dropped
            // (i.e. once its own Arc clone goes out of scope on thread
            // exit). We detach the JoinHandle — joining would deadlock
            // when the test process tries to exit because the listener
            // is still bound to its ephemeral port. Cargo test
            // tolerates detached threads; the OS reaps them at process
            // exit.
            drop(self.server.clone());
            let _ = self.join.take();
        }
    }

    /// UUID-suffixed Credential Manager target so tests don't collide on
    /// the production `opencode:console` slot or on each other. Mirrors
    /// the `unique_target` pattern in `services::windows_cred`.
    fn opencode_unique_target(label: &str) -> String {
        format!(
            "buildmesh-test-{label}-{}",
            uuid::Uuid::new_v4().simple().to_string()
        )
    }

    /// Cleanup guard: deletes the test credential from Windows Credential
    /// Manager, removes the fake home dir, drops the refresh-tracker
    /// entry, and invalidates the usage cache so the next test starts
    /// from a known state.
    struct OpencodeRefreshCleanup {
        target: String,
        home: PathBuf,
    }
    impl Drop for OpencodeRefreshCleanup {
        fn drop(&mut self) {
            #[cfg(windows)]
            {
                let _ = crate::services::windows_cred::delete(&self.target);
            }
            forget_refresh_tracking(&self.target);
            let _ = fs::remove_dir_all(&self.home);
            invalidate_cache();
        }
    }

    /// Builds the JSON blob `OpenCodeConsoleCred` serializes to, then
    /// writes it to `target` via `windows_cred::write`. Centralizes the
    /// field-name order so each test only sets the fields it cares about.
    fn seed_opencode_cred(
        target: &str,
        access_token: &str,
        workspace_id: &str,
        refresh_token: &str,
        expires_at: &str,
        server_id: &str,
    ) {
        let blob = serde_json::json!({
            "access_token": access_token,
            "workspace_id": workspace_id,
            "refresh_token": refresh_token,
            "expires_at": expires_at,
            "server_id": server_id,
        })
        .to_string();
        crate::services::windows_cred::write(target, blob.as_bytes())
            .expect("seed_opencode_cred: write to Credential Manager");
    }

    /// Sets up a fake home with a valid `auth.json` + `opencode.db` so the
    /// SQLite fallback path is exercised when the live path doesn't return
    /// real numbers. Mirrors the structure used by
    /// `opencode_usage_impl_with_sqlite_db_only_returns_sqlite_windows`.
    fn seed_opencode_sqlite(home: &Path, cost: f64) {
        let opencode_dir = home.join(".local").join("share").join("opencode");
        fs::create_dir_all(&opencode_dir).unwrap();
        fs::write(
            opencode_dir.join("auth.json"),
            r#"{"opencode-go": {"type": "api", "key": "sk-test-abc"}}"#,
        )
        .unwrap();
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
            rusqlite::params!["ses_refresh_test", now_ms - 30 * 60 * 1000, cost],
        )
        .unwrap();
    }

    /// Returns the value of a query-string-free form body parameter from
    /// `application/x-www-form-urlencoded`. Tests assert that the seam sent
    /// `grant_type=refresh_token` etc. without re-implementing URL decoding.
    fn form_value(body: &[u8], key: &str) -> Option<String> {
        let s = std::str::from_utf8(body).ok()?;
        for pair in s.split('&') {
            if let Some(eq) = pair.find('=') {
                if &pair[..eq] == key {
                    return Some(url_decode(&pair[eq + 1..]));
                }
            }
        }
        None
    }

    fn url_decode(s: &str) -> String {
        // Minimal decoder: handle `+` → space and `%XX` → byte. The OAuth
        // refresh endpoint's form fields don't contain reserved characters
        // beyond what this handles.
        let mut out = String::with_capacity(s.len());
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'+' => {
                    out.push(' ');
                    i += 1;
                }
                b'%' if i + 2 < bytes.len() => {
                    let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                    if let Some(h) = hex {
                        if let Ok(b) = u8::from_str_radix(h, 16) {
                            out.push(b as char);
                            i += 3;
                            continue;
                        }
                    }
                    out.push('%' as char);
                    i += 1;
                }
                b => {
                    out.push(b as char);
                    i += 1;
                }
            }
        }
        out
    }

    // ── Scenario 1: expired cred, refresh succeeds, live probe succeeds ────
    #[cfg(windows)]
    #[test]
    fn refresh_on_401_expired_cred_refresh_succeeds_then_live_succeeds() {
        let server = MockHttpServer::new();
        let target = opencode_unique_target("expired-refresh-ok");
        let home = std::env::temp_dir().join(format!(
            "opencode_refresh_s1_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _cleanup = OpencodeRefreshCleanup {
            target: target.clone(),
            home: home.clone(),
        };
        seed_opencode_sqlite(&home, 6.0); // SQLite fallback = 50% five-hour

        // Expired credential: `expires_at` in 2020 forces the gate.
        seed_opencode_cred(
            &target,
            "old-access",
            "wrk-old",
            "old-refresh",
            "2020-01-01T00:00:00Z",
            "server-old",
        );

        // Mock the refresh endpoint to return a new bundle.
        server.enqueue(
            "POST",
            "/auth/device/token",
            200,
            br#"{
                "access_token": "new-access",
                "refresh_token": "new-refresh",
                "expires_in": 3600,
                "workspace_id": "wrk-new",
                "server_id": "server-new"
            }"#,
        );

        // Mock the live billing endpoint to return real windows (5-hour 25%).
        server.enqueue(
            "POST",
            "/_server",
            200,
            br#"{"windows":[{"label":"5-hour","usedPercent":25.0,"resetsAt":"2026-07-20T22:00:00Z"}]}"#,
        );

        invalidate_cache();
        let usage = opencode_usage_impl_with_overrides(
            &home,
            &target,
            &server.url("/_server"),
            &server.url("/auth/device/token"),
        );

        // Exactly one refresh and one billing call.
        assert_eq!(
            server.requests_for("POST", "/auth/device/token").len(),
            1,
            "refresh endpoint must be hit exactly once"
        );
        assert_eq!(
            server.requests_for("POST", "/_server").len(),
            1,
            "billing endpoint must be hit exactly once"
        );

        // Refresh request carries the old refresh token and the standard
        // grant/client fields.
        let refresh_req = &server.requests_for("POST", "/auth/device/token")[0];
        assert_eq!(form_value(&refresh_req.body, "grant_type").as_deref(), Some("refresh_token"));
        assert_eq!(form_value(&refresh_req.body, "client_id").as_deref(), Some("opencode-cli"));
        assert_eq!(form_value(&refresh_req.body, "refresh_token").as_deref(), Some("old-refresh"));

        // Live billing uses the NEW bearer (proof that the persisted blob
        // was read back, not the old in-memory token).
        let billing_req = &server.requests_for("POST", "/_server")[0];
        assert_eq!(
            billing_req.headers.get("authorization").map(|s| s.as_str()),
            Some("Bearer new-access")
        );
        let body_str = std::str::from_utf8(&billing_req.body).unwrap();
        assert!(
            body_str.contains("wrk-new"),
            "billing body should include new workspace id, got: {body_str}"
        );

        // Final usage carries the live 25% number, not the SQLite 50%.
        assert!(usage.error.is_none(), "live success must clear error: {:?}", usage.error);
        assert_eq!(usage.windows.len(), 1);
        assert_eq!(usage.windows[0].label, "5-hour");
        assert_eq!(
            usage.windows[0].used_percent,
            Some(25.0),
            "live 25% must beat SQLite 50%"
        );
    }

    // ── Scenario 2: expired cred, refresh fails, SQLite fallback ───────────
    #[cfg(windows)]
    #[test]
    fn refresh_on_401_expired_cred_refresh_fails_falls_back_to_sqlite() {
        let server = MockHttpServer::new();
        let target = opencode_unique_target("expired-refresh-fail");
        let home = std::env::temp_dir().join(format!(
            "opencode_refresh_s2_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _cleanup = OpencodeRefreshCleanup {
            target: target.clone(),
            home: home.clone(),
        };
        seed_opencode_sqlite(&home, 6.0); // SQLite fallback = 50% five-hour

        // Expired credential — gate fires.
        seed_opencode_cred(
            &target,
            "stale-access",
            "wrk-stale",
            "stale-refresh",
            "2020-01-01T00:00:00Z",
            "server-stale",
        );

        // Refresh endpoint returns 500 — the seam must NOT retry the live
        // probe with the stale token (a stale-token retry would 401, which
        // could loop); it must fall through to SQLite.
        server.enqueue(
            "POST",
            "/auth/device/token",
            500,
            br#"{"error":"server_error"}"#,
        );
        // No live billing response queued — the seam MUST NOT call it.

        invalidate_cache();
        let usage = opencode_usage_impl_with_overrides(
            &home,
            &target,
            &server.url("/_server"),
            &server.url("/auth/device/token"),
        );

        // Refresh was attempted, live was NOT.
        assert_eq!(
            server.requests_for("POST", "/auth/device/token").len(),
            1,
            "refresh endpoint must be hit exactly once"
        );
        assert_eq!(
            server.requests_for("POST", "/_server").len(),
            0,
            "billing endpoint must NOT be hit after refresh failure"
        );

        // Final usage is the SQLite 50% — not the stale token's would-be
        // 401, not the refresh error string.
        assert!(
            usage.error.is_none(),
            "SQLite fallback must clear the refresh error: {:?}",
            usage.error
        );
        assert!(
            usage.logged_in,
            "SQLite path keeps the user logged in (auth.json present)"
        );
        assert_eq!(usage.windows.len(), 3);
        assert_eq!(usage.windows[0].label, "5-hour");
        assert_eq!(
            usage.windows[0].used_percent,
            Some(50.0),
            "SQLite 50% fallback should drive the result"
        );
    }

    // ── Scenario 3: fresh cred, no refresh, live probe succeeds ────────────
    #[cfg(windows)]
    #[test]
    fn refresh_on_401_fresh_cred_no_refresh_live_succeeds() {
        let server = MockHttpServer::new();
        let target = opencode_unique_target("fresh-no-refresh");
        let home = std::env::temp_dir().join(format!(
            "opencode_refresh_s3_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _cleanup = OpencodeRefreshCleanup {
            target: target.clone(),
            home: home.clone(),
        };
        seed_opencode_sqlite(&home, 6.0); // SQLite fallback = 50% five-hour

        // Fresh credential: expires in 2099, AND first-seen by the
        // refresh tracker so `refresh_age` returns 0 and the gate does
        // not fire.
        seed_opencode_cred(
            &target,
            "fresh-access",
            "wrk-fresh",
            "fresh-refresh",
            "2099-01-01T00:00:00Z",
            "server-fresh",
        );

        // Only the live billing response — no refresh response.
        server.enqueue(
            "POST",
            "/_server",
            200,
            br#"{"windows":[{"label":"5-hour","usedPercent":30.0,"resetsAt":"2026-07-20T22:00:00Z"}]}"#,
        );

        invalidate_cache();
        let usage = opencode_usage_impl_with_overrides(
            &home,
            &target,
            &server.url("/_server"),
            &server.url("/auth/device/token"),
        );

        // Refresh MUST NOT fire on a fresh credential.
        assert_eq!(
            server.requests_for("POST", "/auth/device/token").len(),
            0,
            "refresh endpoint must NOT be hit on a fresh credential"
        );
        // Live called exactly once.
        assert_eq!(
            server.requests_for("POST", "/_server").len(),
            1,
            "billing endpoint must be hit exactly once"
        );
        // Bearer is the fresh one (no refresh happened).
        let billing_req = &server.requests_for("POST", "/_server")[0];
        assert_eq!(
            billing_req.headers.get("authorization").map(|s| s.as_str()),
            Some("Bearer fresh-access")
        );

        assert!(usage.error.is_none());
        assert_eq!(usage.windows[0].label, "5-hour");
        assert_eq!(
            usage.windows[0].used_percent,
            Some(30.0),
            "live 30% must beat SQLite 50%"
        );
    }

    // ── Scenario 4: fresh cred, live 401, refresh-on-the-spot, retry ──────
    #[cfg(windows)]
    #[test]
    fn refresh_on_401_fresh_cred_live_401_triggers_refresh_then_success() {
        let server = MockHttpServer::new();
        let target = opencode_unique_target("fresh-401-refresh");
        let home = std::env::temp_dir().join(format!(
            "opencode_refresh_s4_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _cleanup = OpencodeRefreshCleanup {
            target: target.clone(),
            home: home.clone(),
        };
        seed_opencode_sqlite(&home, 6.0); // SQLite fallback = 50% five-hour

        // Fresh credential — gate doesn't fire, but the live probe returns
        // 401 (e.g. server revoked or rotated the key between dance and
        // probe). The on-the-spot refresh branch must kick in.
        seed_opencode_cred(
            &target,
            "fresh-access",
            "wrk-fresh",
            "fresh-refresh",
            "2099-01-01T00:00:00Z",
            "server-fresh",
        );

        // Three responses in order: 401, refresh OK, live OK.
        server.enqueue("POST", "/_server", 401, br#"{"error":"unauthorized"}"#);
        server.enqueue(
            "POST",
            "/auth/device/token",
            200,
            br#"{
                "access_token": "rotated-access",
                "refresh_token": "rotated-refresh",
                "expires_in": 3600,
                "workspace_id": "wrk-rotated",
                "server_id": "server-rotated"
            }"#,
        );
        server.enqueue(
            "POST",
            "/_server",
            200,
            br#"{"windows":[{"label":"5-hour","usedPercent":35.0,"resetsAt":"2026-07-20T22:00:00Z"}]}"#,
        );

        invalidate_cache();
        let usage = opencode_usage_impl_with_overrides(
            &home,
            &target,
            &server.url("/_server"),
            &server.url("/auth/device/token"),
        );

        let billing = server.requests_for("POST", "/_server");
        let refreshes = server.requests_for("POST", "/auth/device/token");
        assert_eq!(refreshes.len(), 1, "refresh must be hit exactly once");
        assert_eq!(billing.len(), 2, "billing must be hit twice (initial + retry)");

        // First billing request uses the original (fresh) access token —
        // proves the gate didn't fire and the seam went straight to live.
        assert_eq!(
            billing[0].headers.get("authorization").map(|s| s.as_str()),
            Some("Bearer fresh-access"),
            "first billing call must use the original bearer"
        );

        // Refresh request uses the original refresh token.
        assert_eq!(
            form_value(&refreshes[0].body, "refresh_token").as_deref(),
            Some("fresh-refresh")
        );

        // Retry billing uses the rotated bearer + workspace.
        assert_eq!(
            billing[1].headers.get("authorization").map(|s| s.as_str()),
            Some("Bearer rotated-access"),
            "retry billing call must use the rotated bearer"
        );
        let retry_body = std::str::from_utf8(&billing[1].body).unwrap();
        assert!(
            retry_body.contains("wrk-rotated"),
            "retry billing body should include rotated workspace id, got: {retry_body}"
        );

        // Final usage is the retry's 35%, not SQLite's 50% or any error.
        assert!(usage.error.is_none(), "retry success must clear error: {:?}", usage.error);
        assert_eq!(usage.windows[0].label, "5-hour");
        assert_eq!(
            usage.windows[0].used_percent,
            Some(35.0),
            "retry 35% must win over SQLite 50%"
        );
    }
}
