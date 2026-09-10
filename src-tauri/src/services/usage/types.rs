//! Shared wire types for the Usage Meter seam (issue #1657).
//!
//! This module owns the data shapes every adapter speaks: [`ProviderUsage`],
//! [`UsageWindow`], [`BillingBalance`], [`UsageMeter`], [`ProviderMeters`], plus the
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

/// Amounts reported for a capped budget or uncapped spend meter. `unit` is
/// deliberately broader than a currency code because providers may report
/// credits, tokens, requests, or money. Optional fields stay optional rather
/// than being derived: a missing limit is not zero and a missing percentage is
/// not an unavailable reading when the provider supplied an amount used.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS)]
#[ts(export, export_to = "UsageAmount.ts")]
pub struct UsageAmount {
    pub used: f64,
    pub limit: Option<f64>,
    pub remaining: Option<f64>,
    pub unit: String,
    #[serde(rename = "usedPercent")]
    #[ts(rename = "usedPercent")]
    pub used_percent: Option<f64>,
    #[serde(rename = "resetsAt")]
    #[ts(rename = "resetsAt")]
    pub resets_at: Option<String>,
}

/// An explicit provider-reported Usage Meter state. This supplements the
/// legacy percentage windows and wallet balance so providers can migrate one
/// at a time without treating a valid non-percentage response as missing data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export, export_to = "UsageMeter.ts")]
pub enum UsageMeter {
    /// Usage with a provider-enforced limit (for example a monthly budget).
    Metered { amount: UsageAmount },
    /// Spend is reported, but this account has no individual limit.
    NoIndividualLimit { amount: UsageAmount },
    /// The provider explicitly reports unlimited usage.
    Unlimited,
    /// Another platform owns usage and billing for this authentication source.
    ManagedExternally { platform: String },
    /// The provider genuinely supplied no usable usage information.
    Unavailable,
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
    /// New explicit meters. Kept alongside `windows` and `balance` so existing
    /// adapters remain source-compatible while provider migrations land.
    #[serde(default)]
    pub meters: Vec<UsageMeter>,
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
        meters: vec![],
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
        meters: vec![],
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_provider_usage_deserializes_without_new_fields() {
        let usage: ProviderUsage = serde_json::from_str(
            r#"{"provider":"anthropic","loggedIn":true,"windows":[],"balance":null,"detail":null,"error":null}"#,
        )
        .expect("legacy ProviderUsage should remain compatible");

        assert!(usage.meters.is_empty());
    }

    #[test]
    fn explicit_usage_states_have_stable_tagged_wire_shapes() {
        let amount = UsageAmount {
            used: 25.0,
            limit: Some(100.0),
            remaining: Some(75.0),
            unit: "USD".to_string(),
            used_percent: Some(25.0),
            resets_at: Some("2026-10-01T00:00:00Z".to_string()),
        };
        let states = vec![
            UsageMeter::Metered {
                amount: amount.clone(),
            },
            UsageMeter::NoIndividualLimit { amount },
            UsageMeter::Unlimited,
            UsageMeter::ManagedExternally {
                platform: "AWS Bedrock".to_string(),
            },
            UsageMeter::Unavailable,
        ];

        let value = serde_json::to_value(states).expect("serialize explicit usage states");
        assert_eq!(value[0]["state"], "metered");
        assert_eq!(value[0]["amount"]["used"], 25.0);
        assert_eq!(value[0]["amount"]["usedPercent"], 25.0);
        assert_eq!(value[1]["state"], "no_individual_limit");
        assert_eq!(value[2]["state"], "unlimited");
        assert_eq!(value[3]["state"], "managed_externally");
        assert_eq!(value[3]["platform"], "AWS Bedrock");
        assert_eq!(value[4]["state"], "unavailable");
    }
}
