//! Provider usage fetching — piggybacks on CLI credentials stored by each provider.
//!
//! Endpoints are undocumented / reverse-engineered; treat non-200 responses or
//! shape mismatches as "usage unavailable", never as hard errors.

use reqwest::blocking::Client;
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

#[derive(Debug)]
pub enum UsageError {
    NoCredential,
    RateLimited,
    Http(reqwest::Error),
    Api(u16, String),
    Shape(String),
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UsageError::NoCredential => write!(f, "No credential found for this provider"),
            UsageError::RateLimited => write!(f, "Rate limited — try again later"),
            UsageError::Http(e) => write!(f, "HTTP error: {}", e),
            UsageError::Api(code, msg) => write!(f, "API error ({}): {}", code, msg),
            UsageError::Shape(msg) => write!(f, "Unexpected response shape: {}", msg),
        }
    }
}

impl From<reqwest::Error> for UsageError {
    fn from(e: reqwest::Error) -> Self {
        UsageError::Http(e)
    }
}

fn home_dir() -> PathBuf {
    env::var("USERPROFILE")
        .or_else(|_| env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("C:/Users/alond"))
}

fn codex_home() -> PathBuf {
    env::var("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir())
}

fn anthropic_cred_path() -> PathBuf {
    home_dir().join(".claude").join(".credentials.json")
}

fn codex_auth_path() -> PathBuf {
    codex_home().join(".codex").join("auth.json")
}

#[derive(Deserialize)]
struct AnthropicCred {
    #[serde(rename = "access_token")]
    access_token: Option<String>,
}

fn read_anthropic_token() -> Result<String, UsageError> {
    let path = anthropic_cred_path();
    let content = fs::read_to_string(&path)
        .map_err(|_| UsageError::NoCredential)?;
    let cred: AnthropicCred = serde_json::from_str(&content)
        .map_err(|e| UsageError::Shape(e.to_string()))?;
    cred.access_token
        .ok_or(UsageError::NoCredential)
}

fn parse_anthropic_response(body: &str) -> Result<Vec<UsageWindow>, UsageError> {
    #[derive(Deserialize)]
    struct Resp {
        usage: Option<Vec<AnthropicWindow>>,
    }
    #[derive(Deserialize)]
    struct AnthropicWindow {
        name: Option<String>,
        #[serde(rename = "usedPercent")]
        used_percent: Option<f64>,
        #[serde(rename = "resetsAt")]
        resets_at: Option<String>,
    }

    let resp: Resp = serde_json::from_str(body)
        .map_err(|e| UsageError::Shape(e.to_string()))?;

    let windows = resp.usage.unwrap_or_default()
        .into_iter()
        .map(|w| UsageWindow {
            label: w.name.unwrap_or_else(|| "Unknown".to_string()),
            used_percent: w.used_percent,
            resets_at: w.resets_at,
        })
        .collect();
    Ok(windows)
}

pub fn anthropic_usage() -> ProviderUsage {
    let token = match read_anthropic_token() {
        Ok(t) => t,
        Err(e) => return ProviderUsage {
            provider: "anthropic".to_string(),
            logged_in: false,
            windows: vec![],
            detail: None,
            error: Some(e.to_string()),
        },
    };

    let client = match Client::builder().build() {
        Ok(c) => c,
        Err(e) => return ProviderUsage {
            provider: "anthropic".to_string(),
            logged_in: true,
            windows: vec![],
            detail: None,
            error: Some(format!("Client error: {}", e)),
        },
    };

    let resp = client
        .get("https://api.anthropic.com/api/oauth/usage")
        .header("Authorization", format!("Bearer {}", token))
        .header("anthropic-beta", "oauth-2025-04-20")
        .send();

    match resp {
        Ok(r) if r.status() == 429 => ProviderUsage {
            provider: "anthropic".to_string(),
            logged_in: true,
            windows: vec![],
            detail: None,
            error: Some("Rate limited — usage data temporarily unavailable".to_string()),
        },
        Ok(r) => {
            let status = r.status();
            if !status.is_success() {
                let body = r.text().unwrap_or_default();
                return ProviderUsage {
                    provider: "anthropic".to_string(),
                    logged_in: true,
                    windows: vec![],
                    detail: None,
                    error: Some(format!("API error {}: {}", status.as_u16(), body)),
                };
            }
            let body = r.text().unwrap_or_default();
            match parse_anthropic_response(&body) {
                Ok(windows) => ProviderUsage {
                    provider: "anthropic".to_string(),
                    logged_in: true,
                    windows,
                    detail: None,
                    error: None,
                },
                Err(e) => ProviderUsage {
                    provider: "anthropic".to_string(),
                    logged_in: true,
                    windows: vec![],
                    detail: None,
                    error: Some(format!("Failed to parse response: {}", e)),
                },
            }
        }
        Err(e) => ProviderUsage {
            provider: "anthropic".to_string(),
            logged_in: true,
            windows: vec![],
            detail: None,
            error: Some(format!("Request failed: {}", e)),
        },
    }
}

#[derive(Deserialize)]
struct CodexAuth {
    #[serde(rename = "access_token")]
    access_token: Option<String>,
}

fn read_codex_token() -> Result<String, UsageError> {
    let path = codex_auth_path();
    let content = fs::read_to_string(&path)
        .map_err(|_| UsageError::NoCredential)?;
    let auth: CodexAuth = serde_json::from_str(&content)
        .map_err(|e| UsageError::Shape(e.to_string()))?;
    auth.access_token
        .ok_or(UsageError::NoCredential)
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

    let resp: Resp = serde_json::from_str(body)
        .map_err(|e| UsageError::Shape(e.to_string()))?;

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
    let token = match read_codex_token() {
        Ok(t) => t,
        Err(e) => return ProviderUsage {
            provider: "codex".to_string(),
            logged_in: false,
            windows: vec![],
            detail: None,
            error: Some(e.to_string()),
        },
    };

    let client = match Client::builder().build() {
        Ok(c) => c,
        Err(e) => return ProviderUsage {
            provider: "codex".to_string(),
            logged_in: true,
            windows: vec![],
            detail: None,
            error: Some(format!("Client error: {}", e)),
        },
    };

    let resp = client
        .get("https://chatgpt.com/backend-api/wham/usage")
        .header("Authorization", format!("Bearer {}", token))
        .send();

    match resp {
        Ok(r) if r.status() == 429 => ProviderUsage {
            provider: "codex".to_string(),
            logged_in: true,
            windows: vec![],
            detail: None,
            error: Some("Rate limited — usage data temporarily unavailable".to_string()),
        },
        Ok(r) => {
            let status = r.status();
            if !status.is_success() {
                let body = r.text().unwrap_or_default();
                return ProviderUsage {
                    provider: "codex".to_string(),
                    logged_in: true,
                    windows: vec![],
                    detail: None,
                    error: Some(format!("API error {}: {}", status.as_u16(), body)),
                };
            }
            let body = r.text().unwrap_or_default();
            match parse_codex_response(&body) {
                Ok(windows) => ProviderUsage {
                    provider: "codex".to_string(),
                    logged_in: true,
                    windows,
                    detail: None,
                    error: None,
                },
                Err(e) => ProviderUsage {
                    provider: "codex".to_string(),
                    logged_in: true,
                    windows: vec![],
                    detail: None,
                    error: Some(format!("Failed to parse response: {}", e)),
                },
            }
        }
        Err(e) => ProviderUsage {
            provider: "codex".to_string(),
            logged_in: true,
            windows: vec![],
            detail: None,
            error: Some(format!("Request failed: {}", e)),
        },
    }
}

pub fn minimax_usage(api_key: &str) -> ProviderUsage {
    if api_key.is_empty() {
        return ProviderUsage {
            provider: "minimax".to_string(),
            logged_in: false,
            windows: vec![],
            detail: None,
            error: Some("No API key configured".to_string()),
        };
    }

    let client = match Client::builder().build() {
        Ok(c) => c,
        Err(e) => return ProviderUsage {
            provider: "minimax".to_string(),
            logged_in: false,
            windows: vec![],
            detail: None,
            error: Some(format!("Client error: {}", e)),
        },
    };

    let resp = client
        .get("https://api.minimax.io/v1/token_plan/remains")
        .header("Authorization", format!("Bearer {}", api_key))
        .send();

    match resp {
        Ok(r) if r.status() == 429 => ProviderUsage {
            provider: "minimax".to_string(),
            logged_in: true,
            windows: vec![],
            detail: None,
            error: Some("Rate limited — try again later".to_string()),
        },
        Ok(r) => {
            let status = r.status();
            if !status.is_success() {
                let body = r.text().unwrap_or_default();
                return ProviderUsage {
                    provider: "minimax".to_string(),
                    logged_in: true,
                    windows: vec![],
                    detail: None,
                    error: Some(format!("API error {}: {}", status.as_u16(), body)),
                };
            }
            let body = r.text().unwrap_or_default();
            match serde_json::from_str::<MinimaxResp>(&body) {
                Ok(resp) => ProviderUsage {
                    provider: "minimax".to_string(),
                    logged_in: true,
                    windows: vec![],
                    detail: Some(format!("{} remaining tokens", resp.data.remains_quota)),
                    error: None,
                },
                Err(e) => ProviderUsage {
                    provider: "minimax".to_string(),
                    logged_in: true,
                    windows: vec![],
                    detail: None,
                    error: Some(format!("Failed to parse response: {}", e)),
                },
            }
        }
        Err(e) => ProviderUsage {
            provider: "minimax".to_string(),
            logged_in: true,
            windows: vec![],
            detail: None,
            error: Some(format!("Request failed: {}", e)),
        },
    }
}

#[derive(Deserialize)]
struct MinimaxResp {
    data: MinimaxData,
}

#[derive(Deserialize)]
struct MinimaxData {
    #[serde(rename = "remains_quota")]
    remains_quota: String,
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
        let json = r#"{"usage":[{"name":"5-hour","usedPercent":45.5,"resetsAt":"2026-05-30T12:00:00Z"}]}"#;
        let windows = parse_anthropic_response(json).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "5-hour");
        assert_eq!(windows[0].used_percent, Some(45.5));
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
    fn no_credential_returns_error() {
        let usage = minimax_usage("");
        assert!(!usage.logged_in);
        assert!(usage.error.is_some());
    }
}