//! Antigravity (`agy`) native adapter — credential discovery for the Usage
//! Meter, detection-gated on the `agy` harness.
//!
//! Current CLI (1.2+) refreshes `<agy_dir>/antigravity-oauth-token` in place.
//! Older Windows installs also keep a blob under Windows Credential Manager
//! `gemini:antigravity`, which the new CLI leaves stale after refresh.
//!
//! Credential *selection* (file then keyring) is separate from credential
//! *validity*: callers try each collected token; on HTTP 401/403 they advance
//! to the next source before returning logged-out.
//!
//! The fetch lives here rather than in the `usage.rs` facade (issue #2528):
//! agy owns its quota endpoints, its User-Agent gate and both response
//! parses.

use crate::preferences::ProviderAccount;
use crate::services::http_client::blocking_client_builder;
use crate::services::usage::adapter::{fetch_usage, UsageAdapter};
use crate::services::usage::outcome::{ResponsePolicy, UsageOutcome, UsageReading};
use crate::services::usage::types::{UsageError, UsagePage, UsageWindow};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(windows)]
use crate::services::windows_cred;

/// Credential Manager target older agy CLIs stored the OAuth blob under.
pub(crate) const AGY_CRED_TARGET: &str = "gemini:antigravity";
/// Live OAuth blob filename under [`crate::env::agy_dir`].
pub(crate) const AGY_OAUTH_TOKEN_FILE: &str = "antigravity-oauth-token";

/// Drop-in [`UsageAdapter`] for `agy`.
pub(crate) struct AgyAdapter;

impl UsageAdapter for AgyAdapter {
    fn usage_page(&self) -> Option<UsagePage> {
        Some(UsagePage::new(
            "https://antigravity.google/docs/plans",
            "Usage guide",
        ))
    }

    fn id(&self) -> &'static str {
        "agy"
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("agy")
    }

    // Issue #1745 phase 2 step 12: agy migrated to the outcome seam.
    // Missing tokens → `NoCredential` (gate drops). Auth rejection on the
    // last source → `Rejected`. Client-build and non-auth source failures
    // are transport-class → `Unavailable` (previously `logged_out`, which
    // silently dropped the row).
    //
    // Issue #2528 step 3c: the fetch moved here from `usage.rs`.
    fn fetch(&self, _accounts: &[ProviderAccount]) -> UsageOutcome {
        agy_usage()
    }
}

/// Which store produced an access token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgyTokenSource {
    CliFile,
    Keyring,
}

impl AgyTokenSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::CliFile => "cli_file",
            Self::Keyring => "keyring",
        }
    }
}

/// One usable Antigravity bearer token plus its origin.
#[derive(Debug, Clone)]
pub(crate) struct AgyAccessToken {
    pub access_token: String,
    pub source: AgyTokenSource,
}

#[derive(serde::Deserialize)]
struct AgyTokenField {
    access_token: Option<String>,
}

#[derive(serde::Deserialize)]
struct AgyCred {
    token: Option<AgyTokenField>,
}

/// Parses the agy credential blob (`{ "token": { "access_token": … }, … }`).
/// Same envelope for the CLI oauth file and the Windows keyring blob.
pub(crate) fn parse_agy_token(blob: &[u8]) -> Result<String, UsageError> {
    let text = std::str::from_utf8(blob).map_err(|e| UsageError::Shape(e.to_string()))?;
    let cred: AgyCred = serde_json::from_str(text).map_err(|e| UsageError::Shape(e.to_string()))?;
    cred.token
        .and_then(|t| t.access_token)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| UsageError::NoCredential(AGY_CRED_TARGET.to_string()))
}

pub(crate) fn agy_oauth_token_path() -> PathBuf {
    crate::env::agy_dir().join(AGY_OAUTH_TOKEN_FILE)
}

fn read_agy_cli_file_token(file_path: &Path) -> Result<String, UsageError> {
    let bytes = fs::read(file_path)
        .map_err(|e| UsageError::NoCredential(format!("{} ({e})", file_path.display())))?;
    parse_agy_token(&bytes)
}

#[cfg(windows)]
fn read_agy_keyring_token() -> Result<String, UsageError> {
    parse_agy_token(&windows_cred::read(AGY_CRED_TARGET)?)
}

#[cfg(not(windows))]
fn read_agy_keyring_token() -> Result<String, UsageError> {
    Err(UsageError::NoCredential(
        "Antigravity OAuth token not found (antigravity-oauth-token or gemini:antigravity)"
            .to_string(),
    ))
}

/// Collect usable access tokens in preference order (CLI file, then keyring).
///
/// Matches the Codex multi-source contract: a [`UsageError::Shape`] from the
/// CLI file surfaces immediately (corrupt JSON is not fixed by reading the
/// keyring). [`UsageError::NoCredential`] (missing file, empty token, I/O)
/// advances to the next source. Deduplicates identical bearer strings.
pub(crate) fn collect_agy_access_tokens(
    file_path: &Path,
    keyring: impl FnOnce() -> Result<String, UsageError>,
) -> Result<Vec<AgyAccessToken>, UsageError> {
    let mut tokens = Vec::new();

    match read_agy_cli_file_token(file_path) {
        Ok(access_token) => {
            tracing::debug!(
                target: "services::usage::agy",
                source = AgyTokenSource::CliFile.as_str(),
                path = %file_path.display(),
                "selected Antigravity oauth token"
            );
            tokens.push(AgyAccessToken {
                access_token,
                source: AgyTokenSource::CliFile,
            });
        }
        Err(UsageError::Shape(e)) => {
            tracing::debug!(
                target: "services::usage::agy",
                path = %file_path.display(),
                error = %e,
                "Antigravity CLI oauth file has unexpected shape; not falling back to keyring"
            );
            return Err(UsageError::Shape(e));
        }
        Err(UsageError::NoCredential(path)) => {
            tracing::debug!(
                target: "services::usage::agy",
                path = %path,
                "Antigravity CLI oauth file unavailable"
            );
        }
    }

    match keyring() {
        Ok(access_token) => {
            if tokens.iter().any(|t| t.access_token == access_token) {
                tracing::debug!(
                    target: "services::usage::agy",
                    source = AgyTokenSource::Keyring.as_str(),
                    "keyring token matches cli_file; skipping duplicate"
                );
            } else {
                tracing::debug!(
                    target: "services::usage::agy",
                    source = AgyTokenSource::Keyring.as_str(),
                    "selected Antigravity oauth token"
                );
                tokens.push(AgyAccessToken {
                    access_token,
                    source: AgyTokenSource::Keyring,
                });
            }
        }
        Err(UsageError::Shape(e)) => {
            if tokens.is_empty() {
                tracing::debug!(
                    target: "services::usage::agy",
                    error = %e,
                    "Antigravity keyring blob has unexpected shape"
                );
                return Err(UsageError::Shape(e));
            }
            tracing::debug!(
                target: "services::usage::agy",
                error = %e,
                "Antigravity keyring blob has unexpected shape; keeping cli_file token"
            );
        }
        Err(UsageError::NoCredential(path)) => {
            tracing::debug!(
                target: "services::usage::agy",
                path = %path,
                "Antigravity keyring credential unavailable"
            );
        }
    }

    if tokens.is_empty() {
        Err(UsageError::NoCredential(
            "Antigravity OAuth token not found (antigravity-oauth-token or gemini:antigravity)"
                .to_string(),
        ))
    } else {
        Ok(tokens)
    }
}

/// Production entry: CLI oauth file under [`agy_oauth_token_path`], then keyring.
pub(crate) fn collect_agy_access_tokens_default() -> Result<Vec<AgyAccessToken>, UsageError> {
    collect_agy_access_tokens(&agy_oauth_token_path(), read_agy_keyring_token)
}

// ─── Google / Antigravity (`agy`) ───────────────────────────────────────────
//
// The Antigravity CLI surfaces a per-model quota that is a DIFFERENT product
// from Gemini Code Assist: it lives on the `daily-cloudcode-pa` staging host,
// behind `retrieveUserQuotaSummary` (weekly + 5-hour shared buckets) with
// `fetchAvailableModels` as a five-hour-only fallback, and is gated purely
// by the client User-Agent.
// Auth is separate from `~/.gemini/oauth_creds.json`. Credential discovery
// (CLI oauth file + keyring fallback, Shape-vs-NoCredential contract, 401
// retry across sources) lives in `usage::adapters::agy` so new token-source
// lore does not accrue in this module (#1657).
//
// This path is deliberately best-effort and FRAGILE (staging host, User-Agent
// gate, no token refresh since the Antigravity OAuth client isn't recoverable).
// Per the module contract, any failure degrades to "unavailable", never errors.

const AGY_HOST: &str = "https://daily-cloudcode-pa.googleapis.com";
/// The Antigravity CLI identifies with this User-Agent and the Cloud Code private
/// API allowlists it. Load-bearing: without it the API returns 403 PERMISSION_DENIED.
///
/// The advertised CLI version is ALSO load-bearing: `retrieveUserQuotaSummary`
/// gates the "Gemini Models" (weekly + 5-hour) quota group on it. The API still
/// returns HTTP 200 for an older version, it just silently strips that group —
/// so the Gemini Usage Meters disappear with no error (regression 2026-10-16: an
/// `antigravity/cli/1.0.3` UA was served only the "Claude and GPT models" group).
///
/// Pin provenance — `1.4.0` is the current published Antigravity CLI version
/// (`agy --version`), and was exercised live against the real endpoint on
/// 2026-10-16 varying only this string: `1.1.5` returns only "Claude and GPT
/// models"; `1.2.0` adds "Gemini Models"; `1.4.0` returns "Gemini Models" too,
/// plus the newer "Gemini 4 Argon" group. Keep the pin at or above the gate in
/// `agy_user_agent_meets_gemini_quota_version_gate`.
const AGY_USER_AGENT: &str = "antigravity/cli/1.4.0 windows/amd64";

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

/// Wire shape of `POST /v1internal:retrieveUserQuotaSummary`. Unlike
/// `fetchAvailableModels` (five-hour remainingFraction only), this is the
/// endpoint the CLI `/usage` command wraps: per-group shared weekly + 5-hour
/// buckets. Calling it over HTTP avoids a ~6s `agy` process spawn that stalled
/// the whole Usage Probe after #1324.
#[derive(Deserialize)]
struct AgyQuotaSummaryResp {
    #[serde(default)]
    groups: Vec<AgyQuotaGroup>,
}

#[derive(Deserialize)]
struct AgyQuotaGroup {
    #[serde(rename = "displayName")]
    display_name: String,
    #[serde(default)]
    buckets: Vec<AgyQuotaBucket>,
}

#[derive(Deserialize)]
struct AgyQuotaBucket {
    window: String,
    #[serde(rename = "remainingFraction")]
    remaining_fraction: Option<f64>,
    #[serde(rename = "resetTime")]
    reset_time: Option<String>,
}

fn agy_window_label(window: &str) -> String {
    match window {
        "weekly" => "Weekly".to_string(),
        "5h" => "5-hour".to_string(),
        other => other.to_string(),
    }
}

/// Rank used to put 5-hour before weekly inside each group, matching Anthropic /
/// Codex / OpenCode meter order. Unknown windows keep their relative position
/// after the two known ones.
fn agy_window_rank(window: &str) -> u8 {
    match window {
        "5h" => 0,
        "weekly" => 1,
        _ => 2,
    }
}

fn parse_agy_quota_summary(body: &str) -> Result<(Vec<UsageWindow>, Option<String>), UsageError> {
    let resp: AgyQuotaSummaryResp =
        serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;

    let mut windows = Vec::new();
    let mut saw_gemini_group = false;
    for group in resp.groups {
        if group.display_name.starts_with(GEMINI_DISPLAY_PREFIX) {
            saw_gemini_group = true;
        }
        let mut buckets = group.buckets;
        buckets.sort_by_key(|bucket| agy_window_rank(&bucket.window));
        for bucket in buckets {
            if let Some(fraction) = bucket.remaining_fraction {
                windows.push(UsageWindow {
                    label: format!(
                        "{} — {}",
                        group.display_name,
                        agy_window_label(&bucket.window)
                    ),
                    used_percent: Some((1.0 - fraction) * 100.0),
                    resets_at: bucket.reset_time,
                });
            }
        }
    }
    if windows.is_empty() {
        return Err(UsageError::Shape(
            "retrieveUserQuotaSummary returned no quota buckets".to_string(),
        ));
    }
    // A below-gate User-Agent is served HTTP 200 with the Gemini group silently
    // stripped, so the meters just vanish. Keep the surviving windows, but attach
    // a detail when no Gemini group came back at all, so a re-gate (or a stale
    // pin) is visible rather than silent — the failure mode this module exists to
    // avoid.
    let detail = if saw_gemini_group {
        None
    } else {
        Some(
            "retrieveUserQuotaSummary returned no Gemini quota group — the API \
             may require a newer Antigravity CLI User-Agent"
                .to_string(),
        )
    };
    Ok((windows, detail))
}

fn agy_quota_summary(
    client: &Client,
    token: &str,
) -> Result<(Vec<UsageWindow>, Option<String>), UsageError> {
    let resp = client
        .post(format!("{AGY_HOST}/v1internal:retrieveUserQuotaSummary"))
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", AGY_USER_AGENT)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({}))
        .send()
        .map_err(|e| UsageError::Shape(format!("retrieveUserQuotaSummary failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(UsageError::Shape(format!(
            "retrieveUserQuotaSummary HTTP {}",
            resp.status().as_u16()
        )));
    }
    parse_agy_quota_summary(&resp.text().unwrap_or_default())
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
        .map(|sort| {
            sort.groups
                .iter()
                .flat_map(|g| g.model_ids.iter())
                .collect()
        })
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

/// True when an Antigravity HTTP helper reported 401/403 — the bearer was
/// rejected, so a different credential source may still succeed.
fn agy_http_auth_failure(err: &UsageError) -> bool {
    match err {
        UsageError::Shape(msg) => msg.contains("HTTP 401") || msg.contains("HTTP 403"),
        UsageError::NoCredential(_) => false,
    }
}

/// One-token attempt: quota summary, then model-API fallback. Returns
/// `Err` on auth rejection or hard failure so the caller can try the next
/// credential source; success and model-API outcomes return directly.
fn try_agy_usage_with_token(client: &Client, token: &str) -> Result<UsageOutcome, UsageError> {
    // retrieveUserQuotaSummary is the HTTP surface behind `agy /usage`: both
    // the 5-hour and weekly shared buckets, without booting the CLI. Empty
    // body is enough (project is optional). Fall back to fetchAvailableModels
    // when the summary RPC is missing or empty so older backends still show
    // the five-hour meter.
    match agy_quota_summary(client, token) {
        Ok((windows, detail)) => {
            return Ok(UsageOutcome::Reading {
                windows,
                balance: None,
                meters: vec![],
                detail,
            });
        }
        Err(error) if agy_http_auth_failure(&error) => return Err(error),
        Err(error) => {
            tracing::debug!(
                "Antigravity quota summary unavailable; falling back to model API: {error}"
            );
        }
    }
    // fetchAvailableModels needs the user's cloudaicompanion project, which
    // loadCodeAssist hands back.
    let project = agy_load_project(client, token)?;

    Ok(fetch_usage(
        ResponsePolicy::default(),
        |c| {
            c.post(format!("{AGY_HOST}/v1internal:fetchAvailableModels"))
                .header("Authorization", format!("Bearer {token}"))
                .header("User-Agent", AGY_USER_AGENT)
                .header("Content-Type", "application/json")
                .json(&serde_json::json!({ "project": project }))
        },
        |body| {
            let (windows, detail) = parse_agy_models(body)?;
            Ok(UsageReading {
                windows,
                balance: None,
                meters: Vec::new(),
                detail,
            })
        },
    ))
}

/// Classify a per-source fetch failure into the outcome taxonomy (issue
/// #1758): an auth rejection means the Bearer [REDACTED] gone/bad (`Rejected`, so the
/// gate drops the native row); anything else is transport-class
/// (`Unavailable`, so the row stays visible with red error copy).
fn agy_outcome_from_source_error(error: UsageError) -> UsageOutcome {
    if agy_http_auth_failure(&error) {
        UsageOutcome::Rejected {
            hint: error.to_string(),
        }
    } else {
        UsageOutcome::Unavailable {
            reason: error.to_string(),
        }
    }
}

/// Public Antigravity fetcher. Collects the CLI oauth-file and keyring
/// tokens, then tries each in order until one succeeds.
///
/// Issue #1745 phase 2 step 12: migrated to the outcome seam. Missing
/// tokens → `NoCredential` (the gate drops the row, as before). The
/// client-build failure and non-auth source failures are transport-class →
/// `Unavailable` (previously `logged_out`, which silently dropped the row).
pub fn agy_usage() -> UsageOutcome {
    use crate::services::usage::adapters::agy::collect_agy_access_tokens_default;

    let tokens = match collect_agy_access_tokens_default() {
        Ok(t) => t,
        Err(e) => {
            return UsageOutcome::NoCredential {
                hint: e.to_string(),
            }
        }
    };
    let client = match blocking_client_builder()
        .timeout(Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return UsageOutcome::Unavailable {
                reason: format!("Client error: {e}"),
            }
        }
    };

    let last = tokens.len().saturating_sub(1);
    for (idx, cred) in tokens.iter().enumerate() {
        match try_agy_usage_with_token(&client, &cred.access_token) {
            Ok(outcome) => {
                tracing::debug!(
                    target: "services::usage::agy",
                    source = cred.source.as_str(),
                    "Antigravity usage fetch succeeded"
                );
                return outcome;
            }
            Err(error) if agy_http_auth_failure(&error) && idx < last => {
                tracing::debug!(
                    target: "services::usage::agy",
                    source = cred.source.as_str(),
                    next = tokens[idx + 1].source.as_str(),
                    error = %error,
                    "Antigravity auth rejected; trying next credential source"
                );
                continue;
            }
            Err(error) => {
                // Auth rejection on the last source, or loadCodeAssist failure
                // after quota-summary fell through — classified by
                // `agy_outcome_from_source_error` so rejections drop the row
                // and transport failures stay visible.
                return agy_outcome_from_source_error(error);
            }
        }
    }

    UsageOutcome::NoCredential {
        hint: "Antigravity OAuth token not found (antigravity-oauth-token or gemini:antigravity)"
            .to_string(),
    }
}

#[cfg(test)]
#[path = "agy_tests.rs"]
mod tests;
