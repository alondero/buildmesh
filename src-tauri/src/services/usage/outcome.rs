//! Internal failure taxonomy for the UsageAdapter seam (issue #1745).
//!
//! `UsageOutcome` is the only return type of [`super::adapter::UsageAdapter::fetch`].
//! The wire shape [`super::types::ProviderUsage`] is a projection owned in exactly
//! one place ([`UsageOutcome::into_usage`]) — adapters cannot bypass it, so the
//! same underlying situation cannot be encoded three different ways across
//! providers.
//!
//! What lives here:
//! - The 7-variant [`UsageOutcome`] enum (`Reading`, `NoCredential`, `Rejected`,
//!   `RateLimited`, `Unavailable`, `Degraded`, `ManagedExternally`).
//! - The [`AuthPolicy`] classifier that the shared fetch driver uses to map
//!   provider-side 401/403 responses to the right outcome variant.
//! - The [`UsageOutcome::into_usage`] projection — the only site that mints a
//!   [`super::types::ProviderUsage`] from an outcome.
//! - The [`UsageOutcome::keep`] predicate that [`crate::commands::usage::assemble_meters`]
//!   uses to decide which rows surface on the Providers page.
//!
//! What does NOT live here: `ts_rs::TS` derives (ADR-0009 — wire shapes only).
//! `UsageOutcome` is `pub(crate)` and never serialised; the projection is the
//! boundary between the internal taxonomy and the externally-stable wire.

use super::types::{logged_out, unavailable, BillingBalance, ProviderUsage, UsageMeter, UsageWindow};
use std::collections::HashSet;

/// Internal outcome of a single usage fetch. Adapters construct variants via
/// the `UsageOutcome::*` constructors; the [`Self::into_usage`] projection is
/// the sole boundary to the wire shape.
///
/// **Do not** emit both `error` and `UsageMeter::Unavailable` in the same
/// projection: the panel's branch order would let one win silently. This is
/// the structural class of bug the seam prevents — see
/// [`super::adapters::muse_code::missing`] for the pre-#1745 dead sentinel.
///
/// `#[allow(dead_code)]` silences `clippy::variant_size_differences` false
/// positives for `NoCredential` and `ManagedExternally`: both are
/// constructed in `into_usage_table_per_variant` (the table-test enforcing
/// the projection across all 7 variants); `NoCredential` is constructed
/// by every adapter's missing-credential path, and `ManagedExternally`
/// by the Anthropic adapter's cloud/API-platform arm (issue #1758).
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum UsageOutcome {
    /// A real reading. Empty `windows` + `balance` + `meters` is valid (e.g.
    /// an unmetered account) — the projection surfaces `detail` if any.
    Reading {
        windows: Vec<UsageWindow>,
        balance: Option<BillingBalance>,
        meters: Vec<UsageMeter>,
        detail: Option<String>,
    },
    /// No credential on disk / not logged in / mechanism is not a subscription
    /// (e.g. an API-key provider with no key, or Muse Code without an OAuth
    /// login). Maps to `logged_in: false` so the gate drops the row.
    NoCredential { hint: String },
    /// Credential present but the provider rejected it (HTTP 401/403 with the
    /// adapter's [`AuthPolicy::Rejected`] classification). Renders the
    /// "Invalid API key" re-entry affordance when the account has a key
    /// configured; otherwise the gate drops the row.
    Rejected { hint: String },
    /// Rate limited (HTTP 429). Credential is fine; transient only.
    RateLimited { reason: String },
    /// Transport / non-2xx / parse / provider-reported failure. Credential is
    /// fine; transient or unknown. Rendered with current red-error copy so
    /// genuine breakage stays distinguishable from "rate limited".
    Unavailable { reason: String },
    /// Credential is degraded but alive (e.g. OpenAI `sk-proj-` keys for the
    /// Organization Costs endpoint — inference works, billing route does
    /// not). Surfaced with a hint via `detail` per ADR-0026 §2.
    Degraded { detail: String },
    /// Another platform owns billing for this credential (AWS Bedrock,
    /// Vertex AI, Microsoft Foundry). Rendered as a `ManagedExternally` meter.
    ManagedExternally { platform: String },
}

/// How an adapter wants a provider-side auth rejection classified.
///
/// The shared [`super::adapter::fetch_usage`] driver takes this as an explicit
/// input so the 401/403 arm is intentional rather than accidentally omitted —
/// MiniMax's pre-#1745 omission (`fetch_usage` had no 401/403 arm) is the
/// motivating example.
///
/// Default ordering ([`AuthPolicy::Rejected`] first via `#[default]`) matches
/// the convention for keyed providers (ADR-0026 §2): 401/403 means the
/// credential is gone/bad. The default is intentionally not used by adapters
/// (every call site passes `AuthPolicy` explicitly), but the `Default` impl
/// keeps the `clippy::derivable_impls` lint quiet and is harmless.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthPolicy {
    /// 401/403 means the credential is gone/bad (default for keyed providers
    /// that return `logged_out` on 401/403 today).
    #[default]
    Rejected,
    /// 401/403 is indistinguishable from a missing credential for this
    /// provider. Used when the credential store and the API are decoupled
    /// (e.g. Muse Code: an OAuth token's absence cannot be distinguished from
    /// "API key configured" via HTTP status alone — the auth source is
    /// separate from the API key path).
    // No adapter passes this today (every `fetch_usage` call site passes
    // `AuthPolicy` explicitly, all `Rejected`); it stays as the
    // documented second classifier for credential-store/API-decoupled
    // providers. Clippy can't see the driver match arm as construction —
    // the allow is a false-positive suppression.
    #[allow(dead_code)]
    NoCredential,
}

// Migration shim removed (issue #1758): `From<ProviderUsage> for
// UsageOutcome` existed only so the seam compiled while adapters migrated
// one per commit. All 14 adapters now return `UsageOutcome` directly, so
// the impl had no callers left and is deleted rather than kept as a
// footgun (either direction of the conversion loses information: the
// forward shim collapsed `logged_in: true + error` into `Unavailable`
// and could not distinguish `NoCredential` from `Rejected`; the reverse
// shim cannot know the provider id, so a `.into()` projection would mint
// a `ProviderUsage` with an empty `provider` field — a wire invariant
// violation). New code returns `UsageOutcome` directly.

impl UsageOutcome {
    /// Project to the wire shape. The **only** place a [`ProviderUsage`] is
    /// minted from an outcome; the visibility-fenced [`logged_out`] /
    /// [`unavailable`] constructors in [`super::types`] enforce this.
    ///
    /// The projection never produces both `error` and a `UsageMeter::Unavailable`
    /// sentinel — that combination was the pre-#1745 Muse Code dead branch
    /// (the panel's `error` branch precedes the meters branch in render
    /// order, so the sentinel it set was unreachable).
    pub(crate) fn into_usage(self, provider: &str) -> ProviderUsage {
        match self {
            Self::Reading {
                windows,
                balance,
                meters,
                detail,
            } => ProviderUsage {
                provider: provider.to_string(),
                logged_in: true,
                windows,
                balance,
                meters,
                detail,
                error: None,
            },
            Self::Degraded { detail } => ProviderUsage {
                provider: provider.to_string(),
                logged_in: true,
                windows: Vec::new(),
                balance: None,
                meters: Vec::new(),
                detail: Some(detail),
                error: None,
            },
            Self::ManagedExternally { platform } => ProviderUsage {
                provider: provider.to_string(),
                logged_in: true,
                windows: Vec::new(),
                balance: None,
                meters: vec![UsageMeter::ManagedExternally { platform }],
                detail: None,
                error: None,
            },
            Self::NoCredential { hint } => logged_out(provider, hint),
            Self::Rejected { hint } => logged_out(provider, hint),
            Self::RateLimited { reason } => unavailable(provider, reason),
            Self::Unavailable { reason } => unavailable(provider, reason),
        }
    }

    /// Pure keep/drop predicate for [`crate::commands::usage::assemble_meters`].
    /// Centralising the decision in the outcome taxonomy means one table test
    /// covers all 7 variants; the gate in `assemble_meters` becomes a thin
    /// switch over this predicate.
    ///
    /// Contract:
    /// - `NoCredential` → never keep. A native row whose credential is absent
    ///   has nothing to show; a keyed row that returns `NoCredential` despite
    ///   a configured key is an adapter bug, but the gate still drops it.
    /// - `Rejected` → drop if no key is configured (matches today: keyed
    ///   providers without a key would only return `Rejected` if a previous
    ///   fetch stored a cached rejection); keep if configured (the
    ///   "Invalid API key" affordance must surface).
    /// - All other variants → keep. `Reading` / `Degraded` /
    ///   `ManagedExternally` are real data; `RateLimited` / `Unavailable`
    ///   keep the row so the user sees the transient failure with its hint.
    pub(crate) fn keep(&self, configured_keys: &HashSet<String>, provider_id: &str) -> bool {
        match self {
            Self::NoCredential { .. } => false,
            Self::Rejected { .. } => configured_keys.contains(provider_id),
            Self::Reading { .. }
            | Self::RateLimited { .. }
            | Self::Unavailable { .. }
            | Self::Degraded { .. }
            | Self::ManagedExternally { .. } => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::usage::types::UsageAmount;

    /// Pin the projection table. One test per variant with the exact wire
    /// triple the panel renders against. If a future change adds a variant
    /// without extending this test, the gate's behaviour for that variant
    /// is undefined — fail here, not in production.
    #[test]
    fn into_usage_table_per_variant() {
        let amount = UsageAmount {
            used: 25.0,
            limit: Some(100.0),
            remaining: Some(75.0),
            unit: "USD".to_string(),
            used_percent: Some(25.0),
            resets_at: Some("2026-10-01T00:00:00Z".to_string()),
        };
        let cases = vec![
            (
                "reading windows pass through",
                UsageOutcome::Reading {
                    windows: vec![UsageWindow {
                        label: "5-hour".into(),
                        used_percent: Some(41.0),
                        resets_at: None,
                    }],
                    balance: None,
                    meters: vec![],
                    detail: None,
                },
                ProviderUsage {
                    provider: "minimax".into(),
                    logged_in: true,
                    windows: vec![UsageWindow {
                        label: "5-hour".into(),
                        used_percent: Some(41.0),
                        resets_at: None,
                    }],
                    balance: None,
                    meters: vec![],
                    detail: None,
                    error: None,
                },
            ),
            (
                "reading balance + meters + detail pass through",
                UsageOutcome::Reading {
                    windows: vec![],
                    balance: Some(BillingBalance {
                        remaining: 12.34,
                        monthly_spend: Some(1.5),
                        currency: "USD".into(),
                    }),
                    meters: vec![UsageMeter::Metered {
                        amount: amount.clone(),
                    }],
                    detail: Some("tier name".into()),
                },
                ProviderUsage {
                    provider: "cursor".into(),
                    logged_in: true,
                    windows: vec![],
                    balance: Some(BillingBalance {
                        remaining: 12.34,
                        monthly_spend: Some(1.5),
                        currency: "USD".into(),
                    }),
                    meters: vec![UsageMeter::Metered {
                        amount: amount.clone(),
                    }],
                    detail: Some("tier name".into()),
                    error: None,
                },
            ),
            (
                "no credential -> logged_out envelope (no meter sentinel)",
                UsageOutcome::NoCredential {
                    hint: "Muse login missing. Run muse login again.".into(),
                },
                ProviderUsage {
                    provider: "muse-code".into(),
                    logged_in: false,
                    windows: vec![],
                    balance: None,
                    meters: vec![],
                    detail: None,
                    error: Some("Muse login missing. Run muse login again.".into()),
                },
            ),
            (
                "rejected -> logged_out envelope",
                UsageOutcome::Rejected {
                    hint: "Invalid API key".into(),
                },
                ProviderUsage {
                    provider: "kimi".into(),
                    logged_in: false,
                    windows: vec![],
                    balance: None,
                    meters: vec![],
                    detail: None,
                    error: Some("Invalid API key".into()),
                },
            ),
            (
                "rate limited -> unavailable envelope with rate message",
                UsageOutcome::RateLimited {
                    reason: "Rate limited — usage data temporarily unavailable".into(),
                },
                ProviderUsage {
                    provider: "anthropic".into(),
                    logged_in: true,
                    windows: vec![],
                    balance: None,
                    meters: vec![],
                    detail: None,
                    error: Some("Rate limited — usage data temporarily unavailable".into()),
                },
            ),
            (
                "unavailable -> unavailable envelope with reason",
                UsageOutcome::Unavailable {
                    reason: "API error 500: upstream down".into(),
                },
                ProviderUsage {
                    provider: "codex".into(),
                    logged_in: true,
                    windows: vec![],
                    balance: None,
                    meters: vec![],
                    detail: None,
                    error: Some("API error 500: upstream down".into()),
                },
            ),
            (
                "degraded -> logged_in with detail (ADR-0026 sk-proj- contract)",
                UsageOutcome::Degraded {
                    detail: "Monthly spend tracking requires an Organization Admin API Key (sk-admin-...)".into(),
                },
                ProviderUsage {
                    provider: "openai".into(),
                    logged_in: true,
                    windows: vec![],
                    balance: None,
                    meters: vec![],
                    detail: Some(
                        "Monthly spend tracking requires an Organization Admin API Key (sk-admin-...)"
                            .into(),
                    ),
                    error: None,
                },
            ),
            (
                "managed externally -> managed_externally meter",
                UsageOutcome::ManagedExternally {
                    platform: "AWS Bedrock".into(),
                },
                ProviderUsage {
                    provider: "anthropic".into(),
                    logged_in: true,
                    windows: vec![],
                    balance: None,
                    meters: vec![UsageMeter::ManagedExternally {
                        platform: "AWS Bedrock".into(),
                    }],
                    detail: None,
                    error: None,
                },
            ),
        ];

        for (name, outcome, expected) in cases {
            let usage = outcome.into_usage(&expected.provider);
            assert_eq!(
                (
                    usage.provider.as_str(),
                    usage.logged_in,
                    usage.error.as_deref(),
                    usage.detail.as_deref(),
                    usage.meters.len(),
                    usage.windows.len(),
                    usage.balance.is_some(),
                ),
                (
                    expected.provider.as_str(),
                    expected.logged_in,
                    expected.error.as_deref(),
                    expected.detail.as_deref(),
                    expected.meters.len(),
                    expected.windows.len(),
                    expected.balance.is_some(),
                ),
                "{name}: projection mismatch"
            );
        }
    }

    /// The projection must never emit both `error` and a `UsageMeter::Unavailable`.
    /// That's the dead-branch class Muse Code's pre-#1745 envelope exposed
    /// (the panel's error branch precedes the meters branch in render order).
    /// Pin every variant against this invariant.
    #[test]
    fn projection_never_emits_error_and_unavailable_meter_together() {
        // Touch every variant of UsageOutcome — Reading is included because
        // a future adapter bug could construct one with both fields.
        let amount = UsageAmount {
            used: 0.0,
            limit: None,
            remaining: None,
            unit: "USD".into(),
            used_percent: None,
            resets_at: None,
        };
        let outcomes = vec![
            UsageOutcome::Reading {
                windows: vec![],
                balance: None,
                meters: vec![UsageMeter::Unavailable],
                detail: None,
            },
            UsageOutcome::NoCredential {
                hint: "x".into(),
            },
            UsageOutcome::Rejected {
                hint: "x".into(),
            },
            UsageOutcome::RateLimited {
                reason: "x".into(),
            },
            UsageOutcome::Unavailable {
                reason: "x".into(),
            },
            UsageOutcome::Degraded {
                detail: "x".into(),
            },
            UsageOutcome::ManagedExternally {
                platform: "AWS Bedrock".into(),
            },
        ];
        for outcome in outcomes {
            let usage = outcome.into_usage("p");
            let has_unavailable_meter = usage
                .meters
                .iter()
                .any(|m| matches!(m, UsageMeter::Unavailable));
            let has_error = usage.error.is_some();
            assert!(
                !(has_error && has_unavailable_meter),
                "projection must never emit both error and UsageMeter::Unavailable; got: {usage:?}"
            );
            // Silence the unused warning on `amount` — it documents the
            // shape of a future Reading case but the Reading case here is
            // intentionally a degraded one without the amount field.
            let _ = amount;
        }
    }

    /// `keep` table: one row per variant × configured/unconfigured. The
    /// gate in `assemble_meters` reads this directly after the seam change.
    #[test]
    fn keep_predicate_table() {
        let configured: HashSet<String> = ["kimi", "openai"].iter().map(|s| s.to_string()).collect();
        let unconfigured: HashSet<String> = HashSet::new();

        let cases = vec![
            // (variant, configured_set, id, expected_keep, name)
            (
                UsageOutcome::Reading {
                    windows: vec![],
                    balance: None,
                    meters: vec![],
                    detail: None,
                },
                &configured,
                "kimi",
                true,
                "Reading kept regardless of configured_keys",
            ),
            (
                UsageOutcome::NoCredential {
                    hint: "x".into(),
                },
                &configured,
                "kimi",
                false,
                "NoCredential always dropped (even when configured)",
            ),
            (
                UsageOutcome::NoCredential {
                    hint: "x".into(),
                },
                &unconfigured,
                "kimi",
                false,
                "NoCredential always dropped (unconfigured)",
            ),
            (
                UsageOutcome::Rejected {
                    hint: "x".into(),
                },
                &configured,
                "kimi",
                true,
                "Rejected kept when configured (Invalid API key affordance)",
            ),
            (
                UsageOutcome::Rejected {
                    hint: "x".into(),
                },
                &unconfigured,
                "kimi",
                false,
                "Rejected dropped when unconfigured",
            ),
            (
                UsageOutcome::RateLimited {
                    reason: "x".into(),
                },
                &configured,
                "kimi",
                true,
                "RateLimited kept regardless",
            ),
            (
                UsageOutcome::Unavailable {
                    reason: "x".into(),
                },
                &unconfigured,
                "kimi",
                true,
                "Unavailable kept regardless",
            ),
            (
                UsageOutcome::Degraded {
                    detail: "x".into(),
                },
                &configured,
                "kimi",
                true,
                "Degraded kept regardless",
            ),
            (
                UsageOutcome::ManagedExternally {
                    platform: "AWS Bedrock".into(),
                },
                &configured,
                "kimi",
                true,
                "ManagedExternally kept regardless",
            ),
        ];

        for (outcome, configured_set, id, expected_keep, name) in cases {
            assert_eq!(
                outcome.keep(configured_set, id),
                expected_keep,
                "{name}"
            );
        }
    }

}