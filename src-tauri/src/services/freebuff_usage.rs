//! Freebuff daily Usage Meter using the CLI-managed bearer.
//!
//! The current CLI stores its login under `default` in credentials.json;
//! legacy flat credentials remain supported. The allowance is independent
//! of the active session's status. See docs/development/providers.md.

use serde::Deserialize;
use std::{env, fs, path::PathBuf, time::Duration};

use crate::services::http_client::blocking_client_builder;
use crate::services::usage::outcome::UsageOutcome;
use crate::services::usage::types::{UsageError, UsageWindow};

pub(crate) const FREEBUFF_USAGE_URL: &str = "https://freebuff.com/api/web/freebuff-session";
const FREEBUFF_USER_AGENT: &str = "Buildmesh-Usage-Probe/1.0";
const ERROR_BODY_MAX_CHARS: usize = 120;

// Respect XDG_CONFIG_HOME, then the CLI's default, then the passive WSL fallback.
fn freebuff_credential_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // 1. XDG_CONFIG_HOME override (per XDG Base Directory Specification).
    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
    {
        paths.push(xdg.join("manicode").join("credentials.json"));
    }

    // 2. XDG-style default — `<home>/.config/manicode/credentials.json`.
    paths.push(
        crate::services::usage::types::home_dir()
            .join(".config")
            .join("manicode")
            .join("credentials.json"),
    );

    // 3. WSL fallback (Windows host only).
    #[cfg(target_os = "windows")]
    {
        if let Some(username) = env::var("USERNAME").ok().filter(|s| !s.is_empty()) {
            let linux_path = format!("/home/{}/.config/manicode/credentials.json", username);
            let host_path = crate::env::to_host_path(&linux_path);
            if host_path != linux_path {
                paths.push(PathBuf::from(host_path));
            }
        }
    }

    paths
}

// ─── Credential parsing ─────────────────────────────────────────────────────

#[derive(Deserialize)]
struct FreebuffCredentials {
    #[serde(default, rename = "authToken")]
    auth_token: Option<String>,
}

fn read_freebuff_credentials(candidates: &[PathBuf]) -> Result<String, UsageError> {
    for path in candidates {
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let stored: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| UsageError::Shape(format!("{}: {}", path.display(), e)))?;
        // An explicitly present default entry outranks legacy credentials,
        // even when it is empty or malformed.
        let creds: FreebuffCredentials =
            serde_json::from_value(stored.get("default").unwrap_or(&stored).clone())
                .map_err(|e| UsageError::Shape(format!("{}: {}", path.display(), e)))?;
        if let Some(token) = creds
            .auth_token
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            return Ok(token);
        }
    }
    Err(UsageError::NoCredential(
        candidates
            .last()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "~/.config/manicode/credentials.json".to_string()),
    ))
}

#[derive(Deserialize)]
struct FreebuffUsageResponse {
    freebucks: FreebuffAllowance,
}

#[derive(Deserialize)]
struct FreebuffAllowance {
    daily: FreebuffDaily,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FreebuffDaily {
    limit: f64,
    spent: f64,
    reset_at: Option<String>,
}

/// Freebucks are allowance units, not cash. Project the daily ratio without
/// interpreting wallet credits or per-model admission counts as USD/quota.
fn parse_freebuff_response(body: &str) -> Result<Vec<UsageWindow>, UsageError> {
    let response: FreebuffUsageResponse =
        serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;
    let daily = response.freebucks.daily;
    if !daily.limit.is_finite()
        || daily.limit <= 0.0
        || !daily.spent.is_finite()
        || daily.spent < 0.0
    {
        return Err(UsageError::Shape(
            "Invalid Freebuff daily allowance".to_string(),
        ));
    }
    Ok(vec![UsageWindow {
        label: "Daily".to_string(),
        used_percent: Some((daily.spent / daily.limit * 100.0).clamp(0.0, 100.0)),
        resets_at: daily.reset_at,
    }])
}

fn clamp_error_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "usage endpoint failed".to_string();
    }
    // Collapse any internal whitespace (newlines, tabs, runs of spaces)
    // into single spaces — the error field renders as one line of UI
    // text.
    let one_line: String = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= ERROR_BODY_MAX_CHARS {
        return one_line;
    }
    // Truncate by *character* count (not bytes) so multi-byte UTF-8
    // input doesn't panic on a byte slice. Reserve one char for the
    // ellipsis.
    let truncated: String = one_line.chars().take(ERROR_BODY_MAX_CHARS).collect();
    format!("{truncated}…")
}

// ─── Public fetcher ─────────────────────────────────────────────────────────

/// Read the CLI credential and fetch allowance directly without starting the CLI.
pub fn freebuff_usage() -> UsageOutcome {
    freebuff_usage_with(&freebuff_credential_paths(), FREEBUFF_USAGE_URL)
}

/// Test seam: callers inject a candidate list (so a temp directory can
/// stand in for `<home>/.config/manicode/credentials.json`) and a
/// loopback URL (so the live HTTP fetch can run against a `tiny_http`
/// listener without touching the network). Mirrors
/// `cursor_usage_with_token` / `commandcode_usage_with_path`.
pub(crate) fn freebuff_usage_with(candidates: &[PathBuf], live_url: &str) -> UsageOutcome {
    let auth = match read_freebuff_credentials(candidates) {
        Ok(auth) => auth,
        Err(error @ UsageError::NoCredential(_)) => {
            return UsageOutcome::NoCredential {
                hint: error.to_string(),
            }
        }
        Err(error) => {
            return UsageOutcome::Unavailable {
                reason: error.to_string(),
            }
        }
    };
    let client = match blocking_client_builder()
        .timeout(Duration::from_secs(15))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return UsageOutcome::Unavailable {
                reason: format!("Client error: {error}"),
            }
        }
    };
    let response = match client
        .get(live_url)
        .header("Authorization", format!("Bearer {auth}"))
        .header("User-Agent", FREEBUFF_USER_AGENT)
        .send()
    {
        Ok(response) => response,
        Err(error) => {
            return UsageOutcome::Unavailable {
                reason: format!("Request failed: {error}"),
            }
        }
    };
    let status = response.status().as_u16();
    let body = match response.text() {
        Ok(body) => body,
        Err(error) => {
            return UsageOutcome::Unavailable {
                reason: format!("Failed to read response: {error}"),
            }
        }
    };

    // Status classification is split so a WAF / CDN 403 (HTML body,
    // unparseable envelope) cannot wipe the user's login state -- only a
    // genuine 401 surfaces as `Rejected` on an unparseable body.
    match classify_status(status, &body) {
        StatusOutcome::Rejected(hint) => UsageOutcome::Rejected { hint },
        StatusOutcome::Unavailable(reason) => UsageOutcome::Unavailable { reason },
        StatusOutcome::RateLimited(reason) => UsageOutcome::RateLimited { reason },
        StatusOutcome::Parse => match parse_freebuff_response(&body) {
            Ok(windows) => UsageOutcome::Reading {
                windows,
                balance: None,
                meters: vec![],
                detail: None,
            },
            Err(error) => UsageOutcome::Unavailable {
                reason: format!("Failed to parse response: {error}"),
            },
        },
    }
}

enum StatusOutcome {
    Rejected(String),
    Unavailable(String),
    RateLimited(String),
    Parse,
}

/// Borrow the status field for authentication-error classification.
#[derive(Deserialize)]
struct StatusEnvelope<'a> {
    #[serde(borrow)]
    status: Option<&'a str>,
}

/// Classify 401 (Unauthorized). An unparseable body falls through to
/// the generic "session expired" logout since 401 is genuinely a
/// credential failure.
fn classify_401_error(body: &str) -> StatusOutcome {
    if let Ok(env) = serde_json::from_str::<StatusEnvelope>(body) {
        if env.status == Some("banned") {
            return StatusOutcome::Rejected("Freebuff account suspended".to_string());
        }
    }
    StatusOutcome::Rejected(
        "Freebuff session expired -- run 'freebuff login' to log in".to_string(),
    )
}

/// Classify 403 (Forbidden). An unparseable body (WAF/CDN HTML page,
/// Cloudflare challenge) surfaces as `Unavailable` rather than
/// logging the user out -- the upstream hasn't actually informed us
/// of a credential issue, just that something blocked the request.
/// A valid JSON body with `{"status":"banned"}` IS a credential
/// signal (the upstream is explicitly telling us the account is
/// suspended) and logs the user out the same way a 401 + banned
/// would. PR #1443 round-6 review item #4 -- align 403 banned with
/// 401 banned rather than preserving login on a real account
/// suspension.
/// Default 403 surface -- WAF/CDN HTML block, malformed envelope, or
/// envelope without a recognised `status` field. Extracted so the
/// string lives in one place (PR #1443 round-6 review nitpick).
fn forbidden_default() -> StatusOutcome {
    StatusOutcome::Unavailable("Access forbidden (HTTP 403)".to_string())
}

fn classify_403_error(body: &str) -> StatusOutcome {
    let Ok(env) = serde_json::from_str::<StatusEnvelope>(body) else {
        return forbidden_default();
    };
    match env.status {
        Some("banned") => StatusOutcome::Rejected("Freebuff account suspended".to_string()),
        Some("country_blocked") => {
            StatusOutcome::Unavailable("Freebuff is not available in this region".to_string())
        }
        // Any other `status` value (recognised or absent) indicates the
        // upstream actively rejected the request -- surface as
        // Unavailable so the user can investigate without their login
        // being wiped.
        _ => forbidden_default(),
    }
}

/// Preserve authentication and transient-failure distinctions. A missing route
/// is unavailable; an inactive session still reports allowance with HTTP 200.
fn classify_status(status: u16, body: &str) -> StatusOutcome {
    match status {
        200..=299 => StatusOutcome::Parse,
        401 => classify_401_error(body),
        403 => classify_403_error(body),
        429 => StatusOutcome::RateLimited(
            "Rate limited -- usage data temporarily unavailable".to_string(),
        ),
        s => StatusOutcome::Unavailable(format!("API error {s}: {}", clamp_error_body(body))),
    }
}

#[cfg(test)]
#[path = "freebuff_usage_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "freebuff_daily_tests.rs"]
mod daily_tests;
