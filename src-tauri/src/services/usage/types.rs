//! Shared wire types for the Usage Meter seam (issue #1657).
//!
//! This module owns the data shapes every adapter speaks: [`ProviderUsage`],
//! [`UsageWindow`], [`BillingBalance`], [`ProviderMeters`], plus the
//! [`UsageError`] failure type and the two envelope constructors
//! ([`logged_out`] / [`unavailable`]) that let adapters report
//! "no credential" vs "credential present but fetch failed" precisely.
//!
//! Adapters import from here — never from the fetcher module — so the
//! catalog table can dispatch through the [`crate::services::usage::adapter::UsageAdapter`]
//! seam without pulling in any provider's HTTP code.

use serde::{Deserialize, Serialize};
use std::env;
use std::path::PathBuf;

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
/// status-level failures are handled inline in the shared fetch driver.
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

/// Builds a `ProviderUsage` envelope for the "no credential / bad
/// credential" state — the UI's re-enter affordance reads `error` verbatim.
pub(crate) fn logged_out(provider: &str, error: String) -> ProviderUsage {
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
/// or parse reason.
pub(crate) fn unavailable(provider: &str, error: String) -> ProviderUsage {
    ProviderUsage {
        provider: provider.to_string(),
        logged_in: true,
        windows: vec![],
        balance: None,
        detail: None,
        error: Some(error),
    }
}

/// Resolves the user's home directory. Prefers `USERPROFILE` on Windows so
/// `<home>/.config/...` is identical across the two platforms.
pub(crate) fn home_dir() -> PathBuf {
    env::var("USERPROFILE")
        .or_else(|_| env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default()
}
