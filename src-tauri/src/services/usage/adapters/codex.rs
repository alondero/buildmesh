//! Codex CLI native adapter — self-authenticates via `~/.codex/auth.json`
//! (`$CODEX_HOME` override + WSL fallback), detection-gated on `codex`.
//!
//! ChatGPT Plus/Pro keep rolling quota windows. Business and Enterprise
//! replies may omit `rate_limit` entirely and instead report `plan_type`,
//! `credits`, `spend_control`, and top-level `additional_rate_limits`.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{shared_client, UsageAdapter};
use crate::services::usage::types::{
    home_dir, logged_out, unavailable, BillingBalance, ProviderUsage, UsageAmount, UsageError,
    UsageMeter, UsageWindow,
};
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

/// Drop-in [`UsageAdapter`] for `codex`.
pub(crate) struct CodexAdapter;

impl UsageAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("codex")
    }

    fn fetch(&self, _accounts: &[ProviderAccount]) -> ProviderUsage {
        codex_usage()
    }
}

/// Public Codex fetcher. Walks the discovery list, hits the ChatGPT quota
/// endpoint, and reports the wire contract.
pub(crate) fn codex_usage() -> ProviderUsage {
    let candidates = auth_candidates();
    let endpoint = usage_endpoint();
    codex_usage_with_paths(&candidates, &endpoint)
}

fn auth_candidates() -> Vec<PathBuf> {
    #[cfg(test)]
    if let Some(paths) = CANDIDATES_OVERRIDE.with(|cell| cell.borrow().clone()) {
        return paths;
    }
    discover_codex_auth_paths()
}

fn usage_endpoint() -> String {
    #[cfg(test)]
    if let Some(url) = ENDPOINT_OVERRIDE.with(|cell| cell.borrow().clone()) {
        return url;
    }
    CODEX_USAGE_URL.to_string()
}

#[cfg(test)]
thread_local! {
    static ENDPOINT_OVERRIDE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
    static CANDIDATES_OVERRIDE: std::cell::RefCell<Option<Vec<PathBuf>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
struct EndpointOverrideGuard {
    previous: Option<String>,
}

#[cfg(test)]
impl Drop for EndpointOverrideGuard {
    fn drop(&mut self) {
        ENDPOINT_OVERRIDE.with(|cell| {
            *cell.borrow_mut() = self.previous.take();
        });
    }
}

#[cfg(test)]
fn with_adapter_loopback<F, R>(candidates: Vec<PathBuf>, url: String, f: F) -> R
where
    F: FnOnce() -> R,
{
    let previous_url = ENDPOINT_OVERRIDE.with(|cell| cell.borrow_mut().replace(url));
    let previous_paths = CANDIDATES_OVERRIDE.with(|cell| cell.borrow_mut().replace(candidates));
    let _url_guard = EndpointOverrideGuard {
        previous: previous_url,
    };
    let _paths_guard = CandidatesOverrideGuard {
        previous: previous_paths,
    };
    f()
}

#[cfg(test)]
struct CandidatesOverrideGuard {
    previous: Option<Vec<PathBuf>>,
}

#[cfg(test)]
impl Drop for CandidatesOverrideGuard {
    fn drop(&mut self) {
        CANDIDATES_OVERRIDE.with(|cell| {
            *cell.borrow_mut() = self.previous.take();
        });
    }
}

/// Build the ordered list of candidate Codex auth.json paths (issue #1108,
/// spec §2.2). Priority:
///
/// 1. `$CODEX_HOME/auth.json` if set and non-empty.
/// 2. `<home>/.codex/auth.json` (Windows host default).
/// 3. WSL fallback (Windows host only): the default-WSL-distro UNC form of
///    `/home/<USERNAME>/.codex/auth.json`, built via `env::to_host_path` so
///    the UNC string never escapes the `host_path` module.
fn discover_codex_auth_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Ok(codex_home) = env::var("CODEX_HOME") {
        if !codex_home.is_empty() {
            paths.push(PathBuf::from(codex_home).join("auth.json"));
        }
    }

    paths.push(home_dir().join(".codex").join("auth.json"));

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
struct CodexAuthCredentials {
    access_token: String,
    account_id: Option<String>,
}

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
        let non_empty =
            |s: &Option<String>| s.as_deref().filter(|v| !v.is_empty()).map(str::to_owned);

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

fn read_codex_auth_file(path: &Path) -> Result<CodexAuthCredentials, UsageError> {
    let content = fs::read_to_string(path)
        .map_err(|_| UsageError::NoCredential(path.to_string_lossy().to_string()))?;
    let cred: CodexAuthFile =
        serde_json::from_str(&content).map_err(|e| UsageError::Shape(e.to_string()))?;
    cred.extract_credentials()
        .ok_or_else(|| UsageError::NoCredential(path.to_string_lossy().to_string()))
}

fn read_codex_credentials(
    candidates: &[PathBuf],
) -> Result<(PathBuf, CodexAuthCredentials), UsageError> {
    let first = candidates.first().cloned().unwrap_or_default();
    for path in candidates {
        if path.exists() {
            match read_codex_auth_file(path) {
                Ok(creds) => return Ok((path.clone(), creds)),
                Err(UsageError::Shape(e)) => return Err(UsageError::Shape(e)),
                Err(_) => continue,
            }
        }
    }
    Err(UsageError::NoCredential(
        first.to_string_lossy().to_string(),
    ))
}

/// JSON number that ChatGPT sometimes emits as a number and sometimes as a
/// decimal string (`"25000"`, `"17000.50"`).
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
enum JsonNumber {
    F(f64),
    I(i64),
    U(u64),
    S(String),
}

impl JsonNumber {
    fn to_f64(&self) -> Option<f64> {
        match self {
            Self::F(v) => Some(*v),
            Self::I(v) => Some(*v as f64),
            Self::U(v) => Some(*v as f64),
            Self::S(s) => s.parse().ok(),
        }
    }

    fn to_i64(&self) -> Option<i64> {
        match self {
            Self::F(v) if v.is_finite() => Some(*v as i64),
            Self::I(v) => Some(*v),
            Self::U(v) => i64::try_from(*v).ok(),
            Self::S(s) => s.parse().ok(),
            Self::F(_) => None,
        }
    }
}

#[derive(Deserialize, Debug)]
struct CodexRateWindow {
    used_percent: Option<JsonNumber>,
    limit_window_seconds: Option<JsonNumber>,
    reset_at: Option<JsonNumber>,
}

#[derive(Deserialize, Debug)]
struct CodexRateLimits {
    primary_window: Option<CodexRateWindow>,
    secondary_window: Option<CodexRateWindow>,
    #[serde(default)]
    additional_rate_limits: Vec<CodexRateWindow>,
}

#[derive(Deserialize, Debug)]
struct CodexCredits {
    #[serde(default)]
    unlimited: bool,
    #[serde(default)]
    balance: Option<JsonNumber>,
}

#[derive(Deserialize, Debug)]
struct CodexSpendLimit {
    #[serde(default)]
    used: Option<JsonNumber>,
    #[serde(default)]
    limit: Option<JsonNumber>,
    #[serde(default)]
    remaining: Option<JsonNumber>,
    #[serde(default)]
    used_percent: Option<JsonNumber>,
    #[serde(default)]
    reset_at: Option<JsonNumber>,
}

#[derive(Deserialize, Debug)]
struct CodexSpendControl {
    #[serde(default)]
    individual_limit: Option<CodexSpendLimit>,
}

#[derive(Deserialize, Debug)]
struct CodexNamedAdditional {
    limit_name: String,
    #[serde(default)]
    rate_limit: Option<CodexRateLimits>,
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum CodexAdditionalLimit {
    Named(CodexNamedAdditional),
    Window(CodexRateWindow),
}

#[derive(Deserialize, Debug)]
struct CodexUsageResp {
    // `plan_type` was dropped after #1689 / #1686 — ProviderUsage no
    // longer carries plan, so the parsed value has nowhere to land.
    // Serde's default silently ignores the field, which keeps cached
    // / replayed payloads that still include it parsing cleanly.
    #[serde(default)]
    rate_limit: Option<CodexRateLimits>,
    #[serde(default)]
    credits: Option<CodexCredits>,
    #[serde(default)]
    spend_control: Option<CodexSpendControl>,
    #[serde(default)]
    additional_rate_limits: Option<Vec<CodexAdditionalLimit>>,
}

struct CodexParsed {
    windows: Vec<UsageWindow>,
    balance: Option<BillingBalance>,
    meters: Vec<UsageMeter>,
    detail: Option<String>,
}

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

fn rfc3339_from_epoch(epoch: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(epoch, 0).map(|dt| dt.to_rfc3339())
}

fn push_window(
    window: CodexRateWindow,
    label_override: Option<String>,
    windows: &mut Vec<UsageWindow>,
) {
    let Some(used) = window.used_percent.and_then(|n| n.to_f64()) else {
        return;
    };
    let duration_secs = window
        .limit_window_seconds
        .as_ref()
        .and_then(|n| n.to_i64());
    let duration_label = duration_secs
        .map(format_codex_window_label)
        .unwrap_or_else(|| "5-hour".to_string());
    let label = match label_override {
        Some(name) if !name.is_empty() => {
            if duration_secs.is_none() {
                name
            } else {
                format!("{name} · {duration_label}")
            }
        }
        _ => duration_label,
    };
    let resets_at = window
        .reset_at
        .and_then(|n| n.to_i64())
        .and_then(rfc3339_from_epoch);
    windows.push(UsageWindow {
        label,
        used_percent: Some(used),
        resets_at,
    });
}

fn push_rate_limits(
    limits: CodexRateLimits,
    label_prefix: Option<&str>,
    windows: &mut Vec<UsageWindow>,
) {
    if let Some(primary) = limits.primary_window {
        push_window(primary, label_prefix.map(str::to_string), windows);
    }
    if let Some(secondary) = limits.secondary_window {
        push_window(
            secondary,
            label_prefix.map(str::to_string),
            windows,
        );
    }
    for additional in limits.additional_rate_limits {
        push_window(
            additional,
            label_prefix.map(str::to_string),
            windows,
        );
    }
}

fn parse_codex_response(body: &str) -> Result<CodexParsed, UsageError> {
    let resp: CodexUsageResp =
        serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;

    let mut windows = Vec::new();

    if let Some(rate_limit) = resp.rate_limit {
        push_rate_limits(rate_limit, None, &mut windows);
    }
    for additional in resp.additional_rate_limits.unwrap_or_default() {
        match additional {
            CodexAdditionalLimit::Named(named) => {
                if let Some(limits) = named.rate_limit {
                    push_rate_limits(
                        limits,
                        Some(named.limit_name.as_str()),
                        &mut windows,
                    );
                }
            }
            CodexAdditionalLimit::Window(window) => {
                push_window(window, None, &mut windows);
            }
        }
    }

    let mut meters = Vec::new();
    let mut balance = None;

    if let Some(credits) = resp.credits {
        if credits.unlimited {
            meters.push(UsageMeter::Unlimited);
        }
        if let Some(remaining) = credits.balance.and_then(|n| n.to_f64()) {
            balance = Some(BillingBalance {
                remaining,
                monthly_spend: None,
                currency: "credits".to_string(),
            });
        }
    }

    if let Some(spend) = resp.spend_control {
        if let Some(limit) = spend.individual_limit {
            if let Some(used) = limit.used.and_then(|n| n.to_f64()) {
                let amount = UsageAmount {
                    used,
                    limit: limit.limit.and_then(|n| n.to_f64()),
                    remaining: limit.remaining.and_then(|n| n.to_f64()),
                    unit: "credits".to_string(),
                    used_percent: limit.used_percent.and_then(|n| n.to_f64()),
                    resets_at: limit
                        .reset_at
                        .and_then(|n| n.to_i64())
                        .and_then(rfc3339_from_epoch),
                };
                meters.push(if amount.limit.is_some() {
                    UsageMeter::Metered { amount }
                } else {
                    UsageMeter::NoIndividualLimit { amount }
                });
            }
        }
    }

    // The bar's fill width is the inverse of "% remaining", so emitting
    // the string on top of the bar would duplicate the same number.
    // The "No active" fallback below stays because it explains a
    // different case (why nothing is rendered), not a redundant
    // percentage.
    let detail = if windows.is_empty() && meters.is_empty() && balance.is_none() {
        Some("No active Codex rate-limit windows".to_string())
    } else {
        None
    };

    Ok(CodexParsed {
        windows,
        balance,
        meters,
        detail,
    })
}

fn parsed_to_usage(parsed: CodexParsed) -> ProviderUsage {
    ProviderUsage {
        provider: "codex".to_string(),
        logged_in: true,
        windows: parsed.windows,
        balance: parsed.balance,
        meters: parsed.meters,
        detail: parsed.detail,
        error: None,
    }
}

/// Test seam: pass an explicit candidate list + endpoint so the WSL fallback
/// and the live HTTP round-trip can be exercised in isolation.
fn codex_usage_with_paths(candidates: &[PathBuf], live_url: &str) -> ProviderUsage {
    let creds = match read_codex_credentials(candidates) {
        Ok((_, c)) => c,
        Err(e) => return logged_out("codex", e.to_string()),
    };

    let client = match shared_client() {
        Ok(c) => c,
        Err(e) => return unavailable("codex", e),
    };

    let mut req = client
        .get(live_url)
        .header("Authorization", format!("Bearer {}", creds.access_token));
    if let Some(account_id) = creds.account_id.as_deref() {
        req = req.header("ChatGPT-Account-Id", account_id.to_string());
    }

    let resp = match req.send() {
        Ok(r) => r,
        Err(e) => return unavailable("codex", format!("Request failed: {}", e)),
    };

    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
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
        Ok(parsed) => parsed_to_usage(parsed),
        Err(e) => unavailable("codex", format!("Failed to parse response: {}", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const CONSUMER_PLUS: &str = include_str!("fixtures/codex-consumer-plus.json");
    const SPEND_ONLY_ENTERPRISE: &str = include_str!("fixtures/codex-spend-only-enterprise.json");
    const MIXED_BUSINESS: &str = include_str!("fixtures/codex-mixed-business.json");
    const UNKNOWN_PLAN: &str = include_str!("fixtures/codex-unknown-plan.json");

    #[test]
    fn parse_codex_response_valid() {
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
        let parsed = parse_codex_response(json).unwrap();
        assert_eq!(parsed.windows.len(), 2);
        assert_eq!(parsed.windows[0].label, "5-hour");
        assert_eq!(parsed.windows[0].used_percent, Some(18.5));
        assert_eq!(
            parsed.windows[0].resets_at.as_deref(),
            Some("2025-08-15T20:00:00+00:00")
        );
        assert_eq!(parsed.windows[1].label, "Weekly");
        assert_eq!(parsed.windows[1].used_percent, Some(42.0));
        assert!(parsed.detail.is_none());
        assert!(parsed.meters.is_empty());
    }

    #[test]
    fn parse_codex_response_minimal_primary_only() {
        let json = r#"{"rate_limit":{"primary_window":{"used_percent":50.0}}}"#;
        let parsed = parse_codex_response(json).unwrap();
        assert_eq!(parsed.windows.len(), 1);
        assert_eq!(parsed.windows[0].label, "5-hour");
        assert_eq!(parsed.windows[0].used_percent, Some(50.0));
        assert!(parsed.windows[0].resets_at.is_none());
        assert!(parsed.detail.is_none());
    }

    #[test]
    fn parse_codex_response_additional_rate_limits_are_included() {
        let json = r#"{
            "rate_limit": {
                "primary_window": {"used_percent": 10.0, "limit_window_seconds": 18000},
                "secondary_window": {"used_percent": 20.0, "limit_window_seconds": 604800},
                "additional_rate_limits": [
                    {"used_percent": 30.0, "limit_window_seconds": 86400}
                ]
            }
        }"#;
        let parsed = parse_codex_response(json).unwrap();
        assert_eq!(parsed.windows.len(), 3);
        assert_eq!(parsed.windows[0].label, "5-hour");
        assert_eq!(parsed.windows[1].label, "Weekly");
        assert_eq!(parsed.windows[2].label, "24h");
        assert_eq!(parsed.windows[2].used_percent, Some(30.0));
        assert!(parsed.detail.is_none());
    }

    #[test]
    fn parse_codex_response_dynamic_label_fallback_formats_hours_and_days() {
        assert_eq!(format_codex_window_label(3600), "1-hour");
        assert_eq!(format_codex_window_label(7200), "2h");
        assert_eq!(format_codex_window_label(86400), "24h");
        assert_eq!(format_codex_window_label(172800), "2d");
        assert_eq!(format_codex_window_label(2592000), "30d");
        assert_eq!(format_codex_window_label(12345), "12345s");
    }

    #[test]
    fn parse_codex_response_null_rate_limit_is_valid() {
        let parsed = parse_codex_response(SPEND_ONLY_ENTERPRISE).unwrap();
        assert!(parsed.windows.is_empty());
        assert!(parsed.detail.is_none());
        let balance = parsed.balance.expect("credit balance retained");
        assert_eq!(balance.remaining, 17000.50);
        assert_eq!(balance.currency, "credits");
        assert_eq!(parsed.meters.len(), 1);
        match &parsed.meters[0] {
            UsageMeter::Metered { amount } => {
                assert_eq!(amount.used, 8000.0);
                assert_eq!(amount.limit, Some(25000.0));
                assert_eq!(amount.remaining, Some(17000.0));
                assert_eq!(amount.used_percent, Some(32.0));
                assert_eq!(amount.unit, "credits");
                assert_eq!(
                    amount.resets_at.as_deref(),
                    Some("2026-05-07T07:08:00+00:00")
                );
            }
            other => panic!("expected metered spend control, got {other:?}"),
        }
    }

    #[test]
    fn parse_codex_response_consumer_plus_fixture_keeps_windows() {
        let parsed = parse_codex_response(CONSUMER_PLUS).unwrap();
        assert_eq!(parsed.windows.len(), 2);
        assert_eq!(parsed.windows[0].label, "5-hour");
        assert_eq!(parsed.windows[0].used_percent, Some(18.5));
        assert_eq!(parsed.windows[1].label, "Weekly");
        assert!(parsed.detail.is_none());
        assert!(parsed.meters.is_empty());
        assert!(parsed.balance.is_none());
    }

    #[test]
    fn parse_codex_response_mixed_business_has_windows_and_budget() {
        let parsed = parse_codex_response(MIXED_BUSINESS).unwrap();
        assert_eq!(parsed.windows.len(), 3);
        assert_eq!(parsed.windows[0].label, "5-hour");
        assert_eq!(parsed.windows[1].label, "Weekly");
        assert_eq!(parsed.windows[2].label, "codex_other · 1-hour");
        assert_eq!(parsed.windows[2].used_percent, Some(30.0));
        assert_eq!(parsed.balance.as_ref().map(|b| b.remaining), Some(42.0));
        assert_eq!(parsed.meters.len(), 1);
        match &parsed.meters[0] {
            UsageMeter::Metered { amount } => {
                assert_eq!(amount.used, 2500.5);
                assert_eq!(amount.limit, Some(10000.0));
                assert_eq!(amount.remaining, Some(7499.5));
            }
            other => panic!("expected metered spend, got {other:?}"),
        }
        assert!(parsed.detail.is_none());
    }

    #[test]
    fn parse_codex_response_unknown_plan_degrades_without_failing() {
        let parsed = parse_codex_response(UNKNOWN_PLAN).unwrap();
        assert!(parsed.windows.is_empty());
        assert!(parsed.balance.is_none());
        assert_eq!(parsed.meters, vec![UsageMeter::Unlimited]);
    }

    #[test]
    fn parse_codex_response_spend_without_limit_is_uncapped() {
        let json = r#"{
            "plan_type": "business",
            "rate_limit": null,
            "spend_control": {
                "reached": false,
                "individual_limit": {
                    "used": "12.5",
                    "remaining": null,
                    "used_percent": 0
                }
            }
        }"#;
        let parsed = parse_codex_response(json).unwrap();
        match parsed.meters.as_slice() {
            [UsageMeter::NoIndividualLimit { amount }] => {
                assert_eq!(amount.used, 12.5);
                assert!(amount.limit.is_none());
            }
            other => panic!("expected uncapped spend, got {other:?}"),
        }
    }

    #[test]
    fn parse_codex_response_filters_windows_without_used_percent() {
        let json = r#"{
            "rate_limit": {
                "primary_window": {"used_percent": 10.0, "limit_window_seconds": 18000},
                "secondary_window": {"limit_window_seconds": 604800}
            }
        }"#;
        let parsed = parse_codex_response(json).unwrap();
        assert_eq!(parsed.windows.len(), 1);
        assert_eq!(parsed.windows[0].label, "5-hour");
        assert!(parsed.detail.is_none());
    }

    #[test]
    fn parse_codex_response_all_windows_filtered_reports_detail() {
        let json = r#"{
            "rate_limit": {
                "primary_window": {"limit_window_seconds": 18000},
                "secondary_window": {"limit_window_seconds": 604800}
            }
        }"#;
        let parsed = parse_codex_response(json).unwrap();
        assert!(parsed.windows.is_empty());
        assert_eq!(
            parsed.detail.as_deref(),
            Some("No active Codex rate-limit windows")
        );
    }

    // The bar's fill width is the inverse of "% remaining"; emitting the
    // computed string on top would duplicate the same number.
    #[test]
    fn parse_codex_response_does_not_emit_percent_remaining_when_windows_are_present() {
        let json = r#"{
            "rate_limit": {
                "primary_window": {"used_percent": 44.0, "limit_window_seconds": 18000},
                "secondary_window": {"used_percent": 56.0, "limit_window_seconds": 604800}
            }
        }"#;
        let parsed = parse_codex_response(json).unwrap();
        assert_eq!(parsed.windows.len(), 2);
        assert!(
            parsed.detail.is_none(),
            "detail must not duplicate bar percentages; got: {:?}",
            parsed.detail
        );
    }

    // After #1689 dropped `plan_type` from CodexUsageResp (provider
    // payload), a cached /wham/usage response from before the cut
    // must still parse cleanly. Serde's default drops the unknown
    // field; pin the lenient behavior here so a future regression
    // that adds `#[serde(deny_unknown_fields)]` is caught.
    #[test]
    fn parse_codex_response_does_not_emit_plan_type_round_trip() {
        let json = r#"{
            "plan_type": "plus",
            "rate_limit": {
                "primary_window": {"used_percent": 18.5, "limit_window_seconds": 18000}
            }
        }"#;
        let parsed = parse_codex_response(json).unwrap();
        assert_eq!(parsed.windows.len(), 1);
        assert!(parsed.windows[0].used_percent.is_some());
    }

    #[test]
    fn parse_codex_response_unix_epoch_reset_at_becomes_rfc3339() {
        let json = r#"{
            "rate_limit": {
                "primary_window": {
                    "used_percent": 25.0,
                    "limit_window_seconds": 18000,
                    "reset_at": 1735689600
                }
            }
        }"#;
        let parsed = parse_codex_response(json).unwrap();
        assert_eq!(
            parsed.windows[0].resets_at.as_deref(),
            Some("2025-01-01T00:00:00+00:00")
        );
    }

    #[test]
    fn read_codex_auth_file_legacy_top_level_token() {
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
        let err = read_codex_auth_file(&path).unwrap_err();
        assert!(matches!(err, UsageError::NoCredential(_)), "got {err:?}");
    }

    #[test]
    fn read_codex_credentials_walks_priority_and_returns_first_match() {
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
        fs::write(&real, r#"{"access_token":"sk-first"}"#).unwrap();
        let (path, creds) = read_codex_credentials(&[empty.clone(), real.clone()]).unwrap();
        assert_eq!(path, real);
        assert_eq!(creds.access_token, "sk-first");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_codex_credentials_propagates_shape_error() {
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
        assert!(usage.detail.is_none());

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_adapter_loopback_accepts_spend_only_enterprise() {
        let home = codex_temp_home();
        let auth_path = home.join("auth.json");
        fs::write(&auth_path, r#"{"access_token":"sk-test-ok"}"#).unwrap();
        let body = SPEND_ONLY_ENTERPRISE.to_string();
        let port = codex_live_loopback(move |req| {
            let _ = req.respond(tiny_http::Response::from_string(body.clone()));
        });
        let url = format!("http://127.0.0.1:{port}/wham/usage");

        let usage = with_adapter_loopback(vec![auth_path], url, || CodexAdapter.fetch(&[]));

        assert!(usage.logged_in);
        assert!(usage.error.is_none());
        assert!(usage.windows.is_empty());
        assert_eq!(usage.balance.as_ref().map(|b| b.remaining), Some(17000.50));
        assert!(matches!(
            usage.meters.first(),
            Some(UsageMeter::Metered { .. })
        ));

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_adapter_loopback_mixed_business_keeps_windows_and_budget() {
        let home = codex_temp_home();
        let auth_path = home.join("auth.json");
        fs::write(&auth_path, r#"{"access_token":"sk-test-ok"}"#).unwrap();
        let body = MIXED_BUSINESS.to_string();
        let port = codex_live_loopback(move |req| {
            let _ = req.respond(tiny_http::Response::from_string(body.clone()));
        });
        let url = format!("http://127.0.0.1:{port}/wham/usage");

        let usage = with_adapter_loopback(vec![auth_path], url, || CodexAdapter.fetch(&[]));

        assert!(usage.logged_in);
        assert_eq!(usage.windows.len(), 3);
        assert!(usage.balance.is_some());
        assert_eq!(usage.meters.len(), 1);

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_usage_with_paths_sends_account_id_header_when_present() {
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
        let home = codex_temp_home();
        let auth_path = home.join("auth.json");
        fs::write(&auth_path, r#"{"access_token":"sk-revoked"}"#).unwrap();
        let candidates = vec![auth_path];

        let port = codex_live_loopback(move |req| {
            let _ = req.respond(tiny_http::Response::from_string(r#"{}"#).with_status_code(403));
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
        assert!(usage
            .error
            .as_deref()
            .map(|e| e.contains("Rate limited"))
            .unwrap_or(false));

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_usage_with_paths_network_failure_preserves_logged_in() {
        let home = codex_temp_home();
        let auth_path = home.join("auth.json");
        fs::write(&auth_path, r#"{"access_token":"sk-test"}"#).unwrap();
        let candidates = vec![auth_path];

        let usage = codex_usage_with_paths(&candidates, "http://127.0.0.1:1/wham/usage");
        assert!(usage.logged_in);
        assert!(usage
            .error
            .as_deref()
            .map(|e| e.contains("Request failed"))
            .unwrap_or(false));

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_usage_with_paths_no_credential_returns_logged_out() {
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
        assert!(usage
            .error
            .as_deref()
            .map(|e| e.contains("parse"))
            .unwrap_or(false));

        let _ = fs::remove_dir_all(&home);
    }
}
