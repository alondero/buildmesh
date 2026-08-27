//! Provider usage fetching — piggybacks on CLI credentials stored by each provider.
//!
//! Endpoints are undocumented / reverse-engineered; treat non-200 responses or
//! shape mismatches as "usage unavailable", never as hard errors.

use reqwest::blocking::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};
// `Datelike` powers the month-start computation in
// [`current_month_start_epoch`] (spec §3.1). `Timelike` is only used by the
// test mod for `current_month_start_epoch_is_first_of_utc_month` and is
// imported in the test scope rather than here to keep the production
// imports minimal.
use chrono::Datelike;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
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

fn anthropic_cred_path() -> PathBuf {
    home_dir().join(".claude").join(".credentials.json")
}

/// Build the ordered list of candidate Codex auth.json paths (issue #1108,
/// spec §2.2). Priority:
///
/// 1. `$CODEX_HOME/auth.json` if set and non-empty.
/// 2. `<home>/.codex/auth.json` (Windows host default).
/// 3. WSL fallback (Windows host only): the default-WSL-distro UNC form of
///    `/home/<USERNAME>/.codex/auth.json`, built via `env::to_host_path` so
///    the UNC string never escapes the `host_path` module.
///
/// Resolution is strictly passive: the candidates are returned; the caller
/// walks them in order. We never wake a sleeping WSL distro — if the UNC read
/// fails, we move on to the next candidate.
fn discover_codex_auth_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // 1. CODEX_HOME override (spec §2.2 #1).
    if let Ok(codex_home) = env::var("CODEX_HOME") {
        if !codex_home.is_empty() {
            paths.push(PathBuf::from(codex_home).join("auth.json"));
        }
    }

    // 2. Windows host default (spec §2.2 #2). Same path is correct on Unix
    // hosts (`~/.codex/auth.json`), so the function is host-agnostic.
    paths.push(home_dir().join(".codex").join("auth.json"));

    // 3. WSL fallback (Windows host only, spec §2.2 #3). We use the Windows
    // USERNAME env var as the WSL username — matches the default
    // `wsl.exe` install where WSL user == Windows user. A user with a
    // mismatched WSL username can override via CODEX_HOME (priority #1).
    #[cfg(target_os = "windows")]
    {
        if let Some(username) = env::var("USERNAME").ok().filter(|s| !s.is_empty()) {
            let wsl_linux_path = format!("/home/{}/.codex/auth.json", username);
            let host_path = crate::env::to_host_path(&wsl_linux_path);
            if host_path != wsl_linux_path {
                paths.push(PathBuf::from(host_path));
            }
        }
    }

    paths
}

/// One Codex auth-file credential pair: the bearer token plus the optional
/// `ChatGPT-Account-Id` header value the upstream `/wham/usage` endpoint
/// expects for multi-account subscriptions (issue #1108, spec §2.1).
#[derive(Debug, Clone, PartialEq)]
pub struct CodexAuthCredentials {
    pub access_token: String,
    pub account_id: Option<String>,
}

/// Top-level Codex auth file schema. Spec §2.3 — handles both legacy
/// top-level tokens and the nested `tokens` envelope some Codex CLI versions
/// emit. Both shapes are kept optional so a missing field is just absence.
#[derive(Deserialize, Debug)]
struct CodexAuthFile {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    tokens: Option<CodexNestedTokens>,
}

#[derive(Deserialize, Debug)]
struct CodexNestedTokens {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
}

impl CodexAuthFile {
    /// Pull the bearer + optional account_id out of either shape. Top-level
    /// wins over nested; an empty string is treated as missing so a logged-
    /// out Codex CLI (which writes `"access_token": ""`) returns `None`
    /// rather than a bogus token.
    fn extract_credentials(&self) -> Option<CodexAuthCredentials> {
        let non_empty = |s: &Option<String>| s.as_deref().filter(|v| !v.is_empty()).map(str::to_owned);

        if let Some(token) = non_empty(&self.access_token) {
            return Some(CodexAuthCredentials {
                access_token: token,
                account_id: non_empty(&self.account_id),
            });
        }
        if let Some(nested) = &self.tokens {
            if let Some(token) = non_empty(&nested.access_token) {
                let nested_id = non_empty(&nested.account_id);
                return Some(CodexAuthCredentials {
                    access_token: token,
                    account_id: nested_id.or_else(|| non_empty(&self.account_id)),
                });
            }
        }
        None
    }
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

/// Reads Codex's credentials JSON which has access_token at the top level
/// (legacy) OR nested inside a `tokens` envelope (spec §2.3). Returns both
/// the bearer token and the optional `ChatGPT-Account-Id` so the live probe
/// can forward the header.
fn read_codex_auth_file(path: &Path) -> Result<CodexAuthCredentials, UsageError> {
    let content = fs::read_to_string(path)
        .map_err(|_| UsageError::NoCredential(path.to_string_lossy().to_string()))?;
    let cred: CodexAuthFile = serde_json::from_str(&content)
        .map_err(|e| UsageError::Shape(e.to_string()))?;
    cred.extract_credentials()
        .ok_or_else(|| UsageError::NoCredential(path.to_string_lossy().to_string()))
}

/// Walk the candidate list and return the first readable Codex credentials
/// along with the path they came from. The returned path is used for the
/// `logged_out` error message so the UI can show the user where we looked.
/// Pure: takes an explicit candidate list so tests don't need to mutate
/// `$CODEX_HOME` or `$USERPROFILE` across the process.
fn read_codex_credentials(candidates: &[PathBuf]) -> Result<(PathBuf, CodexAuthCredentials), UsageError> {
    let first = candidates.first().cloned().unwrap_or_default();
    for path in candidates {
        if path.exists() {
            match read_codex_auth_file(path) {
                Ok(creds) => return Ok((path.clone(), creds)),
                Err(UsageError::Shape(e)) => {
                    // Malformed auth file — surface immediately rather than
                    // silently trying the next candidate. A bad JSON shape
                    // isn't fixed by reading a different file.
                    return Err(UsageError::Shape(e));
                }
                Err(_) => continue, // NoCredential → try next path
            }
        }
    }
    Err(UsageError::NoCredential(first.to_string_lossy().to_string()))
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
fn fetch_usage(
    provider: &str,
    build_request: impl FnOnce(&Client) -> RequestBuilder,
    parse: impl FnOnce(&str) -> Result<(Vec<UsageWindow>, Option<String>), UsageError>,
) -> ProviderUsage {
    let client = match Client::builder().build() {
        Ok(c) => c,
        Err(e) => return unavailable(provider, format!("Client error: {}", e)),
    };

    match build_request(&client).send() {
        Ok(r) if r.status() == 429 => unavailable(
            provider,
            "Rate limited — usage data temporarily unavailable".to_string(),
        ),
        Ok(r) if !r.status().is_success() => {
            let code = r.status().as_u16();
            unavailable(
                provider,
                format!("API error {}: {}", code, r.text().unwrap_or_default()),
            )
        }
        Ok(r) => match parse(&r.text().unwrap_or_default()) {
            Ok((windows, detail)) => ProviderUsage {
                provider: provider.to_string(),
                logged_in: true,
                windows,
                balance: None,
                detail,
                error: None,
            },
            Err(e) => unavailable(provider, format!("Failed to parse response: {}", e)),
        },
        Err(e) => unavailable(provider, format!("Request failed: {}", e)),
    }
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

// ─── Codex CLI (`codex`) subscription quotas ───────────────────────────
//
// The Codex CLI (ChatGPT Plus/Team/Pro) stores its OAuth token at
// `~/.codex/auth.json` (override with `$CODEX_HOME`). When authenticated via
// ChatGPT, it polls `GET https://chatgpt.com/backend-api/wham/usage` for
// rolling rate-limit consumption + reset timestamps (issue #1108, spec
// §2.1–2.5).
//
// Spec invariants:
//   - **Passive read-only.** The fetcher never writes `auth.json` or invokes
//     OAuth refresh grants. On 401/403 the response is `logged_out` with a
//     CLI re-auth hint so the user knows exactly how to recover.
//   - **Consumption on the wire.** `UsageWindow.used_percent` stays
//     0.0–100.0; remaining-percentage phrasing is derived in `detail`.
//   - **Dynamic window labels.** 18 000 s → `"5-hour"`, 604 800 s →
//     `"Weekly"`, with `"{N}h"` / `"{N}d"` / `"{N}s"` fallbacks for
//     non-standard second intervals.

/// One window inside the Codex `/wham/usage` payload. The upstream API
/// returns Unix epoch seconds for `resetAt` — converted to RFC3339 in the
/// wire layer. `limitWindowSeconds` drives the dynamic label resolution
/// (see [`format_codex_window_label`]).
///
/// Field names are snake_case per spec §2.4 (the upstream payload schema
/// shown in the spec); the Buildmesh wire contract is camelCase per §4.1
/// (`UsageWindow.usedPercent` / `resetsAt`). No `#[serde(rename)]` here on
/// purpose — the conversion from snake_case `used_percent` to the wire's
/// camelCase `usedPercent` happens via [`UsageWindow`]'s own serde rename.
#[derive(Deserialize, Debug)]
struct CodexRateWindow {
    used_percent: Option<f64>,
    limit_window_seconds: Option<i64>,
    reset_at: Option<i64>,
}

#[derive(Deserialize, Debug)]
struct CodexRateLimits {
    primary_window: Option<CodexRateWindow>,
    secondary_window: Option<CodexRateWindow>,
    #[serde(default)]
    additional_rate_limits: Vec<CodexRateWindow>,
}

#[derive(Deserialize, Debug)]
struct CodexUsageResp {
    #[serde(default)]
    rate_limit: Option<CodexRateLimits>,
}

/// Map an upstream window duration in seconds to the user-facing label
/// (spec §2.5). Hard-coded names for the two tiers Codex currently reports;
/// anything else falls back to a friendly `"{N}h"` / `"{N}d"` / `"{N}s"`
/// format so a new tier the upstream adds tomorrow still renders legibly
/// instead of an empty label.
fn format_codex_window_label(seconds: i64) -> String {
    match seconds {
        18_000 => "5-hour".to_string(),
        604_800 => "Weekly".to_string(),
        86_400 => "24h".to_string(),
        3_600 => "1-hour".to_string(),
        s if s > 0 && s % 86_400 == 0 => format!("{}d", s / 86_400),
        s if s > 0 && s % 3_600 == 0 => format!("{}h", s / 3_600),
        s => format!("{}s", s),
    }
}

/// Parse the Codex `/wham/usage` payload (spec §2.4) into the wire contract.
/// Returns `(windows, detail)` where `detail` carries the user-facing
/// remaining-percentage phrasing when at least one window is present.
///
/// Windows with `used_percent == None` are filtered (a window without a
/// percentage is shape-malformed, not "0% used"). An empty-but-present
/// `rate_limit` object surfaces the `"No active Codex rate-limit windows"`
/// detail so the UI doesn't render an empty state for a malformed reply.
fn parse_codex_response(body: &str) -> Result<(Vec<UsageWindow>, Option<String>), UsageError> {
    let resp: CodexUsageResp =
        serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;
    let rate_limit = resp.rate_limit.ok_or_else(|| {
        UsageError::Shape("missing rate_limit object in Codex response".to_string())
    })?;

    let mut windows = Vec::new();
    let mut highest_used: Option<f64> = None;
    let push = |w: CodexRateWindow, windows: &mut Vec<UsageWindow>, highest: &mut Option<f64>| {
        if let Some(used) = w.used_percent {
            let label = w
                .limit_window_seconds
                .map(format_codex_window_label)
                .unwrap_or_else(|| "5-hour".to_string());
            let resets_at = w
                .reset_at
                .and_then(|epoch| chrono::DateTime::from_timestamp(epoch, 0).map(|dt| dt.to_rfc3339()));
            if highest.is_none_or(|h| used > h) {
                *highest = Some(used);
            }
            windows.push(UsageWindow {
                label,
                used_percent: Some(used),
                resets_at,
            });
        }
    };

    if let Some(p) = rate_limit.primary_window {
        push(p, &mut windows, &mut highest_used);
    }
    if let Some(s) = rate_limit.secondary_window {
        push(s, &mut windows, &mut highest_used);
    }
    for additional in rate_limit.additional_rate_limits {
        push(additional, &mut windows, &mut highest_used);
    }

    let detail = if windows.is_empty() {
        Some("No active Codex rate-limit windows".to_string())
    } else {
        // Headline phrasing uses the highest-used window (typically the most
        // pressed quota). `100.0 - used` switches consumption → remaining on
        // the UI side per the wire-contract normalization invariant (spec §4).
        highest_used.map(|u| format!("{:.1}% remaining", (100.0 - u).max(0.0)))
    };
    Ok((windows, detail))
}

/// Public Codex fetcher. Walks the discovery list, hits the ChatGPT quota
/// endpoint, and reports the wire contract.
pub fn codex_usage() -> ProviderUsage {
    let candidates = discover_codex_auth_paths();
    codex_usage_with_paths(&candidates, "https://chatgpt.com/backend-api/wham/usage")
}

/// Test seam: pass an explicit candidate list + endpoint so the WSL fallback
/// and the live HTTP round-trip can be exercised in isolation. The public
/// [`codex_usage`] is a one-line wrapper around this with the production URL.
fn codex_usage_with_paths(candidates: &[PathBuf], live_url: &str) -> ProviderUsage {
    let creds = match read_codex_credentials(candidates) {
        Ok((_, c)) => c,
        Err(e) => return logged_out("codex", e.to_string()),
    };

    let client = match Client::builder().timeout(Duration::from_secs(15)).build() {
        Ok(c) => c,
        Err(e) => return unavailable("codex", format!("Client error: {}", e)),
    };

    let token = creds.access_token;
    let account_id = creds.account_id;
    let mut req = client
        .get(live_url)
        .header("Authorization", format!("Bearer {}", token));
    if let Some(account_id) = account_id.as_deref() {
        // Multi-account subscriptions require the `ChatGPT-Account-Id`
        // header (spec §2.1); single-account auth files omit the field.
        req = req.header("ChatGPT-Account-Id", account_id.to_string());
    }

    let resp = match req.send() {
        Ok(r) => r,
        Err(e) => return unavailable("codex", format!("Request failed: {}", e)),
    };

    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        // Token expired / revoked: prompt CLI re-auth per spec §2.1
        // ("Run 'codex' in terminal to log in"). `logged_out` flips
        // `logged_in = false` so the UI surfaces the re-auth affordance.
        return logged_out(
            "codex",
            "Codex session expired — run 'codex' in your terminal to log in".to_string(),
        );
    }
    if status.as_u16() == 429 {
        return unavailable(
            "codex",
            "Rate limited — usage data temporarily unavailable".to_string(),
        );
    }
    if !status.is_success() {
        return unavailable(
            "codex",
            format!("API error {}: usage endpoint failed", status.as_u16()),
        );
    }

    let body = match resp.text() {
        Ok(b) => b,
        Err(e) => return unavailable("codex", format!("Failed to read response: {}", e)),
    };
    match parse_codex_response(&body) {
        Ok((windows, detail)) => ProviderUsage {
            provider: "codex".to_string(),
            logged_in: true,
            windows,
            balance: None,
            detail,
            error: None,
        },
        Err(e) => unavailable("codex", format!("Failed to parse response: {}", e)),
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

/// Public OpenAI fetcher. Wired into `commands::usage::cached_or_fetch` so
/// the keyed-provider panel polls it on the same cadence as the other keyed
/// fetchers.
pub fn openai_usage(api_key: &str) -> ProviderUsage {
    openai_usage_with_base_url(api_key, "https://api.openai.com/v1")
}

/// Test seam: pass an explicit base URL so a loopback `tiny_http` server can
/// stand in for the production endpoint in mocked HTTP tests (issue #971
/// pattern).
fn openai_usage_with_base_url(api_key: &str, base_url: &str) -> ProviderUsage {
    if api_key.is_empty() {
        return logged_out("openai", "No API key configured".to_string());
    }

    let client = match Client::builder().timeout(Duration::from_secs(15)).build() {
        Ok(c) => c,
        Err(e) => return unavailable("openai", format!("Client error: {}", e)),
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
            return logged_out("openai", "Invalid API key".to_string());
        }
        Ok(r) if r.status().as_u16() == 429 => {
            return unavailable(
                "openai",
                "Rate limited — usage data temporarily unavailable".to_string(),
            );
        }
        Ok(r) if !r.status().is_success() => {
            let code = r.status().as_u16();
            return unavailable(
                "openai",
                format!("API error {}: inference check failed", code),
            );
        }
        Ok(_) => {} // 2xx — proceed to costs probe
        Err(e) => return unavailable("openai", format!("Inference check failed: {}", e)),
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
            // Project key — graceful degradation. The key is valid for
            // inference; we just can't reach the admin endpoint. Spec §3.2:
            // logged_in=true, balance=None, detail explains the gap. The
            // `error` field stays None so the UI doesn't render the red
            // "fetch failed" affordance — the user is logged in, they just
            // need an admin key for spend.
            return ProviderUsage {
                provider: "openai".to_string(),
                logged_in: true,
                windows: Vec::new(),
                balance: None,
                detail: Some(
                    "Monthly spend tracking requires an Organization Admin API Key (sk-admin-...)"
                        .to_string(),
                ),
                error: None,
            };
        }
        Ok(r) if r.status().as_u16() == 429 => {
            return unavailable(
                "openai",
                "Rate limited — usage data temporarily unavailable".to_string(),
            );
        }
        Ok(r) if !r.status().is_success() => {
            let code = r.status().as_u16();
            return unavailable(
                "openai",
                format!("API error {}: costs query failed", code),
            );
        }
        Ok(r) => r,
        Err(e) => return unavailable("openai", format!("Costs query failed: {}", e)),
    };

    let body = match resp.text() {
        Ok(b) => b,
        Err(e) => return unavailable("openai", format!("Failed to read response: {}", e)),
    };
    match parse_openai_costs_response(&body) {
        Ok(balance) => ProviderUsage {
            provider: "openai".to_string(),
            logged_in: true,
            windows: Vec::new(),
            balance: Some(balance),
            detail: None,
            error: None,
        },
        Err(e) => unavailable("openai", format!("Failed to parse costs response: {}", e)),
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
/// reports the wire contract. Wired into
/// `commands::usage::cached_or_fetch` so the keyed-provider panel polls it
/// on the same cadence as Kimi / OpenRouter. Thin wrapper around
/// [`deepseek_usage_with_url`] with the production endpoint pinned — the
/// test seam keeps the loopback HTTP integration tests (issue #971
/// pattern) runnable without hitting the real DeepSeek API.
pub fn deepseek_usage(api_key: &str) -> ProviderUsage {
    deepseek_usage_with_url(api_key, "https://api.deepseek.com/user/balance")
}

/// Test seam: pass an explicit loopback URL so the live-fetcher tests can
/// stand in a `tiny_http` server for the production endpoint without
/// hitting the real DeepSeek API. Mirrors the
/// `codex_usage_with_paths` / `openai_usage_with_base_url` pattern —
/// keeps the public fetcher a one-line wrapper around this with the
/// production URL. (Note: `kimi_usage` / `openrouter_usage` don't carry
/// this seam and have no loopback tests; their balance parsers are
/// pinned by pure fixtures only. Adding the seam here is the more
/// thorough contract — leaving room for either kimi/openrouter to gain
/// the same seam in a follow-up, or for this seam to be dropped once
/// parity across balance fetchers is the chosen design.)
fn deepseek_usage_with_url(api_key: &str, live_url: &str) -> ProviderUsage {
    if api_key.is_empty() {
        return logged_out("deepseek", "No API key configured".to_string());
    }
    let client = match Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => return unavailable("deepseek", format!("Client error: {}", e)),
    };
    let auth = format!("Bearer {}", api_key);
    let resp = match client.get(live_url).header("Authorization", auth).send() {
        Ok(r) if r.status() == 429 => return unavailable(
            "deepseek",
            "Rate limited — usage data temporarily unavailable".to_string(),
        ),
        Ok(r) if !r.status().is_success() => {
            let code = r.status().as_u16();
            if code == 401 || code == 403 {
                return logged_out("deepseek", "Invalid API key".to_string());
            }
            return unavailable(
                "deepseek",
                format!("API error {}: {}", code, r.text().unwrap_or_default()),
            );
        }
        Ok(r) => r,
        Err(e) => return unavailable("deepseek", format!("Request failed: {}", e)),
    };
    let body = match resp.text() {
        Ok(b) => b,
        Err(e) => return unavailable("deepseek", format!("Failed to read response: {}", e)),
    };
    match parse_deepseek_response(&body) {
        Ok(balance) => ProviderUsage {
            provider: "deepseek".to_string(),
            logged_in: true,
            windows: Vec::new(),
            balance: Some(balance),
            detail: None,
            error: None,
        },
        Err(e) => unavailable("deepseek", format!("Failed to parse response: {}", e)),
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

fn opencode_usage_impl(home: &std::path::Path) -> ProviderUsage {
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
) -> ProviderUsage {
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
        let cached_age = {
            let guard = USAGE_CACHE.lock().unwrap();
            guard
                .get("opencode")
                .map(|(instant, _)| instant.elapsed())
        };
        let now_unix = chrono::Utc::now().timestamp();
        if opencode_needs_refresh(c, cached_age, now_unix) {
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
    // `billing.get` to SolidStart. The `Result::ok` collapse means any
    // error (NoCredential, Shape, transport) is treated identically:
    // fall through to the SQLite path. The returned ProviderUsage
    // carries an `error` for HTTP-level failures (401, 5xx, shape
    // mismatch) which `choose_opencode_usage` checks below. The
    // `X-Server-Id` header is sourced from the persisted credential's
    // `server_id` field (issue #972); pre-#956 blobs fall through to
    // the legacy default and trigger a process-wide warn-once.
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
    // who hasn't run any auth returns logged_out here.
    let _token = match read_opencode_token(auth_path) {
        Ok(t) => t,
        Err(e) => return logged_out("opencode", e.to_string()),
    };

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
    // Pure assembly pins the degradation contract: live wins when it returns
    // a usable envelope, anything else falls through to SQLite.
    choose_opencode_usage(live.as_ref(), sqlite)
}

/// Fires the live `_server billing.get` probe against a parameterized
/// `live_url`. Extracted from `opencode_usage_impl_with_hosts` so the
/// pre-emptive + reactive retry paths share the same wire-binding
/// closure (header set, JSON body, parser) without duplicating the
/// `opencode_live_request_parts` + `fetch_usage` composition.
fn opencode_live_request_at(
    live_url: &str,
    cred: &OpenCodeConsoleCred,
) -> Option<ProviderUsage> {
    let (token, workspace_id, server_id) = opencode_live_request_parts(cred)?;
    let live_url_owned = live_url.to_string();
    Some(fetch_usage(
        "opencode",
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

/// True when the live probe returned an HTTP 401 (the "refresh-on-the-
/// spot" trigger). The error string carries the status code via the
/// `fetch_usage` formatter (`"API error 401: ..."`); substring matching
/// is enough because the only 401 this fetcher produces is from the
/// `_server billing.get` endpoint, not a cloud-hosted generic 401.
fn needs_retry_on_401(live: Option<&ProviderUsage>) -> bool {
    live.and_then(|u| u.error.as_deref())
        .map(|e| e.contains("401"))
        .unwrap_or(false)
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

// ─── Cursor CLI Usage Probe (Issue #1173) ───────────────────────────────────

#[derive(Deserialize, Debug, Clone)]
#[allow(dead_code)]
pub struct CursorModelUsage {
    #[serde(rename = "numRequests", default)]
    pub num_requests: Option<f64>,
    #[serde(rename = "numSlowRequests", default)]
    pub num_slow_requests: Option<f64>,
    #[serde(rename = "maxRequestUsage", default)]
    pub max_request_usage: Option<f64>,
    #[serde(rename = "maxTokenUsage", default)]
    pub max_token_usage: Option<f64>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct CursorUsageResponse {
    #[serde(rename = "gpt-4", default)]
    pub gpt_4: Option<CursorModelUsage>,
    #[serde(rename = "startOfMonth", default)]
    pub start_of_month: Option<String>,
}

/// Compute the next calendar month reset timestamp in RFC3339 format given
/// an ISO 8601 / RFC3339 start-of-month string (e.g. `"2026-08-01T00:00:00.000Z"`).
fn compute_next_month_reset(start_of_month: &str) -> Option<String> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(start_of_month) {
        let utc = dt.with_timezone(&chrono::Utc);
        let (year, month) = if utc.month() == 12 {
            (utc.year() + 1, 1)
        } else {
            (utc.year(), utc.month() + 1)
        };
        chrono::NaiveDate::from_ymd_opt(year, month, 1)
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|naive| naive.and_utc().to_rfc3339())
    } else {
        None
    }
}

/// Parse Cursor quota & usage payload (`GET https://api2.cursor.sh/auth/usage`).
fn parse_cursor_response(body: &str) -> Result<(Vec<UsageWindow>, Option<String>), UsageError> {
    let resp: CursorUsageResponse =
        serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;

    let resets_at = resp
        .start_of_month
        .as_deref()
        .and_then(compute_next_month_reset);

    let mut windows = Vec::new();
    let mut detail = None;

    if let Some(gpt4) = resp.gpt_4 {
        let num = gpt4.num_requests.unwrap_or(0.0);
        let slow = gpt4.num_slow_requests.unwrap_or(0.0);
        let max = gpt4.max_request_usage;

        let used_percent = max.filter(|m| *m > 0.0).map(|m| (num / m) * 100.0);

        windows.push(UsageWindow {
            label: "Fast Requests".to_string(),
            used_percent,
            resets_at: resets_at.clone(),
        });

        if let Some(m) = max {
            if m > 0.0 {
                let remaining = (m - num).max(0.0);
                if slow > 0.0 {
                    detail = Some(format!(
                        "{} of {} fast requests remaining ({} slow requests used)",
                        remaining as i64, m as i64, slow as i64
                    ));
                } else {
                    detail = Some(format!(
                        "{} of {} fast requests remaining",
                        remaining as i64, m as i64
                    ));
                }
            }
        } else if num > 0.0 {
            detail = Some(format!("{} requests used this billing period", num as i64));
        }
    }

    if windows.is_empty() {
        detail = Some("No active Cursor usage windows".to_string());
    }

    Ok((windows, detail))
}

/// Read access token from Cursor's global SQLite database (`state.vscdb`),
/// table `ItemTable`, key `cursorAuth/accessToken`.
fn read_cursor_sqlite_token(path: &Path) -> Result<String, UsageError> {
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| UsageError::NoCredential(format!("{}: {}", path.display(), e)))?;

    let mut stmt = conn
        .prepare("SELECT value FROM ItemTable WHERE key = 'cursorAuth/accessToken'")
        .map_err(|e| UsageError::Shape(format!("Failed to prepare ItemTable query: {}", e)))?;

    let raw: String = stmt
        .query_row([], |row| row.get(0))
        .map_err(|e| UsageError::NoCredential(format!("Key cursorAuth/accessToken not found in {}: {}", path.display(), e)))?;

    let token = if let Ok(parsed) = serde_json::from_str::<String>(&raw) {
        parsed
    } else {
        raw.trim().to_string()
    };

    if token.is_empty() {
        return Err(UsageError::NoCredential(format!("{}: empty token", path.display())));
    }

    Ok(token)
}

/// Read access token from secondary JSON auth file (`auth.json`).
fn read_cursor_auth_json(path: &Path) -> Result<String, UsageError> {
    let content = fs::read_to_string(path)
        .map_err(|_| UsageError::NoCredential(path.to_string_lossy().to_string()))?;

    #[derive(Deserialize)]
    struct CursorAuthFile {
        #[serde(rename = "accessToken", default)]
        access_token_camel: Option<String>,
        #[serde(rename = "access_token", default)]
        access_token_snake: Option<String>,
        #[serde(default)]
        token: Option<String>,
    }

    let parsed: CursorAuthFile = serde_json::from_str(&content)
        .map_err(|e| UsageError::Shape(e.to_string()))?;

    parsed
        .access_token_camel
        .or(parsed.access_token_snake)
        .or(parsed.token)
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| UsageError::NoCredential(path.to_string_lossy().to_string()))
}

/// Discover candidate credential sources for Cursor.
///
/// Priority:
/// 1. `CURSOR_API_KEY` environment variable.
/// 2. Platform-specific `state.vscdb` globalStorage SQLite database:
///    - Windows: `%APPDATA%\Cursor\User\globalStorage\state.vscdb`
///    - macOS: `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb`
///    - Linux: `~/.config/Cursor/User/globalStorage/state.vscdb` (and `$XDG_CONFIG_HOME`)
/// 3. Secondary JSON fallback: `~/.cursor/auth.json`
/// 4. WSL fallback on Windows: `/home/<USERNAME>/.config/Cursor/User/globalStorage/state.vscdb`
///    and `/home/<USERNAME>/.cursor/auth.json` mapped via `env::to_host_path`.
fn discover_cursor_auth_sources() -> (Option<String>, Vec<PathBuf>) {
    let env_token = env::var("CURSOR_API_KEY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let mut paths = Vec::new();

    // Windows %APPDATA%
    if let Ok(appdata) = env::var("APPDATA") {
        if !appdata.is_empty() {
            paths.push(
                PathBuf::from(appdata)
                    .join("Cursor")
                    .join("User")
                    .join("globalStorage")
                    .join("state.vscdb"),
            );
        }
    }

    // macOS ~/Library/Application Support/Cursor/User/globalStorage/state.vscdb
    paths.push(
        home_dir()
            .join("Library")
            .join("Application Support")
            .join("Cursor")
            .join("User")
            .join("globalStorage")
            .join("state.vscdb"),
    );

    // Linux XDG_CONFIG_HOME / ~/.config
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            paths.push(
                PathBuf::from(xdg)
                    .join("Cursor")
                    .join("User")
                    .join("globalStorage")
                    .join("state.vscdb"),
            );
        }
    }
    paths.push(
        home_dir()
            .join(".config")
            .join("Cursor")
            .join("User")
            .join("globalStorage")
            .join("state.vscdb"),
    );

    // Secondary JSON fallback: ~/.cursor/auth.json
    paths.push(home_dir().join(".cursor").join("auth.json"));

    // WSL fallback (Windows host only)
    #[cfg(target_os = "windows")]
    {
        if let Some(username) = env::var("USERNAME").ok().filter(|s| !s.is_empty()) {
            let wsl_sqlite_path = format!(
                "/home/{}/.config/Cursor/User/globalStorage/state.vscdb",
                username
            );
            let host_sqlite_path = crate::env::to_host_path(&wsl_sqlite_path);
            if host_sqlite_path != wsl_sqlite_path {
                paths.push(PathBuf::from(host_sqlite_path));
            }

            let wsl_json_path = format!("/home/{}/.cursor/auth.json", username);
            let host_json_path = crate::env::to_host_path(&wsl_json_path);
            if host_json_path != wsl_json_path {
                paths.push(PathBuf::from(host_json_path));
            }
        }
    }

    (env_token, paths)
}

/// Walk candidate sources and extract the first valid Cursor token.
fn read_cursor_token_from_candidates(
    env_token: Option<String>,
    candidates: &[PathBuf],
) -> Result<String, UsageError> {
    if let Some(token) = env_token.filter(|t| !t.trim().is_empty()) {
        return Ok(token);
    }

    for path in candidates {
        if !path.exists() {
            continue;
        }
        let is_vscdb = path.extension().and_then(|ext| ext.to_str()) == Some("vscdb");
        if is_vscdb {
            match read_cursor_sqlite_token(path) {
                Ok(token) => return Ok(token),
                Err(UsageError::Shape(e)) => return Err(UsageError::Shape(e)),
                Err(_) => continue,
            }
        } else {
            match read_cursor_auth_json(path) {
                Ok(token) => return Ok(token),
                Err(UsageError::Shape(e)) => return Err(UsageError::Shape(e)),
                Err(_) => continue,
            }
        }
    }

    let first = candidates
        .first()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "Cursor credential store".to_string());
    Err(UsageError::NoCredential(first))
}

/// Public Cursor usage fetcher. Reads credential from environment / `state.vscdb` / `auth.json`
/// and hits the Cursor quota endpoint `https://api2.cursor.sh/auth/usage`.
pub fn cursor_usage() -> ProviderUsage {
    let (env_token, candidates) = discover_cursor_auth_sources();
    cursor_usage_with_sources(env_token, &candidates, "https://api2.cursor.sh/auth/usage")
}

/// Test seam: allows passing custom candidate list, environment token, and mock endpoint.
pub fn cursor_usage_with_sources(
    env_token: Option<String>,
    candidates: &[PathBuf],
    live_url: &str,
) -> ProviderUsage {
    let token = match read_cursor_token_from_candidates(env_token, candidates) {
        Ok(t) => t,
        Err(e) => return logged_out("cursor", e.to_string()),
    };

    let client = match Client::builder().timeout(Duration::from_secs(15)).build() {
        Ok(c) => c,
        Err(e) => return unavailable("cursor", format!("Client error: {}", e)),
    };

    let resp = match client
        .get(live_url)
        .header("Authorization", format!("Bearer {}", token))
        .header("User-Agent", "Mozilla/5.0")
        .send()
    {
        Ok(r) => r,
        Err(e) => return unavailable("cursor", format!("Request failed: {}", e)),
    };

    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return logged_out(
            "cursor",
            "Cursor session expired — run 'cursor-agent login' to log in".to_string(),
        );
    }
    if status.as_u16() == 429 {
        return unavailable(
            "cursor",
            "Rate limited — usage data temporarily unavailable".to_string(),
        );
    }
    if !status.is_success() {
        return unavailable(
            "cursor",
            format!("API error {}: usage endpoint failed", status.as_u16()),
        );
    }

    let body = match resp.text() {
        Ok(b) => b,
        Err(e) => return unavailable("cursor", format!("Failed to read response: {}", e)),
    };

    match parse_cursor_response(&body) {
        Ok((windows, detail)) => ProviderUsage {
            provider: "cursor".to_string(),
            logged_in: true,
            windows,
            balance: None,
            detail,
            error: None,
        },
        Err(e) => unavailable("cursor", format!("Failed to parse response: {}", e)),
    }
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

/// Targeted single-provider cache invalidation (issue #970). The refresh
/// seam in `opencode_usage_impl` calls this on a successful `try_refresh()`
/// so the next [`get_cached_usage`] call cannot return a stale envelope
/// minted before the bearer was rotated. Distinct from [`invalidate_cache`]
/// (which clears every provider) so a refresh on one provider doesn't force
/// re-fetching unrelated providers on the next usage-panel poll.
pub fn invalidate_provider_cache(provider: &str) {
    let mut guard = USAGE_CACHE.lock().unwrap();
    guard.remove(provider);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

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
        // Pinned fixture per spec §2.4: `rate_limit.primaryWindow` +
        // `secondaryWindow` with `usedPercent`, `limitWindowSeconds` (dynamic
        // label), and Unix-epoch `resetAt` that the parser converts to
        // RFC3339. `detail` carries the remaining-percentage phrasing.
        let json = r#"{
            "rate_limit": {
                "primary_window": {
                    "used_percent": 18.5,
                    "limit_window_seconds": 18000,
                    "reset_at": 1755288000
                },
                "secondary_window": {
                    "used_percent": 42.0,
                    "limit_window_seconds": 604800,
                    "reset_at": 1755892800
                },
                "additional_rate_limits": []
            }
        }"#;
        let (windows, detail) = parse_codex_response(json).unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].label, "5-hour");
        assert_eq!(windows[0].used_percent, Some(18.5));
        assert_eq!(windows[0].resets_at.as_deref(), Some("2025-08-15T20:00:00+00:00"));
        assert_eq!(windows[1].label, "Weekly");
        assert_eq!(windows[1].used_percent, Some(42.0));
        // Highest-used window drives the detail phrasing (spec §4 wire
        // normalization: 100.0 - used = remaining).
        assert_eq!(detail.as_deref(), Some("58.0% remaining"));
    }

    #[test]
    fn parse_codex_response_minimal_primary_only() {
        // Minimal valid payload: just `primary_window` with no other fields.
        // The default `5-hour` label kicks in when `limit_window_seconds`
        // is absent.
        let json = r#"{"rate_limit":{"primary_window":{"used_percent":50.0}}}"#;
        let (windows, detail) = parse_codex_response(json).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "5-hour");
        assert_eq!(windows[0].used_percent, Some(50.0));
        assert!(windows[0].resets_at.is_none());
        assert_eq!(detail.as_deref(), Some("50.0% remaining"));
    }

    #[test]
    fn parse_codex_response_additional_rate_limits_are_included() {
        // Spec §2.5: `additionalRateLimits` is an opt-in list for tiers the
        // upstream adds. They're appended after primary + secondary so the
        // order is stable.
        let json = r#"{
            "rate_limit": {
                "primary_window": {"used_percent": 10.0, "limit_window_seconds": 18000},
                "secondary_window": {"used_percent": 20.0, "limit_window_seconds": 604800},
                "additional_rate_limits": [
                    {"used_percent": 30.0, "limit_window_seconds": 86400}
                ]
            }
        }"#;
        let (windows, detail) = parse_codex_response(json).unwrap();
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].label, "5-hour");
        assert_eq!(windows[1].label, "Weekly");
        assert_eq!(windows[2].label, "24h");
        assert_eq!(windows[2].used_percent, Some(30.0));
        // Highest-used drives detail (30% → 70% remaining).
        assert_eq!(detail.as_deref(), Some("70.0% remaining"));
    }

    #[test]
    fn parse_codex_response_dynamic_label_fallback_formats_hours_and_days() {
        // Spec §2.5: any non-standard `limit_window_seconds` falls back to a
        // `"{N}h"` / `"{N}d"` friendly label rather than going blank.
        assert_eq!(format_codex_window_label(3600), "1-hour");
        assert_eq!(format_codex_window_label(7200), "2h");
        assert_eq!(format_codex_window_label(86400), "24h");
        assert_eq!(format_codex_window_label(172800), "2d");
        assert_eq!(format_codex_window_label(2592000), "30d");
        // Non-aligned second count gets a `"{N}s"` literal so an upstream
        // oddity doesn't render an empty label.
        assert_eq!(format_codex_window_label(12345), "12345s");
    }

    #[test]
    fn parse_codex_response_missing_rate_limit_is_shape_error() {
        // A body without the `rate_limit` object is malformed — must fail
        // loudly rather than silently returning zero windows (spec §5 wire
        // invariant: missing data ≠ empty data).
        let err = parse_codex_response(r#"{"foo":"bar"}"#).unwrap_err();
        assert!(matches!(err, UsageError::Shape(_)), "expected Shape error, got {err:?}");
    }

    #[test]
    fn parse_codex_response_filters_windows_without_used_percent() {
        // A window with no `used_percent` is malformed (spec §5: 0.0–100.0
        // consumption is required); filtered rather than silently surfaced
        // as a "5-hour: (no data)" row.
        let json = r#"{
            "rate_limit": {
                "primary_window": {"used_percent": 10.0, "limit_window_seconds": 18000},
                "secondary_window": {"limit_window_seconds": 604800}
            }
        }"#;
        let (windows, detail) = parse_codex_response(json).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "5-hour");
        assert_eq!(detail.as_deref(), Some("90.0% remaining"));
    }

    #[test]
    fn parse_codex_response_all_windows_filtered_reports_detail() {
        // All windows lack `used_percent` → empty `windows` + the
        // user-facing "no active windows" detail rather than a Shape error
        // (the shape IS valid, just every window is incomplete).
        let json = r#"{
            "rate_limit": {
                "primary_window": {"limit_window_seconds": 18000},
                "secondary_window": {"limit_window_seconds": 604800}
            }
        }"#;
        let (windows, detail) = parse_codex_response(json).unwrap();
        assert!(windows.is_empty());
        assert_eq!(detail.as_deref(), Some("No active Codex rate-limit windows"));
    }

    #[test]
    fn parse_codex_response_unix_epoch_reset_at_becomes_rfc3339() {
        // The upstream returns Unix epoch seconds in `resetAt`. The wire
        // contract is RFC3339 (matches Anthropic / Kimi / OpenCode) so the
        // parser converts before populating `resets_at`.
        let json = r#"{
            "rate_limit": {
                "primary_window": {
                    "used_percent": 25.0,
                    "limit_window_seconds": 18000,
                    "reset_at": 1735689600
                }
            }
        }"#;
        let (windows, _) = parse_codex_response(json).unwrap();
        assert_eq!(windows[0].resets_at.as_deref(), Some("2025-01-01T00:00:00+00:00"));
    }

    // ── Codex auth-file parser (spec §2.3) ───────────────────────────────

    #[test]
    fn read_codex_auth_file_legacy_top_level_token() {
        // Most common shape: `access_token` at the root, no `tokens` envelope.
        let dir = std::env::temp_dir().join(format!(
            "codex_legacy_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("auth.json");
        fs::write(
            &path,
            r#"{"access_token":"sk-test-legacy","account_id":"acc-123"}"#,
        )
        .unwrap();
        let creds = read_codex_auth_file(&path).unwrap();
        assert_eq!(creds.access_token, "sk-test-legacy");
        assert_eq!(creds.account_id.as_deref(), Some("acc-123"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_codex_auth_file_nested_tokens_envelope() {
        // Spec §2.3 alternative shape: token nested inside `tokens`.
        // Nested `account_id` wins over the top-level one when present.
        let dir = std::env::temp_dir().join(format!(
            "codex_nested_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("auth.json");
        fs::write(
            &path,
            r#"{
                "OPENAI_API_KEY": null,
                "tokens": {
                    "access_token": "sk-test-nested",
                    "account_id": "acc-nested",
                    "refresh_token": "rt-x",
                    "id_token": "id-x"
                }
            }"#,
        )
        .unwrap();
        let creds = read_codex_auth_file(&path).unwrap();
        assert_eq!(creds.access_token, "sk-test-nested");
        assert_eq!(creds.account_id.as_deref(), Some("acc-nested"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_codex_auth_file_empty_token_is_no_credential() {
        // A logged-out Codex CLI writes `"access_token": ""`. We treat this
        // as missing (NoCredential) rather than handing a blank bearer to
        // reqwest (which would 401 the live probe for the wrong reason).
        let dir = std::env::temp_dir().join(format!(
            "codex_empty_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("auth.json");
        fs::write(&path, r#"{"access_token":""}"#).unwrap();
        let err = read_codex_auth_file(&path).unwrap_err();
        assert!(matches!(err, UsageError::NoCredential(_)), "got {err:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_codex_auth_file_missing_file_is_no_credential() {
        let path = std::env::temp_dir().join("definitely_does_not_exist_codex_auth.json");
        let _ = fs::remove_file(&path);
        let err = read_codex_auth_file(&path).unwrap_err();
        assert!(matches!(err, UsageError::NoCredential(_)), "got {err:?}");
    }

    #[test]
    fn read_codex_credentials_walks_priority_and_returns_first_match() {
        // First valid candidate wins. A 2-path list where the first exists
        // but is unreadable (NoCredential) advances to the second.
        let dir = std::env::temp_dir().join(format!(
            "codex_walk_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("empty.json");
        fs::write(&empty, r#"{"access_token":""}"#).unwrap();
        let real = dir.join("real.json");
        fs::write(&real, r#"{"access_token":"sk-real"}"#).unwrap();

        let (path, creds) = read_codex_credentials(&[empty.clone(), real.clone()]).unwrap();
        assert_eq!(path, real);
        assert_eq!(creds.access_token, "sk-real");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_codex_credentials_propagates_shape_error() {
        // A malformed auth.json MUST short-circuit (Shape error) rather than
        // silently trying the next candidate — a bad JSON shape isn't fixed
        // by reading a different file.
        let dir = std::env::temp_dir().join(format!(
            "codex_shape_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let broken = dir.join("broken.json");
        fs::write(&broken, r#"{ not valid json"#).unwrap();
        let next = dir.join("next.json");
        fs::write(&next, r#"{"access_token":"sk-next"}"#).unwrap();

        let err = read_codex_credentials(&[broken, next]).unwrap_err();
        assert!(matches!(err, UsageError::Shape(_)), "got {err:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    // ── Codex live-probe mocked HTTP integration (spec §5) ───────────────

    /// Minimal `tiny_http` loopback dispatcher — mirrors the pattern from
    /// `services::opencode_oauth::tests::spawn_loopback` (#967). The Codex
    /// live endpoint is a single GET (`/wham/usage`) so this is a simpler
    /// shape than the opencode refresh-on-401 matrix.
    fn codex_live_loopback(handler: impl Fn(tiny_http::Request) + Send + 'static) -> u16 {
        use std::thread;
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind loopback");
        let port = match server.server_addr() {
            tiny_http::ListenAddr::IP(std::net::SocketAddr::V4(v4)) => v4.port(),
            other => panic!("expected v4 loopback, got {other:?}"),
        };
        thread::spawn(move || {
            for request in server.incoming_requests() {
                handler(request);
            }
        });
        port
    }

    fn codex_temp_home() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codex_home_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const CODEX_USAGE_BODY: &str = r#"{
        "rate_limit": {
            "primary_window":   {"used_percent": 18.5, "limit_window_seconds": 18000, "reset_at": 1755288000},
            "secondary_window": {"used_percent": 42.0, "limit_window_seconds": 604800, "reset_at": 1755892800}
        }
    }"#;

    #[test]
    fn codex_usage_with_paths_happy_path_returns_windows() {
        // The headline happy path: valid auth.json → 200 → wire contract.
        let home = codex_temp_home();
        let auth_path = home.join("auth.json");
        fs::write(&auth_path, r#"{"access_token":"sk-test-ok"}"#).unwrap();
        let candidates = vec![auth_path];

        let port = codex_live_loopback(move |req| {
            let _ = req.respond(tiny_http::Response::from_string(CODEX_USAGE_BODY));
        });
        let url = format!("http://127.0.0.1:{port}/wham/usage");

        let usage = codex_usage_with_paths(&candidates, &url);
        assert_eq!(usage.provider, "codex");
        assert!(usage.logged_in);
        assert!(usage.error.is_none());
        assert_eq!(usage.windows.len(), 2);
        assert_eq!(usage.windows[0].label, "5-hour");
        assert_eq!(usage.windows[0].used_percent, Some(18.5));
        assert_eq!(usage.detail.as_deref(), Some("58.0% remaining"));

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_usage_with_paths_sends_account_id_header_when_present() {
        // Spec §2.1: `ChatGPT-Account-Id` is forwarded when the auth file
        // carries an `account_id` so multi-account subscriptions are routed
        // correctly. The header absence on single-account auth is exercised
        // by the happy-path test above (no `account_id` field).
        let home = codex_temp_home();
        let auth_path = home.join("auth.json");
        fs::write(
            &auth_path,
            r#"{"access_token":"sk-test","account_id":"acc-xyz"}"#,
        )
        .unwrap();
        let candidates = vec![auth_path];

        let observed_header = Arc::new(std::sync::Mutex::new(String::new()));
        let observed_header_t = observed_header.clone();
        let port = codex_live_loopback(move |req| {
            let acct = req
                .headers()
                .iter()
                .find(|h| h.field.equiv("ChatGPT-Account-Id"))
                .map(|h| h.value.as_str().to_string())
                .unwrap_or_default();
            *observed_header_t.lock().unwrap() = acct;
            let _ = req.respond(tiny_http::Response::from_string(CODEX_USAGE_BODY));
        });
        let url = format!("http://127.0.0.1:{port}/wham/usage");

        let usage = codex_usage_with_paths(&candidates, &url);
        assert!(usage.logged_in);
        assert_eq!(*observed_header.lock().unwrap(), "acc-xyz");

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_usage_with_paths_401_returns_logged_out_with_remediation() {
        // Spec §5 / User Story #8: an expired token transitions to
        // `logged_in = false` with an actionable re-auth hint, NOT a raw
        // "API error 401". This is the headline test that pins the
        // remediation message.
        let home = codex_temp_home();
        let auth_path = home.join("auth.json");
        fs::write(&auth_path, r#"{"access_token":"sk-expired"}"#).unwrap();
        let candidates = vec![auth_path];

        let port = codex_live_loopback(move |req| {
            let _ = req.respond(
                tiny_http::Response::from_string(r#"{"error":"session expired"}"#)
                    .with_status_code(401),
            );
        });
        let url = format!("http://127.0.0.1:{port}/wham/usage");

        let usage = codex_usage_with_paths(&candidates, &url);
        assert!(!usage.logged_in);
        let err = usage.error.unwrap_or_default();
        assert!(
            err.contains("codex") && err.contains("terminal"),
            "remediation message must mention `codex` and `terminal`, got: {err:?}"
        );
        assert!(usage.windows.is_empty());

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_usage_with_paths_403_returns_logged_out_with_remediation() {
        // 403 (Forbidden) collapses with 401 to the same logged-out branch —
        // an account-revoked token behaves identically to an expired one
        // from the user's perspective.
        let home = codex_temp_home();
        let auth_path = home.join("auth.json");
        fs::write(&auth_path, r#"{"access_token":"sk-revoked"}"#).unwrap();
        let candidates = vec![auth_path];

        let port = codex_live_loopback(move |req| {
            let _ = req.respond(
                tiny_http::Response::from_string(r#"{}"#).with_status_code(403),
            );
        });
        let url = format!("http://127.0.0.1:{port}/wham/usage");

        let usage = codex_usage_with_paths(&candidates, &url);
        assert!(!usage.logged_in);
        assert!(usage
            .error
            .as_deref()
            .map(|e| e.contains("terminal"))
            .unwrap_or(false));

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_usage_with_paths_429_preserves_logged_in_and_surfaces_unavailable() {
        // Spec §5: network timeouts and rate limits preserve logged-in
        // status. A 429 returns `unavailable()` (logged_in=true, error=Some)
        // so the UI renders a temporary-availability notice without
        // triggering the re-auth affordance.
        let home = codex_temp_home();
        let auth_path = home.join("auth.json");
        fs::write(&auth_path, r#"{"access_token":"sk-test"}"#).unwrap();
        let candidates = vec![auth_path];

        let port = codex_live_loopback(move |req| {
            let _ = req.respond(tiny_http::Response::empty(429));
        });
        let url = format!("http://127.0.0.1:{port}/wham/usage");

        let usage = codex_usage_with_paths(&candidates, &url);
        assert!(usage.logged_in);
        assert!(usage.error.as_deref().map(|e| e.contains("Rate limited")).unwrap_or(false));

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_usage_with_paths_no_credential_returns_logged_out() {
        // No auth.json on disk → logged_out with the first candidate path
        // surfaced in the error so the user knows where we looked.
        let home = codex_temp_home();
        let candidates = vec![home.join(".codex").join("auth.json")];

        let usage = codex_usage_with_paths(&candidates, "http://127.0.0.1:1/wham/usage");
        assert!(!usage.logged_in);
        assert!(usage.error.is_some());
        assert!(usage.windows.is_empty());

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_usage_with_paths_malformed_body_returns_unavailable() {
        // A 200 with garbage JSON: shape error → unavailable (logged_in=true).
        let home = codex_temp_home();
        let auth_path = home.join("auth.json");
        fs::write(&auth_path, r#"{"access_token":"sk-test"}"#).unwrap();
        let candidates = vec![auth_path];

        let port = codex_live_loopback(move |req| {
            let _ = req.respond(tiny_http::Response::from_string("not json"));
        });
        let url = format!("http://127.0.0.1:{port}/wham/usage");

        let usage = codex_usage_with_paths(&candidates, &url);
        assert!(usage.logged_in);
        assert!(usage.error.as_deref().map(|e| e.contains("parse")).unwrap_or(false));

        let _ = fs::remove_dir_all(&home);
    }

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

    fn openai_temp_home() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "openai_test_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

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

        let usage = openai_usage_with_base_url("sk-admin-test", &base);
        assert_eq!(usage.provider, "openai");
        assert!(usage.logged_in);
        assert!(usage.error.is_none());
        let balance = usage.balance.expect("admin key must populate balance");
        assert_eq!(balance.monthly_spend, Some(12.5));
        assert_eq!(balance.currency, "USD");
        assert!(usage.detail.is_none());
        let _ = fs::remove_dir_all(openai_temp_home());
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

        let usage = openai_usage_with_base_url("sk-proj-test", &base);
        assert_eq!(usage.provider, "openai");
        assert!(usage.logged_in, "project key on org costs must NOT log out");
        assert!(usage.error.is_none(), "degradation must not carry an error");
        assert!(usage.balance.is_none(), "no balance when costs 403");
        let detail = usage.detail.expect("degradation detail must be set");
        assert!(
            detail.contains("Organization Admin") && detail.contains("sk-admin"),
            "detail must explain the org-admin requirement, got: {detail:?}"
        );
        let _ = fs::remove_dir_all(openai_temp_home());
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

        let usage = openai_usage_with_base_url("sk-bad", &base);
        assert_eq!(usage.provider, "openai");
        assert!(!usage.logged_in);
        assert_eq!(usage.error.as_deref(), Some("Invalid API key"));
        assert!(usage.balance.is_none());
        assert!(usage.detail.is_none());
        let _ = fs::remove_dir_all(openai_temp_home());
    }

    #[test]
    fn openai_usage_with_empty_key_returns_logged_out() {
        // Mirror `kimi_usage("")` / `openrouter_usage("")`: the
        // configured-key gate should catch a missing key, but the fetcher
        // still defends with a logged-out message so a misconfigured call
        // surfaces "no key" instead of a confusing 401.
        let usage = openai_usage_with_base_url("", "http://127.0.0.1:1");
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

        let usage = openai_usage_with_base_url("sk-test", &base);
        assert!(usage.logged_in, "429 must not flip to logged_out");
        assert!(usage.error.as_deref().map(|e| e.contains("Rate limited")).unwrap_or(false));
        let _ = fs::remove_dir_all(openai_temp_home());
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
        let usage = parse_grok_response(json).unwrap();
        assert_eq!(usage.windows.len(), 1);
        assert_eq!(usage.windows[0].label, "Weekly Pool");
        assert_eq!(usage.windows[0].used_percent, Some(37.0));
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

    fn spawn_loopback<F>(max_requests: usize, handler: F) -> u16
    where
        F: Fn(tiny_http::Request) + Send + 'static,
    {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::thread;

        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind loopback");
        let port = match server.server_addr() {
            tiny_http::ListenAddr::IP(std::net::SocketAddr::V4(v4)) => v4.port(),
            other => panic!("expected a v4 loopback listener, got {other:?}"),
        };
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_thread = counter.clone();
        thread::spawn(move || {
            for request in server.incoming_requests() {
                handler(request);
                if counter_thread.fetch_add(1, Ordering::SeqCst) + 1 >= max_requests {
                    return;
                }
            }
        });
        port
    }

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
        let usage = opencode_usage_impl_with_hosts(
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
        assert!(usage.logged_in, "live result wins over the SQLite fallback");
        assert!(usage.error.is_none(), "live path carries no error: {:?}", usage.error);
        assert_eq!(usage.windows.len(), 3);
        assert_eq!(usage.windows[0].label, "5-hour");
        assert_eq!(usage.windows[0].used_percent, Some(25.0));

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
        assert!(usage.logged_in, "sqlite fallback is logged_in");
        assert!(
            usage.error.is_none(),
            "sqlite fallback must clear the live 401 error: {:?}",
            usage.error
        );
        assert_eq!(usage.windows.len(), 3);
        assert_eq!(usage.windows[0].label, "5-hour");
        assert_eq!(
            usage.windows[0].used_percent,
            Some(50.0),
            "sqlite fallback returns the seeded 50% 5-hour window"
        );

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
        assert!(usage.logged_in);
        assert!(usage.error.is_none(), "live success: {:?}", usage.error);
        assert_eq!(usage.windows.len(), 3);
        assert_eq!(usage.windows[0].used_percent, Some(25.0));

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
        assert!(usage.logged_in, "live result wins over the SQLite fallback");
        assert!(usage.error.is_none(), "live eventually succeeds: {:?}", usage.error);
        // Three windows from the retry's success body — proves the
        // second live call's token was accepted (the seeded SQLite
        // fallback would have been 50%/...).
        assert_eq!(usage.windows.len(), 3);
        assert_eq!(usage.windows[0].label, "5-hour");
        assert_eq!(usage.windows[0].used_percent, Some(25.0));

        let _ = fs::remove_dir_all(&temp);
    }

    // ─── Cursor CLI Usage Probe Tests (Issue #1173) ─────────────────────────

    #[test]
    fn test_parse_cursor_response_fast_requests() {
        let json = r#"{
            "gpt-4": {
                "numRequests": 125,
                "numSlowRequests": 0,
                "maxRequestUsage": 500,
                "maxTokenUsage": null
            },
            "startOfMonth": "2026-08-01T00:00:00.000Z"
        }"#;

        let (windows, detail) = parse_cursor_response(json).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "Fast Requests");
        assert_eq!(windows[0].used_percent, Some(25.0));
        assert_eq!(windows[0].resets_at.as_deref(), Some("2026-09-01T00:00:00+00:00"));
        assert_eq!(detail.as_deref(), Some("375 of 500 fast requests remaining"));
    }

    #[test]
    fn test_parse_cursor_response_with_slow_requests() {
        let json = r#"{
            "gpt-4": {
                "numRequests": 450,
                "numSlowRequests": 12,
                "maxRequestUsage": 500
            },
            "startOfMonth": "2026-08-01T00:00:00Z"
        }"#;

        let (windows, detail) = parse_cursor_response(json).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].used_percent, Some(90.0));
        assert_eq!(
            detail.as_deref(),
            Some("50 of 500 fast requests remaining (12 slow requests used)")
        );
    }

    #[test]
    fn test_parse_cursor_response_unlimited_requests() {
        let json = r#"{
            "gpt-4": {
                "numRequests": 88
            },
            "startOfMonth": "2026-08-01T00:00:00Z"
        }"#;

        let (windows, detail) = parse_cursor_response(json).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].used_percent, None);
        assert_eq!(detail.as_deref(), Some("88 requests used this billing period"));
    }

    #[test]
    fn test_parse_cursor_response_empty_is_empty_windows() {
        let json = r#"{
            "startOfMonth": "2026-08-01T00:00:00Z"
        }"#;

        let (windows, detail) = parse_cursor_response(json).unwrap();
        assert!(windows.is_empty());
        assert_eq!(detail.as_deref(), Some("No active Cursor usage windows"));
    }

    #[test]
    fn test_parse_cursor_response_invalid_shape() {
        let err = parse_cursor_response("not-json").unwrap_err();
        assert!(matches!(err, UsageError::Shape(_)));
    }

    #[test]
    fn test_compute_next_month_reset_year_boundary() {
        let dec = "2026-12-01T00:00:00Z";
        let next = compute_next_month_reset(dec).unwrap();
        assert_eq!(next, "2027-01-01T00:00:00+00:00");
    }

    #[test]
    fn test_read_cursor_sqlite_token_plain_and_json() {
        let dir = std::env::temp_dir().join(format!(
            "cursor_vscdb_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("state.vscdb");

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable (key, value) VALUES ('cursorAuth/accessToken', '\"test-jwt-token\"')",
            [],
        )
        .unwrap();
        drop(conn);

        let token = read_cursor_sqlite_token(&db_path).unwrap();
        assert_eq!(token, "test-jwt-token");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_cursor_sqlite_token_missing_key() {
        let dir = std::env::temp_dir().join(format!(
            "cursor_vscdb_empty_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("state.vscdb");

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        drop(conn);

        let err = read_cursor_sqlite_token(&db_path).unwrap_err();
        assert!(matches!(err, UsageError::NoCredential(_)));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_cursor_auth_json_variants() {
        let dir = std::env::temp_dir().join(format!(
            "cursor_auth_json_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let json_path = dir.join("auth.json");

        fs::write(&json_path, r#"{"accessToken": "tok-camel"}"#).unwrap();
        assert_eq!(read_cursor_auth_json(&json_path).unwrap(), "tok-camel");

        fs::write(&json_path, r#"{"access_token": "tok-snake"}"#).unwrap();
        assert_eq!(read_cursor_auth_json(&json_path).unwrap(), "tok-snake");

        fs::write(&json_path, r#"{"token": "tok-plain"}"#).unwrap();
        assert_eq!(read_cursor_auth_json(&json_path).unwrap(), "tok-plain");

        fs::write(&json_path, r#"{"other": "value"}"#).unwrap();
        assert!(matches!(
            read_cursor_auth_json(&json_path).unwrap_err(),
            UsageError::NoCredential(_)
        ));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_cursor_token_candidate_priority() {
        let dir = std::env::temp_dir().join(format!(
            "cursor_priority_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let db_path = dir.join("state.vscdb");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable (key, value) VALUES ('cursorAuth/accessToken', 'db-token')",
            [],
        )
        .unwrap();
        drop(conn);

        let json_path = dir.join("auth.json");
        fs::write(&json_path, r#"{"accessToken": "json-token"}"#).unwrap();

        // 1. Env token wins over candidates
        let tok1 = read_cursor_token_from_candidates(
            Some("env-token".to_string()),
            &[db_path.clone(), json_path.clone()],
        )
        .unwrap();
        assert_eq!(tok1, "env-token");

        // 2. DB candidate wins when env token is None
        let tok2 = read_cursor_token_from_candidates(
            None,
            &[db_path.clone(), json_path.clone()],
        )
        .unwrap();
        assert_eq!(tok2, "db-token");

        // 3. JSON candidate used when DB is absent
        let tok3 = read_cursor_token_from_candidates(
            None,
            &[dir.join("nonexistent.vscdb"), json_path.clone()],
        )
        .unwrap();
        assert_eq!(tok3, "json-token");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_cursor_usage_with_sources_live_loopback() {
        let dir = std::env::temp_dir().join(format!(
            "cursor_loopback_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let json_path = dir.join("auth.json");
        fs::write(&json_path, r#"{"accessToken": "test-key"}"#).unwrap();

        // Success 200 OK
        let port_ok = spawn_loopback(1, |req| {
            assert_eq!(
                req.headers()
                    .iter()
                    .find(|h| h.field.equiv("Authorization"))
                    .map(|h| h.value.as_str()),
                Some("Bearer test-key")
            );
            let body = r#"{
                "gpt-4": {
                    "numRequests": 100,
                    "maxRequestUsage": 500
                },
                "startOfMonth": "2026-08-01T00:00:00Z"
            }"#;
            let _ = req.respond(tiny_http::Response::from_string(body));
        });
        let usage = cursor_usage_with_sources(
            None,
            std::slice::from_ref(&json_path),
            &format!("http://127.0.0.1:{port_ok}/auth/usage"),
        );
        assert!(usage.logged_in);
        assert_eq!(usage.windows.len(), 1);
        assert_eq!(usage.windows[0].used_percent, Some(20.0));
        assert!(usage.error.is_none());

        // 401 Unauthorized -> logged_out
        let port_401 = spawn_loopback(1, |req| {
            let _ = req.respond(
                tiny_http::Response::from_string(r#"{"error":"unauthorized"}"#)
                    .with_status_code(401),
            );
        });
        let usage_401 = cursor_usage_with_sources(
            None,
            std::slice::from_ref(&json_path),
            &format!("http://127.0.0.1:{port_401}/auth/usage"),
        );
        assert!(!usage_401.logged_in);
        assert!(usage_401.error.as_deref().unwrap().contains("cursor-agent login"));

        // 429 Rate Limited -> unavailable
        let port_429 = spawn_loopback(1, |req| {
            let _ = req.respond(
                tiny_http::Response::from_string(r#"{"error":"too many requests"}"#)
                    .with_status_code(429),
            );
        });
        let usage_429 = cursor_usage_with_sources(
            None,
            std::slice::from_ref(&json_path),
            &format!("http://127.0.0.1:{port_429}/auth/usage"),
        );
        assert!(usage_429.logged_in);
        assert!(usage_429.error.as_deref().unwrap().contains("Rate limited"));

        let _ = fs::remove_dir_all(&dir);
    }

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
        let usage = deepseek_usage("");
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
        let usage = deepseek_usage_with_url("sk-test", &url);
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
        let usage = deepseek_usage_with_url("sk-bad", &url);
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
        let usage = deepseek_usage_with_url("sk-revoked", &url);
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
        let usage = deepseek_usage_with_url("sk-test", &url);
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
        let usage = deepseek_usage_with_url("sk-test", &url);
        assert!(usage.logged_in);
        assert!(usage
            .error
            .as_deref()
            .map(|e| e.contains("parse") || e.contains("Shape"))
            .unwrap_or(false));
        assert!(usage.balance.is_none());
    }
}

