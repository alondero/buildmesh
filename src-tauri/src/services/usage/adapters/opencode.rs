//! OpenCode native adapter — Buildmesh-owned OAuth credential +
//! local `opencode.db` SQLite fallback. Detection-gated on `opencode`.
//!
//! The fetch lives here rather than in the legacy `usage.rs` module
//! (issue #2528): opencode owns its credential read, its live `_server
//! billing.get` probe, its refresh seam, its offline SQLite fallback and
//! both response parses.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{fetch_usage, UsageAdapter};
use crate::services::usage::cache::{cached_age, invalidate_provider_cache};
use crate::services::usage::outcome::{ResponsePolicy, UsageOutcome, UsageReading};
use crate::services::usage::types::{home_dir, UsageError, UsagePage, UsageWindow};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
// The OpenCode OAuth DTO + parser were extracted to `services::opencode_oauth`
// for #956 so the OAuth dance and the live fetcher don't share a private
// helper. The constant `OPENCODE_CONSOLE_CRED_TARGET` was lifted along with
// it; the parser stays qualified (the live call site reads
// `opencode_oauth::parse_opencode_console_full_credential(...)` through
// `opencode_live_request_parts`) to make the module boundary obvious at every
// read site.
use crate::services::opencode_oauth::device_flow;
use crate::services::opencode_oauth::OpenCodeConsoleCred;
use crate::services::opencode_oauth::OPENCODE_CONSOLE_CRED_TARGET;
use crate::services::opencode_oauth::OPENCODE_CONSOLE_HOST;
// Credential Manager surface — Windows-only. The `cfg(windows)` `read_*`
// helpers catch the gap with `NoCredential`, so callers stay
// one-statement-uniform.
#[cfg(windows)]
use crate::services::windows_cred;

/// Drop-in [`UsageAdapter`] for `opencode`.
pub(crate) struct OpencodeAdapter;

impl UsageAdapter for OpencodeAdapter {
    fn usage_page(&self) -> Option<UsagePage> {
        Some(UsagePage::new("https://opencode.ai/auth", "Open console"))
    }

    fn id(&self) -> &'static str {
        "opencode"
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("opencode")
    }

    // Issue #1745 phase 2 step 18: opencode migrated to the outcome
    // seam. The retry-on-401 gate matches on the `Rejected` outcome
    // (strictly stronger than the old wire-error substring match), and
    // the SQLite fallback builds `Reading` directly.
    //
    // Issue #2528 step 3b: the fetch moved here from `usage.rs`; the
    // ladder is the shared driver's.
    fn fetch(&self, _accounts: &[ProviderAccount]) -> UsageOutcome {
        opencode_usage()
    }
}

#[derive(Deserialize)]
struct OpenCodeAuthEntry {
    key: Option<String>,
}

fn read_opencode_token(path: PathBuf) -> Result<String, UsageError> {
    let content = fs::read_to_string(&path)
        .map_err(|_| UsageError::NoCredential(path.clone().to_string_lossy().to_string()))?;
    let entries: HashMap<String, OpenCodeAuthEntry> =
        serde_json::from_str(&content).map_err(|e| UsageError::Shape(e.to_string()))?;
    if let Some(entry) = entries.get("opencode-go") {
        if let Some(ref key) = entry.key {
            if !key.is_empty() {
                return Ok(key.clone());
            }
        }
    }
    Err(UsageError::NoCredential(path.to_string_lossy().to_string()))
}

fn calculate_opencode_windows_impl(
    conn: &rusqlite::Connection,
) -> Result<Vec<UsageWindow>, String> {
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

    let conn =
        rusqlite::Connection::open(db_path).map_err(|e| format!("Failed to open DB: {}", e))?;

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

/// Pure assembly: combines a live fetch outcome with the SQLite fallback
/// to produce the final [`UsageOutcome`]. The live path wins when it
/// returns `Reading`; any failure — `None` = no credential, or `Some`
/// carrying a non-`Reading` outcome — falls through to the SQLite result
/// so a user mid-OAuth always sees SOMETHING (issue #957 sub-spec
/// point 4). OpenCode's live path uses the plain driver, so the outcome
/// it hands here is a `Reading` or a failure variant.
fn choose_opencode_usage(live: Option<UsageOutcome>, sqlite: UsageOutcome) -> UsageOutcome {
    match live {
        Some(outcome @ UsageOutcome::Reading { .. }) => outcome,
        _ => sqlite,
    }
}

fn opencode_usage_impl(home: &std::path::Path) -> UsageOutcome {
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
) -> UsageOutcome {
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
        let age = cached_age("opencode");
        let now_unix = chrono::Utc::now().timestamp();
        if opencode_needs_refresh(c, age, now_unix) {
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
    // `billing.get` to SolidStart. A missing credential part collapses
    // to `None` via `opencode_live_request_parts`, and HTTP-level
    // failures (401, 5xx, shape mismatch) surface as non-`Reading`
    // outcomes — either way `choose_opencode_usage` falls through to
    // the SQLite path below. The `X-Server-Id` header is sourced from
    // the persisted credential's `server_id` field (issue #972);
    // pre-#956 blobs fall through to the legacy default and trigger a
    // process-wide warn-once.
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
                    let new_cred =
                        cred_from_token(&token, c.workspace_id.as_deref(), c.server_id.as_deref());
                    live = opencode_live_request_at(live_url, &new_cred);
                }
            }
        }
    }

    // ── Offline SQLite fallback (#953) ──────────────────────────────
    //
    // Same auth.json gate as before — a user mid-OAuth (live path
    // failed but auth.json present) still gets real numbers; a user
    // who hasn't run any auth reports `NoCredential` here (the gate
    // drops the row, as the old `logged_out` envelope did).
    let _token = match read_opencode_token(auth_path) {
        Ok(t) => t,
        Err(e) => {
            return UsageOutcome::NoCredential {
                hint: e.to_string(),
            }
        }
    };

    let sqlite = match calculate_opencode_windows(&db_path) {
        Ok(windows) => UsageOutcome::Reading {
            windows,
            balance: None,
            meters: vec![],
            detail: None,
        },
        Err(e) => UsageOutcome::Unavailable {
            reason: format!("Failed to query opencode.db: {e}"),
        },
    };
    // Pure assembly pins the degradation contract: live wins when it returns
    // `Reading`, anything else falls through to SQLite.
    choose_opencode_usage(live, sqlite)
}

/// Fires the live `_server billing.get` probe against a parameterized
/// `live_url`. Extracted from `opencode_usage_impl_with_hosts` so the
/// pre-emptive + reactive retry paths share the same wire-binding
/// closure (header set, JSON body, parser) without duplicating the
/// `opencode_live_request_parts` + `fetch_usage` composition.
fn opencode_live_request_at(live_url: &str, cred: &OpenCodeConsoleCred) -> Option<UsageOutcome> {
    let (token, workspace_id, server_id) = opencode_live_request_parts(cred)?;
    let live_url_owned = live_url.to_string();
    Some(fetch_usage(
        ResponsePolicy::default(),
        move |client| {
            client
                .post(&live_url_owned)
                .header("X-Server-Id", server_id)
                .header("Authorization", format!("Bearer {}", token))
                .json(&[workspace_id])
        },
        |body| {
            let (windows, detail) = parse_opencode_billing_response(body)?;
            Ok(UsageReading {
                windows,
                balance: None,
                meters: Vec::new(),
                detail,
            })
        },
    ))
}

/// True when the live probe's credential was rejected (the
/// "refresh-on-the-spot" trigger). Issue #1758: matches on the
/// `Rejected` outcome instead of substring-matching the wire error
/// string for `"401"` — the outcome is set by the shared driver's
/// 401/403 arm, so a body that merely mentions "401" can no longer
/// trigger a spurious refresh round-trip.
fn needs_retry_on_401(live: Option<&UsageOutcome>) -> bool {
    matches!(live, Some(UsageOutcome::Rejected { .. }))
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
/// the prior credential's fields; the refresh path in
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

/// Public OpenCode fetcher.
///
/// Issue #1745 phase 2 step 18: migrated to the outcome seam. The live
/// `_server billing.get` probe already classified through the shared
/// driver; the SQLite fallback now builds `Reading` directly, and the
/// reactive refresh-on-401 gate matches on the `Rejected` outcome
/// instead of substring-matching the wire error string.
pub fn opencode_usage() -> UsageOutcome {
    opencode_usage_impl(&home_dir())
}

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
const OPENCODE_SERVER_ID: &str = "c83b78a614689c38ebee981f9b39a8b377716db85c1fd7dbab604adc02d3313d";

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
/// `opencode_usage_impl` should refresh the bearer before its live `_server
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

#[cfg(test)]
#[path = "opencode_tests.rs"]
mod tests;
