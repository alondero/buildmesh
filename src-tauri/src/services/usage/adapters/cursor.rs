//! Cursor native adapter — self-authenticates via the Cursor CLI/IDE auth
//! sources, detection-gated on the `cursor` harness.
//!
//! Issue #1674 replaces the legacy-only probe (which read `GET /auth/usage`
//! and rendered `N/A` when an Enterprise account reported requests without an
//! individual maximum) with Cursor's current authenticated Dashboard flow:
//!
//!   1. `GetPlanInfo` — the provider-reported plan name. Routing only; the
//!      glanceable meter does not display it (#1689 removed plan labels).
//!   2. `GetCurrentPeriodUsage` — plan allowance, spend limits and the
//!      billing-cycle bounds. Pro/Team/Ultra report an included `planUsage`;
//!      Enterprise/Business accounts may omit it entirely.
//!   3. `get-aggregated-usage-events` — when there is no usable `planUsage`
//!      (and the plan is org-managed or unknown), aggregate the current cycle's
//!      spend events. Best-effort: any failure is non-fatal.
//!   4. `GET /auth/usage` — the legacy request-bucket response, kept ONLY as a
//!      fallback once the current flow fails or is absent.
//!
//! The panel shows one meter per account: the plan allowance when `planUsage`
//! is usable, otherwise a single spend meter. Individual and pooled limits are
//! never summed or cross-wired; a team pool that is not an individual cap is
//! reported in `detail`, so the user never sees two anonymous dollar blocks.
//!
//! No Cursor admin key is used: every call rides the same personal bearer /
//! WorkOS session credential the Cursor client already stores locally.
//!
//! The endpoints are undocumented and reverse-engineered; per the module
//! contract any shape mismatch degrades to "usage unavailable", never a hard
//! error. Authentication failures (401/403) stay distinguishable from
//! temporary network/transport failures; the required period call — not the
//! optional plan probe — is the auth arbiter.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{shared_client, UsageAdapter};
use crate::services::usage::outcome::UsageOutcome;
use crate::services::usage::types::{
    home_dir, logged_out, unavailable, ProviderUsage, UsageAmount, UsageError, UsageMeter,
    UsageWindow,
};
use base64::Engine as _;
use reqwest::blocking::{Client, RequestBuilder};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Cursor's Connect RPC host (plan + current-period usage, legacy usage).
const CURSOR_API_BASE: &str = "https://api2.cursor.sh";
/// Cursor's dashboard host (billing-cycle aggregate usage events).
const CURSOR_DASHBOARD_BASE: &str = "https://cursor.com";

const PLAN_INFO_PATH: &str = "/aiserver.v1.DashboardService/GetPlanInfo";
const PERIOD_USAGE_PATH: &str = "/aiserver.v1.DashboardService/GetCurrentPeriodUsage";
const AGGREGATED_EVENTS_PATH: &str = "/api/dashboard/get-aggregated-usage-events";
const LEGACY_USAGE_PATH: &str = "/auth/usage";

const SESSION_EXPIRED: &str = "Cursor session expired — run 'cursor-agent login' to log in";

/// Drop-in [`UsageAdapter`] for `cursor`.
pub(crate) struct CursorAdapter;

impl UsageAdapter for CursorAdapter {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("cursor")
    }

    fn fetch(&self, _accounts: &[ProviderAccount]) -> UsageOutcome {
        // TODO(#1745 phase 2): preserve Cursor's bespoke `CurrentFlow`
        // (unauthorized / fallback / usage) on migration; map each branch
        // to the right outcome variant. The shim preserves the wire triple
        // until then.
        cursor_usage().into()
    }
}

/// The two origins the adapter talks to. Split so the production hosts can be
/// pinned while loopback tests point both at one `tiny_http` origin.
#[derive(Clone)]
struct CursorEndpoints {
    api: String,
    dashboard: String,
}

impl CursorEndpoints {
    fn production() -> Self {
        Self {
            api: CURSOR_API_BASE.to_string(),
            dashboard: CURSOR_DASHBOARD_BASE.to_string(),
        }
    }
}

/// Public Cursor fetcher. Reads the credential from the environment, Cursor's
/// `state.vscdb`, or `~/.cursor/auth.json`, then walks the Dashboard flow.
pub(crate) fn cursor_usage() -> ProviderUsage {
    #[cfg(test)]
    if let Some(token) = TOKEN_OVERRIDE.with(|cell| cell.borrow().clone()) {
        let endpoints = CursorEndpoints {
            api: API_BASE_OVERRIDE
                .with(|cell| cell.borrow().clone())
                .unwrap_or_else(|| CURSOR_API_BASE.to_string()),
            dashboard: DASHBOARD_BASE_OVERRIDE
                .with(|cell| cell.borrow().clone())
                .unwrap_or_else(|| CURSOR_DASHBOARD_BASE.to_string()),
        };
        return cursor_usage_with_token(&token, &endpoints);
    }

    let (env_token, candidates) = discover_cursor_auth_sources();
    let token = match read_cursor_token_from_candidates(env_token, &candidates) {
        Ok(token) => token,
        Err(error) => return logged_out("cursor", error.to_string()),
    };
    cursor_usage_with_token(&token, &CursorEndpoints::production())
}

#[cfg(test)]
thread_local! {
    static TOKEN_OVERRIDE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
    static API_BASE_OVERRIDE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
    static DASHBOARD_BASE_OVERRIDE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Scope helper: point the whole adapter (`CursorAdapter::fetch`) at a loopback
/// origin with an injected bearer token for the duration of `f`. Thread-local
/// (not process-wide) so parallel test workers stay isolated.
#[cfg(test)]
fn with_cursor_loopback<F, R>(token: &str, api_base: &str, dashboard_base: &str, f: F) -> R
where
    F: FnOnce() -> R,
{
    let previous = Some((
        TOKEN_OVERRIDE.with(|cell| cell.borrow_mut().replace(token.to_string())),
        API_BASE_OVERRIDE.with(|cell| cell.borrow_mut().replace(api_base.to_string())),
        DASHBOARD_BASE_OVERRIDE.with(|cell| cell.borrow_mut().replace(dashboard_base.to_string())),
    ));
    struct Restore(Option<(Option<String>, Option<String>, Option<String>)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some((token, api, dashboard)) = self.0.take() {
                TOKEN_OVERRIDE.with(|cell| *cell.borrow_mut() = token);
                API_BASE_OVERRIDE.with(|cell| *cell.borrow_mut() = api);
                DASHBOARD_BASE_OVERRIDE.with(|cell| *cell.borrow_mut() = dashboard);
            }
        }
    }
    let _guard = Restore(previous);
    f()
}

/// Why the current Dashboard flow did not produce a meter.
enum CurrentFlow {
    /// A usable meter was built from the Dashboard response. Boxed so the
    /// error side of `Result<_, CurrentFlow>` stays small.
    Usage(Box<ProviderUsage>),
    /// The credential was rejected by the required period call — do not attempt
    /// the legacy fallback, which would use the same credential.
    Unauthorized,
    /// The current flow failed, was absent, or was not applicable — the legacy
    /// endpoint is the documented next step.
    Fallback,
}

fn cursor_usage_with_token(token: &str, endpoints: &CursorEndpoints) -> ProviderUsage {
    let client = match shared_client() {
        Ok(client) => client,
        Err(error) => return unavailable("cursor", error),
    };

    match fetch_current_flow(&client, token, endpoints) {
        CurrentFlow::Usage(usage) => *usage,
        CurrentFlow::Unauthorized => logged_out("cursor", SESSION_EXPIRED.to_string()),
        CurrentFlow::Fallback => fetch_legacy_usage(&client, token, endpoints),
    }
}

fn fetch_current_flow(client: &Client, token: &str, endpoints: &CursorEndpoints) -> CurrentFlow {
    // Step 1 — the account plan. Best-effort: a failed plan lookup (transport,
    // 5xx, shape, or even 401/403) leaves the plan unknown and must not fail
    // the flow or skip the aggregate branch. The required period call below is
    // the auth arbiter.
    let plan = fetch_plan_name(client, token, endpoints);

    // Step 2 — current-period usage. Required.
    let period = match fetch_period_usage(client, token, endpoints) {
        Ok(period) => period,
        Err(flow) => return flow,
    };

    let cycle = CursorCycle::from_period(&period);
    let has_plan_usage = period
        .plan_usage
        .as_ref()
        .is_some_and(CursorPlanUsage::is_usable);
    // Org-managed plans (Enterprise/Business) report spend through the
    // billing-cycle aggregate when they omit `planUsage`. An *unknown* plan is
    // treated as org-managed so a flaky plan probe cannot resurrect the N/A
    // bug; a known non-org plan keeps the legacy path.
    let org_managed = plan.as_deref().is_none_or(is_org_managed_plan);
    let aggregate_spend_cents = if !has_plan_usage && org_managed {
        fetch_aggregate_spend_cents(client, token, endpoints, &cycle)
    } else {
        None
    };

    match map_current_usage(plan.as_deref(), &period, aggregate_spend_cents, &cycle) {
        Some(mapped) => CurrentFlow::Usage(Box::new(mapped.into_provider_usage())),
        None => CurrentFlow::Fallback,
    }
}

/// Case/whitespace-tolerant "this plan is org-managed" test. Exact string
/// equality missed real labels ("Enterprise Team", "enterprise-annual", a
/// trailing newline) and sent the exact accounts this adapter exists for back
/// to the legacy N/A path.
fn is_org_managed_plan(name: &str) -> bool {
    let normalized = name.trim().to_ascii_lowercase();
    normalized.contains("enterprise") || normalized.contains("business")
}

/// `GetPlanInfo` → `planInfo.planName`. Infallible: a rejected or malformed plan
/// route yields `None` and leaves auth arbitration to the required period call.
fn fetch_plan_name(client: &Client, token: &str, endpoints: &CursorEndpoints) -> Option<String> {
    let url = format!("{}{}", endpoints.api, PLAN_INFO_PATH);
    let response = read_connect_json::<CursorPlanInfoResp>(client, &url, token).ok()?;
    response
        .plan_info
        .and_then(|info| info.plan_name)
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

/// `GetCurrentPeriodUsage` → the parsed dashboard period body.
fn fetch_period_usage(
    client: &Client,
    token: &str,
    endpoints: &CursorEndpoints,
) -> Result<CursorPeriodUsageResp, CurrentFlow> {
    let url = format!("{}{}", endpoints.api, PERIOD_USAGE_PATH);
    match read_connect_json::<CursorPeriodUsageResp>(client, &url, token) {
        Ok(response) => Ok(response),
        Err(ConnectError::Unauthorized) => Err(CurrentFlow::Unauthorized),
        Err(ConnectError::Unavailable) => Err(CurrentFlow::Fallback),
    }
}

/// `get-aggregated-usage-events` → `totalCostCents` for `[cycleStart, now]`.
/// Best-effort: returns `None` on any failure so the caller keeps whatever the
/// current-period response already reported.
fn fetch_aggregate_spend_cents(
    client: &Client,
    token: &str,
    endpoints: &CursorEndpoints,
    cycle: &CursorCycle,
) -> Option<f64> {
    let (start_ms, end_ms) = cycle.aggregate_window()?;
    let url = format!("{}{}", endpoints.dashboard, AGGREGATED_EVENTS_PATH);
    let mut request = client
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header("Origin", endpoints.dashboard.clone())
        .header(
            "Referer",
            format!("{}/dashboard?tab=usage", endpoints.dashboard),
        );
    // The dashboard REST surface authenticates the browser session cookie;
    // send it alongside the bearer when the access token carries a usable JWT.
    if let Some(cookie) = workos_session_cookie(token) {
        request = request.header("Cookie", cookie);
    }
    let response = request
        .json(&serde_json::json!({
            "teamId": -1,
            "startDate": start_ms,
            "endDate": end_ms,
        }))
        .send()
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body = response.text().ok()?;
    let parsed: CursorAggregatedUsageResp = serde_json::from_str(&body).ok()?;
    parsed.total_cost_cents.and_then(|value| value.to_f64())
}

/// The legacy `GET /auth/usage` endpoint, retained as the fallback after the
/// current flow fails or is absent.
fn fetch_legacy_usage(client: &Client, token: &str, endpoints: &CursorEndpoints) -> ProviderUsage {
    let url = format!("{}{}", endpoints.api, LEGACY_USAGE_PATH);
    let response = match client
        .get(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", "Mozilla/5.0")
        .send()
    {
        Ok(response) => response,
        Err(error) => return unavailable("cursor", format!("Request failed: {error}")),
    };

    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        return logged_out("cursor", SESSION_EXPIRED.to_string());
    }
    if status == 429 {
        return unavailable(
            "cursor",
            "Rate limited — usage data temporarily unavailable".to_string(),
        );
    }
    if !(200..300).contains(&status) {
        return unavailable(
            "cursor",
            format!("API error {status}: usage endpoint failed"),
        );
    }

    let body = match response.text() {
        Ok(body) => body,
        Err(error) => return unavailable("cursor", format!("Failed to read response: {error}")),
    };
    match parse_legacy_usage_response(&body) {
        Ok((windows, detail)) => ProviderUsage {
            provider: "cursor".to_string(),
            logged_in: true,
            windows,
            balance: None,
            meters: vec![],
            detail,
            error: None,
        },
        Err(error) => unavailable("cursor", format!("Failed to parse response: {error}")),
    }
}

enum ConnectError {
    Unauthorized,
    Unavailable,
}

/// POSTs an empty Connect unary message and parses the JSON reply. 401/403 map
/// to [`ConnectError::Unauthorized`]; every other failure (transport, status,
/// shape) maps to [`ConnectError::Unavailable`].
fn read_connect_json<T: DeserializeOwned>(
    client: &Client,
    url: &str,
    token: &str,
) -> Result<T, ConnectError> {
    let request = connect_post(client, url, token);
    let response = request.send().map_err(|_| ConnectError::Unavailable)?;
    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        return Err(ConnectError::Unauthorized);
    }
    if !(200..300).contains(&status) {
        return Err(ConnectError::Unavailable);
    }
    let body = response.text().map_err(|_| ConnectError::Unavailable)?;
    serde_json::from_str(&body).map_err(|_| ConnectError::Unavailable)
}

fn connect_post(client: &Client, url: &str, token: &str) -> RequestBuilder {
    client
        .post(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("Connect-Protocol-Version", "1")
        .header("Accept", "application/json")
        .body("{}")
}

/// Derives Cursor's `WorkosCursorSessionToken` cookie value from the access
/// token JWT: `<userId>::<token>`, percent-encoded. Returns `None` when the
/// token is not a decodable JWT with a usable `sub` claim (the aggregate-events
/// call still proceeds with the bearer alone).
fn workos_session_cookie(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let subject = claims.get("sub")?.as_str()?;
    let user_id = subject.rsplit('|').next().unwrap_or(subject).trim();
    if user_id.is_empty() {
        return None;
    }
    let value = percent_encode_cookie(&format!("{user_id}::{token}"));
    Some(format!("WorkosCursorSessionToken={value}"))
}

/// Percent-encodes a cookie component. Every byte outside the RFC 3986
/// unreserved set is escaped, so `:` becomes `%3A` (making `::` → `%3A%3A`) and
/// a token containing `;`, `,`, or whitespace cannot split or corrupt the
/// `Cookie` header. Real WorkOS JWTs are base64url plus `.`, all unreserved, so
/// this is a no-op for the documented shape.
fn percent_encode_cookie(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

// ─── Wire types ─────────────────────────────────────────────────────────────

/// JSON number that the Dashboard service sometimes emits as a number and
/// sometimes as a decimal string.
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
enum CursorNumber {
    F(f64),
    I(i64),
    U(u64),
    S(String),
}

impl CursorNumber {
    fn to_f64(&self) -> Option<f64> {
        match self {
            Self::F(value) => value.is_finite().then_some(*value),
            Self::I(value) => Some(*value as f64),
            Self::U(value) => Some(*value as f64),
            Self::S(value) => value
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite()),
        }
    }
}

fn number(value: &Option<CursorNumber>) -> Option<f64> {
    value.as_ref().and_then(CursorNumber::to_f64)
}

fn positive(value: &Option<CursorNumber>) -> Option<f64> {
    number(value).filter(|value| *value > 0.0)
}

/// Clamps a money/percent value to a non-negative finite figure so a malformed
/// upstream (`totalSpend: -100`, `remaining: -500`) cannot render negative
/// dollars in the panel.
fn non_negative(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct CursorPlanInfoResp {
    #[serde(default)]
    plan_info: Option<CursorPlanInfo>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct CursorPlanInfo {
    #[serde(default)]
    plan_name: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct CursorPeriodUsageResp {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    plan_usage: Option<CursorPlanUsage>,
    #[serde(default)]
    spend_limit_usage: Option<CursorSpendLimitUsage>,
    #[serde(default)]
    billing_cycle_start: Option<CursorNumber>,
    #[serde(default)]
    billing_cycle_end: Option<CursorNumber>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct CursorPlanUsage {
    #[serde(default)]
    limit: Option<CursorNumber>,
    #[serde(default)]
    total_spend: Option<CursorNumber>,
    #[serde(default)]
    included_spend: Option<CursorNumber>,
    #[serde(default)]
    bonus_spend: Option<CursorNumber>,
    #[serde(default)]
    remaining: Option<CursorNumber>,
    #[serde(default)]
    total_percent_used: Option<CursorNumber>,
    /// Wire-only for now: Cursor reports per-model-family percentages, but the
    /// glanceable panel keeps one included-allowance meter per account.
    #[serde(default)]
    #[allow(dead_code)]
    auto_percent_used: Option<CursorNumber>,
    #[serde(default)]
    #[allow(dead_code)]
    api_percent_used: Option<CursorNumber>,
}

/// Cursor reports individual and pooled (team) spend limits separately. They
/// are never summed or cross-wired: used/remaining are taken from the same
/// level as the chosen limit, and a missing individual cap is
/// `NoIndividualLimit`, not the team pool's cap.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct CursorSpendLimitUsage {
    /// Wire-only for now: the individual/pooled amounts below already carry the
    /// cap distinction the meter needs.
    #[serde(default)]
    #[allow(dead_code)]
    limit_type: Option<String>,
    #[serde(default)]
    individual_limit: Option<CursorNumber>,
    #[serde(default)]
    individual_used: Option<CursorNumber>,
    #[serde(default)]
    individual_remaining: Option<CursorNumber>,
    #[serde(default)]
    pooled_limit: Option<CursorNumber>,
    #[serde(default)]
    pooled_used: Option<CursorNumber>,
    #[serde(default)]
    pooled_remaining: Option<CursorNumber>,
    #[serde(default)]
    total_spend: Option<CursorNumber>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct CursorAggregatedUsageResp {
    #[serde(default)]
    total_cost_cents: Option<CursorNumber>,
}

/// Billing-cycle bounds from the current-period response (epoch milliseconds).
struct CursorCycle {
    start_ms: Option<i64>,
    end_ms: Option<i64>,
}

impl CursorCycle {
    fn from_period(period: &CursorPeriodUsageResp) -> Self {
        Self {
            start_ms: epoch_ms(&period.billing_cycle_start),
            end_ms: epoch_ms(&period.billing_cycle_end),
        }
    }

    fn resets_at(&self) -> Option<String> {
        self.end_ms
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|datetime| datetime.to_rfc3339())
    }

    /// The aggregate-events window: cycle start through now. The cycle end may
    /// be in the future, so "current-cycle events" must stop at the present. A
    /// missing/zero cycle start disables the aggregate call (the window is
    /// undefined) and the flow falls through to the legacy probe.
    fn aggregate_window(&self) -> Option<(i64, i64)> {
        let start = self.start_ms?;
        let now = chrono::Utc::now().timestamp_millis();
        Some((start, now.max(start)))
    }
}

fn epoch_ms(value: &Option<CursorNumber>) -> Option<i64> {
    number(value)
        .filter(|value| *value > 0.0)
        .map(|value| value as i64)
}

// ─── Mapping ────────────────────────────────────────────────────────────────

struct CursorMapped {
    meters: Vec<UsageMeter>,
    detail: Option<String>,
}

impl CursorMapped {
    fn into_provider_usage(self) -> ProviderUsage {
        ProviderUsage {
            provider: "cursor".to_string(),
            logged_in: true,
            windows: vec![],
            balance: None,
            meters: self.meters,
            detail: self.detail,
            error: None,
        }
    }
}

fn cents_to_dollars(cents: f64) -> f64 {
    cents / 100.0
}

fn money(dollars: f64) -> String {
    format!("USD {dollars:.2}")
}

fn join_detail(parts: Vec<String>) -> Option<String> {
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Maps the Dashboard responses into the shared Usage Meter contract. Returns
/// `None` when the current flow yielded nothing usable, which routes the caller
/// to the legacy fallback.
///
/// One meter per account: the included plan allowance when `planUsage` is
/// usable, otherwise a single spend meter. Spend limits that are not the meter
/// (the individual cap on a standard plan, the team pool everywhere) are
/// reported in `detail`.
fn map_current_usage(
    plan_name: Option<&str>,
    period: &CursorPeriodUsageResp,
    aggregate_spend_cents: Option<f64>,
    cycle: &CursorCycle,
) -> Option<CursorMapped> {
    if period.enabled == Some(false) {
        return None;
    }
    let resets_at = cycle.resets_at();
    let mut detail_parts: Vec<String> = Vec::new();

    if let Some(plan_usage) = period.plan_usage.as_ref().filter(|usage| usage.is_usable()) {
        let meter = plan_usage.to_meter(&resets_at)?;
        if let Some(line) = plan_usage.breakdown_detail() {
            detail_parts.push(line);
        }
        if let Some(line) = individual_cap_detail(period.spend_limit_usage.as_ref()) {
            detail_parts.push(line);
        }
        if let Some(line) = team_pool_detail(period.spend_limit_usage.as_ref()) {
            detail_parts.push(line);
        }
        return Some(CursorMapped {
            meters: vec![meter],
            detail: join_detail(detail_parts),
        });
    }

    // No usable plan-usage object. Org-managed plans (and unknown plans) use
    // the current-cycle aggregate spend; a known non-org plan uses legacy.
    if !plan_name.is_none_or(is_org_managed_plan) {
        return None;
    }
    let meter = spend_meter(
        period.spend_limit_usage.as_ref(),
        aggregate_spend_cents,
        &resets_at,
    )?;
    if let Some(line) = team_pool_detail(period.spend_limit_usage.as_ref()) {
        detail_parts.push(line);
    }
    Some(CursorMapped {
        meters: vec![meter],
        detail: join_detail(detail_parts),
    })
}

impl CursorPlanUsage {
    /// A usable plan-usage object exposes either a positive included limit or a
    /// total percentage. `planUsage: {}` (or one with only zeroes) is the
    /// "present but unusable" shape Enterprise accounts can return.
    fn is_usable(&self) -> bool {
        positive(&self.limit).is_some() || number(&self.total_percent_used).is_some()
    }

    fn to_meter(&self, resets_at: &Option<String>) -> Option<UsageMeter> {
        if let Some(limit_cents) = positive(&self.limit) {
            let used_cents = non_negative(
                number(&self.total_spend)
                    .or_else(|| number(&self.included_spend))
                    .or_else(|| number(&self.remaining).map(|remaining| limit_cents - remaining))
                    .unwrap_or(0.0),
            );
            let remaining_cents =
                non_negative(number(&self.remaining).unwrap_or(limit_cents - used_cents));
            let used_percent = number(&self.total_percent_used)
                .or_else(|| Some(used_cents / limit_cents * 100.0))
                .map(|percent| percent.clamp(0.0, 100.0));
            return Some(UsageMeter::Metered {
                amount: UsageAmount {
                    used: cents_to_dollars(used_cents),
                    limit: Some(cents_to_dollars(limit_cents)),
                    remaining: Some(cents_to_dollars(remaining_cents)),
                    unit: "USD".to_string(),
                    used_percent,
                    resets_at: resets_at.clone(),
                },
            });
        }
        let percent = non_negative(number(&self.total_percent_used)?).min(100.0);
        Some(UsageMeter::Metered {
            amount: UsageAmount {
                used: percent,
                limit: Some(100.0),
                remaining: Some(100.0 - percent),
                unit: "%".to_string(),
                used_percent: Some(percent),
                resets_at: resets_at.clone(),
            },
        })
    }

    /// Included / bonus spend breakdown. Kept in `detail` rather than its own
    /// meter so the glanceable panel shows one allowance bar per account.
    fn breakdown_detail(&self) -> Option<String> {
        let included = number(&self.included_spend)
            .map(non_negative)
            .map(cents_to_dollars);
        let bonus = number(&self.bonus_spend)
            .map(non_negative)
            .map(cents_to_dollars);
        if included.is_none() && bonus.is_none() {
            return None;
        }
        let mut parts = Vec::new();
        if let Some(included) = included {
            parts.push(format!("Included spend {}", money(included)));
        }
        if let Some(bonus) = bonus {
            parts.push(format!("Bonus spend {}", money(bonus)));
        }
        Some(parts.join(" · "))
    }
}

/// Builds the single spend meter for an account with no usable `planUsage`.
/// Only an individual cap counts as a cap: a team pool alone is
/// `NoIndividualLimit`, and the pool is reported separately in `detail`. Used
/// and remaining always come from the same level as the chosen limit, so a
/// pooled figure can never be reported against an individual cap.
fn spend_meter(
    spend_limit_usage: Option<&CursorSpendLimitUsage>,
    aggregate_spend_cents: Option<f64>,
    resets_at: &Option<String>,
) -> Option<UsageMeter> {
    let individual_limit = spend_limit_usage.and_then(|usage| positive(&usage.individual_limit));
    let used_cents = spend_limit_usage
        .and_then(|usage| number(&usage.individual_used))
        .or(aggregate_spend_cents)
        .or_else(|| spend_limit_usage.and_then(|usage| number(&usage.total_spend)));
    // A zero amount is a valid reading (issue #1674: "Zero Enterprise spend is
    // treated as a valid result"); only a genuinely absent spend amount with no
    // cap is unusable.
    if individual_limit.is_none() && used_cents.is_none() {
        return None;
    }
    let used_cents = non_negative(used_cents.unwrap_or(0.0));
    let remaining_cents = spend_limit_usage
        .and_then(|usage| number(&usage.individual_remaining))
        .map(non_negative)
        .or_else(|| individual_limit.map(|limit| (limit - used_cents).max(0.0)));
    let used_percent = individual_limit.map(|limit| (used_cents / limit * 100.0).clamp(0.0, 100.0));
    let amount = UsageAmount {
        used: cents_to_dollars(used_cents),
        limit: individual_limit.map(cents_to_dollars),
        remaining: remaining_cents.map(cents_to_dollars),
        unit: "USD".to_string(),
        used_percent,
        resets_at: resets_at.clone(),
    };
    Some(if individual_limit.is_some() {
        UsageMeter::Metered { amount }
    } else {
        UsageMeter::NoIndividualLimit { amount }
    })
}

/// Individual-cap line for `detail` on a standard plan, where the meter is the
/// included allowance rather than the cap. Level-correct: the individual used
/// amount (falling back to overall spend), never the team pool's.
fn individual_cap_detail(spend_limit_usage: Option<&CursorSpendLimitUsage>) -> Option<String> {
    let usage = spend_limit_usage?;
    let limit = positive(&usage.individual_limit)?;
    let used = non_negative(
        number(&usage.individual_used)
            .or_else(|| number(&usage.total_spend))
            .unwrap_or(0.0),
    );
    Some(format!(
        "Individual cap: {} of {}",
        money(cents_to_dollars(used)),
        money(cents_to_dollars(limit))
    ))
}

/// Team-pool line for `detail`: a pooled limit is real information, but it is
/// not the individual cap, so it never becomes the meter's limit.
fn team_pool_detail(spend_limit_usage: Option<&CursorSpendLimitUsage>) -> Option<String> {
    let usage = spend_limit_usage?;
    let limit = positive(&usage.pooled_limit)?;
    let used = number(&usage.pooled_used).map(non_negative).or_else(|| {
        number(&usage.pooled_remaining).map(|remaining| non_negative(limit - remaining))
    });
    Some(match used {
        Some(used) => format!(
            "Team pool: {} of {}",
            money(cents_to_dollars(used)),
            money(cents_to_dollars(limit))
        ),
        None => format!("Team pool limit {}", money(cents_to_dollars(limit))),
    })
}

// ─── Legacy `/auth/usage` (fallback) ────────────────────────────────────────

#[derive(Deserialize, Debug, Clone)]
struct CursorModelUsage {
    #[serde(rename = "numRequests", default)]
    num_requests: Option<f64>,
    #[serde(rename = "numSlowRequests", default)]
    num_slow_requests: Option<f64>,
    #[serde(rename = "maxRequestUsage", default)]
    max_request_usage: Option<f64>,
    /// Wire-only: the legacy response can carry a token cap, but the probe has
    /// always surfaced the request allowance.
    #[serde(rename = "maxTokenUsage", default)]
    #[allow(dead_code)]
    max_token_usage: Option<f64>,
}

#[derive(Deserialize, Debug, Clone)]
struct CursorUsageResponse {
    #[serde(rename = "gpt-4", default)]
    gpt_4: Option<CursorModelUsage>,
    #[serde(rename = "startOfMonth", default)]
    start_of_month: Option<String>,
}

/// Compute the next calendar month reset timestamp in RFC3339 format given
/// an ISO 8601 / RFC3339 start-of-month string (e.g. `"2026-08-01T00:00:00.000Z"`).
fn compute_next_month_reset(start_of_month: &str) -> Option<String> {
    use chrono::Datelike;
    if let Ok(datetime) = chrono::DateTime::parse_from_rfc3339(start_of_month) {
        let utc = datetime.with_timezone(&chrono::Utc);
        let (year, month) = if utc.month() == 12 {
            (utc.year() + 1, 1)
        } else {
            (utc.year(), utc.month() + 1)
        };
        chrono::NaiveDate::from_ymd_opt(year, month, 1)
            .and_then(|date| date.and_hms_opt(0, 0, 0))
            .map(|naive| naive.and_utc().to_rfc3339())
    } else {
        None
    }
}

/// Parse the legacy Cursor quota & usage payload (`GET /auth/usage`).
fn parse_legacy_usage_response(
    body: &str,
) -> Result<(Vec<UsageWindow>, Option<String>), UsageError> {
    let response: CursorUsageResponse =
        serde_json::from_str(body).map_err(|error| UsageError::Shape(error.to_string()))?;

    let resets_at = response
        .start_of_month
        .as_deref()
        .and_then(compute_next_month_reset);

    let mut windows = Vec::new();
    let mut detail = None;

    if let Some(gpt4) = response.gpt_4 {
        let used = gpt4.num_requests.unwrap_or(0.0);
        let slow = gpt4.num_slow_requests.unwrap_or(0.0);
        let max = gpt4.max_request_usage;

        let used_percent = max.filter(|max| *max > 0.0).map(|max| (used / max) * 100.0);

        windows.push(UsageWindow {
            label: "Fast Requests".to_string(),
            used_percent,
            resets_at: resets_at.clone(),
        });

        if let Some(max) = max {
            if max > 0.0 {
                let remaining = (max - used).max(0.0);
                detail = Some(if slow > 0.0 {
                    format!(
                        "{} of {} fast requests remaining ({} slow requests used)",
                        remaining as i64, max as i64, slow as i64
                    )
                } else {
                    format!(
                        "{} of {} fast requests remaining",
                        remaining as i64, max as i64
                    )
                });
            }
        } else if used > 0.0 {
            detail = Some(format!("{} requests used this billing period", used as i64));
        }
    }

    if windows.is_empty() {
        detail = Some("No active Cursor usage windows".to_string());
    }

    Ok((windows, detail))
}

// ─── Credential discovery ───────────────────────────────────────────────────

/// Read access token from Cursor's global SQLite database (`state.vscdb`),
/// table `ItemTable`, key `cursorAuth/accessToken`. The token is trimmed so a
/// padded value never becomes `Bearer "  tok  "` (a spurious 401 → logged-out).
fn read_cursor_sqlite_token(path: &Path) -> Result<String, UsageError> {
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| UsageError::NoCredential(format!("{}: {}", path.display(), error)))?;

    let mut statement = conn
        .prepare("SELECT value FROM ItemTable WHERE key = 'cursorAuth/accessToken'")
        .map_err(|error| {
            UsageError::Shape(format!("Failed to prepare ItemTable query: {}", error))
        })?;

    let raw: String = statement.query_row([], |row| row.get(0)).map_err(|error| {
        UsageError::NoCredential(format!(
            "Key cursorAuth/accessToken not found in {}: {}",
            path.display(),
            error
        ))
    })?;

    let token = serde_json::from_str::<String>(&raw)
        .unwrap_or(raw)
        .trim()
        .to_string();
    if token.is_empty() {
        return Err(UsageError::NoCredential(format!(
            "{}: empty token",
            path.display()
        )));
    }
    Ok(token)
}

/// Read access token from the secondary JSON auth file (`auth.json`). Trimmed
/// for the same reason as [`read_cursor_sqlite_token`].
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

    let parsed: CursorAuthFile =
        serde_json::from_str(&content).map_err(|error| UsageError::Shape(error.to_string()))?;

    parsed
        .access_token_camel
        .or(parsed.access_token_snake)
        .or(parsed.token)
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
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
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty());

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
        if let Some(username) = env::var("USERNAME").ok().filter(|value| !value.is_empty()) {
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
    if let Some(token) = env_token.filter(|token| !token.trim().is_empty()) {
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
                Err(UsageError::Shape(error)) => return Err(UsageError::Shape(error)),
                Err(_) => continue,
            }
        } else {
            match read_cursor_auth_json(path) {
                Ok(token) => return Ok(token),
                Err(UsageError::Shape(error)) => return Err(UsageError::Shape(error)),
                Err(_) => continue,
            }
        }
    }

    let first = candidates
        .first()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|| "Cursor credential store".to_string());
    Err(UsageError::NoCredential(first))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const PLAN_ENTERPRISE: &str = include_str!("fixtures/cursor-plan-info-enterprise.json");
    const PERIOD_PRO: &str = include_str!("fixtures/cursor-period-usage-pro.json");
    const PERIOD_PRO_STRINGS: &str = include_str!("fixtures/cursor-period-usage-pro-strings.json");
    const PERIOD_ENTERPRISE_CAPPED: &str =
        include_str!("fixtures/cursor-period-usage-enterprise-capped.json");
    const PERIOD_ENTERPRISE_UNCAPPED: &str =
        include_str!("fixtures/cursor-period-usage-enterprise-uncapped.json");
    const EVENTS_ZERO: &str = include_str!("fixtures/cursor-aggregated-events-zero.json");
    const EVENTS_SPEND: &str = include_str!("fixtures/cursor-aggregated-events-spend.json");
    const LEGACY_USAGE: &str = include_str!("fixtures/cursor-legacy-auth-usage.json");

    /// Enterprise period with no `planUsage` and no `spendLimitUsage` at all —
    /// the aggregate-events call is the only spend source (finding #3).
    const PERIOD_ENTERPRISE_NO_LIMITS: &str = r#"{
        "enabled": true,
        "billingCycleStart": 1751328000000,
        "billingCycleEnd": 1754006400000
    }"#;

    fn period(body: &str) -> CursorPeriodUsageResp {
        serde_json::from_str(body).expect("fixture parses")
    }

    fn cycle(body: &str) -> CursorCycle {
        CursorCycle::from_period(&period(body))
    }

    // ── Pure mapping ──────────────────────────────────────────────────────

    #[test]
    fn standard_plan_emits_one_meter_and_carries_caps_in_detail() {
        let mapped = map_current_usage(Some("pro"), &period(PERIOD_PRO), None, &cycle(PERIOD_PRO))
            .expect("standard plan maps");

        assert_eq!(mapped.meters.len(), 1, "one allowance meter per account");
        match &mapped.meters[0] {
            UsageMeter::Metered { amount } => {
                assert_eq!(amount.used, 232.22);
                assert_eq!(amount.limit, Some(400.0));
                assert_eq!(amount.remaining, Some(167.78));
                assert_eq!(amount.unit, "USD");
                assert_eq!(amount.used_percent, Some(58.055));
                assert_eq!(
                    amount.resets_at.as_deref(),
                    Some("2025-08-01T00:00:00+00:00")
                );
            }
            other => panic!("expected metered plan allowance, got {other:?}"),
        }
        let detail = mapped.detail.expect("detail");
        assert!(
            detail.contains("Included spend USD 232.22"),
            "got {detail:?}"
        );
        assert!(detail.contains("Bonus spend USD 0.00"), "got {detail:?}");
        assert!(
            detail.contains("Individual cap: USD 12.50 of USD 50.00"),
            "got {detail:?}"
        );
    }

    #[test]
    fn standard_plan_never_reports_pooled_spend_against_an_individual_cap() {
        // individual cap present, individual used absent, pooled used present:
        // the individual cap line must show 0.00, not the team pool figure.
        let body = r#"{
            "planUsage": {"limit": 40000, "remaining": 16778},
            "spendLimitUsage": {
                "individualLimit": 5000,
                "pooledLimit": 500000,
                "pooledUsed": 123456
            }
        }"#;
        let mapped = map_current_usage(Some("pro"), &period(body), None, &cycle(body)).unwrap();
        let detail = mapped.detail.expect("detail");
        assert!(
            detail.contains("Individual cap: USD 0.00 of USD 50.00"),
            "individual line conflated the pool: {detail:?}"
        );
        assert!(
            !detail.contains("Individual cap: USD 1234.56"),
            "pooled spend leaked onto the individual cap: {detail:?}"
        );
        assert!(
            detail.contains("Team pool: USD 1234.56 of USD 5000.00"),
            "team pool line missing: {detail:?}"
        );
    }

    #[test]
    fn enterprise_without_plan_usage_uses_individual_cap() {
        let mapped = map_current_usage(
            Some("enterprise"),
            &period(PERIOD_ENTERPRISE_CAPPED),
            None,
            &cycle(PERIOD_ENTERPRISE_CAPPED),
        )
        .expect("enterprise capped maps");

        assert_eq!(mapped.meters.len(), 1);
        match &mapped.meters[0] {
            UsageMeter::Metered { amount } => {
                assert_eq!(amount.used, 234.56);
                assert_eq!(amount.limit, Some(1000.0));
                assert_eq!(amount.remaining, Some(765.44));
            }
            other => panic!("expected metered individual cap, got {other:?}"),
        }
        let detail = mapped.detail.expect("team pool detail");
        assert!(
            detail.contains("Team pool: USD 1234.56 of USD 5000.00"),
            "got {detail:?}"
        );
    }

    #[test]
    fn enterprise_without_individual_cap_uses_aggregate_spend() {
        let mapped = map_current_usage(
            Some("enterprise"),
            &period(PERIOD_ENTERPRISE_UNCAPPED),
            Some(98765.0),
            &cycle(PERIOD_ENTERPRISE_UNCAPPED),
        )
        .expect("enterprise uncapped maps");

        assert_eq!(mapped.meters.len(), 1);
        match &mapped.meters[0] {
            UsageMeter::NoIndividualLimit { amount } => {
                assert_eq!(amount.used, 987.65);
                assert_eq!(amount.limit, None);
                assert_eq!(amount.remaining, None);
                assert_eq!(amount.used_percent, None);
            }
            other => panic!("expected no-individual-limit spend, got {other:?}"),
        }
        let detail = mapped.detail.expect("team pool detail");
        assert!(
            detail.contains("Team pool limit USD 5000.00"),
            "got {detail:?}"
        );
    }

    #[test]
    fn enterprise_without_spend_limit_still_uses_aggregate_spend() {
        // No planUsage AND no spendLimitUsage: the aggregate figure alone must
        // produce the meter (finding #3).
        let mapped = map_current_usage(
            Some("enterprise"),
            &period(PERIOD_ENTERPRISE_NO_LIMITS),
            Some(98765.0),
            &cycle(PERIOD_ENTERPRISE_NO_LIMITS),
        )
        .expect("aggregate-only enterprise maps");

        match &mapped.meters[0] {
            UsageMeter::NoIndividualLimit { amount } => {
                assert_eq!(amount.used, 987.65);
                assert_eq!(amount.limit, None);
            }
            other => panic!("expected no-individual-limit spend, got {other:?}"),
        }
    }

    #[test]
    fn no_plan_usage_individual_cap_ignores_pooled_used() {
        // No individual used amount and no aggregate, but a pooled used amount:
        // the individual cap meter must not adopt the pooled figure.
        let body = r#"{
            "enabled": true,
            "spendLimitUsage": {
                "individualLimit": 5000,
                "pooledLimit": 500000,
                "pooledUsed": 123456
            },
            "billingCycleStart": 1751328000000,
            "billingCycleEnd": 1754006400000
        }"#;
        let mapped =
            map_current_usage(Some("enterprise"), &period(body), None, &cycle(body)).unwrap();
        match &mapped.meters[0] {
            UsageMeter::Metered { amount } => {
                assert_eq!(amount.used, 0.0, "pooled spend leaked onto the cap");
                assert_eq!(amount.limit, Some(50.0));
            }
            other => panic!("expected metered individual cap, got {other:?}"),
        }
    }

    #[test]
    fn zero_enterprise_spend_is_a_valid_result() {
        let mapped = map_current_usage(
            Some("enterprise"),
            &period(PERIOD_ENTERPRISE_NO_LIMITS),
            Some(0.0),
            &cycle(PERIOD_ENTERPRISE_NO_LIMITS),
        )
        .expect("zero spend still maps");

        match &mapped.meters[0] {
            UsageMeter::NoIndividualLimit { amount } => {
                assert_eq!(amount.used, 0.0);
                assert_eq!(amount.limit, None);
            }
            other => panic!("expected no-individual-limit zero spend, got {other:?}"),
        }
    }

    #[test]
    fn enterprise_without_plan_usage_or_spend_is_absent() {
        // No individual cap, no reported spend, no aggregate events → the
        // caller must fall through to the legacy endpoint.
        assert!(map_current_usage(
            Some("enterprise"),
            &period(PERIOD_ENTERPRISE_NO_LIMITS),
            None,
            &cycle(PERIOD_ENTERPRISE_NO_LIMITS),
        )
        .is_none());
    }

    #[test]
    fn unknown_plan_without_plan_usage_is_org_managed() {
        // A failed plan probe must not resurrect the N/A bug: unknown plans use
        // the aggregate branch.
        let mapped = map_current_usage(
            None,
            &period(PERIOD_ENTERPRISE_NO_LIMITS),
            Some(5000.0),
            &cycle(PERIOD_ENTERPRISE_NO_LIMITS),
        )
        .expect("unknown plan maps");
        assert!(matches!(
            mapped.meters.as_slice(),
            [UsageMeter::NoIndividualLimit { .. }]
        ));
    }

    #[test]
    fn plan_label_variants_are_org_managed() {
        for label in [
            "enterprise",
            "Enterprise Team",
            "enterprise-annual",
            "  Enterprise  ",
            "Business",
        ] {
            assert!(is_org_managed_plan(label), "{label} should be org-managed");
        }
        assert!(!is_org_managed_plan("pro"));
        assert!(!is_org_managed_plan("team"));
    }

    #[test]
    fn non_enterprise_without_plan_usage_is_absent() {
        assert!(map_current_usage(
            Some("pro"),
            &period(PERIOD_ENTERPRISE_CAPPED),
            Some(1234.0),
            &cycle(PERIOD_ENTERPRISE_CAPPED),
        )
        .is_none());
    }

    #[test]
    fn disabled_account_is_absent() {
        let body = r#"{"enabled": false, "planUsage": {"limit": 1000, "totalSpend": 10}}"#;
        assert!(map_current_usage(Some("pro"), &period(body), None, &cycle(body)).is_none());
    }

    #[test]
    fn percent_only_plan_usage_maps_to_percent_meter() {
        let body = r#"{"planUsage": {"totalPercentUsed": 42.5}}"#;
        let mapped = map_current_usage(Some("pro"), &period(body), None, &cycle(body)).unwrap();
        match &mapped.meters[0] {
            UsageMeter::Metered { amount } => {
                assert_eq!(amount.used, 42.5);
                assert_eq!(amount.limit, Some(100.0));
                assert_eq!(amount.remaining, Some(57.5));
                assert_eq!(amount.unit, "%");
            }
            other => panic!("expected percent meter, got {other:?}"),
        }
    }

    #[test]
    fn plan_usage_derives_used_from_remaining_when_spend_is_absent() {
        let body = r#"{"planUsage": {"limit": 10000, "remaining": 4000}}"#;
        let mapped = map_current_usage(Some("pro"), &period(body), None, &cycle(body)).unwrap();
        match &mapped.meters[0] {
            UsageMeter::Metered { amount } => {
                assert_eq!(amount.used, 60.0);
                assert_eq!(amount.remaining, Some(40.0));
            }
            other => panic!("expected metered allowance, got {other:?}"),
        }
    }

    #[test]
    fn plan_usage_falls_back_to_included_spend() {
        let body = r#"{"planUsage": {"limit": 10000, "includedSpend": 2500}}"#;
        let mapped = map_current_usage(Some("pro"), &period(body), None, &cycle(body)).unwrap();
        match &mapped.meters[0] {
            UsageMeter::Metered { amount } => assert_eq!(amount.used, 25.0),
            other => panic!("expected metered allowance, got {other:?}"),
        }
    }

    #[test]
    fn string_encoded_numbers_map_identically_to_numeric() {
        let strings = map_current_usage(
            Some("pro"),
            &period(PERIOD_PRO_STRINGS),
            None,
            &cycle(PERIOD_PRO_STRINGS),
        )
        .unwrap();
        assert_eq!(strings.meters.len(), 1);
        match &strings.meters[0] {
            UsageMeter::Metered { amount } => {
                assert_eq!(amount.used, 232.22);
                assert_eq!(amount.limit, Some(400.0));
                assert_eq!(amount.remaining, Some(167.78));
                assert_eq!(amount.used_percent, Some(58.055));
            }
            other => panic!("expected metered allowance, got {other:?}"),
        }
    }

    #[test]
    fn negative_amounts_are_clamped_to_zero() {
        let body = r#"{"planUsage": {"limit": 10000, "totalSpend": -100, "remaining": -500}}"#;
        let mapped = map_current_usage(Some("pro"), &period(body), None, &cycle(body)).unwrap();
        match &mapped.meters[0] {
            UsageMeter::Metered { amount } => {
                assert_eq!(amount.used, 0.0);
                assert_eq!(amount.remaining, Some(0.0));
                assert_eq!(amount.used_percent, Some(0.0));
            }
            other => panic!("expected metered allowance, got {other:?}"),
        }

        let percent_body = r#"{"planUsage": {"totalPercentUsed": -5}}"#;
        let percent = map_current_usage(
            Some("pro"),
            &period(percent_body),
            None,
            &cycle(percent_body),
        )
        .unwrap();
        match &percent.meters[0] {
            UsageMeter::Metered { amount } => {
                assert_eq!(amount.used, 0.0);
                assert_eq!(amount.remaining, Some(100.0));
            }
            other => panic!("expected percent meter, got {other:?}"),
        }

        let aggregate = map_current_usage(
            Some("enterprise"),
            &period(PERIOD_ENTERPRISE_NO_LIMITS),
            Some(-100.0),
            &cycle(PERIOD_ENTERPRISE_NO_LIMITS),
        )
        .unwrap();
        match &aggregate.meters[0] {
            UsageMeter::NoIndividualLimit { amount } => assert_eq!(amount.used, 0.0),
            other => panic!("expected no-individual-limit spend, got {other:?}"),
        }
    }

    #[test]
    fn plan_usage_with_only_zero_limit_is_unusable() {
        let body = r#"{"planUsage": {"limit": 0, "totalSpend": 0}}"#;
        assert!(!period(body)
            .plan_usage
            .as_ref()
            .expect("planUsage present")
            .is_usable());
    }

    // ── WorkOS session cookie ─────────────────────────────────────────────

    fn jwt_with_subject(subject: &str) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(format!(r#"{{"sub":"{subject}"}}"#));
        format!("{header}.{payload}.signature")
    }

    #[test]
    fn workos_cookie_uses_the_jwt_subject_tail() {
        let token = jwt_with_subject("auth0|user_01ABC");
        let cookie = workos_session_cookie(&token).expect("cookie derived");
        assert!(cookie.starts_with("WorkosCursorSessionToken=user_01ABC%3A%3A"));
        assert!(cookie.ends_with(&token));
    }

    #[test]
    fn workos_cookie_percent_encodes_reserved_characters() {
        // A token whose trailing segment contains `;` and a space must not be
        // interpolated raw into the Cookie header.
        let token = format!("{}; injected", jwt_with_subject("auth0|user_01ABC"));
        let cookie = workos_session_cookie(&token).expect("cookie derived");
        assert!(!cookie.contains(';'), "raw ; leaked: {cookie}");
        assert!(!cookie.contains(' '), "raw space leaked: {cookie}");
        assert!(cookie.contains("%3B%20injected"), "got {cookie}");
    }

    #[test]
    fn workos_cookie_is_none_for_opaque_tokens() {
        assert!(workos_session_cookie("not-a-jwt").is_none());
        assert!(workos_session_cookie("a.b.c").is_none());
    }

    // ── Legacy fallback parsing ───────────────────────────────────────────

    #[test]
    fn legacy_response_maps_request_buckets() {
        let (windows, detail) = parse_legacy_usage_response(LEGACY_USAGE).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "Fast Requests");
        assert_eq!(windows[0].used_percent, Some(25.0));
        assert_eq!(
            windows[0].resets_at.as_deref(),
            Some("2026-09-01T00:00:00+00:00")
        );
        assert_eq!(
            detail.as_deref(),
            Some("375 of 500 fast requests remaining")
        );
    }

    #[test]
    fn legacy_response_without_limit_reports_requests_used() {
        let body = r#"{"gpt-4": {"numRequests": 88}, "startOfMonth": "2026-08-01T00:00:00Z"}"#;
        let (windows, detail) = parse_legacy_usage_response(body).unwrap();
        assert_eq!(windows[0].used_percent, None);
        assert_eq!(
            detail.as_deref(),
            Some("88 requests used this billing period")
        );
    }

    #[test]
    fn legacy_response_empty_body_lists_no_windows() {
        let (windows, detail) =
            parse_legacy_usage_response(r#"{"startOfMonth": "2026-08-01T00:00:00Z"}"#).unwrap();
        assert!(windows.is_empty());
        assert_eq!(detail.as_deref(), Some("No active Cursor usage windows"));
    }

    #[test]
    fn legacy_response_invalid_shape_is_a_shape_error() {
        assert!(matches!(
            parse_legacy_usage_response("not-json").unwrap_err(),
            UsageError::Shape(_)
        ));
    }

    #[test]
    fn next_month_reset_rolls_over_the_year() {
        assert_eq!(
            compute_next_month_reset("2026-12-01T00:00:00Z").unwrap(),
            "2027-01-01T00:00:00+00:00"
        );
    }

    // ── Credential discovery ──────────────────────────────────────────────

    #[test]
    fn sqlite_token_trims_padded_json_and_plain_values() {
        let dir = temp_dir("cursor_vscdb");
        let db_path = dir.join("state.vscdb");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable (key, value) VALUES ('cursorAuth/accessToken', '\"  test-jwt-token  \"')",
            [],
        )
        .unwrap();
        drop(conn);

        assert_eq!(
            read_cursor_sqlite_token(&db_path).unwrap(),
            "test-jwt-token"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sqlite_token_missing_key_is_no_credential() {
        let dir = temp_dir("cursor_vscdb_empty");
        let db_path = dir.join("state.vscdb");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        drop(conn);

        assert!(matches!(
            read_cursor_sqlite_token(&db_path).unwrap_err(),
            UsageError::NoCredential(_)
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn auth_json_accepts_camel_snake_and_plain_keys_and_trims() {
        let dir = temp_dir("cursor_auth_json");
        let path = dir.join("auth.json");

        fs::write(&path, r#"{"accessToken": "  tok-camel  "}"#).unwrap();
        assert_eq!(read_cursor_auth_json(&path).unwrap(), "tok-camel");

        fs::write(&path, r#"{"access_token": "tok-snake"}"#).unwrap();
        assert_eq!(read_cursor_auth_json(&path).unwrap(), "tok-snake");

        fs::write(&path, r#"{"token": "tok-plain"}"#).unwrap();
        assert_eq!(read_cursor_auth_json(&path).unwrap(), "tok-plain");

        fs::write(&path, r#"{"other": "value"}"#).unwrap();
        assert!(matches!(
            read_cursor_auth_json(&path).unwrap_err(),
            UsageError::NoCredential(_)
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn token_candidate_priority_prefers_env_then_db_then_json() {
        let dir = temp_dir("cursor_priority");
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

        assert_eq!(
            read_cursor_token_from_candidates(
                Some("env-token".to_string()),
                &[db_path.clone(), json_path.clone()],
            )
            .unwrap(),
            "env-token"
        );
        assert_eq!(
            read_cursor_token_from_candidates(None, &[db_path.clone(), json_path.clone()]).unwrap(),
            "db-token"
        );
        assert_eq!(
            read_cursor_token_from_candidates(None, &[dir.join("missing.vscdb"), json_path])
                .unwrap(),
            "json-token"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "{label}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ── Loopback orchestration ────────────────────────────────────────────

    /// A request the loopback server served, captured so tests can assert the
    /// exact method, path, headers and body the adapter put on the wire.
    #[derive(Debug, Clone)]
    struct Captured {
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl Captured {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(field, _)| field.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        }
    }

    type Recorder = Arc<Mutex<Vec<Captured>>>;

    /// Reuses the shared `spawn_loopback` helper (issue #1657 seam) so the
    /// worker thread is bounded by `max_requests` and terminates, rather than
    /// leaking a per-test infinite accept loop.
    fn loopback(
        responses: Vec<(&'static str, u16, &'static str)>,
        max_requests: usize,
    ) -> (u16, Recorder) {
        let recorder: Recorder = Arc::new(Mutex::new(Vec::new()));
        let recorder_for_thread = Arc::clone(&recorder);
        let port = crate::services::usage::spawn_loopback(
            max_requests,
            move |mut request: tiny_http::Request| {
                let method = request.method().as_str().to_string();
                let path = request.url().to_string();
                let headers = request
                    .headers()
                    .iter()
                    .map(|header| {
                        (
                            header.field.as_str().to_string(),
                            header.value.as_str().to_string(),
                        )
                    })
                    .collect();
                let mut body = String::new();
                let _ = std::io::Read::read_to_string(request.as_reader(), &mut body);
                recorder_for_thread.lock().unwrap().push(Captured {
                    method,
                    path: path.clone(),
                    headers,
                    body,
                });

                let (status, payload) = responses
                    .iter()
                    .find(|(suffix, _, _)| path.ends_with(*suffix))
                    .map(|(_, status, body)| (*status, *body))
                    .unwrap_or((404, "{}"));
                let _ = request
                    .respond(tiny_http::Response::from_string(payload).with_status_code(status));
            },
        );
        (port, recorder)
    }

    fn endpoints(port: u16) -> CursorEndpoints {
        let base = format!("http://127.0.0.1:{port}");
        CursorEndpoints {
            api: base.clone(),
            dashboard: base,
        }
    }

    fn captured(recorder: &Recorder) -> Vec<Captured> {
        recorder.lock().unwrap().clone()
    }

    fn find(recorder: &Recorder, suffix: &str) -> Captured {
        captured(recorder)
            .into_iter()
            .find(|request| request.path.ends_with(suffix))
            .unwrap_or_else(|| panic!("no request captured for {suffix}"))
    }

    #[test]
    fn current_flow_sends_connect_requests_and_skips_legacy() {
        let (port, recorder) = loopback(
            vec![
                (PLAN_INFO_PATH, 200, PLAN_ENTERPRISE),
                (PERIOD_USAGE_PATH, 200, PERIOD_PRO),
            ],
            2,
        );
        let usage = cursor_usage_with_token("test-token", &endpoints(port));

        assert!(usage.logged_in);
        assert!(usage.error.is_none());
        assert_eq!(usage.provider, "cursor");
        assert_eq!(usage.meters.len(), 1, "one allowance meter");
        assert!(usage.windows.is_empty());

        let requests = captured(&recorder);
        assert_eq!(requests.len(), 2, "plan + period, no legacy: {requests:?}");
        for request in &requests {
            assert_eq!(request.method, "POST", "{request:?}");
            assert_eq!(request.body, "{}", "{request:?}");
            assert_eq!(request.header("Connect-Protocol-Version"), Some("1"));
            assert_eq!(request.header("Content-Type"), Some("application/json"));
            assert_eq!(request.header("Authorization"), Some("Bearer test-token"));
        }
        assert!(requests.iter().any(|r| r.path.ends_with(PLAN_INFO_PATH)));
        assert!(requests.iter().any(|r| r.path.ends_with(PERIOD_USAGE_PATH)));
        assert!(!requests.iter().any(|r| r.path.ends_with(LEGACY_USAGE_PATH)));
    }

    #[test]
    fn plan_label_variant_with_uncapped_period_still_aggregates() {
        // Finding #2a: an org plan label that is not exactly "enterprise" must
        // still take the aggregate branch.
        let (port, recorder) = loopback(
            vec![
                (
                    PLAN_INFO_PATH,
                    200,
                    r#"{"planInfo":{"planName":"Enterprise Team"}}"#,
                ),
                (PERIOD_USAGE_PATH, 200, PERIOD_ENTERPRISE_UNCAPPED),
                (AGGREGATED_EVENTS_PATH, 200, EVENTS_SPEND),
                (LEGACY_USAGE_PATH, 200, LEGACY_USAGE),
            ],
            3,
        );
        let usage = cursor_usage_with_token("test-token", &endpoints(port));

        assert!(usage.logged_in);
        match &usage.meters[0] {
            UsageMeter::NoIndividualLimit { amount } => assert_eq!(amount.used, 987.65),
            other => panic!("expected no-individual-limit spend, got {other:?}"),
        }
        let requests = captured(&recorder);
        assert!(requests
            .iter()
            .any(|r| r.path.ends_with(AGGREGATED_EVENTS_PATH)));
        assert!(!requests.iter().any(|r| r.path.ends_with(LEGACY_USAGE_PATH)));
    }

    #[test]
    fn failed_plan_probe_still_aggregates_instead_of_falling_back() {
        // Finding #2b: a 5xx on the optional plan probe must not force the
        // legacy path for an org account.
        let (port, recorder) = loopback(
            vec![
                (PLAN_INFO_PATH, 500, "{}"),
                (PERIOD_USAGE_PATH, 200, PERIOD_ENTERPRISE_UNCAPPED),
                (AGGREGATED_EVENTS_PATH, 200, EVENTS_SPEND),
                (LEGACY_USAGE_PATH, 200, LEGACY_USAGE),
            ],
            3,
        );
        let usage = cursor_usage_with_token("test-token", &endpoints(port));

        assert!(usage.logged_in);
        match &usage.meters[0] {
            UsageMeter::NoIndividualLimit { amount } => assert_eq!(amount.used, 987.65),
            other => panic!("expected no-individual-limit spend, got {other:?}"),
        }
        assert!(!captured(&recorder)
            .iter()
            .any(|r| r.path.ends_with(LEGACY_USAGE_PATH)));
    }

    #[test]
    fn plan_unauthorized_does_not_block_the_required_period_call() {
        // Finding #7: a 401 on the optional plan route must not short-circuit
        // to logged-out; the required period call is the auth arbiter.
        let (port, recorder) = loopback(
            vec![
                (PLAN_INFO_PATH, 401, r#"{"error":"unauthorized"}"#),
                (PERIOD_USAGE_PATH, 200, PERIOD_PRO),
                (LEGACY_USAGE_PATH, 200, LEGACY_USAGE),
            ],
            2,
        );
        let usage = cursor_usage_with_token("test-token", &endpoints(port));

        assert!(usage.logged_in);
        assert!(usage.error.is_none());
        assert_eq!(usage.meters.len(), 1);
        let requests = captured(&recorder);
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().any(|r| r.path.ends_with(PERIOD_USAGE_PATH)));
        assert!(!requests.iter().any(|r| r.path.ends_with(LEGACY_USAGE_PATH)));
    }

    #[test]
    fn unauthorized_period_stays_logged_out_without_the_legacy_endpoint() {
        let (port, recorder) = loopback(
            vec![
                (PLAN_INFO_PATH, 200, PLAN_ENTERPRISE),
                (PERIOD_USAGE_PATH, 401, r#"{"error":"unauthorized"}"#),
                (LEGACY_USAGE_PATH, 200, LEGACY_USAGE),
            ],
            2,
        );
        let usage = cursor_usage_with_token("test-token", &endpoints(port));

        assert!(!usage.logged_in);
        assert!(usage
            .error
            .as_deref()
            .is_some_and(|error| error.contains("cursor-agent login")));
        assert!(
            !captured(&recorder)
                .iter()
                .any(|r| r.path.ends_with(LEGACY_USAGE_PATH)),
            "a rejected credential must not retry against the legacy endpoint"
        );
    }

    #[test]
    fn current_flow_failure_falls_back_to_legacy_in_order() {
        let (port, recorder) = loopback(
            vec![
                (PLAN_INFO_PATH, 500, "{}"),
                (PERIOD_USAGE_PATH, 500, "{}"),
                (LEGACY_USAGE_PATH, 200, LEGACY_USAGE),
            ],
            3,
        );
        let usage = cursor_usage_with_token("test-token", &endpoints(port));

        assert!(usage.logged_in);
        assert!(usage.error.is_none());
        assert_eq!(usage.windows.len(), 1);
        assert_eq!(usage.windows[0].label, "Fast Requests");

        let requests = captured(&recorder);
        let period_index = requests
            .iter()
            .position(|r| r.path.ends_with(PERIOD_USAGE_PATH))
            .expect("period usage attempted");
        let legacy_index = requests
            .iter()
            .position(|r| r.path.ends_with(LEGACY_USAGE_PATH))
            .expect("legacy fallback attempted");
        assert!(
            legacy_index > period_index,
            "legacy must run after the current flow: {requests:?}"
        );
        let legacy = find(&recorder, LEGACY_USAGE_PATH);
        assert_eq!(legacy.method, "GET");
        assert_eq!(legacy.header("User-Agent"), Some("Mozilla/5.0"));
        assert_eq!(legacy.header("Authorization"), Some("Bearer test-token"));
    }

    #[test]
    fn enterprise_uncapped_aggregate_request_carries_body_and_cookie() {
        let token = jwt_with_subject("auth0|user_01ABC");
        let (port, recorder) = loopback(
            vec![
                (PLAN_INFO_PATH, 200, PLAN_ENTERPRISE),
                (PERIOD_USAGE_PATH, 200, PERIOD_ENTERPRISE_UNCAPPED),
                (AGGREGATED_EVENTS_PATH, 200, EVENTS_SPEND),
                (LEGACY_USAGE_PATH, 200, LEGACY_USAGE),
            ],
            3,
        );
        let usage = cursor_usage_with_token(&token, &endpoints(port));

        assert!(usage.logged_in);
        match &usage.meters[0] {
            UsageMeter::NoIndividualLimit { amount } => assert_eq!(amount.used, 987.65),
            other => panic!("expected no-individual-limit spend, got {other:?}"),
        }

        let aggregate = find(&recorder, AGGREGATED_EVENTS_PATH);
        assert_eq!(aggregate.method, "POST");
        let body: serde_json::Value = serde_json::from_str(&aggregate.body).expect("json body");
        assert_eq!(body["teamId"], -1);
        assert_eq!(body["startDate"], 1751328000000i64);
        let end = body["endDate"].as_i64().expect("endDate number");
        assert!(end >= 1751328000000);
        assert_eq!(
            aggregate.header("Cookie"),
            Some(format!("WorkosCursorSessionToken=user_01ABC%3A%3A{token}").as_str())
        );
        assert!(aggregate
            .header("Content-Type")
            .is_some_and(|value| value.starts_with("application/json")));
        assert!(!captured(&recorder)
            .iter()
            .any(|r| r.path.ends_with(LEGACY_USAGE_PATH)));
    }

    #[test]
    fn enterprise_without_spend_limit_fetches_aggregate_events() {
        // Finding #3 end-to-end: no planUsage, no spendLimitUsage, aggregate 200.
        let (port, recorder) = loopback(
            vec![
                (PLAN_INFO_PATH, 200, PLAN_ENTERPRISE),
                (PERIOD_USAGE_PATH, 200, PERIOD_ENTERPRISE_NO_LIMITS),
                (AGGREGATED_EVENTS_PATH, 200, EVENTS_SPEND),
                (LEGACY_USAGE_PATH, 200, LEGACY_USAGE),
            ],
            3,
        );
        let usage = cursor_usage_with_token("test-token", &endpoints(port));

        assert!(usage.logged_in);
        match &usage.meters[0] {
            UsageMeter::NoIndividualLimit { amount } => assert_eq!(amount.used, 987.65),
            other => panic!("expected no-individual-limit spend, got {other:?}"),
        }
        assert!(!captured(&recorder)
            .iter()
            .any(|r| r.path.ends_with(LEGACY_USAGE_PATH)));
    }

    #[test]
    fn enterprise_zero_spend_maps_without_falling_back() {
        let (port, recorder) = loopback(
            vec![
                (PLAN_INFO_PATH, 200, PLAN_ENTERPRISE),
                (PERIOD_USAGE_PATH, 200, PERIOD_ENTERPRISE_NO_LIMITS),
                (AGGREGATED_EVENTS_PATH, 200, EVENTS_ZERO),
                (LEGACY_USAGE_PATH, 200, LEGACY_USAGE),
            ],
            3,
        );
        let usage = cursor_usage_with_token("test-token", &endpoints(port));

        assert!(usage.logged_in);
        assert!(usage.error.is_none());
        match &usage.meters[0] {
            UsageMeter::NoIndividualLimit { amount } => assert_eq!(amount.used, 0.0),
            other => panic!("expected zero no-individual-limit spend, got {other:?}"),
        }
        assert!(!captured(&recorder)
            .iter()
            .any(|r| r.path.ends_with(LEGACY_USAGE_PATH)));
    }

    #[test]
    fn adapter_fetch_returns_the_current_flow_result_end_to_end() {
        let (port, recorder) = loopback(
            vec![
                (PLAN_INFO_PATH, 200, PLAN_ENTERPRISE),
                (PERIOD_USAGE_PATH, 200, PERIOD_ENTERPRISE_UNCAPPED),
                (AGGREGATED_EVENTS_PATH, 200, EVENTS_SPEND),
                (LEGACY_USAGE_PATH, 200, LEGACY_USAGE),
            ],
            3,
        );
        let base = format!("http://127.0.0.1:{port}");

        let usage = with_cursor_loopback("test-token", &base, &base, || {
            CursorAdapter.fetch(&[]).into_usage("cursor")
        });

        assert_eq!(usage.provider, "cursor");
        assert!(usage.logged_in);
        assert_eq!(usage.meters.len(), 1);
        assert!(!captured(&recorder)
            .iter()
            .any(|r| r.path.ends_with(LEGACY_USAGE_PATH)));
    }

    #[test]
    fn failed_legacy_fallback_surfaces_unavailable() {
        let (port, _recorder) = loopback(
            vec![
                (PLAN_INFO_PATH, 500, "{}"),
                (PERIOD_USAGE_PATH, 500, "{}"),
                (LEGACY_USAGE_PATH, 500, "{}"),
            ],
            3,
        );
        let usage = cursor_usage_with_token("test-token", &endpoints(port));

        assert!(usage.logged_in, "credential present but the fetch failed");
        assert!(usage.error.is_some());
        assert_eq!(usage.provider, "cursor");
    }

    #[test]
    fn legacy_rate_limit_preserves_logged_in_state() {
        let (port, _recorder) = loopback(
            vec![
                (PLAN_INFO_PATH, 500, "{}"),
                (PERIOD_USAGE_PATH, 500, "{}"),
                (LEGACY_USAGE_PATH, 429, "{}"),
            ],
            3,
        );
        let usage = cursor_usage_with_token("test-token", &endpoints(port));

        assert!(usage.logged_in);
        assert!(usage
            .error
            .as_deref()
            .is_some_and(|error| error.contains("Rate limited")));
    }
}
