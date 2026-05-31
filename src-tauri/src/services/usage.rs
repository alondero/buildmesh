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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageWindow {
    pub label: String,
    #[serde(rename = "usedPercent")]
    pub used_percent: Option<f64>,
    #[serde(rename = "resetsAt")]
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderUsage {
    pub provider: String,
    #[serde(rename = "loggedIn")]
    pub logged_in: bool,
    pub windows: Vec<UsageWindow>,
    pub detail: Option<String>,
    pub error: Option<String>,
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

fn logged_out(provider: &str, error: String) -> ProviderUsage {
    ProviderUsage {
        provider: provider.to_string(),
        logged_in: false,
        windows: vec![],
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
    let unavailable = |error: String| ProviderUsage {
        provider: provider.to_string(),
        logged_in: true,
        windows: vec![],
        detail: None,
        error: Some(error),
    };

    let client = match Client::builder().build() {
        Ok(c) => c,
        Err(e) => return unavailable(format!("Client error: {}", e)),
    };

    match build_request(&client).send() {
        Ok(r) if r.status() == 429 => {
            unavailable("Rate limited — usage data temporarily unavailable".to_string())
        }
        Ok(r) if !r.status().is_success() => {
            let code = r.status().as_u16();
            unavailable(format!("API error {}: {}", code, r.text().unwrap_or_default()))
        }
        Ok(r) => match parse(&r.text().unwrap_or_default()) {
            Ok((windows, detail)) => ProviderUsage {
                provider: provider.to_string(),
                logged_in: true,
                windows,
                detail,
                error: None,
            },
            Err(e) => unavailable(format!("Failed to parse response: {}", e)),
        },
        Err(e) => unavailable(format!("Request failed: {}", e)),
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
struct MinimaxCategory {
    #[serde(default)]
    category: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    end_time: i64,
    #[serde(default)]
    current_interval_total_count: i64,
    #[serde(default)]
    current_interval_usage_count: i64,
    #[serde(default)]
    current_weekly_total_count: i64,
    #[serde(default)]
    current_weekly_usage_count: i64,
    #[serde(default)]
    weekly_end_time: i64,
}

#[derive(Deserialize, Debug)]
struct MinimaxResp {
    #[serde(default)]
    category_remains: Vec<MinimaxCategory>,
}

fn parse_minimax_response(body: &str) -> Result<(Vec<UsageWindow>, Option<String>), UsageError> {
    let resp: MinimaxResp =
        serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;

    let mut windows = vec![];
    let mut text_gen_detail = None;

    for cat in resp.category_remains {
        let is_relevant = cat.category == "text_generation"
            || cat.category == "coding-plan-vlm"
            || cat.category == "coding-plan-search"
            || cat.current_interval_usage_count > 0
            || cat.current_weekly_usage_count > 0;

        if !is_relevant {
            continue;
        }

        if cat.current_interval_total_count > 0 {
            let used_percent = (cat.current_interval_usage_count as f64
                / cat.current_interval_total_count as f64)
                * 100.0;
            let resets_at = if cat.end_time > 0 {
                chrono::DateTime::from_timestamp_millis(cat.end_time).map(|dt| dt.to_rfc3339())
            } else {
                None
            };
            windows.push(UsageWindow {
                label: format!("{} (5-hour)", cat.display_name),
                used_percent: Some(used_percent),
                resets_at,
            });
        }

        if cat.current_weekly_total_count > 0 {
            let used_percent = (cat.current_weekly_usage_count as f64
                / cat.current_weekly_total_count as f64)
                * 100.0;
            let resets_at = if cat.weekly_end_time > 0 {
                chrono::DateTime::from_timestamp_millis(cat.weekly_end_time).map(|dt| dt.to_rfc3339())
            } else {
                None
            };
            windows.push(UsageWindow {
                label: format!("{} (Weekly)", cat.display_name),
                used_percent: Some(used_percent),
                resets_at,
            });
        }

        if cat.category == "text_generation" && cat.current_interval_total_count > 0 {
            let remaining = cat.current_interval_total_count - cat.current_interval_usage_count;
            text_gen_detail = Some(format!(
                "{} / {} text generation requests remaining (5-hour window)",
                remaining, cat.current_interval_total_count
            ));
        }
    }

    if windows.is_empty() && text_gen_detail.is_none() {
        text_gen_detail = Some("No active token plan quotas found".to_string());
    }

    Ok((windows, text_gen_detail))
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
        |body| parse_minimax_response(body),
    )
}

fn google_cred_path() -> PathBuf {
    home_dir().join(".gemini").join("oauth_creds.json")
}

#[derive(Deserialize)]
struct GoogleOAuthCred {
    access_token: Option<String>,
}

fn read_google_token(path: PathBuf) -> Result<String, UsageError> {
    let content = fs::read_to_string(&path).map_err(|_| UsageError::NoCredential(path.clone().to_string_lossy().to_string()))?;
    let cred: GoogleOAuthCred =
        serde_json::from_str(&content).map_err(|e| UsageError::Shape(e.to_string()))?;
    cred.access_token.ok_or(UsageError::NoCredential(path.to_string_lossy().to_string()))
}

#[derive(Deserialize, Debug)]
struct GoogleQuotaBucket {
    #[serde(rename = "modelId")]
    model_id: String,
    #[serde(rename = "remainingFraction")]
    remaining_fraction: Option<f64>,
    #[serde(rename = "remainingAmount")]
    remaining_amount: Option<String>,
    #[serde(rename = "resetTime")]
    reset_time: Option<String>,
}

#[derive(Deserialize, Debug)]
struct GoogleQuotaResp {
    buckets: Option<Vec<GoogleQuotaBucket>>,
}

fn parse_google_response(body: &str) -> Result<(Vec<UsageWindow>, Option<String>), UsageError> {
    let resp: GoogleQuotaResp =
        serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;

    let mut windows = vec![];
    let mut detail = None;

    if let Some(buckets) = resp.buckets {
        for bucket in buckets {
            if let Some(fraction) = bucket.remaining_fraction {
                let used_percent = (1.0 - fraction) * 100.0;
                
                windows.push(UsageWindow {
                    label: bucket.model_id.clone(),
                    used_percent: Some(used_percent),
                    resets_at: bucket.reset_time.clone(),
                });

                if detail.is_none() {
                    if let Some(amt) = &bucket.remaining_amount {
                        detail = Some(format!("Remaining: {} requests", amt));
                    }
                }
            }
        }
    }

    if windows.is_empty() {
        detail = Some("No active usage quotas found".to_string());
    }

    Ok((windows, detail))
}

pub fn agy_usage(project: &str) -> ProviderUsage {
    let token = match read_google_token(google_cred_path()) {
        Ok(t) => t,
        Err(e) => return logged_out("agy", e.to_string()),
    };
    fetch_usage(
        "agy",
        |c| {
            c.post("https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota")
                .header("Authorization", format!("Bearer {}", token))
                .header("Content-Type", "application/json")
                .json(&serde_json::json!({ "project": project }))
        },
        |body| parse_google_response(body),
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
            "base_resp": {
                "status_code": 0,
                "status_msg": "success"
            }
        }"#;
        let (windows, detail) = parse_minimax_response(json).unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].label, "Text Generation (5-hour)");
        assert_eq!(windows[0].used_percent, Some((55.0 / 15000.0) * 100.0));
        assert!(windows[0].resets_at.is_some());
        assert_eq!(windows[1].label, "Text Generation (Weekly)");
        assert_eq!(windows[1].used_percent, Some((732.0 / 150000.0) * 100.0));
        assert!(windows[1].resets_at.is_some());
        assert_eq!(detail, Some("14945 / 15000 text generation requests remaining (5-hour window)".to_string()));
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
}
