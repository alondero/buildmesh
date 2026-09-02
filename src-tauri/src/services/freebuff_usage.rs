//! Freebuff First-class Model Provider — credential parsing + quota fetching.
//!
//! Freebuff is an AI-coding CLI built on Codebuff (issue #1437). Its
//! CLI-managed credential lives at `~/.config/manicode/credentials.json`
//! (or `$XDG_CONFIG_HOME/manicode/credentials.json` if the user overrides
//! XDG defaults), with the shape:
//!
//! ```json
//! { "authToken": "...", "fingerprintId": "...", "email": "..." }
//! ```
//!
//! All three fields are required for a usable bearer. The quota endpoint
//! (URL below) is reverse-engineered from upstream traffic; treat non-200
//! responses or shape mismatches as "usage unavailable" rather than errors.
//!
//! Extracted from `services/usage.rs` (issue #1438 review): the prior
//! monolithic placement put 738 lines in a 6,200-line god-module. The
//! public surface is just [`freebuff_usage`], re-exported from
//! `services::usage` so `commands::usage.rs` need not change.
//!
//! `native_harness_for("freebuff") = Some("freebuff")` in
//! `commands/usage.rs` makes the meter card detection-gated: it appears
//! only when the freebuff binary is installed.

use reqwest::blocking::Client;
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use super::usage::{logged_out, unavailable, BillingBalance, ProviderUsage, UsageError, UsageWindow};

// ─── Constants ───────────────────────────────────────────────────────────────

/// Production Freebuff quota endpoint. Replace if the upstream host moves —
/// the test seam [`freebuff_usage_with`] lets callers swap it for a
/// loopback listener without touching the file or the production code path.
pub(crate) const FREEBUFF_USAGE_URL: &str = "https://www.codebuff.com/api/usage";

/// Identity advertised on every quota probe. Some upstreams / reverse
/// proxies drop or rate-limit bare `reqwest` clients that don't carry a
/// recognisable User-Agent; this string is the explicit, honest equivalent
/// of Cursor's `Mozilla/5.0` workaround at `services/usage.rs` (the
/// `cursor_usage_with_sources` HTTP fetch).
const FREEBUFF_USER_AGENT: &str = "Buildmesh-Usage-Probe/1.0";

/// Hard cap on the body text we embed in error strings forwarded to the UI.
/// A misbehaving upstream (e.g. Cloudflare HTML error page, raw stack
/// trace) could otherwise leak tens of kilobytes into the `<UsagePanel>`
/// card via Tauri IPC. 120 chars + a 1-char `…` suffix fits in any
/// `<UsagePanel>` rendering budget; truncation prevents UI breakage.
const ERROR_BODY_MAX_CHARS: usize = 120;

// ─── Domain types ────────────────────────────────────────────────────────────

/// Strongly-typed Freebuff credential triple. Replaces the anonymous
/// `(String, String, String)` that earlier drafts exposed — the named
/// fields make call sites self-documenting (`auth.token` reads better
/// than positional destructuring at the call site, and prevents the
/// "is it token or fingerprint" argument-order bug class).
#[derive(Debug, Clone, PartialEq)]
pub struct FreebuffAuth {
    pub token: String,
    pub fingerprint: String,
    pub email: String,
}

// ─── Credential discovery ───────────────────────────────────────────────────

/// Returns the ordered list of Freebuff credential-file candidates (issue
/// #1438). Priority:
///
/// 1. `$XDG_CONFIG_HOME/manicode/credentials.json` if the env var is set
///    to a non-empty path (XDG Base Directory Specification).
/// 2. `<home>/.config/manicode/credentials.json` — the XDG default the
///    upstream Freebuff CLI writes on every platform when XDG_CONFIG_HOME
///    is not configured. `home_dir()` prefers `USERPROFILE` then `HOME`
///    so on Windows host the path resolves to
///    `%USERPROFILE%\.config\manicode\credentials.json`; on Linux/macOS
///    to `~/.config/manicode/credentials.json`.
/// 3. WSL fallback (Windows host only):
///    `/home/<USERNAME>/.config/manicode/credentials.json` mapped via
///    `env::to_host_path` so the UNC string never escapes the
///    `host_path` module. Mirrors the WSL fallback in
///    `discover_codex_auth_paths` (issue #1108, spec §2.2 #3).
///
/// Resolution is strictly passive — the candidates are returned; the
/// caller walks them in order. We never wake a sleeping WSL distro; if
/// the UNC read fails, we move on to the next candidate.
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
        super::usage::home_dir()
            .join(".config")
            .join("manicode")
            .join("credentials.json"),
    );

    // 3. WSL fallback (Windows host only).
    #[cfg(target_os = "windows")]
    {
        if let Some(username) = env::var("USERNAME").ok().filter(|s| !s.is_empty()) {
            let linux_path =
                format!("/home/{}/.config/manicode/credentials.json", username);
            let host_path = crate::env::to_host_path(&linux_path);
            if host_path != linux_path {
                paths.push(PathBuf::from(host_path));
            }
        }
    }

    paths
}

// ─── Credential parsing ─────────────────────────────────────────────────────

/// One Freebuff credentials.json entry. Each field is `Option<String>`
/// rather than `String` so the parser does not hard-fail on a partial /
/// in-flight credential write — the missing-field branch in
/// [`read_freebuff_credentials`] reports `NoCredential` instead.
/// All three fields are required for a usable bearer: `authToken` is the
/// OAuth-style access token, `fingerprintId` is a device fingerprint the
/// upstream quota API expects on every request, and `email` identifies
/// the account for telemetry / detail labels.
#[derive(Deserialize, Debug, Default)]
struct FreebuffCredentials {
    #[serde(default, rename = "authToken")]
    auth_token: Option<String>,
    #[serde(default, rename = "fingerprintId")]
    fingerprint_id: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

/// Walk the candidates in order and return the first credential triple
/// whose three required fields are all non-blank (after `.trim()`). The
/// trailing `Err` carries the path we last attempted so the UI can
/// surface "No credential found at <path>" (per
/// [`UsageError::Display`]). `.trim()`-and-check guards against
/// whitespace-padded values like `"   "` reaching the upstream — `"   "`
/// is a valid JSON string that would deserialize fine but produce a
/// useless bearer.
fn read_freebuff_credentials(candidates: &[PathBuf]) -> Result<FreebuffAuth, UsageError> {
    let mut last_attempted: Option<PathBuf> = None;
    for path in candidates {
        last_attempted = Some(path.clone());
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue, // candidate missing → try next
        };
        let creds: FreebuffCredentials = serde_json::from_str(&content)
            .map_err(|e| UsageError::Shape(format!("{}: {}", path.to_string_lossy(), e)))?;
        let token = trimmed_non_blank(creds.auth_token.as_deref());
        let fingerprint = trimmed_non_blank(creds.fingerprint_id.as_deref());
        let email = trimmed_non_blank(creds.email.as_deref());
        if let (Some(token), Some(fingerprint), Some(email)) = (token, fingerprint, email) {
            return Ok(FreebuffAuth { token, fingerprint, email });
        }
    }
    Err(UsageError::NoCredential(
        last_attempted
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| {
                "$XDG_CONFIG_HOME/manicode/credentials.json \
                 (or ~/.config/manicode/credentials.json)"
                    .to_string()
            }),
    ))
}

/// Returns `Some(trimmed_str)` if `raw` is `Some` and trimmed is
/// non-empty, else `None`. Centralises the `.trim().is_empty()` check
/// so the credential reader stays compact.
fn trimmed_non_blank(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

// ─── Response parsing ───────────────────────────────────────────────────────

/// Response envelope from the Freebuff streak / session quota endpoint.
/// `windows` and `earned_sessions` are optional so a partial response
/// (one meter kind available) still maps cleanly to a populated
/// `windows` or `balance`. `currency` is optional — many upstream
/// responses omit it; we fall back to USD (Freebuff's documented
/// pricing is USD-only today).
#[derive(Deserialize, Debug, Default)]
struct FreebuffUsageWindow {
    #[serde(default)]
    label: Option<String>,
    #[serde(default, rename = "usedPercent")]
    used_percent: Option<f64>,
    #[serde(default, rename = "resetsAt")]
    resets_at: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
struct FreebuffUsageResponse {
    #[serde(default)]
    windows: Vec<FreebuffUsageWindow>,
    /// Earned sessions available for the Freebuff streak program.
    /// Surfaced through `BillingBalance.remaining`; `monthly_spend` is
    /// `None` because the upstream endpoint reports a wallet-style
    /// counter, not period spend. Matches the Kimi / MiniMax pattern
    /// (#537).
    #[serde(default, rename = "earnedSessions")]
    earned_sessions: Option<f64>,
    /// Some upstream responses include a currency code (e.g. `"USD"`);
    /// many omit the field entirely. When omitted we fall back to
    /// `"USD"` — Freebuff's documented pricing is USD-only today, and
    /// the fallback keeps the `<BalanceCard>` shape well-formed rather
    /// than `None`.
    #[serde(default)]
    currency: Option<String>,
}

/// Parse the Freebuff usage body into the `(Vec<UsageWindow>,
/// Option<BillingBalance>)` pair that [`ProviderUsage`] consumes.
/// Returns `Result` (not `Option`) so a malformed body surfaces a
/// `Shape` error rather than silently zeroing every meter — same
/// contract as `parse_kimi_response` / `parse_openrouter_response`
/// / `parse_commandcode_credits_response`.
fn parse_freebuff_response(
    body: &str,
) -> Result<(Vec<UsageWindow>, Option<BillingBalance>), UsageError> {
    let resp: FreebuffUsageResponse =
        serde_json::from_str(body).map_err(|e| UsageError::Shape(e.to_string()))?;

    let windows = resp
        .windows
        .into_iter()
        .map(|w| UsageWindow {
            // Default the label so an upstream omission still produces a
            // readable card heading. Mirrors Command Code's hard-coded
            // labels.
            label: w.label.unwrap_or_else(|| "Daily".to_string()),
            used_percent: w
                .used_percent
                .filter(|p| p.is_finite())
                .map(|p| p.clamp(0.0, 100.0)),
            resets_at: w.resets_at,
        })
        .collect();

    let balance = resp
        .earned_sessions
        // Guard against NaN, ±∞, and negative wallet counts. Without this
        // filter an upstream bug (or a hostile response) could leak
        // invalid floats into TypeScript — `<BalanceCard>` does not
        // check `isFinite`, so it would forward the bad number verbatim
        // to the user's React tree.
        .filter(|earned| earned.is_finite() && *earned >= 0.0)
        .map(|earned| BillingBalance {
            remaining: earned,
            monthly_spend: None,
            currency: resp.currency.unwrap_or_else(|| "USD".to_string()),
        });

    Ok((windows, balance))
}

// ─── Error-body hygiene ─────────────────────────────────────────────────────

/// Clamp an upstream error body into a single short line that fits inside
/// the `unavailable(...)` `error: String` field. Two lines of defence:
///
/// 1. Collapse all whitespace (incl. newlines) to single spaces —
///    Cloudflare and most reverse proxies emit multi-line HTML / JSON
///    error pages that would otherwise render as a 4-line card.
/// 2. Truncate to [`ERROR_BODY_MAX_CHARS`] characters with a trailing
///    `…` so a server-side raw stack trace cannot reach the
///    `<UsagePanel>` via Tauri IPC.
///
/// Empty input yields `"usage endpoint failed"` so the UI copy never
/// reads `"API error 500: "` with a trailing colon.
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

/// Public Freebuff usage fetcher. Reads the CLI-managed credential and
/// queries the upstream quota endpoint directly — spawning the CLI
/// would make a usage refresh wait on an interactive process startup
/// (same rationale that drives `commandcode_usage`).
pub fn freebuff_usage() -> ProviderUsage {
    freebuff_usage_with(&freebuff_credential_paths(), FREEBUFF_USAGE_URL)
}

/// Test seam: callers inject a candidate list (so a temp directory can
/// stand in for `<home>/.config/manicode/credentials.json`) and a
/// loopback URL (so the live HTTP fetch can run against a `tiny_http`
/// listener without touching the network). Mirrors
/// `cursor_usage_with_sources` / `commandcode_usage_with_path`.
pub(crate) fn freebuff_usage_with(
    candidates: &[PathBuf],
    live_url: &str,
) -> ProviderUsage {
    let auth = match read_freebuff_credentials(candidates) {
        Ok(auth) => auth,
        Err(error) => return logged_out("freebuff", error.to_string()),
    };
    let client = match Client::builder().timeout(Duration::from_secs(15)).build() {
        Ok(client) => client,
        Err(error) => return unavailable("freebuff", format!("Client error: {error}")),
    };
    let response = match client
        .get(live_url)
        .header("Authorization", format!("Bearer {}", auth.token))
        .header("X-Fingerprint-Id", auth.fingerprint.as_str())
        .header("X-User-Email", auth.email.as_str())
        .header("User-Agent", FREEBUFF_USER_AGENT)
        .send()
    {
        Ok(response) => response,
        Err(error) => return unavailable("freebuff", format!("Request failed: {error}")),
    };
    let status = response.status().as_u16();
    let body = match response.text() {
        Ok(body) => body,
        Err(error) => return unavailable("freebuff", format!("Failed to read response: {error}")),
    };

    // Status classification is split so a WAF / CDN 403 (HTML body,
    // unparseable envelope) cannot wipe the user's login state -- only a
    // genuine 401 surfaces as `logged_out` on an unparseable body.
    match classify_status(status, &body) {
        StatusOutcome::Logout(msg) => logged_out("freebuff", msg),
        StatusOutcome::Unavailable(msg) => unavailable("freebuff", msg),
        StatusOutcome::EmptyCard => ProviderUsage {
            provider: "freebuff".to_string(),
            logged_in: true,
            windows: vec![],
            balance: None,
            detail: Some("no active session".to_string()),
            error: None,
        },
        StatusOutcome::Parse => match parse_freebuff_response(&body) {
            Ok((windows, balance)) => ProviderUsage {
                provider: "freebuff".to_string(),
                logged_in: true,
                windows,
                balance,
                detail: None,
                error: None,
            },
            Err(error) => unavailable("freebuff", format!("Failed to parse response: {}", error)),
        },
    }
}

/// Internal classification result for [`freebuff_usage_with`]. Kept
/// private (not the cross-provider `StatusDecision`) because freebuff
/// returns `(Vec<UsageWindow>, Option<BillingBalance>)` — its own
/// shape, not the `(Vec<UsageWindow>, Option<String>)` shape that
/// the cross-provider driver expects. The fetcher handles each
/// variant inline.
enum StatusOutcome {
    Logout(String),
    Unavailable(String),
    /// 404 with the documented "no active session" body -- empty OR
    /// `{"status":"none"}`. A generic 404 (HTML page, malformed JSON)
    /// falls through to `Unavailable` so a retired upstream route
    /// doesn't masquerade as a healthy empty session.
    EmptyCard,
    Parse,
}

/// Borrowed slice of the upstream error envelope. Deserialised in place
/// (no heap allocation) so [`is_no_active_session`] and the auth-error
/// classifiers inspect only the `status` field without allocating a
/// full `serde_json::Value` AST. PR #1443 round-6 review item #2.
#[derive(Deserialize)]
struct StatusEnvelope<'a> {
    #[serde(borrow)]
    status: Option<&'a str>,
}

/// Is this 404 body actually a "no active session" reply? The upstream
/// returns either an empty body or `{"status":"none"}` for the
/// documented "no active session" state; anything else (HTML page,
/// generic error JSON) is a routing failure and surfaces as
/// `Unavailable("API error 404: ...")`. Zero heap allocations --
/// borrows the `status` slice from `body`.
fn is_no_active_session(body: &str) -> bool {
    body.is_empty()
        || serde_json::from_str::<StatusEnvelope>(body)
            .is_ok_and(|e| e.status == Some("none"))
}

/// Classify 401 (Unauthorized). An unparseable body falls through to
/// the generic "session expired" logout since 401 is genuinely a
/// credential failure.
fn classify_401_error(body: &str) -> StatusOutcome {
    if let Ok(env) = serde_json::from_str::<StatusEnvelope>(body) {
        if env.status == Some("banned") {
            return StatusOutcome::Logout("Freebuff account suspended".to_string());
        }
    }
    StatusOutcome::Logout(
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
        Some("banned") => StatusOutcome::Logout("Freebuff account suspended".to_string()),
        Some("country_blocked") => StatusOutcome::Unavailable(
            "Freebuff is not available in this region".to_string(),
        ),
        // Any other `status` value (recognised or absent) indicates the
        // upstream actively rejected the request -- surface as
        // Unavailable so the user can investigate without their login
        // being wiped.
        _ => forbidden_default(),
    }
}

/// Single source of truth for HTTP status classification. PR #1443
/// round-6 review item #6: 404 handling is self-contained in one
/// arm so every status code reads top-to-bottom without mental
/// back-tracking. `200..=299 => Parse` lives at the top for the
/// common-path-first convention (round-6 review nitpick).
fn classify_status(status: u16, body: &str) -> StatusOutcome {
    match status {
        200..=299 => StatusOutcome::Parse,
        401 => classify_401_error(body),
        403 => classify_403_error(body),
        404 => {
            if is_no_active_session(body) {
                StatusOutcome::EmptyCard
            } else {
                StatusOutcome::Unavailable(format!("API error 404: {}", clamp_error_body(body)))
            }
        }
        429 => StatusOutcome::Unavailable(
            "Rate limited -- usage data temporarily unavailable".to_string(),
        ),
        s => StatusOutcome::Unavailable(format!("API error {s}: {}", clamp_error_body(body))),
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────
//
// Pinning the four-part contract:
//   1. credentials.json parsing recognises the three required fields and
//      reports `NoCredential` when any are missing/blank/whitespace-only.
//   2. The HTTP fetcher hits the streak / session quota endpoint with the
//      bearer + fingerprint + email + user-agent headers, maps the
//      response into `UsageWindow` (daily quota) + `BillingBalance`
//      (earned sessions).
//   3. Graceful degradation on 401/403/429 follows the Kimi / Command
//      Code contract (logged_out on auth failure, unavailable on rate
//      limit).
//   4. Detection gating is a separate concern (lives in
//      `commands/usage.rs`); this module just fetches.

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use crate::services::usage::spawn_loopback;

    /// Helper: write a `credentials.json` with the standard three fields
    /// into a tempdir. Mirrors the cursor / commandcode test fixtures.
    fn write_freebuff_credentials(dir: &Path, body: &str) -> PathBuf {
        let cfg_dir = dir.join(".config").join("manicode");
        std::fs::create_dir_all(&cfg_dir).expect("mkdir");
        let path = cfg_dir.join("credentials.json");
        std::fs::write(&path, body).expect("write credentials.json");
        path
    }

    // ── parse_freebuff_response — issue #1438 ────────────────────────────

    #[test]
    fn parse_freebuff_response_maps_windows_and_balance() {
        // Headline shape: window array with non-default label/percent +
        // reset timestamp, plus an `earnedSessions` figure. This mirrors
        // the documented contract (`<Daily>` is the only required label,
        // the upstream may add `<Weekly>` / `<Monthly>` rows in the
        // future).
        let body = r#"{
            "windows": [
                { "label": "Daily", "usedPercent": 35.5, "resetsAt": "2026-09-02T00:00:00Z" }
            ],
            "earnedSessions": 12.0,
            "currency": "USD"
        }"#;
        let (windows, balance) = parse_freebuff_response(body).expect("parse");
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "Daily");
        assert_eq!(windows[0].used_percent, Some(35.5));
        assert_eq!(
            windows[0].resets_at.as_deref(),
            Some("2026-09-02T00:00:00Z")
        );
        let bal = balance.expect("balance present");
        assert_eq!(bal.remaining, 12.0);
        assert_eq!(bal.currency, "USD");
        assert_eq!(bal.monthly_spend, None);
    }

    #[test]
    fn parse_freebuff_response_clamps_out_of_range_percent() {
        // 150.0% is nonsense (could be a stale meter or an upstream
        // bug); the parser clamps to 100.0 so the UI's progress bar
        // doesn't render a backwards fill. Matches the
        // `used_percent.clamp(0.0, 100.0)` rule shared by codex /
        // openai parsers.
        let body = r#"{ "windows": [{ "usedPercent": 150.0 }], "earnedSessions": 0 }"#;
        let (windows, _) = parse_freebuff_response(body).expect("parse");
        assert_eq!(windows[0].used_percent, Some(100.0));
    }

    #[test]
    fn parse_freebuff_response_default_label_when_window_lacks_label() {
        // Upstream omission of `label` must still produce a readable
        // meter. The parser falls back to "Daily" so the React side
        // renders a known string rather than an empty heading.
        let body = r#"{ "windows": [{ "usedPercent": 10.0 }] }"#;
        let (windows, _) = parse_freebuff_response(body).expect("parse");
        assert_eq!(windows[0].label, "Daily");
        assert_eq!(windows[0].used_percent, Some(10.0));
    }

    #[test]
    fn parse_freebuff_response_default_currency_when_endpoint_omits_currency() {
        // Endpoints that omit the currency field default to USD — the
        // only documented currency. The fallback is checked against the
        // wallet card's render path (`<BalanceCard>` always carries a
        // currency).
        let body = r#"{ "earnedSessions": 5.5 }"#;
        let (_, balance) = parse_freebuff_response(body).expect("parse");
        assert_eq!(balance.unwrap().currency, "USD");
    }

    #[test]
    fn parse_freebuff_response_malformed_body_returns_shape_error() {
        // The parser returns `Result` so a malformed body surfaces as
        // a `UsageError::Shape` rather than silently zeroing every
        // meter — matches the kimi / openrouter contract.
        let body = "not-json-at-all";
        assert!(parse_freebuff_response(body).is_err());
    }

    #[test]
    fn parse_freebuff_response_filters_negative_earned_sessions() {
        // A negative earned-session figure is nonsense — guard against
        // it so the wallet card renders zero / hides. Tested alongside
        // the NaN / Infinity branches below.
        let body = r#"{ "earnedSessions": -5.0 }"#;
        let (_, balance) = parse_freebuff_response(body).expect("parse");
        assert!(balance.is_none(), "negative earnedSessions must be filtered out");
    }

    #[test]
    fn parse_freebuff_response_filters_nan_earned_sessions() {
        // NaN invalid floats must NOT leak to TypeScript — the
        // upstream must have sent bad data, but the parser drops it
        // rather than forwarding invalid JSON numbers.
        let body = r#"{ "earnedSessions": null }"#;
        let (_, balance) = parse_freebuff_response(body).expect("parse");
        assert!(balance.is_none(), "null earnedSessions must yield None");
    }

    // ── clamp_error_body — review item #4A ────────────────────────────────

    #[test]
    fn clamp_error_body_returns_placeholder_for_empty_input() {
        // Empty body must not produce `format!("API error {code}: ")` —
        // the `<UsagePanel>` would render a trailing colon and look
        // broken. The placeholder matches the original `unavailable`
        // copy.
        assert_eq!(clamp_error_body(""), "usage endpoint failed");
        assert_eq!(clamp_error_body("   \n  \t  "), "usage endpoint failed");
    }

    #[test]
    fn clamp_error_body_collapses_whitespace_to_single_line() {
        // Cloudflare / nginx emit HTML error pages with newlines and
        // runs of spaces. Collapse to a single line so the UI's
        // `<UsagePanel>` card doesn't break its layout.
        let body = "<html>\n  <body><h1>502 Bad Gateway</h1></body>\n</html>";
        let result = clamp_error_body(body);
        assert!(!result.contains('\n'), "must collapse newlines, got {result:?}");
        assert!(result.starts_with("<html>"));
        assert!(result.contains("502 Bad Gateway"));
    }

    #[test]
    fn clamp_error_body_truncates_long_bodies_with_ellipsis() {
        // A 15 KB raw upstream stack trace must NOT reach the
        // `<UsagePanel>`. Truncation adds a `…` suffix so the UI
        // renders one trailing char instead of cutting mid-glyph.
        let long_body = "a".repeat(ERROR_BODY_MAX_CHARS * 10);
        let result = clamp_error_body(&long_body);
        let char_count = result.chars().count();
        assert!(char_count <= ERROR_BODY_MAX_CHARS + 1, "got {char_count} chars");
        assert!(result.ends_with('…'), "expected `…` suffix, got {result:?}");
    }

    #[test]
    fn clamp_error_body_preserves_short_bodies_verbatim() {
        // A short body (under the cap) must NOT be truncated —
        // appending `…` to a passing string would confuse the UI.
        let body = "backend unavailable";
        assert_eq!(clamp_error_body(body), "backend unavailable");
    }

    #[test]
    fn clamp_error_body_handles_multibyte_utf8_by_char_count() {
        // Truncation must be by *char*, not byte — a body with CJK
        // glyphs would otherwise panic on a mid-codepoint byte slice.
        // This pins the `chars().take(...)` choice.
        let body = "测".repeat(ERROR_BODY_MAX_CHARS * 2);
        let result = clamp_error_body(&body);
        let char_count = result.chars().count();
        assert!(char_count <= ERROR_BODY_MAX_CHARS + 1);
        // The body has no spaces / newlines, so it doesn't collapse
        // and truncation kicks in directly.
        assert!(result.ends_with('…'));
    }

    // ── freebuff_credential_paths — issue #1438 + review item #4E ────────

    #[test]
    fn freebuff_credential_paths_includes_xdg_default() {
        // The XDG-style path is always present regardless of platform
        // — it's the primary candidate the upstream CLI writes to.
        let paths = freebuff_credential_paths();
        assert!(!paths.is_empty(), "must have at least the XDG candidate");
        let first = paths[0].to_string_lossy();
        assert!(
            first.ends_with(".config/manicode/credentials.json")
                || first.ends_with(".config\\manicode\\credentials.json"),
            "first candidate must be the XDG-style credentials.json, got {first}"
        );
    }

    #[test]
    fn freebuff_credential_paths_respects_xdg_config_home_override() {
        // XDG_CONFIG_HOME overrides the default — a user who sets
        // `XDG_CONFIG_HOME=/custom/cfg` must see their directory in
        // the candidate list ahead of `<home>/.config`. We use a
        // tempdir so the test is hermetic regardless of the caller's
        // actual XDG_CONFIG_HOME (production code reads it via
        // `env::var_os`).
        let dir = tempfile::tempdir().unwrap();
        let previous = env::var_os("XDG_CONFIG_HOME");
        // SAFETY: single-threaded test run (the env var mutation
        // would race with parallel tests; the test harness serialises
        // env-mutating tests via static locks where used). Setting
        // before read and restoring after keeps the cross-test
        // contract.
        unsafe { env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let paths = freebuff_credential_paths();
        // SAFETY: pair with the `set_var` above.
        unsafe {
            match previous.as_deref() {
                Some(v) => env::set_var("XDG_CONFIG_HOME", v),
                None => env::remove_var("XDG_CONFIG_HOME"),
            }
        }
        let first = paths[0].to_string_lossy();
        assert!(
            first.starts_with(dir.path().to_string_lossy().as_ref()),
            "first candidate must honour XDG_CONFIG_HOME, got {first}"
        );
    }

    // ── read_freebuff_credentials — issue #1438 + review item #4C ────────

    #[test]
    fn read_freebuff_credentials_parses_three_required_fields() {
        // Happy path: all three required fields present and non-empty.
        let dir = tempfile::tempdir().unwrap();
        let path = write_freebuff_credentials(
            dir.path(),
            r#"{
                "authToken": "fb_test_token_1",
                "fingerprintId": "fb_fp_test",
                "email": "user@example.com"
            }"#,
        );
        let candidates = vec![path];
        let auth = read_freebuff_credentials(&candidates).expect("credentials");
        assert_eq!(auth.token, "fb_test_token_1");
        assert_eq!(auth.fingerprint, "fb_fp_test");
        assert_eq!(auth.email, "user@example.com");
    }

    #[test]
    fn read_freebuff_credentials_missing_field_returns_no_credential() {
        // A credentials.json with only `authToken` (typical in-flight
        // write during token rotation) must NOT be treated as a usable
        // bearer — the upstream quota endpoint would 401 with
        // incomplete metadata.
        let dir = tempfile::tempdir().unwrap();
        let path = write_freebuff_credentials(
            dir.path(),
            r#"{ "authToken": "fb_test_token_2" }"#,
        );
        let candidates = vec![path];
        let err = read_freebuff_credentials(&candidates).expect_err("missing fields");
        assert!(
            matches!(err, UsageError::NoCredential(_)),
            "missing fields must yield NoCredential, got {err:?}"
        );
    }

    #[test]
    fn read_freebuff_credentials_blank_field_is_treated_as_missing() {
        // Literal empty strings count as missing. Trims first (see
        // the next test for whitespace handling).
        let dir = tempfile::tempdir().unwrap();
        let path = write_freebuff_credentials(
            dir.path(),
            r#"{
                "authToken": "fb_test_token_3",
                "fingerprintId": "",
                "email": "user@example.com"
            }"#,
        );
        let candidates = vec![path];
        let err = read_freebuff_credentials(&candidates).expect_err("blank field");
        assert!(matches!(err, UsageError::NoCredential(_)));
    }

    #[test]
    fn read_freebuff_credentials_whitespace_only_field_is_treated_as_missing() {
        // Review item #4C — `"   "` is valid JSON but a useless
        // bearer. Must be filtered out so an upstream proxy never
        // receives `Authorization: Bearer    ` (whitespace-only
        // bearer). This is the regression guard for the
        // `.trim().is_empty()` change.
        let dir = tempfile::tempdir().unwrap();
        let path = write_freebuff_credentials(
            dir.path(),
            r#"{
                "authToken": "fb_test_token_3b",
                "fingerprintId": "   ",
                "email": "user@example.com"
            }"#,
        );
        let candidates = vec![path];
        let err = read_freebuff_credentials(&candidates).expect_err("whitespace field");
        assert!(
            matches!(err, UsageError::NoCredential(_)),
            "whitespace-only field must yield NoCredential, got {err:?}"
        );
    }

    #[test]
    fn read_freebuff_credentials_trims_surrounding_whitespace_on_valid_fields() {
        // Padded but non-blank values (e.g. `authToken: "  abc  "`)
        // are trimmed before being treated as a usable bearer. The
        // JSON parser preserves internal whitespace, so without the
        // `.trim()` step the upstream quota API would receive a
        // `Bearer  abc  ` token and 401.
        let dir = tempfile::tempdir().unwrap();
        let path = write_freebuff_credentials(
            dir.path(),
            r#"{
                "authToken": "  fb_test_token_trim  ",
                "fingerprintId": "fb_fp",
                "email": "user@example.com"
            }"#,
        );
        let candidates = vec![path];
        let auth = read_freebuff_credentials(&candidates).expect("trim ok");
        assert_eq!(auth.token, "fb_test_token_trim");
        assert_eq!(auth.fingerprint, "fb_fp");
    }

    #[test]
    fn read_freebuff_credentials_missing_candidate_walks_to_next() {
        // If the primary XDG-style path is absent, the reader must
        // walk to the next candidate (test seam exercises the
        // fallback resolution). We supply an empty-marker first
        // candidate (no file at all) and a valid second one to prove
        // the walker does not bail after the first miss.
        let dir = tempfile::tempdir().unwrap();
        let valid_path = write_freebuff_credentials(
            dir.path(),
            r#"{
                "authToken": "fb_test_token_4",
                "fingerprintId": "fb_fp_4",
                "email": "user@example.com"
            }"#,
        );
        let missing = dir.path().join("nonexistent_credentials.json");
        let candidates = vec![missing, valid_path];
        let auth = read_freebuff_credentials(&candidates).expect("walker");
        assert_eq!(auth.token, "fb_test_token_4");
    }

    #[test]
    fn read_freebuff_credentials_invalid_json_returns_shape_error() {
        // Garbage in the credentials file (not JSON at all) is a
        // `UsageError::Shape`, signalling an upstream / file-format
        // breakage — not "no credential" (the file IS there).
        let dir = tempfile::tempdir().unwrap();
        let path = write_freebuff_credentials(dir.path(), "not-json");
        let candidates = vec![path.clone()];
        let err = read_freebuff_credentials(&candidates).expect_err("shape");
        match err {
            UsageError::Shape(msg) => assert!(
                msg.contains("credentials.json"),
                "Shape error must surface the path so the UI can hint where to look, got {msg}"
            ),
            other => panic!("expected Shape, got {other:?}"),
        }
    }

    // ── freebuff_usage_with — issue #1438 ────────────────────────────────

    /// Helper used by the loopback tests that need exactly one
    /// candidate file.
    fn one_candidate(path: &std::path::Path) -> Vec<PathBuf> {
        vec![path.to_path_buf()]
    }

    #[test]
    fn freebuff_usage_no_credential_returns_logged_out() {
        // No candidate path that exists → `logged_out` envelope so
        // the UI can render the re-login prompt rather than a blank
        // gauge.
        let dir = tempfile::tempdir().unwrap();
        let missing = vec![dir.path().join("nope").join("credentials.json")];
        let port = spawn_loopback(1, |req| {
            let _ = req.respond(tiny_http::Response::empty(204));
        });
        let usage = freebuff_usage_with(&missing, &format!("http://127.0.0.1:{port}/usage"));
        assert_eq!(usage.provider, "freebuff");
        assert!(!usage.logged_in);
        assert!(
            usage.error.as_deref().is_some_and(|e| e.contains("No credential")),
            "expected No credential error, got {:?}",
            usage.error
        );
    }

    #[test]
    fn freebuff_usage_401_returns_logged_out_with_relogin_copy() {
        // 401 (Unauthorized) signals a bad credential — surface as
        // `logged_out` with a copy that points the user at
        // `freebuff login`, matching the kimi_usage 401/403 branch
        // (`logged_out` so the UI shows the re-enter affordance, not
        // a generic failure card).
        let dir = tempfile::tempdir().unwrap();
        let path = write_freebuff_credentials(
            dir.path(),
            r#"{
                "authToken": "fb_revoked_token",
                "fingerprintId": "fb_fp",
                "email": "user@example.com"
            }"#,
        );
        let port = spawn_loopback(1, |req| {
            let _ = req.respond(tiny_http::Response::empty(401));
        });
        let usage = freebuff_usage_with(
            &one_candidate(&path),
            &format!("http://127.0.0.1:{port}/usage"),
        );
        assert_eq!(usage.provider, "freebuff");
        assert!(!usage.logged_in, "401 must flip to logged_out");
        assert_eq!(usage.windows.len(), 0);
        assert!(
            usage
                .error
                .as_deref()
                .is_some_and(|e| e.contains("freebuff login")),
            "401 prompt must reference 'freebuff login', got {:?}",
            usage.error
        );
    }

    #[test]
    fn freebuff_usage_403_with_unparseable_body_preserves_logged_in() {
        // 403 with an unparseable body (HTML error page from a WAF
        // / CDN, or an empty body) MUST NOT log the user out --
        // doing so would wipe their login state on a Cloudflare
        // block, which is the wrong UX (PR #1443 review round-4
        // item #2). The fetcher treats unparseable 403 as
        // `unavailable` and keeps `logged_in = true`.
        let dir = tempfile::tempdir().unwrap();
        let path = write_freebuff_credentials(
            dir.path(),
            r#"{
                "authToken": "fb_token",
                "fingerprintId": "fb_fp",
                "email": "user@example.com"
            }"#,
        );
        let port = spawn_loopback(1, |req| {
            let _ = req.respond(tiny_http::Response::empty(403));
        });
        let usage = freebuff_usage_with(
            &one_candidate(&path),
            &format!("http://127.0.0.1:{port}/usage"),
        );
        assert!(usage.logged_in, "403 with unparseable body must NOT log the user out");
        assert_eq!(
            usage.error.as_deref(),
            Some("Access forbidden (HTTP 403)")
        );
    }

    #[test]
    fn freebuff_usage_403_with_country_blocked_body_preserves_logged_in() {
        // 403 with the documented `{"status":"country_blocked"}` body
        // -- the user IS logged in (token valid), just not available
        // in this region. Surface as `unavailable`, not `logged_out`.
        let dir = tempfile::tempdir().unwrap();
        let path = write_freebuff_credentials(
            dir.path(),
            r#"{
                "authToken": "fb_token",
                "fingerprintId": "fb_fp",
                "email": "user@example.com"
            }"#,
        );
        let port = spawn_loopback(1, |req| {
            let _ = req.respond(
                tiny_http::Response::from_string(r#"{"status":"country_blocked"}"#)
                    .with_status_code(403),
            );
        });
        let usage = freebuff_usage_with(
            &one_candidate(&path),
            &format!("http://127.0.0.1:{port}/usage"),
        );
        assert!(usage.logged_in);
        assert_eq!(
            usage.error.as_deref(),
            Some("Freebuff is not available in this region")
        );
    }

    #[test]
    fn freebuff_usage_403_with_banned_body_logs_out() {
        // 403 with a valid JSON `{"status":"banned"}` body -- the
        // upstream IS actively informing us the account is suspended,
        // NOT a WAF block (a Cloudflare HTML block fails JSON parsing
        // and falls into the unparseable 403 arm). Log out so the
        // user sees the suspension and re-authenticates -- mirror the
        // 401 + banned behaviour. PR #1443 round-6 review item #4.
        let dir = tempfile::tempdir().unwrap();
        let path = write_freebuff_credentials(
            dir.path(),
            r#"{
                "authToken": "fb_token",
                "fingerprintId": "fb_fp",
                "email": "user@example.com"
            }"#,
        );
        let port = spawn_loopback(1, |req| {
            let _ = req.respond(
                tiny_http::Response::from_string(r#"{"status":"banned"}"#)
                    .with_status_code(403),
            );
        });
        let usage = freebuff_usage_with(
            &one_candidate(&path),
            &format!("http://127.0.0.1:{port}/usage"),
        );
        assert!(!usage.logged_in, "403 banned must log the user out");
        assert_eq!(usage.error.as_deref(), Some("Freebuff account suspended"));
    }

    #[test]
    fn freebuff_usage_404_with_no_active_session_body_surfaces_empty_card() {
        // The documented "no active session" reply -- empty body OR
        // `{"status":"none"}`. Token is still good; surface as a
        // logged-in card with empty windows + "no active session"
        // detail.
        let dir = tempfile::tempdir().unwrap();
        let path = write_freebuff_credentials(
            dir.path(),
            r#"{
                "authToken": "fb_token",
                "fingerprintId": "fb_fp",
                "email": "user@example.com"
            }"#,
        );

        // Empty 404 body.
        let port_empty = spawn_loopback(1, |req| {
            let _ = req.respond(tiny_http::Response::empty(404));
        });
        let usage_empty = freebuff_usage_with(
            &one_candidate(&path),
            &format!("http://127.0.0.1:{port_empty}/usage"),
        );
        assert!(usage_empty.logged_in, "404 must not log the user out");
        assert!(usage_empty.error.is_none());
        assert!(usage_empty.windows.is_empty());
        assert_eq!(
            usage_empty.detail.as_deref(),
            Some("no active session")
        );

        // {"status":"none"} body.
        let port_status = spawn_loopback(1, |req| {
            let _ = req.respond(
                tiny_http::Response::from_string(r#"{"status":"none"}"#).with_status_code(404),
            );
        });
        let usage_status = freebuff_usage_with(
            &one_candidate(&path),
            &format!("http://127.0.0.1:{port_status}/usage"),
        );
        assert!(usage_status.logged_in);
        assert!(usage_status.windows.is_empty());
        assert_eq!(
            usage_status.detail.as_deref(),
            Some("no active session")
        );
    }

    #[test]
    fn freebuff_usage_404_with_html_body_surfaces_unavailable() {
        // Generic 404 (HTML page from a retired route or missing
        // reverse-proxy entry) must surface as `unavailable`, NOT as
        // a logged-in "no active session" card. PR #1443 review
        // round-4 item #4.
        let dir = tempfile::tempdir().unwrap();
        let path = write_freebuff_credentials(
            dir.path(),
            r#"{
                "authToken": "fb_token",
                "fingerprintId": "fb_fp",
                "email": "user@example.com"
            }"#,
        );
        let port = spawn_loopback(1, |req| {
            let _ = req.respond(
                tiny_http::Response::from_string("<html>not found</html>").with_status_code(404),
            );
        });
        let usage = freebuff_usage_with(
            &one_candidate(&path),
            &format!("http://127.0.0.1:{port}/usage"),
        );
        assert!(usage.logged_in);
        assert!(
            usage
                .error
                .as_deref()
                .is_some_and(|e| e.starts_with("API error 404:")),
            "generic 404 must surface as Unavailable, got: {:?}",
            usage.error
        );
    }

    #[test]
    fn freebuff_usage_429_preserves_logged_in() {
        // 429 is transient (rate limit). We surface `unavailable`
        // but keep `logged_in = true` so the user doesn't think
        // their session is bad — same contract as Kimi / OpenRouter
        // / DeepSeek.
        let dir = tempfile::tempdir().unwrap();
        let path = write_freebuff_credentials(
            dir.path(),
            r#"{
                "authToken": "fb_test_token",
                "fingerprintId": "fb_fp",
                "email": "user@example.com"
            }"#,
        );
        let port = spawn_loopback(1, |req| {
            let _ = req.respond(tiny_http::Response::empty(429));
        });
        let usage = freebuff_usage_with(
            &one_candidate(&path),
            &format!("http://127.0.0.1:{port}/usage"),
        );
        assert!(usage.logged_in, "429 must NOT flip to logged_out");
        assert!(
            usage
                .error
                .as_deref()
                .is_some_and(|e| e.contains("Rate limited")),
            "429 copy must say 'Rate limited', got {:?}",
            usage.error
        );
    }

    #[test]
    fn freebuff_usage_500_returns_unavailable_with_clamped_body() {
        // 500 is `unavailable` (logged_in stays true because the
        // credential may be valid; the upstream just failed). The
        // body is clamped to ~120 chars + `…` so a 15 KB
        // HTML error page from Cloudflare does NOT reach the
        // `<UsagePanel>` card. Pin the truncation behaviour here.
        let dir = tempfile::tempdir().unwrap();
        let path = write_freebuff_credentials(
            dir.path(),
            r#"{
                "authToken": "fb_test_token",
                "fingerprintId": "fb_fp",
                "email": "user@example.com"
            }"#,
        );
        // Long body designed to exceed ERROR_BODY_MAX_CHARS so the
        // truncation branch fires.
        let long_body = "stack trace begins ".repeat(20);
        let expected_detail = clamp_error_body(&long_body);
        let port = spawn_loopback(1, move |req| {
            // `long_body` is moved into the `Fn` closure via `move`; `Fn`
            // closures can be called multiple times so `tiny_http::Response::from_string`
            // consuming it would require `Clone` (one clone per request).
            let _ = req.respond(
                tiny_http::Response::from_string(long_body.clone()).with_status_code(500),
            );
        });
        let usage = freebuff_usage_with(
            &one_candidate(&path),
            &format!("http://127.0.0.1:{port}/usage"),
        );
        assert!(usage.logged_in, "500 must NOT flip to logged_out");
        let error = usage.error.as_deref().expect("error string");
        assert!(
            error.starts_with("API error 500: "),
            "expected leading 'API error 500: ' prefix, got {error:?}"
        );
        assert!(
            error.ends_with('…'),
            "long upstream body must be truncated with '…' suffix, got {error:?}"
        );
        assert!(
            error.contains(&expected_detail),
            "expected the clamped detail fragment in the error, got {error:?}"
        );
    }

    #[test]
    fn freebuff_usage_live_loopback_maps_windows_and_balance() {
        // Happy-path end-to-end: real credentials.json on disk + a
        // tiny_http listener serving a valid response → populated
        // `UsageWindow`(s) + `BillingBalance`. Inspected headers
        // prove the fetcher threads auth + fingerprint + email +
        // user-agent through.
        use std::sync::{Arc, Mutex};

        let dir = tempfile::tempdir().unwrap();
        let path = write_freebuff_credentials(
            dir.path(),
            r#"{
                "authToken": "fb_live_token",
                "fingerprintId": "fb_live_fp",
                "email": "live@example.com"
            }"#,
        );
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_thread = captured.clone();
        let port = spawn_loopback(1, move |req| {
            // Capture every header name+value sent so we can assert
            // the four required ones (`Authorization`,
            // `X-Fingerprint-Id`, `X-User-Email`, `User-Agent`) all
            // reached the wire. `tiny_http::Header` exposes
            // `field: HeaderField` + `value: String`; we serialise
            // as `"<FieldName>: <value>"` so the lookup helper
            // below can match case-insensitively.
            for header in req.headers() {
                let name = header.field.as_str().as_str().to_string();
                let value = header.value.as_str().to_string();
                captured_thread
                    .lock()
                    .unwrap()
                    .push(format!("{}: {}", name, value));
            }
            let body = r#"{
                "windows": [
                    { "label": "Daily", "usedPercent": 22.5, "resetsAt": "2026-09-02T00:00:00Z" }
                ],
                "earnedSessions": 7,
                "currency": "USD"
            }"#;
            let _ = req.respond(tiny_http::Response::from_string(body));
        });
        let usage = freebuff_usage_with(
            &one_candidate(&path),
            &format!("http://127.0.0.1:{port}/usage"),
        );
        assert_eq!(usage.provider, "freebuff");
        assert!(usage.logged_in);
        assert_eq!(usage.windows.len(), 1);
        assert_eq!(usage.windows[0].label, "Daily");
        assert_eq!(usage.windows[0].used_percent, Some(22.5));
        assert_eq!(
            usage.windows[0].resets_at.as_deref(),
            Some("2026-09-02T00:00:00Z")
        );
        let balance = usage.balance.expect("balance present");
        assert_eq!(balance.remaining, 7.0);
        assert_eq!(balance.currency, "USD");
        assert_eq!(balance.monthly_spend, None);

        // Header assertions: the fetcher must thread auth +
        // fingerprint + email + User-Agent exactly as the
        // upstream expects. An accidental drop of any of these
        // would 401 / 403 / rate-limit in production. Review item
        // #4D pins the User-Agent assertion explicitly.
        let headers = captured.lock().unwrap();
        let header_lookup = |name: &str| -> Option<String> {
            // Captured lines look like `"<FieldName>: <value>"`.
            // Search the prefix case-insensitively against the
            // requested header name.
            let prefix = format!("{}:", name);
            headers
                .iter()
                .find(|h| h.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase()))
                .and_then(|h| h.split_once(": ").map(|(_, v)| v.to_string()))
        };
        let auth = header_lookup("Authorization").expect("Authorization header");
        assert_eq!(auth, "Bearer fb_live_token");
        assert_eq!(
            header_lookup("X-Fingerprint-Id").as_deref(),
            Some("fb_live_fp")
        );
        assert_eq!(
            header_lookup("X-User-Email").as_deref(),
            Some("live@example.com")
        );
        // Review item #4D — User-Agent must reach the wire so
        // Cloudflare / reverse proxies don't drop the request.
        assert_eq!(
            header_lookup("User-Agent").as_deref(),
            Some(FREEBUFF_USER_AGENT),
        );
    }

    #[test]
    fn freebuff_usage_malformed_live_body_returns_unavailable_with_parse_error() {
        // 200 + garbage body → `unavailable` (logged_in stays true;
        // the credential works, just not for this endpoint shape).
        // Same contract as codex / kimi shape-failure.
        let dir = tempfile::tempdir().unwrap();
        let path = write_freebuff_credentials(
            dir.path(),
            r#"{
                "authToken": "fb_test_token",
                "fingerprintId": "fb_fp",
                "email": "user@example.com"
            }"#,
        );
        let port = spawn_loopback(1, |req| {
            let _ = req.respond(tiny_http::Response::from_string("not-json"));
        });
        let usage = freebuff_usage_with(
            &one_candidate(&path),
            &format!("http://127.0.0.1:{port}/usage"),
        );
        assert!(usage.logged_in, "shape error must NOT flip to logged_out");
        assert!(
            usage
                .error
                .as_deref()
                .is_some_and(|e| e.contains("Failed to parse response")),
            "expected parse-error copy, got {:?}",
            usage.error
        );
        // Critical: empty meter fields, not bogus zero values.
        assert!(usage.windows.is_empty());
        assert!(usage.balance.is_none());
    }
}
