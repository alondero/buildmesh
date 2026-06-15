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
#[ts(export, export_to = "ProviderUsage.ts")]
/// Generated to src/types/generated/ProviderUsage.ts (issue #404). `loggedIn`
/// is camelCase on the wire per `#[ts(rename = "loggedIn")]`.
pub struct ProviderUsage {
    pub provider: String,
    #[serde(rename = "loggedIn")]
    #[ts(rename = "loggedIn")]
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

/// Minimal FFI to the Windows Credential Manager (`advapi32!CredReadW`) — just
/// enough to read a generic credential blob, so we avoid a winapi/windows dep.
#[cfg(windows)]
mod windows_cred {
    use super::UsageError;
    use std::os::windows::ffi::OsStrExt;

    #[repr(C)]
    struct Filetime {
        _low: u32,
        _high: u32,
    }

    #[repr(C)]
    struct CredentialW {
        _flags: u32,
        _typ: u32,
        _target_name: *mut u16,
        _comment: *mut u16,
        _last_written: Filetime,
        credential_blob_size: u32,
        credential_blob: *mut u8,
        _persist: u32,
        _attribute_count: u32,
        _attributes: *mut core::ffi::c_void,
        _target_alias: *mut u16,
        _user_name: *mut u16,
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn CredReadW(target: *const u16, typ: u32, flags: u32, cred: *mut *mut CredentialW) -> i32;
        fn CredFree(buf: *mut core::ffi::c_void);
    }

    const CRED_TYPE_GENERIC: u32 = 1;

    pub fn read(target: &str) -> Result<Vec<u8>, UsageError> {
        let wide: Vec<u16> = std::ffi::OsStr::new(target)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `wide` is a NUL-terminated UTF-16 string; CredReadW writes a
        // single owned pointer we free with CredFree after copying its blob.
        unsafe {
            let mut ptr: *mut CredentialW = std::ptr::null_mut();
            if CredReadW(wide.as_ptr(), CRED_TYPE_GENERIC, 0, &mut ptr) == 0 || ptr.is_null() {
                return Err(UsageError::NoCredential(target.to_string()));
            }
            let cred = &*ptr;
            // `from_raw_parts` requires a non-null pointer even for length 0, so
            // guard the empty/null-blob case rather than risk UB.
            let blob = if cred.credential_blob.is_null() || cred.credential_blob_size == 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts(cred.credential_blob, cred.credential_blob_size as usize)
                    .to_vec()
            };
            CredFree(ptr as *mut core::ffi::c_void);
            Ok(blob)
        }
    }
}

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
}
